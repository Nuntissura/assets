use core::str::FromStr;

use super::common::TagExt;
use super::{Language, Script, hb_tag_t, script, tag_table};
use crate::Error;
use crate::provider::{Context, ProviderResult};

pub(super) struct ThreeTags {
    data: [hb_tag_t; 3],
    len: usize,
    overflow: bool,
}
impl ThreeTags {
    fn new() -> Self {
        Self {
            data: [hb_tag_t(0); 3],
            len: 0,
            overflow: false,
        }
    }
    pub(super) fn push(&mut self, tag: hb_tag_t) {
        if self.len == 3 {
            self.overflow = true;
        } else {
            self.data[self.len] = tag;
            self.len += 1;
        }
    }
    pub(super) fn extend_from_slice(&mut self, tags: &[hb_tag_t]) {
        // The generated source table contains at most three tags per record.
        if tags.len() > 3 {
            self.overflow = true;
            return;
        }
        for tag in tags {
            self.push(*tag);
        }
    }
    fn left(&self) -> usize {
        3 - self.len
    }
    fn is_full(&self) -> bool {
        self.len == 3
    }
}
impl core::ops::Deref for ThreeTags {
    type Target = [hb_tag_t];
    fn deref(&self) -> &[hb_tag_t] {
        &self.data[..self.len]
    }
}

/// Converts an `Script` and an `Language` to script and language tags.
pub fn tags_from_script_and_language(
    script: Option<Script>,
    language: Option<&Language>,
    meter: &Context<'_>,
) -> ProviderResult<(ThreeTags, ThreeTags)> {
    // Static language table matching scans at most 63 input bytes per source comparison.
    meter.units(
        (tag_table::LANGUAGE_MATCH_WORK_BOUND as u64)
            .checked_mul(63)
            .ok_or(Error::Overflow)?,
    )?;
    let mut needs_script = true;
    let mut scripts = ThreeTags::new();
    let mut languages = ThreeTags::new();

    let mut private_use_subtag = None;
    let mut prefix = "";
    if let Some(language) = language {
        let language = language.as_str();
        if language.starts_with("x-") {
            private_use_subtag = Some(language);
        } else {
            let bytes = language.as_bytes();
            let mut i = 1;
            while i < bytes.len() {
                if bytes.get(i - 1) == Some(&b'-') && bytes.get(i + 1) == Some(&b'-') {
                    if bytes[i] == b'x' {
                        private_use_subtag = Some(&language[i..]);
                        if prefix.is_empty() {
                            prefix = &language[..i - 1];
                        }

                        break;
                    } else {
                        prefix = &language[..i - 1];
                    }
                }

                i += 1;
            }

            if prefix.is_empty() {
                prefix = &language[..i];
            }
        }

        needs_script = !parse_private_use_subtag(
            private_use_subtag,
            "-hbsc",
            u8::to_ascii_lowercase,
            &mut scripts,
        );

        let needs_language = !parse_private_use_subtag(
            private_use_subtag,
            "-hbot",
            u8::to_ascii_uppercase,
            &mut languages,
        );

        if needs_language {
            if let Ok(prefix) = Language::from_str(prefix) {
                tags_from_language(&prefix, &mut languages);
            }
        }
    }

    if needs_script {
        all_tags_from_script(script, &mut scripts);
    }

    meter.poll()?;
    if scripts.overflow || languages.overflow {
        return Err(Error::InternalInvariant);
    }
    Ok((scripts, languages))
}

fn parse_private_use_subtag(
    private_use_subtag: Option<&str>,
    prefix: &str,
    normalize: fn(&u8) -> u8,
    tags: &mut ThreeTags,
) -> bool {
    let private_use_subtag = match private_use_subtag {
        Some(v) => v,
        None => return false,
    };

    let private_use_subtag = match private_use_subtag.find(prefix) {
        Some(idx) => &private_use_subtag[idx + prefix.len()..],
        None => return false,
    };

    let mut tag = [0u8; 4];
    let mut tag_len = 0;
    for c in private_use_subtag.bytes().take(4) {
        if c.is_ascii_alphanumeric() {
            tag[tag_len] = (normalize)(&c);
            tag_len += 1;
        } else {
            break;
        }
    }

    if tag_len == 0 {
        return false;
    }

    let mut tag = hb_tag_t::from_bytes_lossy(&tag[..tag_len]);

    // Some bits magic from HarfBuzz...
    if tag.as_u32() & 0xDFDFDFDF == hb_tag_t::default_script().as_u32() {
        tag = hb_tag_t(tag.as_u32() ^ !0xDFDFDFDF);
    }

    tags.push(tag);

    true
}

