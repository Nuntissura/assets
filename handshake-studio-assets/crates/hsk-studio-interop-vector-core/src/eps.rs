//! EPS import (lane SMB-DXF-EPS-IMPORT; parity row AF-07, START_LOG lead decision 2026-10-16).
//!
//! Stage 1, in process and without a PostScript interpreter: the DOS binary EPS header (magic
//! `C5 D0 D3 C6`, then little-endian u32 offset/length pairs for the PostScript section, the
//! Windows metafile preview and the TIFF preview, and a u16 checksum; layout as the Adobe EPSF 3.0
//! specification, Technical Note 5002, describes it, not re-fetched: UNVERIFIED) and the DSC
//! header comments (`%%BoundingBox`, `%%HiResBoundingBox` and their `(atend)` form read from the
//! trailer, `%%LanguageLevel`, `%%DocumentFonts`, `%%DocumentNeededResources`) plus the EPSI
//! interchange preview (`%%BeginPreview: w h depth lines` hex lines). [`import_eps_preview`]
//! returns the preview (TIFF or WMF bytes for the host's decoders, an EPSI bitmap decoded to
//! 8-bit gray) with the typed loss `eps_vector_not_decoded`; [`import_eps`] refuses with
//! [`VectorError::EpsVectorNotDecoded`] naming the routes.
//!
//! Stage 2, vector content: [`import_eps_with`] hands the PostScript section to an
//! [`EpsPdfConverter`] (the isolated, capped `stet` worker of feature `eps-worker`, see
//! `eps_worker`; never an interpreter in this process) and reads the resulting PDF through the
//! same [`PdfDrawingProvider`] port and Folio builder as the `.ai` import. Losses:
//! `eps_via_pdf_conversion` (the drawing is the interpreter's rendering of the program, not the
//! program), plus the builder's own (text as outlines where the host cannot lay it out).
//!
//! DSC comments are read line by line at the start of a line only, after the DOS header is
//! removed; nothing inside PostScript strings or procedures is interpreted (proof:
//! `tests/eps_import.rs` `dsc_is_line_tokenised_against_decoys`; bounds: `hostile_eps_is_bounded`).
#![deny(clippy::arithmetic_side_effects)]
use crate::ai_import::{AiImport, DrawingSource, PdfDrawingProvider, document_from_drawing};
use crate::error::{PreservedBlob, Result, VectorError};
use crate::ids::IdMint;
use crate::model::{ImportPolicy, LossEntry, LossKind, VectorProposal};
use hsk_studio_accord::CancellationToken;

/// DOS binary EPS magic (`C5 D0 D3 C6`).
pub const DOS_EPS_MAGIC: [u8; 4] = [0xC5, 0xD0, 0xD3, 0xC6];
/// Length of the DOS binary EPS header.
pub const DOS_EPS_HEADER_LEN: usize = 30;
/// The routes to EPS vector content, carried by [`VectorError::EpsVectorNotDecoded`].
pub const EPS_ROUTE: &str = "import_eps_with_the_isolated_eps_worker_or_import_eps_preview_for_the_raster_preview";
/// Largest EPSI preview side (pixels).
pub const MAX_PREVIEW_SIDE: u32 = 16_384;
/// Largest EPSI preview area (pixels).
pub const MAX_PREVIEW_PIXELS: u64 = 64 * 1024 * 1024;
/// Header bytes scanned for DSC comments, and trailer bytes scanned for `(atend)` values.
const DSC_WINDOW: usize = 64 * 1024;
/// Most DSC header lines read.
const MAX_HEADER_LINES: usize = 4096;
/// Most font names kept from `%%DocumentFonts` / `%%DocumentNeededResources`.
const MAX_FONTS: usize = 256;

