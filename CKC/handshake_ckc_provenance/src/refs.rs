//! Typed `atelier://` ref builders and parsers shared by the CKC surfaces (ported verbatim from
//! `feat/WP-CKC-posekit-overhaul`; storage-agnostic).

use uuid::Uuid;

pub fn character_ref(character_internal_id: Uuid) -> String {
    format!("atelier://character/{character_internal_id}")
}

pub fn sheet_version_ref(character_internal_id: Uuid, version_id: Uuid) -> String {
    format!("atelier://sheet/{character_internal_id}/{version_id}")
}

pub fn sheet_artifact_ref(link_id: Uuid) -> String {
    format!("atelier://sheet-artifact/{link_id}")
}

pub fn media_asset_ref(asset_id: Uuid) -> String {
    format!("atelier://media/{asset_id}")
}

pub fn collection_ref(collection_id: Uuid) -> String {
    format!("atelier://collection/{collection_id}")
}

pub fn tag_ref(tag_id: Uuid) -> String {
    format!("atelier://tag/{tag_id}")
}

pub fn parse_character_ref(value: &str) -> Option<Uuid> {
    value
        .strip_prefix("atelier://character/")
        .and_then(|id| Uuid::parse_str(id).ok())
}

pub fn parse_sheet_version_ref(value: &str) -> Option<(Uuid, Uuid)> {
    let rest = value.strip_prefix("atelier://sheet/")?;
    let (character, version) = rest.split_once('/')?;
    let character = Uuid::parse_str(character).ok()?;
    let version = Uuid::parse_str(version).ok()?;
    Some((character, version))
}

pub fn parse_sheet_artifact_ref(value: &str) -> Option<Uuid> {
    value
        .strip_prefix("atelier://sheet-artifact/")
        .and_then(|id| Uuid::parse_str(id).ok())
}

pub fn parse_media_asset_ref(value: &str) -> Option<Uuid> {
    value
        .strip_prefix("atelier://media/")
        .and_then(|id| Uuid::parse_str(id).ok())
}

pub fn parse_collection_ref(value: &str) -> Option<Uuid> {
    value
        .strip_prefix("atelier://collection/")
        .and_then(|id| Uuid::parse_str(id).ok())
}

pub fn parse_tag_ref(value: &str) -> Option<Uuid> {
    value
        .strip_prefix("atelier://tag/")
        .and_then(|id| Uuid::parse_str(id).ok())
}

pub fn validate_character_ref(value: &str) -> bool {
    parse_character_ref(value).is_some()
}

pub fn validate_sheet_version_ref(value: &str) -> bool {
    parse_sheet_version_ref(value).is_some()
}

pub fn validate_sheet_artifact_ref(value: &str) -> bool {
    parse_sheet_artifact_ref(value).is_some()
}

pub fn validate_media_asset_ref(value: &str) -> bool {
    parse_media_asset_ref(value).is_some()
}

pub fn validate_collection_ref(value: &str) -> bool {
    parse_collection_ref(value).is_some()
}

pub fn validate_tag_ref(value: &str) -> bool {
    parse_tag_ref(value).is_some()
}
