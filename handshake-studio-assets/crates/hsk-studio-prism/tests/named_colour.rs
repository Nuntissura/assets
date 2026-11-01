//! Lane SMB-COL-SWATCH-GROUPS-1, MT-31251 (STU-COL-188 through STU-COL-126): ICC named-colour
//! (`nmcl` / `ncl2`) profiles parsed by Prism's profile admission. Every profile here is built from
//! scratch with invented colour names (no branded book data).
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_prism::*;

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}
fn name32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..s.len()].copy_from_slice(s.as_bytes());
    out
}
/// One colour: root name, legacy-encoded Lab words, CMYK device words.
struct Colour(&'static str, [u16; 3], [u16; 4]);
fn ncl2(prefix: &str, suffix: &str, colours: &[Colour], device_coords: u32) -> Vec<u8> {
    let mut t = b"ncl2\0\0\0\0".to_vec();
    t.extend_from_slice(&be32(0)); // vendor flag
    t.extend_from_slice(&be32(colours.len() as u32));
    t.extend_from_slice(&be32(device_coords));
    t.extend_from_slice(&name32(prefix));
    t.extend_from_slice(&name32(suffix));
    for c in colours {
        t.extend_from_slice(&name32(c.0));
        for w in c.1 {
            t.extend_from_slice(&w.to_be_bytes());
        }
        for w in &c.2[..device_coords as usize] {
            t.extend_from_slice(&w.to_be_bytes());
        }
    }
    t
}
/// A v4 `nmcl` profile (CMYK data space, `pcs` PCS) holding one tag.
fn profile(class: &[u8; 4], pcs: &[u8; 4], tag_sig: &[u8; 4], tag: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 128];
    b[8] = 4;
    b[12..16].copy_from_slice(class);
    b[16..20].copy_from_slice(b"CMYK");
    b[20..24].copy_from_slice(pcs);
    b[36..40].copy_from_slice(b"acsp");
    b.extend_from_slice(&be32(1));
    let offset = 132 + 12;
    b.extend_from_slice(tag_sig);
    b.extend_from_slice(&be32(offset as u32));
    b.extend_from_slice(&be32(tag.len() as u32));
    b.extend_from_slice(tag);
    let size = b.len() as u32;
    b[0..4].copy_from_slice(&be32(size));
    b
}
fn colours() -> Vec<Colour> {
    vec![
        // L 50 (0x7F80), a +16 (0x9000), b -32 (0x6000).
        Colour("Zorblue 001", [0x7F80, 0x9000, 0x6000], [65535, 0, 32768, 0]),
        // L 100 (0xFF00), a 0, b 0.
        Colour("Zorblue 002", [0xFF00, 0x8000, 0x8000], [0, 0, 0, 0]),
    ]
}
fn parse(bytes: &[u8]) -> Result<NamedColourTable, NamedColourError> {
    let id = DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap();
    parse_named_colour_profile(ProfileInput { profile_id: &id, bytes, expected_sha256: profile_hash(bytes) }, &CancellationToken::default())
}

#[test]
fn named_colour_profile_decodes_names_measured_lab_and_device_values() {
    let bytes = profile(b"nmcl", b"Lab ", b"ncl2", &ncl2("FAKE ", " TEST", &colours(), 4));
    let t = parse(&bytes).unwrap();
    assert_eq!((t.prefix.as_str(), t.suffix.as_str(), t.device_coords, t.entries.len()), ("FAKE ", " TEST", 4, 2));
    assert_eq!(t.sha256, profile_hash(&bytes));
    assert_eq!(t.display_name(0).as_deref(), Some("FAKE Zorblue 001 TEST"));
    let e = t.entry("Zorblue 001").unwrap();
    assert_eq!(e.pcs_lab, [50.0, 16.0, -32.0], "legacy 16-bit Lab: L = w*100/65280, a/b = w/256 - 128");
    assert_eq!(e.device, vec![1.0, 0.0, 32768.0 / 65535.0, 0.0]);
    assert_eq!(t.entry("Zorblue 002").unwrap().pcs_lab, [100.0, 0.0, 0.0]);
    assert!(t.entry("Zorblue 003").is_none(), "a missing named entry is absent, not approximated");
    // Zero device coordinates are legal.
    let bare = parse(&profile(b"nmcl", b"Lab ", b"ncl2", &ncl2("", "", &colours(), 0))).unwrap();
    assert!(bare.entries.iter().all(|e| e.device.is_empty()));
    assert_eq!(bare.display_name(1).as_deref(), Some("Zorblue 002"));
    assert!(NAMED_COLOUR_DESCRIPTOR.contains("STU-COL-188"));
}

