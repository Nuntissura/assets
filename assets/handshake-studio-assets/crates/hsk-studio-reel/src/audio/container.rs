//! Codec dispatch hooks from the container readers to the audio codecs: compressed audio tracks of
//! ISO BMFF (MP4 / MOV / M4A) and Matroska / WebM decode through the shared delivery loop when a
//! `SegmentRequest` asks for `OutputMode::Decoded`. ISO BMFF: sample entries `mp4a` (`esds`:
//! objectTypeIndication 0x40 / 0x66-0x68 AAC, 0x69 / 0x6B MPEG audio), `.mp3`, `fLaC` (`dfLa`), `Opus`
//! (`dOps`); a single edit trims the encoder delay and sets the presented length. Matroska: `A_FLAC`,
//! `A_VORBIS`, `A_OPUS` (CodecDelay = pre-skip), `A_AAC` (+ the legacy `A_AAC/MPEG2|4/LC` ids),
//! `A_MPEG/L1|L2|L3`. Any other codec returns `None` so the container keeps its own typed
//! `MEDIA_CODEC_UNAVAILABLE`.
#![deny(clippy::arithmetic_side_effects)]
use super::aac::{aac_decoder, asc_from_esds};
use super::external::ExternalAudioDecoder;
use super::engine::{self, Ids, StreamShape};
use super::flac::{FlacDecoder, FlacLimits, streaminfo_from_config};
use super::mp3::{
    MP3_DECODER_ID, MP3_DECODER_VERSION, Mp3Decoder, frame_len, hdr_channels, hdr_frame_samples,
    hdr_sample_rate_hz, header_is_valid,
};
use super::ogg_audio::FlacPackets;
use super::opus::{OPUS_DECODER_ID, OPUS_DECODER_VERSION, OPUS_RATE, OpusHead, OpusPacketDecoder};
use super::packet::{Decode, PacketDecoder, PacketSource};
use super::vorbis::{VORBIS_DECODER_ID, VORBIS_DECODER_VERSION, VorbisDecoder, split_xiph_lacing};
use super::{FLAC_DECODER_ID, FLAC_DECODER_VERSION, MAX_AUDIO_CHANNELS};
use crate::error::ReelError;
use crate::isobmff::{SampleEntry, TrackIndex, read_sample};
use crate::matroska::MkvTrack;
use crate::mkv_content::ContentPlan;
use crate::model::*;
use crate::time::exact_ticks_per_unit;
use hsk_studio_accord::CancellationToken;

/// Largest codec configuration kept from a sound sample entry.
const MAX_CONFIG: usize = 64 * 1024;

/// Codec configuration children of an ISO BMFF / QuickTime sound sample entry, kept as
/// box type + payload: `esds`, `dfLa`, `dOps` (directly, or inside a QuickTime `wave` box). `rest` is the
/// entry after the fields of its sound description version (version 1 carries 16 more bytes first).
pub(crate) fn sound_entry_config(version: u16, rest: &[u8]) -> Vec<u8> {
    let children = if version == 1 {
        rest.get(16..).unwrap_or(&[])
    } else {
        rest
    };
    find_config(children, 0).unwrap_or_default()
}

fn find_config(mut data: &[u8], depth: u32) -> Option<Vec<u8>> {
    while data.len() >= 8 {
        let size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let kind = [data[4], data[5], data[6], data[7]];
        if size < 8 || size > data.len() {
            return None;
        }
        let payload = data.get(8..size)?;
        match &kind {
            b"esds" | b"dfLa" | b"dOps" if payload.len() <= MAX_CONFIG => {
                let mut out = kind.to_vec();
                out.extend_from_slice(payload);
                return Some(out);
            }
            b"wave" if depth < 2 => {
                if let Some(found) = find_config(payload, depth.saturating_add(1)) {
                    return Some(found);
                }
            }
            _ => {}
        }
        data = data.get(size..)?;
    }
    None
}

