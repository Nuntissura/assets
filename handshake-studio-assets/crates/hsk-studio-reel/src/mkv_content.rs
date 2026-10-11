//! Matroska `ContentEncodings` (RFC 9559 section 5.1.4.1.31): header stripping and zlib compression are
//! undone for frame data (scope 0x1) and for `CodecPrivate` (scope 0x2); encryption and everything else
//! this reader cannot undo is a typed refusal for THAT track (other tracks stay usable).
//!
//! Element ids and semantics (RFC 9559): ContentEncodings 0x6D80, ContentEncoding 0x6240,
//! ContentEncodingOrder 0x5031 (default 0; decoders start with the HIGHEST value and work down, values are
//! unique), ContentEncodingScope 0x5032 (default 1; bit 0x1 = all frame contents excluding lacing data,
//! 0x2 = CodecPrivate, 0x4 = the next ContentEncoding's own data, which the RFC says players do not
//! support), ContentEncodingType 0x5033 (0 compression, 1 encryption), ContentCompression 0x5034
//! (present iff type is 0) with ContentCompAlgo 0x4254 (default 0: 0 zlib RFC 1950, 1 bzlib and 2 lzo1x
//! deprecated, 3 header stripping) and ContentCompSettings 0x4255 (for header stripping: the octets removed
//! from the start of every frame), ContentEncryption 0x5035 with ContentEncAlgo 0x47E1.
//!
//! Lacing data is never encoded: a laced block is split first and each frame is decoded on its own.
//!
//! Hostile input: the number of encodings, the stripped-header size and every decoded frame are bounded;
//! zlib output is capped (`decompress_to_vec_zlib_with_limit`) so a decompression bomb is a typed
//! `LimitExceeded`, never an unbounded allocation. All arithmetic is checked (V4-01).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::ReelError;
use crate::matroska::{elems, uint};
use crate::model::{Chunk, ChunkSink};
use miniz_oxide::inflate::{TINFLStatus, decompress_to_vec_zlib_with_limit};

const ID_CONTENT_ENCODING: u32 = 0x6240;
const ID_ORDER: u32 = 0x5031;
const ID_SCOPE: u32 = 0x5032;
const ID_TYPE: u32 = 0x5033;
const ID_COMPRESSION: u32 = 0x5034;
const ID_COMP_ALGO: u32 = 0x4254;
const ID_COMP_SETTINGS: u32 = 0x4255;
const ID_ENCRYPTION: u32 = 0x5035;
const ID_ENC_ALGO: u32 = 0x47E1;

