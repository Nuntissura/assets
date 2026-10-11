//! The only module of this crate that uses `unsafe`: a thin owner of one rav1d decoder context.
//!
//! rav1d 1.1.0 (BSD-2-Clause, Rust port of dav1d) exposes the dav1d C ABI as Rust functions
//! (`rav1d::src::lib::dav1d_*`); its Rust API is not published yet. Everything unsafe is confined to this
//! file: opening/closing the context, handing it owned packet buffers, and copying a decoded picture
//! into plain owned byte planes. Callers see only safe types.
#![allow(unsafe_code)]

use crate::error::ReelError;
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422,
    DAV1D_PIXEL_LAYOUT_I444,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{
    dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_get_picture,
    dav1d_open, dav1d_picture_unref, dav1d_send_data, rav1d_version,
};
use std::mem::MaybeUninit;
use std::ptr::NonNull;

/// Pixel layout code of a picture (`I400`..`I444`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Layout {
    Mono,
    I420,
    I422,
    I444,
}

/// Colour description of the sequence header the picture was decoded under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SequenceColor {
    /// `color_description_present_flag`: when false the code points below are "unspecified" (2).
    pub described: bool,
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

/// One decoded picture copied out of the decoder: tight planes, no pointers into rav1d.
pub(super) struct RawPicture {
    pub tag: u64,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub layout: Layout,
    pub color: SequenceColor,
    pub film_grain_applied: bool,
    /// One byte per sample at 8 bits, little-endian u16 above; Y then U then V (Y only for `Mono`).
    pub planes: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SendStatus {
    Accepted,
    /// The decoder still holds an undelivered picture: pull pictures, then send again.
    Full,
}

pub(super) fn version() -> &'static str {
    rav1d_version()
}

/// Map a negative dav1d result (`-errno`) to a typed failure. The classification is stable across
/// platforms because it compares against the platform's own errno constants.
fn map_error(result: i32) -> ReelError {
    let errno = result.checked_neg().unwrap_or(i32::MAX);
    if errno == libc::ENOMEM || errno == libc::ERANGE {
        ReelError::LimitExceeded {
            what: "av1-frame-resources",
        }
    } else if errno == libc::ENOPROTOOPT {
        ReelError::Unsupported {
            what: "av1-unsupported-stream-feature",
        }
    } else {
        ReelError::Corrupt {
            what: "av1-bitstream",
        }
    }
}

/// An open rav1d context. Deterministic configuration: one thread, one frame of delay, film grain
/// applied, operating point 0, highest spatial layer only, logging off.
pub(super) struct Rav1d {
    ctx: Option<Dav1dContext>,
    max_frame_bytes: usize,
}

impl Rav1d {
    /// `frame_size_limit_px` bounds width*height before rav1d allocates a picture (0 = unlimited);
    /// `max_frame_bytes` bounds one packed output frame.
    pub(super) fn open(
        frame_size_limit_px: u32,
        max_frame_bytes: usize,
    ) -> Result<Self, ReelError> {
        let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: `dav1d_default_settings` fully initialises the pointed-to settings.
        unsafe { dav1d_default_settings(NonNull::new_unchecked(settings.as_mut_ptr())) };
        // SAFETY: initialised by the call above.
        let mut settings = unsafe { settings.assume_init() };
        settings.n_threads = 1;
        settings.max_frame_delay = 1;
        settings.apply_grain = 1;
        settings.operating_point = 0;
        settings.all_layers = 0;
        settings.frame_size_limit = frame_size_limit_px;
        // SAFETY: the logger is a pair of nullable pointers; all-zero bytes is `{ cookie: None,
        // callback: None }`, which disables logging (the type itself is not nameable outside rav1d).
        unsafe { std::ptr::write_bytes(std::ptr::addr_of_mut!(settings.logger), 0, 1) };
        let mut ctx: Option<Dav1dContext> = None;
        // SAFETY: both pointers are valid for the call; `ctx` receives the new context.
        let result = unsafe {
            dav1d_open(
                Some(NonNull::from(&mut ctx)),
                Some(NonNull::from(&mut settings)),
            )
        };
        if result.0 != 0 || ctx.is_none() {
            return Err(map_error(if result.0 < 0 {
                result.0
            } else {
                -libc::EINVAL
            }));
        }
        Ok(Self {
            ctx,
            max_frame_bytes,
        })
    }

    /// Hand one temporal unit to the decoder in a buffer owned by rav1d (so nothing it keeps can
    /// dangle). `tag` comes back on the picture.
    pub(super) fn send_data(&mut self, bytes: &[u8], tag: u64) -> Result<SendStatus, ReelError> {
        let mut data = Dav1dData::default();
        // SAFETY: `data` is valid to write; on success `data.data` points to `bytes.len()` writable bytes.
        let buffer = unsafe { dav1d_data_create(Some(NonNull::from(&mut data)), bytes.len()) };
        if buffer.is_null() {
            return Err(ReelError::LimitExceeded {
                what: "av1-packet-buffer",
            });
        }
        // SAFETY: `buffer` is a fresh allocation of `bytes.len()` bytes that cannot overlap `bytes`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len()) };
        data.m.timestamp = tag as i64;
        // SAFETY: `ctx` came from `dav1d_open` and is not closed; `data` is valid to read and write.
        let result = unsafe { dav1d_send_data(self.ctx, Some(NonNull::from(&mut data))) };
        if result.0 == 0 {
            return Ok(SendStatus::Accepted);
        }
        // SAFETY: on failure the data was not consumed; release our reference exactly once.
        unsafe { dav1d_data_unref(Some(NonNull::from(&mut data))) };
        if result.0 == -libc::EAGAIN {
            Ok(SendStatus::Full)
        } else {
            Err(map_error(result.0))
        }
    }