/// Samples of one track read in decode order (Matroska frames have their content encodings undone).
struct TrackPackets<'a> {
    src: &'a mut dyn ReadSeek,
    samples: &'a [SampleEntry],
    next: usize,
    limits: Limits,
    plan: Option<&'a ContentPlan>,
    scratch: Vec<u8>,
    bytes: u64,
}

impl PacketSource for TrackPackets<'_> {
    fn next_packet(&mut self, cancel: &CancellationToken) -> Result<Option<Vec<u8>>, ReelError> {
        crate::error::check_cancel(cancel)?;
        let Some(sample) = self.samples.get(self.next) else {
            return Ok(None);
        };
        self.next = self.next.saturating_add(1);
        let mut buf = Vec::new();
        read_sample(self.src, sample, &mut buf, &self.limits, 0)?;
        self.bytes = self.bytes.saturating_add(u64::from(sample.size));
        if let Some(plan) = self.plan
            && plan.has_frame_steps()
        {
            plan.decode_frame(&mut buf, &mut self.scratch, self.limits.max_chunk_bytes)?;
        }
        Ok(Some(buf))
    }

    fn bytes_read(&self) -> u64 {
        self.bytes
    }
}

/// MPEG audio frames carried in container samples (one or more whole frames per sample).
struct Mp3Packets {
    decoder: Mp3Decoder,
    channels: usize,
}

impl PacketDecoder for Mp3Packets {
    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, ReelError> {
        let mut at = 0usize;
        let mut frames = 0usize;
        while let Some(h) = packet
            .get(at..at.saturating_add(4))
            .and_then(|h| <[u8; 4]>::try_from(h).ok())
        {
            if !header_is_valid(&h) {
                return Err(ReelError::Corrupt {
                    what: "mpeg-audio-header",
                });
            }
            if usize::from(hdr_channels(&h)) != self.channels {
                return Err(ReelError::Unsupported {
                    what: "mpeg-audio-channel-change",
                });
            }
            let len = frame_len(&h, 0);
            let frame = packet
                .get(at..at.saturating_add(len))
                .ok_or(ReelError::Truncated { delivered: 0 })?;
            let before = out.len();
            let n = self.decoder.decode_frame(frame, out)?;
            let spf = hdr_frame_samples(&h);
            if n == 0 {
                out.resize(
                    before.saturating_add(spf.saturating_mul(self.channels)),
                    0.0,
                );
            }
            frames = frames.saturating_add(spf);
            at = at.saturating_add(len);
            if len == 0 {
                break;
            }
        }
        Ok(frames)
    }
}

/// A codec instance with the stream shape it produces.
struct Codec {
    rate: u32,
    channels: u16,
    pre_skip: u64,
    decoder: Box<dyn PacketDecoder>,
    ids: (&'static str, &'static str),
}

fn mpeg_audio_codec(first_sample: &[u8]) -> Result<Codec, ReelError> {
    let h = first_sample
        .get(..4)
        .and_then(|h| <[u8; 4]>::try_from(h).ok())
        .filter(header_is_valid)
        .ok_or(ReelError::Corrupt {
            what: "mpeg-audio-header",
        })?;
    let channels = hdr_channels(&h);
    Ok(Codec {
        rate: hdr_sample_rate_hz(&h),
        channels,
        pre_skip: 0,
        decoder: Box::new(Mp3Packets {
            decoder: Mp3Decoder::new(),
            channels: usize::from(channels),
        }),
        ids: (MP3_DECODER_ID, MP3_DECODER_VERSION),
    })
}

/// AAC-LC through the D24-A decoder choice (`aac::aac_decoder`: external OS decoder, else Studio's with
/// feature `studio-aac`, else `MEDIA_CODEC_UNAVAILABLE aac-lc`).
fn aac_codec(asc: &[u8], ext: &Ext<'_>) -> Result<Codec, ReelError> {
    let config = super::aac::parse_audio_specific_config(asc)?;
    let (decoder, ids) = aac_decoder(asc, ext.audio, ext.limits, ext.cancel)?;
    Ok(Codec {
        rate: config.sample_rate,
        channels: config.channels,
        pre_skip: 0,
        decoder,
        ids,
    })
}