#[test]
fn malformed_and_unsupported_named_colour_profiles_are_refused_typed() {
    let good = ncl2("", "", &colours(), 4);
    assert_eq!(parse(&profile(b"prtr", b"Lab ", b"ncl2", &good)).unwrap_err(), NamedColourError::NotNamedColourClass);
    assert_eq!(parse(&profile(b"nmcl", b"XYZ ", b"ncl2", &good)).unwrap_err().code(), "unsupported_pcs");
    assert_eq!(parse(&profile(b"nmcl", b"Lab ", b"desc", &good)).unwrap_err(), NamedColourError::MissingNamedColourTag);
    let mut wrong_type = good.clone();
    wrong_type[..4].copy_from_slice(b"text");
    assert_eq!(parse(&profile(b"nmcl", b"Lab ", b"ncl2", &wrong_type)).unwrap_err(), NamedColourError::TagType);
    let mut v3 = profile(b"nmcl", b"Lab ", b"ncl2", &good);
    v3[8] = 3;
    assert_eq!(parse(&v3).unwrap_err(), NamedColourError::UnsupportedVersion);
    // Truncated: the table claims more colours than the tag holds.
    let truncated = &good[..good.len() - 5];
    assert_eq!(parse(&profile(b"nmcl", b"Lab ", b"ncl2", truncated)).unwrap_err(), NamedColourError::Truncated);
    assert_eq!(parse(&profile(b"nmcl", b"Lab ", b"ncl2", &good[..60])).unwrap_err(), NamedColourError::Truncated);
    // Names: 8-bit or control bytes are refused.
    let mut high = good.clone();
    high[20 + 64] = 0xC3;
    assert_eq!(parse(&profile(b"nmcl", b"Lab ", b"ncl2", &high)).unwrap_err(), NamedColourError::InvalidName);
    let mut ctl = good.clone();
    ctl[20] = 0x07;
    assert_eq!(parse(&profile(b"nmcl", b"Lab ", b"ncl2", &ctl)).unwrap_err(), NamedColourError::InvalidName);
    // Identity and framing come from the shared admission: a hash mismatch, a broken size field.
    let bytes = profile(b"nmcl", b"Lab ", b"ncl2", &good);
    let id = DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap();
    let wrong = ProfileInput { profile_id: &id, bytes: &bytes, expected_sha256: [0; 32] };
    assert_eq!(parse_named_colour_profile(wrong, &CancellationToken::default()).unwrap_err(), NamedColourError::Wide(WideError::HashMismatch));
    let mut short = bytes.clone();
    short[3] ^= 1;
    assert_eq!(parse(&short).unwrap_err().code(), "malformed_profile");
    let canceled = CancellationToken::default();
    canceled.cancel();
    let ok = ProfileInput { profile_id: &id, bytes: &bytes, expected_sha256: profile_hash(&bytes) };
    assert_eq!(parse_named_colour_profile(ok, &canceled).unwrap_err(), NamedColourError::Canceled);
}

#[test]
fn named_colour_dims_u32_max_and_aggregate_over_budget_are_refused_before_storing() {
    let good = ncl2("", "", &colours(), 4);
    let mut count_max = good.clone();
    count_max[12..16].copy_from_slice(&be32(u32::MAX));
    assert_eq!(parse(&profile(b"nmcl", b"Lab ", b"ncl2", &count_max)).unwrap_err(), NamedColourError::CountLimit, "colour count u32::MAX");
    let mut dev_max = good.clone();
    dev_max[16..20].copy_from_slice(&be32(u32::MAX));
    assert_eq!(parse(&profile(b"nmcl", b"Lab ", b"ncl2", &dev_max)).unwrap_err(), NamedColourError::DeviceCoordLimit, "device coordinates u32::MAX");
    // Aggregate: a count within the bound whose table exceeds the tag is refused before any colour is stored.
    let mut over = good.clone();
    over[12..16].copy_from_slice(&be32(MAX_NAMED_COLOURS as u32));
    assert_eq!(parse(&profile(b"nmcl", b"Lab ", b"ncl2", &over)).unwrap_err(), NamedColourError::Truncated, "aggregate over the tag");
    let mut beyond = good;
    beyond[12..16].copy_from_slice(&be32(MAX_NAMED_COLOURS as u32 + 1));
    assert_eq!(parse(&profile(b"nmcl", b"Lab ", b"ncl2", &beyond)).unwrap_err(), NamedColourError::CountLimit);
    // A profile over the shared byte bound.
    let id = DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap();
    let big = vec![0u8; MAX_WIDE_PROFILE_BYTES + 1];
    let input = ProfileInput { profile_id: &id, bytes: &big, expected_sha256: profile_hash(&big) };
    assert_eq!(parse_named_colour_profile(input, &CancellationToken::default()).unwrap_err(), NamedColourError::Wide(WideError::ProfileLimit));
}
