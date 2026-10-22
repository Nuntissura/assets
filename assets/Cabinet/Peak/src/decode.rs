use crate::{Content, Settings};

/// No I/O: supply a bounded prefix and indicate whether more bytes exist at the host.
pub fn decode_text(bytes: &[u8], settings: &Settings, has_more: bool) -> Content {
    if !settings.valid() {
        return Content::Failed {
            reason: "Invalid preview settings".into(),
        };
    }
    let bounded = &bytes[..bytes.len().min(settings.max_input_bytes)];
    let mut truncated = has_more || bytes.len() > bounded.len();
    let decoded = if bounded.starts_with(&[0xff, 0xfe]) || bounded.starts_with(&[0xfe, 0xff]) {
        let le = bounded[0] == 0xff;
        let data = &bounded[2..];
        if data.len() % 2 != 0 && !truncated {
            return Content::Failed {
                reason: "Incomplete UTF-16 code unit".into(),
            };
        }
        let words: Vec<u16> = data
            .chunks_exact(2)
            .map(|b| {
                if le {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })
            .collect();
        let words = if truncated && words.last().is_some_and(|v| (0xd800..=0xdbff).contains(v)) {
            &words[..words.len() - 1]
        } else {
            &words
        };
        match String::from_utf16(words) {
            Ok(s) => s,
            Err(_) => {
                return Content::Failed {
                    reason: "Malformed UTF-16 text".into(),
                }
            }
        }
    } else {
        let b = bounded.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bounded);
        match std::str::from_utf8(b) {
            Ok(s) => s.to_owned(),
            Err(e) if e.error_len().is_none() && truncated => {
                String::from_utf8(b[..e.valid_up_to()].to_vec()).unwrap()
            }
            Err(_) => {
                return Content::Unsupported {
                    reason: "Not supported UTF-8 or BOM-marked UTF-16 text".into(),
                }
            }
        }
    };
    if decoded
        .chars()
        .any(|c| c == '\0' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
    {
        return Content::Unsupported {
            reason: "Binary or control-rich content".into(),
        };
    }
    let mut lines = decoded.split_inclusive('\n');
    let body: String = lines.by_ref().take(settings.max_text_lines).collect();
    if lines.next().is_some() {
        truncated = true;
    }
    Content::Text { body, truncated }
}
pub fn decode_bytes(name: &str, bytes: &[u8], settings: &Settings) -> Content {
    if !settings.valid() {
        return Content::Failed {
            reason: "Invalid preview settings".into(),
        };
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if matches!(ext.as_str(), "heic" | "heif" | "avif" | "jxl") {
        return Content::Unsupported {
            reason: format!("{ext} requires an enabled host preview provider"),
        };
    }
    if matches!(
        ext.as_str(),
        "raf"
            | "nef"
            | "nrw"
            | "cr2"
            | "cr3"
            | "crw"
            | "arw"
            | "sr2"
            | "srf"
            | "srw"
            | "dng"
            | "orf"
            | "rw2"
            | "pef"
            | "3fr"
            | "iiq"
    ) {
        #[cfg(feature = "raw")]
        {
            return decode_raw(bytes, settings);
        }
        #[cfg(not(feature = "raw"))]
        {
            return Content::Unsupported {
                reason: "Camera RAW provider is not enabled".into(),
            };
        }
    }
    #[cfg(feature = "images")]
    if matches!(
        ext.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "webp"
            | "gif"
            | "bmp"
            | "tif"
            | "tiff"
            | "ico"
            | "tga"
            | "pnm"
            | "pbm"
            | "pgm"
            | "ppm"
            | "qoi"
            | "hdr"
            | "exr"
            | "dds"
            | "ff"
    ) {
        return decode_image_with_format(bytes, settings, image::ImageFormat::from_extension(&ext));
    }
    decode_text(bytes, settings, false)
}
#[cfg(feature = "images")]
pub fn decode_image(bytes: &[u8], settings: &Settings) -> Content {
    decode_image_with_format(bytes, settings, None)
}
/// An explicit format supports containers such as TGA without a signature to sniff.
#[cfg(feature = "images")]
pub fn decode_image_with_format(
    bytes: &[u8],
    settings: &Settings,
    format: Option<image::ImageFormat>,
) -> Content {
    if !settings.valid() || bytes.len() > settings.max_input_bytes {
        return Content::Failed {
            reason: "Image input exceeds configured bounds".into(),
        };
    }
    let result = (|| -> Result<Content, String> {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes));
        if let Some(format) = format {
            reader.set_format(format);
        } else {
            reader = reader.with_guessed_format().map_err(|e| e.to_string())?;
        }
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(settings.max_decode_pixels.min(u32::MAX as u64) as u32);
        limits.max_image_height = limits.max_image_width;
        limits.max_alloc = Some(settings.max_decode_pixels.saturating_mul(16));
        reader.limits(limits);
        let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
        use image::ImageDecoder;
        let (w, h) = decoder.dimensions();
        if u64::from(w) * u64::from(h) > settings.max_decode_pixels {
            return Err("Image pixel count exceeds configured decode bounds".into());
        }
        let orientation = decoder.orientation().map_err(|e| e.to_string())?;
        let mut image = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
        image.apply_orientation(orientation);
        Ok(dynamic_content(image, settings))
    })();
    result.unwrap_or_else(|reason| Content::Failed {
        reason: crate::bounded_string(&reason, 4096),
    })
}
#[cfg(feature = "images")]
fn dynamic_content(image: image::DynamicImage, settings: &Settings) -> Content {
    let (w, h) = (image.width(), image.height());
    let pixels = u64::from(w) * u64::from(h);
    if pixels == 0 || pixels > settings.max_decode_pixels {
        return Content::Failed {
            reason: "Decoded source exceeds configured decode pixels".into(),
        };
    }
    let scale = 1.0_f64
        .min((settings.max_output_pixels as f64 / pixels as f64).sqrt())
        .min(f64::from(settings.width) / f64::from(w))
        .min(f64::from(settings.height) / f64::from(h));
    let mut tw = (f64::from(w) * scale).floor().max(1.0) as u32;
    let mut th = (f64::from(h) * scale).floor().max(1.0) as u32;
    // A narrow source rounds its short axis up; preserve the exact output ceiling.
    if u64::from(tw) * u64::from(th) > settings.max_output_pixels {
        if tw >= th {
            tw = (settings.max_output_pixels / u64::from(th)).max(1) as u32;
        } else {
            th = (settings.max_output_pixels / u64::from(tw)).max(1) as u32;
        }
    }
    let image = if tw < w || th < h {
        image.thumbnail(tw, th)
    } else {
        image
    };
    let rgba = image.into_rgba8();
    Content::Image {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    }
}
/// Embedded preview first. Full development is an explicit host policy, not a silent fallback.
#[cfg(feature = "raw")]
pub fn decode_raw(bytes: &[u8], settings: &Settings) -> Content {
    decode_raw_with_options(bytes, settings, false)
}
#[cfg(feature = "raw")]
pub fn decode_raw_with_options(bytes: &[u8], settings: &Settings, allow_develop: bool) -> Content {
    decode_raw_with_fallback(bytes, settings, allow_develop, |bytes, settings| {
        let result = std::panic::catch_unwind(|| -> Result<Content, String> {
            let source = rawler::rawsource::RawSource::new_from_slice(bytes);
            let decoder = rawler::get_decoder(&source).map_err(|e| e.to_string())?;
            let params = rawler::decoders::RawDecodeParams::default();
            // Inspect dummy/header dimensions before allocating sensor/development buffers.
            let header = decoder
                .raw_image(&source, &params, true)
                .map_err(|e| e.to_string())?;
            if (header.width as u64).saturating_mul(header.height as u64)
                > settings.max_decode_pixels
            {
                return Err("RAW development exceeds configured decode pixels".into());
            }
            let raw = decoder
                .raw_image(&source, &params, false)
                .map_err(|e| e.to_string())?;
            let developed = rawler::imgop::develop::RawDevelop::default()
                .develop_intermediate(&raw)
                .map_err(|e| e.to_string())?;
            let image = developed
                .to_dynamic_image()
                .ok_or_else(|| "RAW developer could not produce a display image".to_owned())?;
            Ok(dynamic_content(image, settings))
        });
        match result {
            Ok(Ok(c)) => c,
            Ok(Err(reason)) => Content::Failed {
                reason: crate::bounded_string(&reason, 4096),
            },
            Err(_) => Content::Failed {
                reason: "RAW development rejected malformed input".into(),
            },
        }
    })
}
#[cfg(feature = "raw")]
pub fn decode_raw_with_fallback<F: FnOnce(&[u8], &Settings) -> Content>(
    bytes: &[u8],
    settings: &Settings,
    allow_develop: bool,
    developer: F,
) -> Content {
    if !settings.valid() || bytes.len() > settings.max_input_bytes {
        return Content::Failed {
            reason: "RAW input exceeds configured bounds".into(),
        };
    }
    let result = std::panic::catch_unwind(|| -> Result<Option<Content>, String> {
        let source = rawler::rawsource::RawSource::new_from_slice(bytes);
        let decoder = rawler::get_decoder(&source).map_err(|e| e.to_string())?;
        let params = rawler::decoders::RawDecodeParams::default();
        if let Some(image) = decoder
            .preview_image(&source, &params)
            .map_err(|e| e.to_string())?
        {
            return Ok(Some(dynamic_content(image, settings)));
        }
        if let Some(image) = decoder
            .thumbnail_image(&source, &params)
            .map_err(|e| e.to_string())?
        {
            return Ok(Some(dynamic_content(image, settings)));
        }
        Ok(None)
    });
    match result {
        Ok(Ok(Some(content))) => content,
        Ok(Ok(None)) if allow_develop => {
            let content = developer(bytes, settings);
            match crate::validate_content(&content, settings) {
                Ok(()) => content,
                Err(reason) => Content::Failed { reason },
            }
        }
        Ok(Ok(None)) => Content::Unsupported {
            reason: "No embedded RAW preview; full development is disabled".into(),
        },
        Ok(Err(reason)) => Content::Failed {
            reason: crate::bounded_string(&reason, 4096),
        },
        Err(_) => Content::Failed {
            reason: "RAW decoder rejected malformed input".into(),
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_and_line_limits() {
        let mut s = Settings::default();
        s.max_text_lines = 1;
        assert_eq!(
            decode_text(&[255, 254, 65, 0, 10, 0, 66, 0], &s, false),
            Content::Text {
                body: "A\n".into(),
                truncated: true
            }
        );
    }
    #[test]
    fn code_is_language_agnostic() {
        assert_eq!(
            decode_bytes("a.unknown", b"function custom() {}", &Settings::default()),
            Content::Text {
                body: "function custom() {}".into(),
                truncated: false
            }
        );
    }
    #[test]
    fn corrupt_and_binary_are_honest() {
        assert!(matches!(
            decode_text(&[0, 255], &Settings::default(), false),
            Content::Unsupported { .. }
        ));
        assert!(matches!(
            decode_text(&[255, 254, 0], &Settings::default(), false),
            Content::Failed { .. }
        ));
    }
}
#[cfg(all(test, feature = "images"))]
mod image_tests {
    use super::*;
    #[test]
    fn representative_codec_encoder_to_preview_roundtrips() {
        use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb, Rgba};
        let rgb = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(4, 3, Rgb([180, 80, 40])));
        let rgba =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(4, 3, Rgba([180, 80, 40, 255])));
        let float = DynamicImage::ImageRgb32F(ImageBuffer::from_pixel(4, 3, Rgb([0.7, 0.3, 0.1])));
        let wide = DynamicImage::ImageRgba16(ImageBuffer::from_pixel(
            4,
            3,
            Rgba([45000, 20000, 10000, 65535]),
        ));
        for (name, format, source) in [
            ("fixture.jpg", ImageFormat::Jpeg, &rgb),
            ("fixture.png", ImageFormat::Png, &rgba),
            ("fixture.webp", ImageFormat::WebP, &rgba),
            ("fixture.gif", ImageFormat::Gif, &rgba),
            ("fixture.bmp", ImageFormat::Bmp, &rgb),
            ("fixture.tiff", ImageFormat::Tiff, &rgba),
            ("fixture.ico", ImageFormat::Ico, &rgba),
            ("fixture.tga", ImageFormat::Tga, &rgba),
            ("fixture.ppm", ImageFormat::Pnm, &rgb),
            ("fixture.qoi", ImageFormat::Qoi, &rgba),
            ("fixture.hdr", ImageFormat::Hdr, &float),
            ("fixture.exr", ImageFormat::OpenExr, &float),
            ("fixture.ff", ImageFormat::Farbfeld, &wide),
        ] {
            let mut bytes = std::io::Cursor::new(Vec::new());
            source
                .write_to(&mut bytes, format)
                .unwrap_or_else(|e| panic!("{name} encoder: {e}"));
            let content = decode_bytes(name, bytes.get_ref(), &Settings::default());
            match content {
                Content::Image {
                    width,
                    height,
                    rgba,
                } => {
                    assert_eq!((width, height), (4, 3), "{name}");
                    assert_eq!(rgba.len(), 48, "{name}");
                    assert!(
                        rgba.chunks_exact(4)
                            .any(|pixel| pixel[..3].iter().any(|channel| *channel > 0)),
                        "{name}: empty pixels"
                    );
                }
                other => panic!("{name} preview: {other:?}"),
            }
        }
    }
    #[test]
    fn actual_png_decode_and_pixel_rejection() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(4, 4)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        assert!(matches!(
            decode_image(bytes.get_ref(), &Settings::default()),
            Content::Image {
                width: 4,
                height: 4,
                ..
            }
        ));
        let mut s = Settings::default();
        s.max_output_pixels = 4;
        assert!(matches!(
            decode_image(bytes.get_ref(), &s),
            Content::Image {
                width: 2,
                height: 2,
                ..
            }
        ));
        s.max_decode_pixels = 15;
        assert!(matches!(
            decode_image(bytes.get_ref(), &s),
            Content::Failed { .. }
        ));
    }
    #[test]
    fn malformed_image_returns_failure() {
        assert!(matches!(
            decode_image(b"not an image", &Settings::default()),
            Content::Failed { .. }
        ));
    }
}
#[cfg(all(test, feature = "raw"))]
mod raw_tests {
    use super::*;
    #[test]
    fn malformed_raw_is_failure_not_fabricated_thumbnail() {
        assert!(matches!(
            decode_raw(b"not raw", &Settings::default()),
            Content::Failed { .. } | Content::Unsupported { .. }
        ));
    }
}
