//! Bounded external-file consumer transport, distinct from persisted document JSON.
use crate::*;
use hsk_studio_accord::{ActorContext, DomainId};
use hsk_studio_folio::TileRef;
use serde::{Deserialize, Serialize};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    pub account_id: String,
    pub principal_id: String,
    pub owner_account_id: String,
    pub owner_principal_id: String,
    pub access_space_id: String,
    pub session_id: String,
}
impl Actor {
    pub fn validated(&self) -> Result<ActorContext, Error> {
        ActorContext::new(
            &self.account_id,
            &self.principal_id,
            &self.owner_account_id,
            &self.owner_principal_id,
            &self.access_space_id,
            &self.session_id,
        )
        .map_err(|_| Error::InvalidInput)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tile {
    pub tile_ref: TileRef,
    pub bounds: Rect,
    pub sample_format: String,
    pub colour_path: String,
    pub colour_stride_bytes: u64,
    pub colour_sha256: String,
    pub alpha_path: String,
    pub alpha_stride_bytes: u64,
    pub alpha_sha256: String,
    pub transfer: String,
    pub alpha_association: String,
    pub profile_path: String,
    pub profile_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub before: Tile,
    pub replacement: Tile,
    pub expected_property_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskWire {
    pub bounds: Rect,
    pub sample_format: String,
    pub path: String,
    pub stride_bytes: u64,
    pub sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentRevision {
    pub layer_id: String,
    pub object_key: String,
    pub column: i64,
    pub row: i64,
    pub revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Current {
    pub document_id: String,
    pub layer_id: String,
    pub actor: Actor,
    pub cancel_epoch: u64,
    pub revisions: Vec<CurrentRevision>,
    pub byte_limit: u64,
    pub sink: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub transport_version: u8,
    pub operation: String,
    pub document_id: String,
    pub layer_id: String,
    pub command_id: String,
    pub correlation_id: u64,
    pub actor: Actor,
    pub base_revision: u64,
    pub cancel_epoch: u64,
    pub grid: Grid,
    pub rectangle: Rect,
    pub mask: MaskWire,
    pub tiles: Vec<Item>,
    pub byte_limit: u64,
    pub current: Current,
}
pub fn parse(bytes: &[u8]) -> Result<Input, Error> {
    if bytes.len() > 262144 {
        return Err(Error::BudgetExceeded);
    }
    let value: Input = serde_json::from_slice(bytes).map_err(|_| Error::InvalidInput)?;
    if value.tiles.len() > MAX_TILES || value.current.revisions.len() > MAX_TILES {
        return Err(Error::BudgetExceeded);
    }
    Ok(value)
}
pub fn digest(s: &str) -> Result<[u8; 32], Error> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::InvalidInput);
    }
    let mut a = [0; 32];
    for (i, b) in a.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| Error::InvalidInput)?
    }
    Ok(a)
}
pub fn hex(bytes: &[u8]) -> String {
    const H: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char)
    }
    s
}
pub struct LoadedTile {
    pub colour: Vec<u8>,
    pub alpha: Vec<u8>,
    pub profile: Vec<u8>,
    pub id: DomainId,
}
impl LoadedTile {
    pub fn borrowed<'a>(&'a self, t: &'a Tile) -> Result<ResolvedTile<'a>, Error> {
        Ok(ResolvedTile {
            tile_ref: &t.tile_ref,
            bounds: t.bounds,
            sample_format: &t.sample_format,
            colour_bytes: &self.colour,
            colour_stride_bytes: t.colour_stride_bytes,
            colour_sha256: digest(&t.colour_sha256)?,
            alpha_bytes: &self.alpha,
            alpha_stride_bytes: t.alpha_stride_bytes,
            alpha_sha256: digest(&t.alpha_sha256)?,
            transfer: &t.transfer,
            alpha_association: &t.alpha_association,
            profile: hsk_studio_prism::ProfileInput {
                profile_id: &self.id,
                bytes: &self.profile,
                expected_sha256: digest(&t.profile_sha256)?,
            },
        })
    }
}
#[derive(Serialize)]
struct CountsWire {
    attempted: u32,
    admitted: u32,
    changed: u32,
    output_bytes: u64,
    scratch_bytes: u64,
    retained_preimage_bytes: u64,
}
fn counts(c: Counts) -> CountsWire {
    CountsWire {
        attempted: c.attempted,
        admitted: c.admitted,
        changed: c.changed,
        output_bytes: c.output_bytes,
        scratch_bytes: c.scratch_bytes,
        retained_preimage_bytes: c.retained_preimage_bytes,
    }
}
pub fn output(outcome: &Outcome, frames: &[Vec<u8>]) -> serde_json::Value {
    let frames: Vec<_> = frames.iter().map(|b| hex(b)).collect();
    match outcome {
        Outcome::Accepted(p) => {
            let updates:Vec<_>=p.updates.iter().map(|u|{
                let conversions:Vec<_>=u.conversions.iter().map(|r|serde_json::json!({"source_profile_id":r.source_profile_id.as_str(),"destination_profile_id":r.destination_profile_id.as_str(),"source_sha256":hex(&r.source_sha256),"destination_sha256":hex(&r.destination_sha256),"engine":r.engine_name,"engine_version":r.engine_version,"intent":r.intent.code(),"bit_depth":r.bit_depth,"bpc":r.black_point_compensation,"options":{"cicp":r.options.allow_use_cicp_transfer,"fixed_point":r.options.prefer_fixed_point,"extended_range_rgb_xyz":r.options.allow_extended_range_rgb_xyz,"clamped_output":r.options.clamped_output}})).collect();
                serde_json::json!({"tile_ref":u.after.tile_ref,"property":"samples","previous_revision":u.previous_revision,"revision":u.revision,"damage":u.damage,"after":{"tile_ref":u.after.tile_ref,"profile_sha256":hex(&u.after.profile_sha256),"sample_format":"rgb_f32le","transfer":"linear_light","alpha_association":"straight","bounds":u.after.bounds,"colour_stride_bytes":u.after.colour_stride_bytes,"alpha_stride_bytes":u.after.alpha_stride_bytes,"colour_hex":hex(&u.after.colour),"alpha_hex":hex(&u.after.alpha),"colour_sha256":hex(&u.after.colour_sha256),"alpha_sha256":hex(&u.after.alpha_sha256)},"preimage":{"tile_ref":u.preimage.tile_ref,"profile_sha256":hex(&u.preimage.profile_sha256),"sample_format":"rgb_f32le","transfer":"linear_light","alpha_association":"straight","bounds":u.preimage.bounds,"colour_stride_bytes":u.preimage.colour_stride_bytes,"alpha_stride_bytes":u.preimage.alpha_stride_bytes,"colour_hex":hex(&u.preimage.colour),"alpha_hex":hex(&u.preimage.alpha),"colour_sha256":hex(&u.preimage.colour_sha256),"alpha_sha256":hex(&u.preimage.alpha_sha256)},"conversions":conversions})
            }).collect();
            serde_json::json!({"disposition":if updates.is_empty(){"no_change"}else{"changed"},"document_id":p.document_id.as_str(),"command_id":p.command_id,"correlation_id":p.correlation_id,"cancel_epoch":p.cancel_epoch,"actor":{"account_id":p.actor.account_id(),"principal_id":p.actor.principal_id(),"owner_account_id":p.actor.owner_account_id(),"owner_principal_id":p.actor.owner_principal_id(),"access_space_id":p.actor.access_space_id(),"session_id":p.actor.session_id()},"updates":updates,"lease_handle":p.lease.as_ref().map(Lease::handle),"counts":counts(p.counts),"diagnostic_frames_hex":frames})
        }
        Outcome::Rejected(f) => {
            serde_json::json!({"disposition":"rejected","error":f.error.code(),"delivery":f.delivery.as_ref().map(|_|"accepted").unwrap_or_else(|e|e.code()),"counts":counts(f.counts),"diagnostic_frames_hex":frames})
        }
        Outcome::ReconciliationRequired {
            token,
            counts: c,
            delivery,
        } => {
            serde_json::json!({"disposition":"reconciliation_required","pending_token":token,"delivery":delivery.code(),"counts":counts(*c),"diagnostic_frames_hex":frames})
        }
    }
}