/// More encodings than any real file stacks (mkvmerge writes one).
const MAX_CONTENT_ENCODINGS: usize = 16;
/// Largest header-stripping prefix accepted (real ones are a few bytes).
const MAX_STRIPPED_HEADER: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MkvCompression {
    /// ContentCompAlgo 0: a zlib stream (RFC 1950) per frame.
    Zlib,
    /// ContentCompAlgo 1 (deprecated, refused).
    Bzlib,
    /// ContentCompAlgo 2 (deprecated, refused).
    Lzo1x,
    /// ContentCompAlgo 3: these octets were removed from the start of every frame.
    HeaderStripping(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MkvEncodingKind {
    Compression(MkvCompression),
    /// ContentEncodingType 1; `algo` is ContentEncAlgo (5 = AES). Never undone here.
    Encryption {
        algo: u64,
    },
    /// Type 0 without a ContentCompression, or an unknown type/algorithm: not interpretable.
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvContentEncoding {
    pub order: u64,
    /// Scope bit field as stored (0x1 frames, 0x2 CodecPrivate, 0x4 next encoding).
    pub scope: u64,
    pub kind: MkvEncodingKind,
}

/// Parse the children of a `ContentEncodings` master. Structural EBML damage is `Corrupt`; semantic
/// oddities are recorded as `MkvEncodingKind::Malformed` and refused when the track is delivered.
pub(crate) fn parse_content_encodings(
    payload: &[u8],
) -> Result<Vec<MkvContentEncoding>, ReelError> {
    let mut out = Vec::new();
    for item in elems(payload) {
        let (id, body) = item?;
        if id != ID_CONTENT_ENCODING {
            continue;
        }
        if out.len() >= MAX_CONTENT_ENCODINGS {
            return Err(ReelError::LimitExceeded {
                what: "content-encoding-count",
            });
        }
        out.push(parse_one(body)?);
    }
    Ok(out)
}

fn parse_one(payload: &[u8]) -> Result<MkvContentEncoding, ReelError> {
    let (mut order, mut scope, mut kind_type) = (0u64, 1u64, 0u64);
    let (mut compression, mut algo, mut settings) = (false, 0u64, Vec::new());
    let mut encryption: Option<u64> = None;
    for item in elems(payload) {
        let (id, body) = item?;
        match id {
            ID_ORDER => order = uint(body)?,
            ID_SCOPE => scope = uint(body)?,
            ID_TYPE => kind_type = uint(body)?,
            ID_COMPRESSION => {
                compression = true;
                for field in elems(body) {
                    let (fid, fbody) = field?;
                    match fid {
                        ID_COMP_ALGO => algo = uint(fbody)?,
                        ID_COMP_SETTINGS => {
                            if fbody.len() > MAX_STRIPPED_HEADER {
                                return Err(ReelError::LimitExceeded {
                                    what: "content-encoding-settings",
                                });
                            }
                            settings = fbody.to_vec();
                        }
                        _ => {}
                    }
                }
            }
            ID_ENCRYPTION => {
                let mut enc_algo = 0u64;
                for field in elems(body) {
                    let (fid, fbody) = field?;
                    if fid == ID_ENC_ALGO {
                        enc_algo = uint(fbody)?;
                    }
                }
                encryption = Some(enc_algo);
            }
            _ => {}
        }
    }
    let kind = match (kind_type, compression, encryption) {
        (0, true, _) => match algo {
            0 => MkvEncodingKind::Compression(MkvCompression::Zlib),
            1 => MkvEncodingKind::Compression(MkvCompression::Bzlib),
            2 => MkvEncodingKind::Compression(MkvCompression::Lzo1x),
            3 => MkvEncodingKind::Compression(MkvCompression::HeaderStripping(settings)),
            _ => MkvEncodingKind::Malformed,
        },
        (1, _, enc) => MkvEncodingKind::Encryption {
            algo: enc.unwrap_or(0),
        },
        _ => MkvEncodingKind::Malformed,
    };
    Ok(MkvContentEncoding { order, scope, kind })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Zlib,
    /// Prepend these octets.
    Restore(Vec<u8>),
}

/// The decode pipeline of one track in decoding order (highest `ContentEncodingOrder` first).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ContentPlan {
    frame: Vec<Step>,
    private: Vec<Step>,
}

impl ContentPlan {
    /// `Ok(None)`: the track has no encodings. `Err`: an encoding this reader cannot undo, with the typed
    /// reason (encryption is `Unsupported content-encryption`).
    pub(crate) fn from_encodings(
        encodings: &[MkvContentEncoding],
    ) -> Result<Option<Self>, ReelError> {
        if encodings.is_empty() {
            return Ok(None);
        }
        if encodings
            .iter()
            .any(|e| matches!(e.kind, MkvEncodingKind::Encryption { .. }))
        {
            return Err(ReelError::Unsupported {
                what: "content-encryption",
            });
        }
        let mut ordered: Vec<&MkvContentEncoding> = encodings.iter().collect();
        ordered.sort_by_key(|encoding| std::cmp::Reverse(encoding.order));
        if ordered
            .windows(2)
            .any(|pair| pair[0].order == pair[1].order)
        {
            return Err(ReelError::Corrupt {
                what: "content-encoding-order",
            });
        }
        let mut plan = Self::default();
        for encoding in ordered {
            let step = match &encoding.kind {
                MkvEncodingKind::Compression(MkvCompression::Zlib) => Step::Zlib,
                MkvEncodingKind::Compression(MkvCompression::HeaderStripping(octets)) => {
                    Step::Restore(octets.clone())
                }
                MkvEncodingKind::Compression(MkvCompression::Bzlib | MkvCompression::Lzo1x) => {
                    return Err(ReelError::Unsupported {
                        what: "content-compression-algo",
                    });
                }
                MkvEncodingKind::Encryption { .. } => {
                    return Err(ReelError::Unsupported {
                        what: "content-encryption",
                    });
                }
                MkvEncodingKind::Malformed => {
                    return Err(ReelError::Unsupported {
                        what: "content-encoding",
                    });
                }
            };
            if encoding.scope == 0 || encoding.scope > 7 {
                return Err(ReelError::Corrupt {
                    what: "content-encoding-scope",
                });
            }
            if encoding.scope & 0x4 != 0 {
                return Err(ReelError::Unsupported {
                    what: "content-encoding-scope-next",
                });
            }
            if encoding.scope & 0x1 != 0 {
                plan.frame.push(step.clone());
            }
            if encoding.scope & 0x2 != 0 {
                plan.private.push(step);
            }
        }
        Ok(Some(plan))
    }

    pub(crate) fn has_frame_steps(&self) -> bool {
        !self.frame.is_empty()
    }

    pub(crate) fn has_private_steps(&self) -> bool {
        !self.private.is_empty()
    }

    /// Undo the frame-scope encodings in place. `scratch` is reused between calls; the decoded frame never
    /// exceeds `max` bytes (`LimitExceeded`), a damaged stream is `Corrupt`.
    pub(crate) fn decode_frame(
        &self,
        data: &mut Vec<u8>,
        scratch: &mut Vec<u8>,
        max: usize,
    ) -> Result<(), ReelError> {
        run(&self.frame, data, scratch, max)
    }

    /// Undo the private-scope encodings of `CodecPrivate`.
    pub(crate) fn decode_private(&self, data: &[u8], max: usize) -> Result<Vec<u8>, ReelError> {
        let (mut current, mut scratch) = (data.to_vec(), Vec::new());
        run(&self.private, &mut current, &mut scratch, max)?;
        Ok(current)
    }
}

fn too_large() -> ReelError {
    ReelError::LimitExceeded {
        what: "content-decoded-bytes",
    }
}

fn run(
    steps: &[Step],
    data: &mut Vec<u8>,
    scratch: &mut Vec<u8>,
    max: usize,
) -> Result<(), ReelError> {
    for step in steps {
        match step {
            Step::Restore(octets) => {
                let total = octets.len().checked_add(data.len()).ok_or_else(too_large)?;
                if total > max {
                    return Err(too_large());
                }
                scratch.clear();
                scratch.extend_from_slice(octets);
                scratch.extend_from_slice(data);
                std::mem::swap(data, scratch);
            }
            Step::Zlib => {
                let decoded = decompress_to_vec_zlib_with_limit(data, max).map_err(|error| {
                    if error.status == TINFLStatus::HasMoreOutput {
                        too_large()
                    } else {
                        ReelError::Corrupt {
                            what: "content-zlib",
                        }
                    }
                })?;
                *data = decoded;
            }
        }
    }
    if data.len() > max {
        return Err(too_large());
    }
    Ok(())
}

/// Chunk sink that undoes a track's frame encodings before the packet reaches the real sink.
pub(crate) struct DecodeSink<'a> {
    inner: &'a mut dyn ChunkSink,
    plan: &'a ContentPlan,
    max: usize,
    frame: Vec<u8>,
    scratch: Vec<u8>,
}

impl<'a> DecodeSink<'a> {
    pub(crate) fn new(inner: &'a mut dyn ChunkSink, plan: &'a ContentPlan, max: usize) -> Self {
        Self {
            inner,
            plan,
            max,
            frame: Vec::new(),
            scratch: Vec::new(),
        }
    }
}

impl ChunkSink for DecodeSink<'_> {
    fn accept(&mut self, chunk: Chunk<'_>) -> Result<(), ReelError> {
        self.frame.clear();
        self.frame.extend_from_slice(chunk.bytes);
        self.plan
            .decode_frame(&mut self.frame, &mut self.scratch, self.max)?;
        self.inner.accept(Chunk {
            bytes: &self.frame,
            ..chunk
        })
    }
}
