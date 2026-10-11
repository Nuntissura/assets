//! Character-reference decoding for text that a URL parser or CSS tokenizer reads afterwards: URL-bearing
//! attribute values and foreign-content style text.
//!
//! `entity::decode_entities` produces display text (it requires `;` and knows a small named set). That is not
//! enough here: an HTML tokenizer decodes `&#115` (no semicolon), `&#x0000000068;` (any number of digits) and
//! `&colon;` / `&sol;` / `&Tab;` before the URL parser sees the value, so a scheme can be spelled with them.
//! This decoder follows the tokenizer where it changes URL or CSS structure:
//! - numeric references decode with or without `;` and with any number of digits (0, surrogates and values above
//!   U+10FFFF become U+FFFD);
//! - named references need `;` and come from the common set plus the names that spell URL and CSS punctuation
//!   (verified against the WHATWG named character reference table); the legacy semicolon-less names (`&amp`,
//!   `&copy`, ...) never produce URL-structural characters and stay verbatim;
//! - a well-formed `&name;` outside those sets cannot be decoded faithfully, so the value is flagged
//!   `undecodable` and callers fail closed instead of classifying text a browser would read differently.
#![deny(clippy::arithmetic_side_effects)]

use crate::entity::named;

pub(crate) struct Decoded {
    pub text: String,
    /// A well-formed `&name;` was left verbatim because this decoder does not know it.
    pub undecodable: bool,
}

/// Names whose characters spell URL or CSS structure.
fn punctuation(name: &str) -> Option<char> {
    Some(match name {
        "colon" => ':',
        "sol" => '/',
        "bsol" => '\\',
        "period" => '.',
        "Tab" => '\t',
        "NewLine" => '\n',
        "quest" => '?',
        "num" => '#',
        "plus" => '+',
        "lpar" => '(',
        "rpar" => ')',
        "semi" => ';',
        "comma" => ',',
        "ast" => '*',
        "lowbar" => '_',
        "excl" => '!',
        "percnt" => '%',
        "equals" => '=',
        "commat" => '@',
        "lcub" | "lbrace" => '{',
        "rcub" | "rbrace" => '}',
        "lsqb" | "lbrack" => '[',
        "rsqb" | "rbrack" => ']',
        "Hat" => '^',
        "grave" => '`',
        "vert" | "verbar" => '|',
        "dollar" => '$',
        _ => return None,
    })
}

/// Decode the references in `input` (linear time; every reference is at least 3 bytes and yields at most 4).
pub(crate) fn decode_value(input: &str) -> Decoded {
    let mut text = String::with_capacity(input.len());
    let mut undecodable = false;
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        text.push_str(&rest[..amp]);
        let after = &rest[amp.saturating_add(1)..];
        if let Some(num) = after.strip_prefix('#') {
            let (radix, digits_src) = match num.strip_prefix(['x', 'X']) {
                Some(hex) => (16u32, hex),
                None => (10u32, num),
            };
            let digits = digits_src.bytes().take_while(|b| char::from(*b).is_digit(radix)).count();
            if digits == 0 {
                text.push('&');
                rest = after;
                continue;
            }
            let value = digits_src.bytes().take(digits).fold(0u32, |acc, b| {
                acc.saturating_mul(radix).saturating_add(char::from(b).to_digit(radix).unwrap_or(0))
            });
            text.push(char::from_u32(value).filter(|c| *c != '\0').unwrap_or('\u{fffd}'));
            let tail = &digits_src[digits..];
            rest = tail.strip_prefix(';').unwrap_or(tail);
            continue;
        }
        let name_len = after.bytes().take_while(u8::is_ascii_alphanumeric).count();
        if name_len == 0 || !after.as_bytes()[0].is_ascii_alphabetic() {
            text.push('&');
            rest = after;
            continue;
        }
        let name = &after[..name_len];
        let tail = &after[name_len..];
        if let Some(next) = tail.strip_prefix(';') {
            match named(name).or_else(|| punctuation(name)) {
                Some(c) => text.push(c),
                None => {
                    undecodable = true;
                    text.push('&');
                    text.push_str(name);
                    text.push(';');
                }
            }
            rest = next;
        } else {
            text.push('&');
            text.push_str(name);
            rest = tail;
        }
    }
    text.push_str(rest);
    Decoded { text, undecodable }
}