/// What a codec choice needs beyond the configuration bytes: the route's external audio decoder.
struct Ext<'a> {
    audio: Option<&'a dyn ExternalAudioDecoder>,
    limits: &'a Limits,
    cancel: &'a CancellationToken,
}

fn flac_codec(config: &[u8]) -> Result<Codec, ReelError> {
    let limits = FlacLimits {
        max_channels: MAX_AUDIO_CHANNELS as u8,
        ..FlacLimits::default()
    };
    let info = streaminfo_from_config(config, &limits)?;
    Ok(Codec {
        rate: info.sample_rate,
        channels: u16::from(info.channels),
        pre_skip: 0,
        decoder: Box::new(FlacPackets(FlacDecoder::new(info, limits)?)),
        ids: (FLAC_DECODER_ID, FLAC_DECODER_VERSION),
    })
}

fn opus_codec(head: OpusHead) -> Result<Codec, ReelError> {
    Ok(Codec {
        rate: OPUS_RATE,
        channels: u16::from(head.channels),
        pre_skip: u64::from(head.pre_skip),
        decoder: Box::new(OpusPacketDecoder::new(&head)?),
        ids: (OPUS_DECODER_ID, OPUS_DECODER_VERSION),
    })
}

fn first_sample(
    src: &mut dyn ReadSeek,
    track: &TrackIndex,
    limits: &Limits,
) -> Result<Vec<u8>, ReelError> {
    let sample = track.samples().first().ok_or(ReelError::Corrupt {
        what: "audio-track-empty",
    })?;
    let mut buf = Vec::new();
    read_sample(src, sample, &mut buf, limits, 0)?;
    Ok(buf)
}

/// The ISO BMFF codec of `track`, `None` when it is not one of the audio codecs.
fn isobmff_codec(
    src: &mut dyn ReadSeek,
    track: &TrackIndex,
    limits: &Limits,
    ext: &Ext<'_>,
) -> Option<Result<Codec, ReelError>> {
    let config = track.codec_config.as_slice();
    let (tag, payload) = (config.get(..4), config.get(4..).unwrap_or(&[]));
    Some(match &track.codec {
        // Without its configuration box the codec is unidentified: the container's own
        // MEDIA_CODEC_UNAVAILABLE naming the sample entry stands (`None`).
        b"mp4a" => {
            if tag != Some(b"esds") {
                return None;
            }
            match asc_from_esds(payload) {
                Ok((0x40 | 0x66..=0x68, asc)) => aac_codec(&asc, ext),
                Ok((0x69 | 0x6B, _)) => {
                    first_sample(src, track, limits).and_then(|s| mpeg_audio_codec(&s))
                }
                Ok((oti, _)) => Err(ReelError::CodecUnavailable {
                    codec: format!("mp4a.{oti:02x}"),
                }),
                Err(e) => Err(e),
            }
        }
        b".mp3" => first_sample(src, track, limits).and_then(|s| mpeg_audio_codec(&s)),
        b"fLaC" => {
            if tag != Some(b"dfLa") {
                return None;
            }
            flac_codec(payload)
        }
        b"Opus" => {
            if tag != Some(b"dOps") {
                return None;
            }
            OpusHead::parse_dops(payload).and_then(opus_codec)
        }
        _ => return None,
    })
}

fn frames_of(ticks: i64, tps: u64) -> Result<u64, ReelError> {
    let t = u64::try_from(ticks).map_err(|_| ReelError::Corrupt {
        what: "audio-edit-list",
    })?;
    // Nearest frame (edit times converted from the media timescale may carry a rounding).
    Ok(t.saturating_add(tps / 2).checked_div(tps).unwrap_or(0))
}

