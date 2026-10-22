use crate::refs::*;
use uuid::Uuid;

type SingleRefCase = (
    &'static str,
    fn(Uuid) -> String,
    fn(&str) -> Option<Uuid>,
    fn(&str) -> bool,
);

const SINGLE_REFS: [SingleRefCase; 5] = [
    ("character", character_ref, parse_character_ref, validate_character_ref),
    ("sheet-artifact", sheet_artifact_ref, parse_sheet_artifact_ref, validate_sheet_artifact_ref),
    ("media", media_asset_ref, parse_media_asset_ref, validate_media_asset_ref),
    ("collection", collection_ref, parse_collection_ref, validate_collection_ref),
    ("tag", tag_ref, parse_tag_ref, validate_tag_ref),
];

#[test]
fn single_refs_preserve_build_parse_and_validate_roundtrips() {
    let id = Uuid::from_u128(0x123456789abcdef00123456789abcdef);
    for (kind, build, parse, validate) in SINGLE_REFS {
        let value = build(id);
        assert_eq!(value, format!("atelier://{kind}/{id}"));
        assert_eq!(parse(&value), Some(id));
        assert!(validate(&value));
    }
}

#[test]
fn single_refs_reject_wrong_prefix_malformed_uuid_and_extra_segments() {
    let id = Uuid::from_u128(1);
    for (kind, _, parse, validate) in SINGLE_REFS {
        for value in [
            format!("ATELIER://{kind}/{id}"),
            format!("atelier://wrong-kind/{id}"),
            format!("atelier://{kind}/"),
            format!("atelier://{kind}/not-a-uuid"),
            format!("atelier://{kind}/{id}/"),
            format!("atelier://{kind}/{id}/extra"),
            format!(" atelier://{kind}/{id}"),
        ] {
            assert_eq!(parse(&value), None, "{value}");
            assert!(!validate(&value), "{value}");
        }
    }
}

#[test]
fn reference_uuid_acceptance_matches_uuid_parser() {
    let id = Uuid::from_u128(0x123456789abcdef00123456789abcdef);
    let hyphenated = id.to_string();
    for tail in [
        hyphenated.clone(),
        hyphenated.to_uppercase(),
        hyphenated.replace('-', ""),
        format!("{{{id}}}"),
        format!("urn:uuid:{id}"),
    ] {
        let expected = Uuid::parse_str(&tail).ok();
        for (kind, _, parse, validate) in SINGLE_REFS {
            let value = format!("atelier://{kind}/{tail}");
            assert_eq!(parse(&value), expected, "{value}");
            assert_eq!(validate(&value), expected.is_some(), "{value}");
        }
        let value = format!("atelier://sheet/{tail}/{tail}");
        let pair = expected.map(|uuid| (uuid, uuid));
        assert_eq!(parse_sheet_version_ref(&value), pair, "{value}");
        assert_eq!(validate_sheet_version_ref(&value), pair.is_some(), "{value}");
    }
}

#[test]
fn sheet_refs_preserve_character_and_version_identity() {
    let character = Uuid::from_u128(1);
    let version = Uuid::from_u128(2);
    let value = sheet_version_ref(character, version);
    assert_eq!(value, format!("atelier://sheet/{character}/{version}"));
    assert_eq!(parse_sheet_version_ref(&value), Some((character, version)));
    assert!(validate_sheet_version_ref(&value));
}

#[test]
fn sheet_refs_reject_missing_or_malformed_pair_and_extra_slashes() {
    let character = Uuid::from_u128(1);
    let version = Uuid::from_u128(2);
    for value in [
        format!("ATELIER://sheet/{character}/{version}"),
        format!("atelier://character/{character}/{version}"),
        format!("atelier://sheet/{character}"),
        format!("atelier://sheet//{version}"),
        format!("atelier://sheet/{character}/"),
        format!("atelier://sheet/not-a-uuid/{version}"),
        format!("atelier://sheet/{character}/not-a-uuid"),
        format!("atelier://sheet/{character}/{version}/"),
        format!("atelier://sheet/{character}/{version}/extra"),
    ] {
        assert_eq!(parse_sheet_version_ref(&value), None, "{value}");
        assert!(!validate_sheet_version_ref(&value), "{value}");
    }
}