/// One preview embedded in an EPS file.
#[derive(Clone, Debug, PartialEq)]
pub enum EpsPreview {
    /// TIFF bytes from the DOS header (decode with the host's TIFF reader).
    Tiff(Vec<u8>),
    /// Windows metafile bytes from the DOS header (no WMF decoder in Studio: kept as bytes).
    Wmf(Vec<u8>),
    /// EPSI bitmap, expanded to one byte per pixel, 0 black to 255 white (PostScript `image`
    /// sample order: row 0 is the top row; UNVERIFIED against an Adobe-written EPSI file).
    Epsi { width: u32, height: u32, depth: u8, gray: Vec<u8> },
}

/// What the DSC header and the DOS header say.
#[derive(Clone, Debug, PartialEq)]
pub struct EpsInfo {
    /// `EPSF-x.y` from the first line, when present (a plain PostScript file has none).
    pub epsf_version: Option<String>,
    pub dos_header: bool,
    /// `%%BoundingBox` (integers, PostScript points).
    pub bounding_box: Option<[i64; 4]>,
    /// `%%HiResBoundingBox`.
    pub hires_bounding_box: Option<[f64; 4]>,
    pub language_level: Option<u8>,
    /// Font names from `%%DocumentFonts` and `font` entries of `%%DocumentNeededResources`.
    pub fonts: Vec<String>,
    /// Byte range of the PostScript section inside the file.
    pub postscript: std::ops::Range<usize>,
    /// Byte range of the TIFF preview in the file (DOS header only).
    pub tiff: Option<std::ops::Range<usize>>,
    /// Byte range of the WMF preview in the file (DOS header only).
    pub wmf: Option<std::ops::Range<usize>>,
    /// `%%BeginPreview` was present.
    pub epsi: bool,
}