/// Decoded delivery of a compressed audio track of an ISO BMFF file; `None` when the track's codec is
/// not an audio codec of this module.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_isobmff_track(
    src: &mut dyn ReadSeek,
    track: &TrackIndex,
    provider_id: &str,
    req: &SegmentRequest,
    limits: &Limits,
    cancel: &CancellationToken,
    sink: &mut dyn ChunkSink,
    audio: Option<&dyn ExternalAudioDecoder>,
) -> Option<Result<SegmentReceipt, ReelError>> {
    let ext = Ext {
        audio,
        limits,
        cancel,
    };
    let codec = match isobmff_codec(src, track, limits, &ext)? {
        Ok(codec) => codec,
        Err(e) => return Some(Err(e)),
    };
    Some((|| {
        let tps = exact_ticks_per_unit(codec.rate).ok_or(ReelError::Unsupported {
            what: "sample-rate-not-tick-exact",
        })?;
        // Decoded frames the media holds (media duration at the codec rate).
        let media = u128::from(track.media_duration)
            .saturating_mul(u128::from(codec.rate))
            .checked_div(u128::from(track.media_timescale))
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(0);
        // `EditSegment::media_time` is in media-timescale units; `duration` in ticks. A file without an
        // edit list carries one synthetic open-ended edit (duration i64::MAX).
        let (skip, total) = match track.edits.as_slice() {
            [edit] if edit.duration == i64::MAX && edit.presentation_start <= 0 => {
                (codec.pre_skip, media.saturating_sub(codec.pre_skip))
            }
            [edit] if edit.presentation_start == 0 => {
                let media_time = edit.media_time.ok_or(ReelError::Unsupported {
                    what: "audio-edit-list",
                })?;
                let skip = u128::try_from(media_time)
                    .ok()
                    .and_then(|m| m.checked_mul(u128::from(codec.rate)))
                    .and_then(|m| m.checked_div(u128::from(track.media_timescale)))
                    .and_then(|m| u64::try_from(m).ok())
                    .ok_or(ReelError::Corrupt {
                        what: "audio-edit-list",
                    })?;
                // The edit duration is in the movie timescale (often milliseconds): it may round past
                // the media; the media bounds what can be presented.
                (
                    skip,
                    frames_of(edit.duration, tps)?.min(media.saturating_sub(skip)),
                )
            }
            _ => {
                return Err(ReelError::Unsupported {
                    what: "audio-edit-list",
                });
            }
        };
        let shape = StreamShape {
            rate: codec.rate,
            channels: codec.channels,
            total: Some(total),
            skip,
        };
        engine::plan(&shape, req)?;
        let mut source = Decode {
            source: TrackPackets {
                src,
                samples: track.samples(),
                next: 0,
                limits: *limits,
                plan: None,
                scratch: Vec::new(),
                bytes: 0,
            },
            decoder: codec.decoder,
        };
        engine::deliver(
            &mut source,
            &shape,
            Ids {
                codec: provider_id.into(),
                decoder_id: codec.ids.0,
                decoder_version: codec.ids.1,
            },
            req,
            limits,
            cancel,
            sink,
        )
    })())
}

/// AudioSpecificConfig for the legacy Matroska ids without CodecPrivate (`A_AAC/MPEG2/LC`, ...).
fn legacy_aac_asc(rate: f64, channels: u16) -> Result<Vec<u8>, ReelError> {
    const RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];
    let index = RATES
        .iter()
        .position(|r| f64::from(*r) == rate)
        .ok_or(ReelError::Unsupported {
            what: "aac-explicit-sample-rate",
        })?;
    let config: u16 = match channels {
        1..=6 => channels,
        8 => 7,
        _ => {
            return Err(ReelError::Unsupported {
                what: "aac-channel-configuration",
            });
        }
    };
    let v: u16 = (2u16 << 11) | ((index as u16) << 7) | (config << 3);
    Ok(v.to_be_bytes().to_vec())
}

