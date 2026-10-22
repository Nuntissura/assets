//! AV1 encode through rav1e 0.8.1 (BSD-2-Clause + AOMedia Patent License 1.0; cargo feature `av1-encode`,
//! default off; lead-approved dependency, lane SMB-REEL-ENCODE). D24 ships AV1.
//!
//! The encoder only ever sees frames Studio rendered itself (trusted input), runs in process and is
//! wrapped in `catch_unwind`: a rav1e panic becomes the typed `Unsupported { what: "av1-encoder-fault" }`
//! and the encoder refuses further use. Every bound (raster, pixel count, bit depth, chroma layout,
//! frame rate, bitrate, quantizer, speed, plane sizes) is checked before rav1e allocates anything.
//!
//! Output: one temporal unit per shown frame in presentation order (rav1e never reorders shown frames;
//! `frame_index` is the input frame number). [`Av1Encoder::config_record`] builds the ISO BMFF `av1C`
//! record from rav1e's container header plus the sequence header OBU of the first temporal unit
//! (AV1-ISOBMFF 2.3: configOBUs carry the sequence header).
#![deny(clippy::arithmetic_side_effects)]

pub use crate::export::Av1Settings;
use crate::error::ReelError;
use crate::model::{ChromaFormat, VideoFormat};
use crate::mux::video_timescale;
use rav1e::prelude::{
    ChromaSampling, ColorDescription, ColorPrimaries, Config, Context, EncoderConfig, EncoderStatus,
    FrameType, MatrixCoefficients, PixelRange, Rational, SpeedSettings, TransferCharacteristics,
};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub const AV1_ENCODER_ID: &str = "rav1e";
pub const AV1_ENCODER_VERSION: &str = "0.8.1";
pub const AV1_ENCODER_LICENCE: &str = "BSD-2-Clause + AOMedia-Patent-1.0";

/// Largest raster side and pixel count accepted (8K UHD class).
pub const AV1_MAX_SIDE: u32 = 8192;
pub const AV1_MAX_PIXELS: u64 = 8192 * 4352;
/// Largest target bitrate (bits per second) a recipe may ask for.
pub const AV1_MAX_BITRATE: u32 = 1_000_000_000;

fn unsupported(what: &'static str) -> ReelError {
    ReelError::Unsupported { what }
}

fn limit(what: &'static str) -> ReelError {
    ReelError::LimitExceeded { what }
}

/// One encoded temporal unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Av1Packet {
    pub data: Vec<u8>,
    /// Input (= presentation) frame number.
    pub frame_index: u64,
    pub keyframe: bool,
}

enum Ctx {
    Eight(Context<u8>),
    Wide(Context<u16>),
}

pub struct Av1Encoder {
    ctx: Option<Ctx>,
    format: VideoFormat,
    container_header: Vec<u8>,
    frames_in: u64,
    flushed: bool,
}

fn chroma_sampling(chroma: ChromaFormat) -> Result<ChromaSampling, ReelError> {
    Ok(match chroma {
        ChromaFormat::Yuv420 => ChromaSampling::Cs420,
        ChromaFormat::Yuv422 => ChromaSampling::Cs422,
        ChromaFormat::Yuv444 => ChromaSampling::Cs444,
        ChromaFormat::Mono => return Err(unsupported("av1-encode-mono")),
    })
}

/// H.273 code points rav1e can signal; anything else is written as unspecified (no colour description).
fn color_description(format: &VideoFormat) -> Option<ColorDescription> {
    let c = format.color;
    let primaries = match c.primaries {
        1 => ColorPrimaries::BT709,
        9 => ColorPrimaries::BT2020,
        12 => ColorPrimaries::SMPTE432,
        _ => return None,
    };
    let transfer = match c.transfer {
        1 => TransferCharacteristics::BT709,
        13 => TransferCharacteristics::SRGB,
        16 => TransferCharacteristics::SMPTE2084,
        18 => TransferCharacteristics::HLG,
        _ => return None,
    };
    let matrix = match c.matrix {
        0 => MatrixCoefficients::Identity,
        1 => MatrixCoefficients::BT709,
        9 => MatrixCoefficients::BT2020NCL,
        _ => return None,
    };
    Some(ColorDescription {
        color_primaries: primaries,
        transfer_characteristics: transfer,
        matrix_coefficients: matrix,
    })
}

fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, ReelError> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| unsupported("av1-encoder-fault"))
}