    /// The next decoded picture, or `None` when the decoder needs more data.
    pub(super) fn get_picture(&mut self) -> Result<Option<RawPicture>, ReelError> {
        let mut picture = Dav1dPicture::default();
        // SAFETY: `ctx` is open; `picture` is valid to write.
        let result = unsafe { dav1d_get_picture(self.ctx, Some(NonNull::from(&mut picture))) };
        if result.0 == -libc::EAGAIN {
            return Ok(None);
        }
        if result.0 != 0 {
            return Err(map_error(result.0));
        }
        let copied = self.copy_picture(&picture);
        // SAFETY: `picture` was filled by a successful `dav1d_get_picture` and is released once.
        unsafe { dav1d_picture_unref(Some(NonNull::from(&mut picture))) };
        copied.map(Some)
    }

    fn copy_picture(&self, picture: &Dav1dPicture) -> Result<RawPicture, ReelError> {
        let corrupt = |what| ReelError::Corrupt { what };
        let (width, height) = (
            u32::try_from(picture.p.w).map_err(|_| corrupt("av1-picture-size"))?,
            u32::try_from(picture.p.h).map_err(|_| corrupt("av1-picture-size"))?,
        );
        let bit_depth = match picture.p.bpc {
            8 => 8u8,
            10 => 10,
            12 => 12,
            _ => return Err(corrupt("av1-bit-depth")),
        };
        let layout = match picture.p.layout {
            DAV1D_PIXEL_LAYOUT_I400 => Layout::Mono,
            DAV1D_PIXEL_LAYOUT_I420 => Layout::I420,
            DAV1D_PIXEL_LAYOUT_I422 => Layout::I422,
            DAV1D_PIXEL_LAYOUT_I444 => Layout::I444,
            _ => return Err(corrupt("av1-pixel-layout")),
        };
        // SAFETY: rav1d keeps the sequence and frame headers alive while the picture is referenced.
        let sequence = picture
            .seq_hdr
            .map(|p| unsafe { p.as_ref() })
            .ok_or(corrupt("av1-missing-sequence-header"))?;
        let film_grain_applied = picture
            .frame_hdr
            // SAFETY: as above.
            .map(|p| unsafe { p.as_ref() }.film_grain.present != 0)
            .unwrap_or(false);
        let color = SequenceColor {
            described: sequence.color_description_present != 0,
            primaries: u16::try_from(sequence.pri).unwrap_or(2),
            transfer: u16::try_from(sequence.trc).unwrap_or(2),
            matrix: u16::try_from(sequence.mtrx).unwrap_or(2),
            full_range: sequence.color_range != 0,
        };
        let bytes_per_sample = if bit_depth > 8 { 2usize } else { 1 };
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let dims: &[(usize, usize)] = match layout {
            Layout::Mono => &[(w, h)],
            Layout::I420 => &[(w, h), (cw, ch), (cw, ch)],
            Layout::I422 => &[(w, h), (cw, h), (cw, h)],
            Layout::I444 => &[(w, h), (w, h), (w, h)],
        };
        let total = dims
            .iter()
            .try_fold(0usize, |acc, (pw, ph)| {
                pw.checked_mul(*ph)?
                    .checked_mul(bytes_per_sample)
                    .and_then(|n| acc.checked_add(n))
            })
            .ok_or(ReelError::LimitExceeded {
                what: "av1-frame-bytes",
            })?;
        if total > self.max_frame_bytes {
            return Err(ReelError::LimitExceeded {
                what: "av1-frame-bytes",
            });
        }
        let mut planes = Vec::with_capacity(dims.len());
        for (index, (pw, ph)) in dims.iter().enumerate() {
            let base = picture.data[index]
                .ok_or(corrupt("av1-missing-plane"))?
                .as_ptr()
                .cast::<u8>()
                .cast_const();
            let stride = picture.stride[usize::from(index > 0)];
            let row_bytes = pw * bytes_per_sample;
            if stride.unsigned_abs() < row_bytes {
                return Err(corrupt("av1-plane-stride"));
            }
            let mut plane = Vec::with_capacity(row_bytes * ph);
            for row in 0..*ph {
                let step = isize::try_from(row)
                    .ok()
                    .and_then(|r| r.checked_mul(stride))
                    .ok_or(corrupt("av1-plane-stride"))?;
                // SAFETY: rav1d allocates `ph` rows of `stride` bytes (at least `row_bytes` of samples
                // each) for this plane, valid until the picture is unreferenced, which happens after
                // this copy.
                let line = unsafe { std::slice::from_raw_parts(base.offset(step), row_bytes) };
                if bytes_per_sample == 1 {
                    plane.extend_from_slice(line);
                } else {
                    for pair in line.chunks_exact(2) {
                        plane.extend_from_slice(
                            &u16::from_ne_bytes([pair[0], pair[1]]).to_le_bytes(),
                        );
                    }
                }
            }
            planes.push(plane);
        }
        Ok(RawPicture {
            tag: picture.m.timestamp as u64,
            width,
            height,
            bit_depth,
            layout,
            color,
            film_grain_applied,
            planes,
        })
    }
}

impl Drop for Rav1d {
    fn drop(&mut self) {
        // SAFETY: `ctx` came from `dav1d_open`; `dav1d_close` takes it out and leaves `None`.
        unsafe { dav1d_close(Some(NonNull::from(&mut self.ctx))) };
    }
}