fn matroska_codec(
    src: &mut dyn ReadSeek,
    track: &MkvTrack,
    limits: &Limits,
    ext: &Ext<'_>,
) -> Option<Result<Codec, ReelError>> {
    let info = &track.info;
    let private = info.codec_private.as_slice();
    Some(match info.codec_id.as_str() {
        "A_FLAC" => flac_codec(private),
        "A_VORBIS" => split_xiph_lacing(private).and_then(|headers| match headers.as_slice() {
            [ident, _comment, setup] => {
                let decoder = VorbisDecoder::new(ident, setup)?;
                Ok(Codec {
                    rate: decoder.info.sample_rate,
                    channels: u16::from(decoder.info.channels),
                    pre_skip: 0,
                    decoder: Box::new(decoder),
                    ids: (VORBIS_DECODER_ID, VORBIS_DECODER_VERSION),
                })
            }
            _ => Err(ReelError::Corrupt {
                what: "vorbis-codec-private",
            }),
        }),
        "A_OPUS" => OpusHead::parse(private).and_then(opus_codec),
        "A_AAC" => aac_codec(private, ext),
        "A_AAC/MPEG2/LC" | "A_AAC/MPEG4/LC" => legacy_aac_asc(info.sampling_frequency, info.channels)
            .and_then(|asc| aac_codec(&asc, ext)),
        "A_MPEG/L1" | "A_MPEG/L2" | "A_MPEG/L3" => first_sample(src, &track.index, limits)
            .and_then(|s| {
                // Undo header stripping (mkvmerge strips MPEG audio frame headers) before parsing.
                let mut s = s;
                if let Ok(Some(plan)) = ContentPlan::from_encodings(&info.content_encodings) {
                    plan.decode_frame(&mut s, &mut Vec::new(), limits.max_chunk_bytes)?;
                }
                mpeg_audio_codec(&s)
            }),
        _ => return None,
    })
}

/// Decoded delivery of a compressed audio track of a Matroska / WebM file; `None` when the track's
/// CodecID is not an audio codec of this module. The stream has no declared total (a range past the
/// end is `MEDIA_TRUNCATED` with the delivered count); Opus `CodecDelay` is the pre-skip.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_matroska_track(
    src: &mut dyn ReadSeek,
    track: &MkvTrack,
    plan: &Result<Option<ContentPlan>, ReelError>,
    provider_id: &str,
    req: &SegmentRequest,
    limits: &Limits,
    cancel: &CancellationToken,
    sink: &mut dyn ChunkSink,
    audio: Option<&dyn ExternalAudioDecoder>,
) -> Option<Result<SegmentReceipt, ReelError>> {
    let ext = Ext {
        audio,
        limits,
        cancel,
    };
    let codec = match matroska_codec(src, track, limits, &ext)? {
        Ok(codec) => codec,
        Err(e) => return Some(Err(e)),
    };
    // A track whose content encodings cannot be undone (encryption, ...) is refused here, once it is
    // known to be an audio codec of this module.
    let plan = match plan {
        Ok(plan) => plan.as_ref(),
        Err(e) => return Some(Err(e.clone())),
    };
    Some((|| {
        let delay = u128::from(track.info.codec_delay_ns)
            .saturating_mul(u128::from(codec.rate))
            .saturating_add(500_000_000)
            .checked_div(1_000_000_000)
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(0);
        let shape = StreamShape {
            rate: codec.rate,
            channels: codec.channels,
            total: None,
            skip: if delay > 0 { delay } else { codec.pre_skip },
        };
        engine::plan(&shape, req)?;
        let mut source = Decode {
            source: TrackPackets {
                src,
                samples: track.index.samples(),
                next: 0,
                limits: *limits,
                plan,
                scratch: Vec::new(),
                bytes: 0,
            },
            decoder: codec.decoder,
        };
        engine::deliver(
            &mut source,
            &shape,
            Ids {
                codec: provider_id.into(),
                decoder_id: codec.ids.0,
                decoder_version: codec.ids.1,
            },
            req,
            limits,
            cancel,
            sink,
        )
    })())
}