impl Av1Encoder {
    /// Checks every bound, then configures rav1e for `format` at `ticks_per_frame`.
    pub fn new(
        format: VideoFormat,
        ticks_per_frame: u64,
        settings: Av1Settings,
    ) -> Result<Self, ReelError> {
        let (w, h) = (format.width, format.height);
        if w == 0 || h == 0 {
            return Err(unsupported("av1-encode-zero-dimension"));
        }
        if w > AV1_MAX_SIDE
            || h > AV1_MAX_SIDE
            || u64::from(w).saturating_mul(u64::from(h)) > AV1_MAX_PIXELS
        {
            return Err(limit("av1-encode-dimensions"));
        }
        if !matches!(format.bit_depth, 8 | 10) {
            return Err(unsupported("av1-encode-bit-depth"));
        }
        let sampling = chroma_sampling(format.chroma)?;
        if (format.chroma != ChromaFormat::Yuv444 && !w.is_multiple_of(2))
            || (format.chroma == ChromaFormat::Yuv420 && !h.is_multiple_of(2))
        {
            return Err(unsupported("av1-encode-odd-dimension"));
        }
        if settings.speed > 10 {
            return Err(unsupported("av1-encode-speed"));
        }
        if settings.max_keyframe_interval == 0 {
            return Err(unsupported("av1-encode-keyframe-interval"));
        }
        let bitrate = match settings.bitrate {
            None => 0i32,
            Some(0) => return Err(unsupported("av1-encode-zero-bitrate")),
            Some(b) if b > AV1_MAX_BITRATE => return Err(limit("av1-encode-bitrate")),
            Some(b) => i32::try_from(b).map_err(|_| limit("av1-encode-bitrate"))?,
        };
        let (timescale, delta) = video_timescale(ticks_per_frame)?;
        let enc = EncoderConfig {
            width: usize::try_from(w).map_err(|_| limit("av1-encode-dimensions"))?,
            height: usize::try_from(h).map_err(|_| limit("av1-encode-dimensions"))?,
            time_base: Rational::new(u64::from(delta), u64::from(timescale)),
            bit_depth: usize::from(format.bit_depth),
            chroma_sampling: sampling,
            pixel_range: if format.color.full_range {
                PixelRange::Full
            } else {
                PixelRange::Limited
            },
            color_description: color_description(&format),
            max_key_frame_interval: u64::from(settings.max_keyframe_interval),
            quantizer: usize::from(settings.quantizer),
            bitrate,
            speed_settings: SpeedSettings::from_preset(settings.speed),
            ..EncoderConfig::default()
        };
        let config = Config::new().with_encoder_config(enc);
        let ctx = guarded(|| {
            if format.bit_depth == 8 {
                config.new_context::<u8>().map(Ctx::Eight)
            } else {
                config.new_context::<u16>().map(Ctx::Wide)
            }
        })?
        .map_err(|_| unsupported("av1-encode-config-rejected"))?;
        let container_header = guarded(|| match &ctx {
            Ctx::Eight(c) => c.container_sequence_header(),
            Ctx::Wide(c) => c.container_sequence_header(),
        })?;
        Ok(Self {
            ctx: Some(ctx),
            format,
            container_header,
            frames_in: 0,
            flushed: false,
        })
    }

    pub fn format(&self) -> &VideoFormat {
        &self.format
    }

    /// The `av1C` record: rav1e's 4-byte container header plus the sequence header OBU found in
    /// `first_unit` (the first temporal unit), when it carries one.
    pub fn config_record(&self, first_unit: &[u8]) -> Vec<u8> {
        let mut record = self.container_header.clone();
        if let Some(obu) = sequence_header_obu(first_unit) {
            record.extend_from_slice(obu);
        }
        record
    }

    fn check_planes(&self, planes: &[&[u8]]) -> Result<(), ReelError> {
        let dims = self.format.plane_dims();
        if planes.len() != dims.len() {
            return Err(unsupported("av1-encode-planes"));
        }
        let bps = self.format.bytes_per_sample() as u64;
        for (plane, (w, h)) in planes.iter().zip(dims) {
            let expected = u64::from(w)
                .checked_mul(u64::from(h))
                .and_then(|n| n.checked_mul(bps))
                .ok_or(limit("av1-encode-dimensions"))?;
            if plane.len() as u64 != expected {
                return Err(unsupported("av1-encode-plane-size"));
            }
        }
        Ok(())
    }