impl EpsInfo {
    /// The bounding box to place the drawing with: the high-resolution one when given.
    pub fn bounds(&self) -> Option<[f64; 4]> {
        self.hires_bounding_box.or_else(|| self.bounding_box.map(|b| b.map(|v| v as f64)))
    }
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    let end = at.checked_add(4).ok_or(VectorError::MalformedEps("dos_header"))?;
    let b = bytes.get(at..end).ok_or(VectorError::MalformedEps("dos_header"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A section `[offset, offset + len)` of the file; zero length means absent.
fn section(bytes: &[u8], offset: u32, len: u32, what: &'static str) -> Result<Option<std::ops::Range<usize>>> {
    if len == 0 {
        return Ok(None);
    }
    let start = usize::try_from(offset).map_err(|_| VectorError::MalformedEps(what))?;
    let end = start.checked_add(usize::try_from(len).map_err(|_| VectorError::MalformedEps(what))?).ok_or(VectorError::MalformedEps(what))?;
    if start < DOS_EPS_HEADER_LEN || end > bytes.len() {
        return Err(VectorError::MalformedEps(what));
    }
    Ok(Some(start..end))
}

/// Lines of `text` split at CR, LF or CRLF (DSC allows all three).
fn dsc_lines(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    text.split(|b| *b == b'\n' || *b == b'\r').filter(|l| !l.is_empty())
}

/// The value after `%%Key:` at the start of a line (exact, case-sensitive per DSC), trimmed.
fn dsc_value<'a>(line: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let rest = line.strip_prefix(b"%%")?.strip_prefix(key)?.strip_prefix(b":")?;
    Some(rest.trim_ascii())
}

fn numbers(v: &[u8]) -> Option<Vec<f64>> {
    let text = std::str::from_utf8(v).ok()?;
    text.split_ascii_whitespace().map(|t| t.parse::<f64>().ok().filter(|f| f.is_finite())).collect()
}

fn ibox(v: &[u8]) -> Result<Option<[i64; 4]>> {
    if v == b"(atend)" {
        return Ok(None);
    }
    let n = numbers(v).ok_or(VectorError::MalformedEps("bounding_box"))?;
    let [a, b, c, d] = n.as_slice() else { return Err(VectorError::MalformedEps("bounding_box")) };
    let to_i = |x: f64| -> Result<i64> {
        if x.fract() == 0.0 && x.abs() <= 1.0e9 { Ok(x as i64) } else { Err(VectorError::MalformedEps("bounding_box")) }
    };
    Ok(Some([to_i(*a)?, to_i(*b)?, to_i(*c)?, to_i(*d)?]))
}

fn fbox(v: &[u8]) -> Result<Option<[f64; 4]>> {
    if v == b"(atend)" {
        return Ok(None);
    }
    let n = numbers(v).ok_or(VectorError::MalformedEps("hires_bounding_box"))?;
    let [a, b, c, d] = n.as_slice() else { return Err(VectorError::MalformedEps("hires_bounding_box")) };
    if [a, b, c, d].iter().any(|x| x.abs() > 1.0e9) {
        return Err(VectorError::MalformedEps("hires_bounding_box"));
    }
    Ok(Some([*a, *b, *c, *d]))
}

fn push_fonts(fonts: &mut Vec<String>, v: &[u8], resources: bool) {
    let Ok(text) = std::str::from_utf8(v) else { return };
    let mut tokens = text.split_ascii_whitespace().peekable();
    while let Some(t) = tokens.next() {
        if fonts.len() >= MAX_FONTS {
            return;
        }
        if resources {
            // `font Name Name ...` until the next resource type keyword.
            if t == "font" {
                while let Some(name) = tokens.next_if(|n| !matches!(*n, "procset" | "file" | "pattern" | "form" | "encoding" | "font")) {
                    if fonts.len() < MAX_FONTS && name != "(atend)" && !fonts.iter().any(|f| f == name) {
                        fonts.push(name.to_owned());
                    }
                }
            }
        } else if t != "(atend)" && !fonts.iter().any(|f| f == t) {
            fonts.push(t.to_owned());
        }
    }
}

/// Reads the DOS header and the DSC header comments (module docs). Not an EPS (neither the DOS
/// magic nor `%!PS-Adobe` / `%!` at the start) is `Unsupported("not_an_eps_file")`.
pub fn read_eps(bytes: &[u8], policy: &ImportPolicy) -> Result<EpsInfo> {
    if bytes.len() > policy.limits.max_bytes {
        return Err(VectorError::BudgetExceeded("bytes"));
    }
    let mut info = EpsInfo {
        epsf_version: None,
        dos_header: false,
        bounding_box: None,
        hires_bounding_box: None,
        language_level: None,
        fonts: Vec::new(),
        postscript: 0..bytes.len(),
        tiff: None,
        wmf: None,
        epsi: false,
    };
    if bytes.starts_with(&DOS_EPS_MAGIC) {
        if bytes.len() < DOS_EPS_HEADER_LEN {
            return Err(VectorError::MalformedEps("dos_header"));
        }
        info.dos_header = true;
        info.postscript = section(bytes, u32_at(bytes, 4)?, u32_at(bytes, 8)?, "dos_postscript_section")?.ok_or(VectorError::MalformedEps("dos_postscript_section"))?;
        info.wmf = section(bytes, u32_at(bytes, 12)?, u32_at(bytes, 16)?, "dos_wmf_section")?;
        info.tiff = section(bytes, u32_at(bytes, 20)?, u32_at(bytes, 24)?, "dos_tiff_section")?;
    }
    let ps = &bytes[info.postscript.clone()];
    if !ps.starts_with(b"%!") {
        return Err(VectorError::Unsupported("not_an_eps_file"));
    }
    let head = &ps[..ps.len().min(DSC_WINDOW)];
    let mut lines = dsc_lines(head);
    let first = lines.next().unwrap_or_default();
    if let Some(at) = first.windows(5).position(|w| w == b"EPSF-") {
        let v = &first[at..];
        let end = v.iter().position(|b| b.is_ascii_whitespace()).unwrap_or(v.len());
        info.epsf_version = std::str::from_utf8(&v[..end]).ok().map(str::to_owned);
    }
    let (mut box_atend, mut hires_atend, mut fonts_atend) = (false, false, false);
    for (n, line) in lines.enumerate() {
        if n >= MAX_HEADER_LINES || line.starts_with(b"%%EndComments") || !line.starts_with(b"%") {
            break;
        }
        if let Some(v) = dsc_value(line, b"BoundingBox") {
            box_atend = v == b"(atend)";
            info.bounding_box = ibox(v)?;
        } else if let Some(v) = dsc_value(line, b"HiResBoundingBox") {
            hires_atend = v == b"(atend)";
            info.hires_bounding_box = fbox(v)?;
        } else if let Some(v) = dsc_value(line, b"LanguageLevel") {
            info.language_level = std::str::from_utf8(v).ok().and_then(|t| t.parse::<u8>().ok());
        } else if let Some(v) = dsc_value(line, b"DocumentFonts") {
            fonts_atend |= v == b"(atend)";
            push_fonts(&mut info.fonts, v, false);
        } else if let Some(v) = dsc_value(line, b"DocumentNeededResources") {
            fonts_atend |= v == b"(atend)";
            push_fonts(&mut info.fonts, v, true);
        } else if let Some(v) = line.strip_prefix(b"%%+") {
            // Continuation lines only extend the font lists (the only multi-line values kept).
            let v = v.trim_ascii();
            if v.starts_with(b"font ") {
                push_fonts(&mut info.fonts, v, true);
            }
        }
    }
    info.epsi = dsc_lines(head).any(|l| l.starts_with(b"%%BeginPreview:"));
    if box_atend || hires_atend || fonts_atend {
        let tail = &ps[ps.len().saturating_sub(DSC_WINDOW)..];
        let trailer = tail.windows(9).rposition(|w| w == b"%%Trailer").map_or(tail, |at| &tail[at..]);
        for line in dsc_lines(trailer) {
            if let Some(v) = dsc_value(line, b"BoundingBox").filter(|_| box_atend) {
                info.bounding_box = ibox(v)?;
            } else if let Some(v) = dsc_value(line, b"HiResBoundingBox").filter(|_| hires_atend) {
                info.hires_bounding_box = fbox(v)?;
            } else if let Some(v) = dsc_value(line, b"DocumentFonts").filter(|_| fonts_atend) {
                push_fonts(&mut info.fonts, v, false);
            } else if let Some(v) = dsc_value(line, b"DocumentNeededResources").filter(|_| fonts_atend) {
                push_fonts(&mut info.fonts, v, true);
            }
        }
    }
    if let Some(b) = info.bounds()
        && !(b[2] > b[0] && b[3] > b[1])
    {
        return Err(VectorError::MalformedEps("bounding_box_empty"));
    }
    Ok(info)
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => c.checked_sub(b'0'),
        b'a'..=b'f' => c.checked_sub(b'a').and_then(|v| v.checked_add(10)),
        b'A'..=b'F' => c.checked_sub(b'A').and_then(|v| v.checked_add(10)),
        _ => None,
    }
}

/// Decodes the EPSI preview: `%%BeginPreview: width height depth lines`, then `%` hex lines up to
/// `%%EndPreview`. The size is checked (side, area and the declared data) before allocating.
fn epsi(ps: &[u8], cancel: &CancellationToken) -> Result<Option<EpsPreview>> {
    let head = &ps[..ps.len().min(DSC_WINDOW)];
    let Some(start) = head.windows(15).position(|w| w == b"%%BeginPreview:") else { return Ok(None) };
    let mut lines = dsc_lines(&ps[start..]);
    let decl = lines.next().and_then(|l| dsc_value(l, b"BeginPreview")).ok_or(VectorError::MalformedEps("epsi_header"))?;
    let n = numbers(decl).ok_or(VectorError::MalformedEps("epsi_header"))?;
    let [w, h, d, _lines] = n.as_slice() else { return Err(VectorError::MalformedEps("epsi_header")) };
    let as_u32 = |v: f64| -> Result<u32> {
        if v.fract() == 0.0 && v >= 0.0 && v <= f64::from(u32::MAX) { Ok(v as u32) } else { Err(VectorError::MalformedEps("epsi_header")) }
    };
    let (width, height, depth) = (as_u32(*w)?, as_u32(*h)?, as_u32(*d)?);
    let depth = u8::try_from(depth).ok().filter(|d| matches!(d, 1 | 2 | 4 | 8)).ok_or(VectorError::MalformedEps("epsi_depth"))?;
    if width == 0 || height == 0 {
        return Err(VectorError::MalformedEps("epsi_header"));
    }
    if width > MAX_PREVIEW_SIDE || height > MAX_PREVIEW_SIDE {
        return Err(VectorError::BudgetExceeded("epsi_preview_side"));
    }
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or(VectorError::BudgetExceeded("epsi_preview_pixels"))?;
    if pixels > MAX_PREVIEW_PIXELS {
        return Err(VectorError::BudgetExceeded("epsi_preview_pixels"));
    }
    // Bytes per row in the hex data (rows padded to whole bytes).
    let row_bits = u64::from(width).checked_mul(u64::from(depth)).ok_or(VectorError::BudgetExceeded("epsi_preview_pixels"))?;
    let row_bytes = usize::try_from(row_bits.div_ceil(8)).map_err(|_| VectorError::BudgetExceeded("epsi_preview_pixels"))?;
    let data_len = row_bytes.checked_mul(usize::try_from(height).map_err(|_| VectorError::BudgetExceeded("epsi_preview_pixels"))?).ok_or(VectorError::BudgetExceeded("epsi_preview_pixels"))?;
    // Hex text is two characters per byte: data the file cannot hold is refused before allocating.
    if data_len > ps.len() / 2 {
        return Err(VectorError::MalformedEps("epsi_data_truncated"));
    }
    let mut data = Vec::with_capacity(data_len);
    let mut ended = false;
    for (n, line) in lines.enumerate() {
        if n.is_multiple_of(1024) {
            cancel.check().map_err(|_| VectorError::Canceled)?;
        }
        if line.starts_with(b"%%EndPreview") {
            ended = true;
            break;
        }
        let body = line.strip_prefix(b"%").ok_or(VectorError::MalformedEps("epsi_data"))?;
        let digits: Vec<u8> = body.iter().copied().filter(|c| !c.is_ascii_whitespace()).collect();
        if !digits.len().is_multiple_of(2) {
            return Err(VectorError::MalformedEps("epsi_data"));
        }
        for pair in digits.chunks_exact(2) {
            let (hi, lo) = (hex_value(pair[0]), hex_value(pair[1]));
            let (Some(hi), Some(lo)) = (hi, lo) else { return Err(VectorError::MalformedEps("epsi_data")) };
            if data.len() >= data_len {
                return Err(VectorError::MalformedEps("epsi_data_overrun"));
            }
            data.push(hi.wrapping_shl(4) | lo);
        }
    }
    if !ended || data.len() != data_len {
        return Err(VectorError::MalformedEps("epsi_data_truncated"));
    }
    let max = u16::from(u8::MAX >> 8u32.saturating_sub(u32::from(depth)).min(7));
    let mut gray = Vec::with_capacity(usize::try_from(pixels).unwrap_or(0));
    for row in data.chunks_exact(row_bytes) {
        for x in 0..width {
            let bit = u64::from(x).saturating_mul(u64::from(depth));
            let byte = row[usize::try_from(bit / 8).unwrap_or(0)];
            let shift = 8u64.saturating_sub(bit % 8).saturating_sub(u64::from(depth));
            let mask = u8::try_from(max).unwrap_or(u8::MAX);
            let v = u16::from((byte >> shift) & mask);
            gray.push(u8::try_from(v.saturating_mul(255).checked_div(max).unwrap_or(0)).unwrap_or(u8::MAX));
        }
    }
    Ok(Some(EpsPreview::Epsi { width, height, depth, gray }))
}

/// An EPS opened by its preview only.
#[derive(Clone, Debug, PartialEq)]
pub struct EpsPreviewImport {
    pub info: EpsInfo,
    /// TIFF first, then WMF, then EPSI; `None` when the file carries no preview.
    pub preview: Option<EpsPreview>,
    /// `eps_vector_not_decoded`, plus `eps_preview_raster` or `eps_preview_missing`.
    pub loss: Vec<LossEntry>,
    /// The original bytes, byte-exact.
    pub preserved: PreservedBlob,
}

/// Opens an EPS by its preview (stage 1, module docs): no PostScript is run.
pub fn import_eps_preview(bytes: &[u8], policy: &ImportPolicy, cancel: &CancellationToken) -> Result<EpsPreviewImport> {
    cancel.check().map_err(|_| VectorError::Canceled)?;
    let info = read_eps(bytes, policy)?;
    let preview = if let Some(r) = info.tiff.clone() {
        Some(EpsPreview::Tiff(bytes[r].to_vec()))
    } else if let Some(r) = info.wmf.clone() {
        Some(EpsPreview::Wmf(bytes[r].to_vec()))
    } else {
        epsi(&bytes[info.postscript.clone()], cancel)?
    };
    let mut loss = vec![LossEntry { path: "/eps".into(), kind: LossKind::Unsupported, code: "eps_vector_not_decoded" }];
    loss.push(match preview {
        Some(_) => LossEntry { path: "/eps/preview".into(), kind: LossKind::Approximated, code: "eps_preview_raster" },
        None => LossEntry { path: "/eps/preview".into(), kind: LossKind::Unsupported, code: "eps_preview_missing" },
    });
    Ok(EpsPreviewImport { info, preview, loss, preserved: PreservedBlob { kind: "eps", bytes: bytes.to_vec() } })
}

/// Always refuses with [`VectorError::EpsVectorNotDecoded`] (route [`EPS_ROUTE`], bytes preserved)
/// once the file reads as EPS: vector content needs [`import_eps_with`].
pub fn import_eps(bytes: &[u8], policy: &ImportPolicy, cancel: &CancellationToken) -> Result<VectorProposal> {
    cancel.check().map_err(|_| VectorError::Canceled)?;
    read_eps(bytes, policy)?;
    Err(VectorError::EpsVectorNotDecoded { route: EPS_ROUTE, preserved: PreservedBlob { kind: "eps", bytes: bytes.to_vec() } })
}

/// Turns the PostScript section of an EPS into PDF bytes (one page, the bounding box at the
/// origin). Implemented by the isolated worker host (`eps_worker::WorkerEpsConverter`); a host
/// may supply its own isolated converter. It must never run PostScript in the caller's process.
pub trait EpsPdfConverter {
    fn eps_to_pdf(&self, postscript: &[u8], cancel: &CancellationToken) -> Result<Vec<u8>>;
}

/// Imports EPS vector content through `converter` and `provider` (stage 2, module docs).
pub fn import_eps_with(bytes: &[u8], policy: &ImportPolicy, converter: &dyn EpsPdfConverter, provider: &dyn PdfDrawingProvider, mint: &mut dyn IdMint, cancel: &CancellationToken) -> Result<AiImport> {
    cancel.check().map_err(|_| VectorError::Canceled)?;
    policy.validate()?;
    let info = read_eps(bytes, policy)?;
    let pdf = converter.eps_to_pdf(&bytes[info.postscript.clone()], cancel)?;
    cancel.check().map_err(|_| VectorError::Canceled)?;
    let drawing = provider.drawing(&pdf, cancel)?;
    if drawing.pages.is_empty() {
        return Err(VectorError::EpsConversionFailed("no_pages"));
    }
    let mut losses = vec![(LossKind::Approximated, "eps_via_pdf_conversion")];
    if info.epsf_version.is_none() {
        losses.push((LossKind::Approximated, "eps_not_epsf_conforming"));
    }
    if info.tiff.is_some() || info.wmf.is_some() || info.epsi {
        losses.push((LossKind::Ignored, "eps_preview_not_imported"));
    }
    if drawing.pages.len() > 1 {
        losses.push((LossKind::Approximated, "eps_multiple_pages"));
    }
    let source = DrawingSource { format: "eps", unit: "pt", root: "/eps", source_len: bytes.len(), decoded_len: info.postscript.len(), losses };
    document_from_drawing(&drawing, &source, policy, provider, mint, cancel)
}
