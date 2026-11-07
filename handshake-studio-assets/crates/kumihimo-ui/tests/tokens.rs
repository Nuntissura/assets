//! B0: tokens are generated from the designer's design-system.json and UI source names no colour.
use kumihimo_ui::display::Prim;
use kumihimo_ui::tokens::{ColorToken, SOURCE_FNV64, TextToken, Theme, size};
use std::path::Path;

fn crate_file(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}
fn fnv64(b: &[u8]) -> u64 {
    b.iter().fold(0xCBF2_9CE4_8422_2325u64, |h, &x| (h ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01B3))
}
/// The `{...}` object that follows `"key": ` at or after `from`.
fn object_after<'a>(s: &'a str, key: &str, from: usize) -> &'a str {
    let at = s[from..].find(&format!("\"{key}\": {{")).map(|i| i + from).unwrap();
    let open = at + s[at..].find('{').unwrap();
    let mut depth = 0;
    for (i, ch) in s[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &s[open..=open + i];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced {key}")
}
/// `"name": "#hex"` pairs of a flat JSON object.
fn hex_pairs(obj: &str) -> Vec<(String, String)> {
    obj.split(',')
        .filter_map(|kv| {
            let mut parts = kv.split("\": \"");
            let k = parts.next()?.trim().trim_start_matches('{').trim().trim_matches('"').to_owned();
            let v = parts.next()?.trim().trim_end_matches('}').trim().trim_matches('"').to_owned();
            v.starts_with('#').then_some((k, v))
        })
        .collect()
}

#[test]
fn generated_tokens_match_design_system_json() {
    let raw = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("design/design-system.json")).unwrap();
    assert_eq!(fnv64(&raw), SOURCE_FNV64, "src/tokens.rs is stale: rerun tools/gen_tokens.py");
    let s = String::from_utf8(raw).unwrap();
    let color = s.find("\"color\": {").unwrap();
    let themes = s[color..].find("\"themes\"").unwrap() + color;
    for (theme, key) in [(Theme::Dark, "dark"), (Theme::Light, "light")] {
        let pairs = hex_pairs(object_after(&s, key, themes));
        assert!(pairs.len() > 40, "{key}: {} tokens", pairs.len());
        for (name, hex) in pairs {
            let t = ColorToken::from_name(&name).unwrap_or_else(|| panic!("token {name} not generated"));
            assert_eq!(t.resolve(theme).hex(), hex.to_uppercase(), "{key} {name}");
        }
    }
    let fixed = hex_pairs(object_after(&s, "fixed", color));
    assert!(fixed.len() > 40);
    for (name, hex) in fixed {
        let t = ColorToken::from_name(&name).unwrap();
        let want = if hex.len() == 9 && hex.to_uppercase().ends_with("FF") { hex[..7].to_uppercase() } else { hex.to_uppercase() };
        assert_eq!(t.resolve(Theme::Dark).hex(), want, "fixed {name}");
        assert_eq!(t.resolve(Theme::Light), t.resolve(Theme::Dark), "fixed {name} is theme-free");
    }
    assert_eq!(size::FIELD_H, 24.0);
    assert_eq!(size::ROW_LAYER_H, 32.0);
    assert_eq!(size::DOCK_GROUP_HEADER_H, 26.0);
    assert_eq!(TextToken::Value.style().tnum, true);
    assert_eq!(TextToken::MenuTitle.style().tracking, 0.4);
    // AC-TEXT-03: no style under the 11 px floor.
    assert!(TextToken::ALL.iter().all(|t| t.style().size >= 11.0));
    // The display list holds exactly the designer's primitive list (renderer.primitives).
    let renderer = object_after(&s, "renderer", 0);
    let ids: Vec<&str> = renderer.split("\"id\": \"").skip(1).map(|r| &r[..r.find('"').unwrap()]).collect();
    assert_eq!(ids, Prim::KINDS.to_vec());
}

/// AC-COL-01 source half: UI code never types a colour; only tokens.rs (generated) and the painter's pixel plumbing do.
#[test]
fn ui_source_has_no_colour_literal() {
    for f in ["src/widgets.rs", "src/emit.rs", "src/tree.rs", "src/layout.rs", "src/checks.rs", "src/gallery.rs", "src/hit.rs", "src/semantic.rs"] {
        let src = crate_file(f);
        for (i, line) in src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            assert!(!code.contains("Rgba8::new(") && !code.contains("Rgba8 { r"), "{f}:{}: colour literal", i + 1);
            let hex = code.match_indices('#').any(|(at, _)| {
                let tail: String = code[at + 1..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                tail.len() == 6 || tail.len() == 8
            });
            assert!(!hex, "{f}:{}: hex colour literal", i + 1);
        }
    }
}