    /// Submit one frame (planes Y, U, V in `VideoFormat` delivery layout) and collect every temporal unit
    /// that became ready.
    pub fn encode(&mut self, planes: &[&[u8]], out: &mut Vec<Av1Packet>) -> Result<(), ReelError> {
        if self.flushed {
            return Err(unsupported("av1-encode-after-finish"));
        }
        self.check_planes(planes)?;
        let dims = self.format.plane_dims();
        let bps = self.format.bytes_per_sample();
        let ctx = self.ctx.as_mut().ok_or(unsupported("av1-encoder-fault"))?;
        let sent = guarded(|| -> Result<(), EncoderStatus> {
            match ctx {
                Ctx::Eight(c) => {
                    let mut frame = c.new_frame();
                    for ((dst, src), (w, _)) in frame.planes.iter_mut().zip(planes).zip(&dims) {
                        dst.copy_from_raw_u8(src, (*w as usize).saturating_mul(bps), bps);
                    }
                    c.send_frame(frame)
                }
                Ctx::Wide(c) => {
                    let mut frame = c.new_frame();
                    for ((dst, src), (w, _)) in frame.planes.iter_mut().zip(planes).zip(&dims) {
                        dst.copy_from_raw_u8(src, (*w as usize).saturating_mul(bps), bps);
                    }
                    c.send_frame(frame)
                }
            }
        });
        match sent {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err(unsupported("av1-encode-send-refused")),
            Err(e) => {
                self.ctx = None;
                return Err(e);
            }
        }
        self.frames_in = self.frames_in.checked_add(1).ok_or(limit("av1-encode-frames"))?;
        self.drain(out)
    }

    /// Flush and collect the remaining temporal units.
    pub fn finish(&mut self, out: &mut Vec<Av1Packet>) -> Result<(), ReelError> {
        if !self.flushed {
            let ctx = self.ctx.as_mut().ok_or(unsupported("av1-encoder-fault"))?;
            let flushed = guarded(|| match ctx {
                Ctx::Eight(c) => c.flush(),
                Ctx::Wide(c) => c.flush(),
            });
            if let Err(e) = flushed {
                self.ctx = None;
                return Err(e);
            }
            self.flushed = true;
        }
        self.drain(out)
    }

    fn drain(&mut self, out: &mut Vec<Av1Packet>) -> Result<(), ReelError> {
        loop {
            let ctx = self.ctx.as_mut().ok_or(unsupported("av1-encoder-fault"))?;
            let received = guarded(|| match ctx {
                Ctx::Eight(c) => c
                    .receive_packet()
                    .map(|p| (p.data, p.input_frameno, p.frame_type == FrameType::KEY)),
                Ctx::Wide(c) => c
                    .receive_packet()
                    .map(|p| (p.data, p.input_frameno, p.frame_type == FrameType::KEY)),
            });
            match received {
                Err(e) => {
                    self.ctx = None;
                    return Err(e);
                }
                Ok(Ok((data, frame_index, keyframe))) => out.push(Av1Packet {
                    data,
                    frame_index,
                    keyframe,
                }),
                Ok(Err(EncoderStatus::Encoded)) => continue,
                Ok(Err(EncoderStatus::NeedMoreData | EncoderStatus::LimitReached)) => {
                    return Ok(());
                }
                Ok(Err(_)) => return Err(unsupported("av1-encoder-fault")),
            }
        }
    }

    /// Frames submitted so far.
    pub fn frames_in(&self) -> u64 {
        self.frames_in
    }
}

/// The first OBU of type OBU_SEQUENCE_HEADER (1) of a temporal unit (AV1 5.3: header byte, optional
/// extension byte, LEB128 size when `obu_has_size_field`). Bounded: a unit whose OBUs do not parse yields
/// `None` (no configOBUs), never a read past the slice.
fn sequence_header_obu(unit: &[u8]) -> Option<&[u8]> {
    let mut at = 0usize;
    while at < unit.len() {
        let header = *unit.get(at)?;
        let obu_type = (header >> 3) & 0x0F;
        let has_extension = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        let mut cursor = at.checked_add(1)?.checked_add(usize::from(has_extension))?;
        if !has_size {
            return None;
        }
        let mut size = 0u64;
        for i in 0..8u32 {
            let byte = *unit.get(cursor)?;
            cursor = cursor.checked_add(1)?;
            size |= u64::from(byte & 0x7F) << i.checked_mul(7)?;
            if byte & 0x80 == 0 {
                break;
            }
        }
        let end = cursor.checked_add(usize::try_from(size).ok()?)?;
        if end > unit.len() {
            return None;
        }
        if obu_type == 1 {
            return unit.get(at..end);
        }
        at = end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_header_obu_is_found_and_bounded() {
        // temporal delimiter (type 2, size 0) then a 3-byte sequence header (type 1).
        let unit = [0x12, 0x00, 0x0A, 0x03, 0xAA, 0xBB, 0xCC, 0x32, 0x00];
        assert_eq!(sequence_header_obu(&unit), Some(&unit[2..7]));
        // A size running past the slice yields nothing.
        assert_eq!(sequence_header_obu(&[0x0A, 0x7F, 0x00]), None);
        assert_eq!(sequence_header_obu(&[0x0A, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]), None);
        assert_eq!(sequence_header_obu(&[]), None);
    }
}