fn lang_cmp(s1: &str, s2: &str) -> core::cmp::Ordering {
    let da = s1.find('-').unwrap_or(s1.len());
    let db = s2.find('-').unwrap_or(s2.len());
    let n = core::cmp::max(da, db);
    let ea = core::cmp::min(n, s1.len());
    let eb = core::cmp::min(n, s2.len());
    s1[..ea].cmp(&s2[..eb])
}

fn tags_from_language(language: &Language, tags: &mut ThreeTags) {
    let language = language.as_str();

    // Check for matches of multiple subtags.
    if tag_table::tags_from_complex_language(language, tags) {
        return;
    }

    let mut sublang = language;

    // Find a language matching in the first component.
    if let Some(i) = language.find('-') {
        // If there is an extended language tag, use it.
        if language.len() >= 6 {
            let extlang = match language[i + 1..].find('-') {
                Some(idx) => idx == 3,
                None => language.len() - i - 1 == 3,
            };

            if extlang && language.as_bytes()[i + 1].is_ascii_alphabetic() {
                sublang = &language[i + 1..];
            }
        }
    }

    use tag_table::OPEN_TYPE_LANGUAGES as LANGUAGES;

    if let Ok(mut idx) = LANGUAGES.binary_search_by(|v| lang_cmp(v.language, sublang)) {
        while idx != 0 && LANGUAGES[idx].language == LANGUAGES[idx - 1].language {
            idx -= 1;
        }

        let len = core::cmp::min(tags.left(), LANGUAGES.len() - idx - 1);
        for i in 0..len {
            if LANGUAGES[idx + i].language != LANGUAGES[idx].language {
                break;
            }

            if LANGUAGES[idx + i].tag.is_null() {
                break;
            }

            if tags.is_full() {
                break;
            }

            tags.push(LANGUAGES[idx + i].tag);
        }

        return;
    }

    if language.len() == 3 {
        tags.push(hb_tag_t::from_bytes_lossy(language.as_bytes()).to_uppercase());
    }
}

fn all_tags_from_script(script: Option<Script>, tags: &mut ThreeTags) {
    if let Some(script) = script {
        if let Some(tag) = new_tag_from_script(script) {
            // Script::Myanmar maps to 'mym2', but there is no 'mym3'.
            if tag != hb_tag_t::from_bytes(b"mym2") {
                let mut tag3 = tag.to_bytes();
                tag3[3] = b'3';
                tags.push(hb_tag_t::from_bytes(&tag3));
            }

            if !tags.is_full() {
                tags.push(tag);
            }
        }

        if !tags.is_full() {
            tags.push(old_tag_from_script(script));
        }
    }
}

fn new_tag_from_script(script: Script) -> Option<hb_tag_t> {
    match script {
        script::BENGALI => Some(hb_tag_t::from_bytes(b"bng2")),
        script::DEVANAGARI => Some(hb_tag_t::from_bytes(b"dev2")),
        script::GUJARATI => Some(hb_tag_t::from_bytes(b"gjr2")),
        script::GURMUKHI => Some(hb_tag_t::from_bytes(b"gur2")),
        script::KANNADA => Some(hb_tag_t::from_bytes(b"knd2")),
        script::MALAYALAM => Some(hb_tag_t::from_bytes(b"mlm2")),
        script::ORIYA => Some(hb_tag_t::from_bytes(b"ory2")),
        script::TAMIL => Some(hb_tag_t::from_bytes(b"tml2")),
        script::TELUGU => Some(hb_tag_t::from_bytes(b"tel2")),
        script::MYANMAR => Some(hb_tag_t::from_bytes(b"mym2")),
        _ => None,
    }
}

fn old_tag_from_script(script: Script) -> hb_tag_t {
    // This seems to be accurate as of end of 2012.
    match script {
        // Katakana and Hiragana both map to 'kana'.
        script::HIRAGANA => hb_tag_t::from_bytes(b"kana"),

        // Spaces at the end are preserved, unlike ISO 15924.
        script::LAO => hb_tag_t::from_bytes(b"lao "),
        script::YI => hb_tag_t::from_bytes(b"yi  "),
        // Unicode-5.0 additions.
        script::NKO => hb_tag_t::from_bytes(b"nko "),
        // Unicode-5.1 additions.
        script::VAI => hb_tag_t::from_bytes(b"vai "),

        // Else, just change first char to lowercase and return.
        _ => hb_tag_t(script.tag().as_u32() | 0x20000000),
    }
}
