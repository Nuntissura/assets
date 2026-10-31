//! Save a selection to a persistent Folio channel and load it back (lane SMB-FOLIO-CHANNEL-RECORD;
//! STU-RAS-022, MT-038 AC-001, MT-30095; polarity per STU-RAS-144, Studio reading, Photoshop
//! behaviour UNVERIFIED, D5). The record is Folio's `StudioRasterRecords.channels`; every operation
//! takes the caller's command context and emits exactly one terminal Observe frame.
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CorrelationId};
use hsk_studio_folio::{
    MAX_CHANNEL_NAME_BYTES, StudioChannelColour, StudioChannelKind, StudioRasterRecords, TileRef,
};
use hsk_studio_observe::{DeliveryClass, DeliveryError, Outcome, SinkPort};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::selection::{
    CombineMode, CoveragePlane, RectF, SelectionContext, SelectionLimits, ellipse, feather,
};
use hsk_studio_pigment::selection_channel_roundtrip::*;

const W: u32 = 300;
const H: u32 = 200;
const PROFILE: &str = "SCPF-019abcde-0000-7000-8000-000000000001";

fn sdoc() -> String {
    "SDOC-019abcde-0000-7000-8000-000000000001".to_owned()
}
fn cmd() -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(actor, ClientSessionId::parse("cli-1").unwrap(), None, 7, CorrelationId::new(42), None)
}
#[derive(Default)]
struct Frames(Vec<(DeliveryClass, usize)>);
impl SinkPort for Frames {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.len()));
        Ok(())
    }
}
struct Mint;
impl ChannelTileMint for Mint {
    fn artifact_manifest_id(&mut self, _: &str, _: &str, sha: &str) -> String {
        format!("manifest-{}", &sha[..16])
    }
}
fn grid() -> ChannelGrid<'static> {
    ChannelGrid { width: W, height: H, tile_edge: 128, colour_profile_id: PROFILE }
}
fn new_channel(id: &str, kind: StudioChannelKind, depth: u8) -> SaveTarget {
    SaveTarget::New(NewChannel {
        channel_id: id.to_owned(),
        name: format!("Alpha {id}"),
        kind,
        colour: StudioChannelColour::Rgb { r: 1.0, g: 0.0, b: 0.0 },
        opacity: 50.0,
        depth,
    })
}
/// A feathered ellipse (soft edge: every 16-bit level band is exercised) partly outside the canvas.
fn selection() -> CoveragePlane {
    let ctx = SelectionContext::new(Rect { x: 0, y: 0, width: W, height: H });
    let e = ellipse(RectF { x: -20.0, y: 30.0, width: 220.0, height: 140.0 }, &ctx).unwrap();
    feather(&e, 6.0, &ctx).unwrap()
}
fn none() -> Vec<ChannelTilePayload> {
    Vec::new()
}
fn save(records: &StudioRasterRecords, plane: &CoveragePlane, target: &SaveTarget, stored: &[ChannelTilePayload]) -> ChannelEdit {
    let mut frames = Frames::default();
    let out = save_selection(records, plane, target, &grid(), stored, &mut Mint, &SelectionLimits::default(), &cmd(), &CancellationToken::default(), &mut frames);
    assert!(out.delivery.is_ok(), "one terminal frame delivered");
    assert_eq!(frames.0.len(), 1);
    assert_eq!(frames.0[0].0, DeliveryClass::Terminal);
    out.result.expect("save succeeds")
}
fn load(edit: &ChannelEdit, id: &str, into: Option<&CoveragePlane>, mode: CombineMode) -> CoveragePlane {
    let mut frames = Frames::default();
    let out = load_selection_attributed(&edit.records, id, edit.payloads.as_slice(), into, mode, &SelectionLimits::default(), &cmd(), &CancellationToken::default(), &mut frames);
    assert_eq!(out.delivery.map(|r| r.outcome), Ok(Outcome::Success));
    out.result.expect("load succeeds")
}
fn quantised8(v: u16) -> u16 {
    let q = (u32::from(v) * 255 + 32767) / 65535;
    (q * 257) as u16
}

#[test]
fn save_then_load_is_identical_at_16_and_32_bits_and_quantised_at_8() {
    let plane = selection();
    assert!(plane.data.iter().any(|v| *v != 0 && *v != u16::MAX), "fixture has soft coverage");
    let records = StudioRasterRecords::new(sdoc());
    for kind in [StudioChannelKind::MaskedAreaAlpha, StudioChannelKind::SelectedAreaAlpha, StudioChannelKind::SpotColour] {
        for depth in [8u8, 16, 32] {
            let edit = save(&records, &plane, &new_channel("c", kind, depth), &none());
            let c = edit.records.channel("c").unwrap();
            assert_eq!((c.width, c.height, c.depth, c.kind), (W, H, depth, kind));
            // 300 x 200 on a 128 grid: 3 x 2 cells, every one stored.
            assert_eq!(c.tiles.len(), 6);
            assert_eq!(edit.payloads.len(), 6);
            let loaded = load(&edit, "c", None, CombineMode::Replace);
            assert_eq!(loaded.bounds, Rect { x: 0, y: 0, width: W, height: H });
            for y in 0..H as i64 {
                for x in 0..W as i64 {
                    let want = if depth == 8 { quantised8(plane.at(x, y)) } else { plane.at(x, y) };
                    assert_eq!(loaded.at(x, y), want, "{kind:?}/{depth} at ({x},{y})");
                }
            }
            // Folio validates the returned record set (full grid, formats, extent).
            hsk_studio_folio::validate_channels(&edit.records.channels, hsk_studio_folio::Budget { nodes: 1 << 20, ..Default::default() }, &CancellationToken::default(), &hsk_studio_folio::Unresolved)
                .expect("saved record validates");
        }
    }
    // The input record set is untouched: a save returns a new set.
    assert!(records.channels.is_empty());
}

#[test]
fn polarity_follows_the_kind_and_payloads_hold_the_panel_tone() {
    // Full selection in the top-left 10 x 10 pixels only.
    let plane = CoveragePlane::filled(Rect { x: 0, y: 0, width: 10, height: 10 }, u16::MAX, &SelectionLimits::default()).unwrap();
    let records = StudioRasterRecords::new(sdoc());
    let tone_at = |edit: &ChannelEdit, x: usize, y: usize| {
        let p = &edit.payloads[0];
        let at = (y * 128 + x) * 2;
        u16::from_le_bytes([p.bytes[at], p.bytes[at + 1]])
    };
    let masked = save(&records, &plane, &new_channel("m", StudioChannelKind::MaskedAreaAlpha, 16), &none());
    let selected = save(&records, &plane, &new_channel("s", StudioChannelKind::SelectedAreaAlpha, 16), &none());
    let spot = save(&records, &plane, &new_channel("p", StudioChannelKind::SpotColour, 16), &none());
    // Masked-area alpha: the selection is white (the dark tone marks the masked region).
    assert_eq!((tone_at(&masked, 0, 0), tone_at(&masked, 50, 50)), (u16::MAX, 0));
    // Selected-area alpha and spot: the selection (or ink) is the dark tone.
    assert_eq!((tone_at(&selected, 0, 0), tone_at(&selected, 50, 50)), (0, u16::MAX));
    assert_eq!((tone_at(&spot, 0, 0), tone_at(&spot, 50, 50)), (0, u16::MAX));
    for k in [StudioChannelKind::MaskedAreaAlpha, StudioChannelKind::SelectedAreaAlpha, StudioChannelKind::SpotColour] {
        for t in [0u16, 1, 32768, u16::MAX] {
            assert_eq!(tone_from_coverage(k, coverage_from_tone(k, t)), t);
        }
    }
    assert!(polarity_differs(StudioChannelKind::MaskedAreaAlpha, StudioChannelKind::SelectedAreaAlpha));
    assert!(!polarity_differs(StudioChannelKind::SelectedAreaAlpha, StudioChannelKind::SpotColour));
    // Padding beyond the extent (third column cell spans 256..384 > 300) is zero bytes.
    let edge_cell = masked.payloads.iter().find(|p| p.column == 2 && p.row == 0).unwrap();
    assert!(edge_cell.bytes[(299 - 256 + 1) * 2..128 * 2].iter().all(|b| *b == 0));
    // Payload digests are SHA-256 of the bytes and match the record.
    for (t, p) in masked.records.channel("m").unwrap().tiles.iter().zip(&masked.payloads) {
        assert_eq!(t.content_digest.digest, p.sha256_hex);
        assert_eq!(t.content_digest.digest.len(), 64);
        assert_eq!(t.layer_id, "m");
    }
}

#[test]
fn load_combines_into_a_selection_and_save_combines_into_a_channel() {
    let limits = SelectionLimits::default();
    let left = CoveragePlane::filled(Rect { x: 0, y: 0, width: 100, height: H }, u16::MAX, &limits).unwrap();
    let right = CoveragePlane::filled(Rect { x: 200, y: 0, width: 100, height: H }, u16::MAX, &limits).unwrap();
    let records = StudioRasterRecords::new(sdoc());
    let edit = save(&records, &left, &new_channel("a", StudioChannelKind::SelectedAreaAlpha, 8), &none());
    // Load with Extend into the right half: both halves selected.
    let both = load(&edit, "a", Some(&right), CombineMode::Extend);
    assert_eq!((both.at(10, 10), both.at(250, 10), both.at(150, 10)), (u16::MAX, u16::MAX, 0));
    // Save into the existing channel with Extend, then Diminish the left half out again.
    let target = SaveTarget::Existing { channel_id: "a".to_owned(), mode: CombineMode::Extend };
    let grown = save(&edit.records, &right, &target, &edit.payloads);
    assert_eq!(grown.records.channels.len(), 1, "an existing channel is rewritten in place");
    let loaded = load(&grown, "a", None, CombineMode::Replace);
    assert_eq!((loaded.at(10, 10), loaded.at(250, 10), loaded.at(150, 10)), (u16::MAX, u16::MAX, 0));
    let target = SaveTarget::Existing { channel_id: "a".to_owned(), mode: CombineMode::Diminish };
    let shrunk = save(&grown.records, &left, &target, &grown.payloads);
    let loaded = load(&shrunk, "a", None, CombineMode::Replace);
    assert_eq!((loaded.at(10, 10), loaded.at(250, 10)), (0, u16::MAX));
}

fn refused_save(records: &StudioRasterRecords, plane: &CoveragePlane, target: &SaveTarget, grid: &ChannelGrid<'_>, limits: &SelectionLimits) -> (ChannelError, Outcome) {
    let mut frames = Frames::default();
    let out = save_selection(records, plane, target, grid, none().as_slice(), &mut Mint, limits, &cmd(), &CancellationToken::default(), &mut frames);
    (out.result.expect_err("refused"), out.delivery.expect("failure is reported").outcome)
}

#[test]
fn save_and_load_refusals_are_typed_and_reported() {
    let plane = selection();
    let limits = SelectionLimits::default();
    let records = StudioRasterRecords::new(sdoc());
    let edit = save(&records, &plane, &new_channel("c", StudioChannelKind::MaskedAreaAlpha, 32), &none());
    // Duplicate id, unknown id, names, extent, depth, tile edge.
    assert_eq!(refused_save(&edit.records, &plane, &new_channel("c", StudioChannelKind::SpotColour, 8), &grid(), &limits).0, ChannelError::DuplicateChannel);
    let unknown = SaveTarget::Existing { channel_id: "nope".to_owned(), mode: CombineMode::Replace };
    assert_eq!(refused_save(&edit.records, &plane, &unknown, &grid(), &limits).0, ChannelError::UnknownChannel);
    let long = SaveTarget::New(NewChannel { name: "n".repeat(MAX_CHANNEL_NAME_BYTES + 1), ..match new_channel("d", StudioChannelKind::MaskedAreaAlpha, 8) { SaveTarget::New(n) => n, _ => unreachable!() } });
    assert_eq!(refused_save(&records, &plane, &long, &grid(), &limits).0, ChannelError::NameTooLong);
    let empty = SaveTarget::New(NewChannel { name: String::new(), ..match new_channel("d", StudioChannelKind::MaskedAreaAlpha, 8) { SaveTarget::New(n) => n, _ => unreachable!() } });
    assert_eq!(refused_save(&records, &plane, &empty, &grid(), &limits).0, ChannelError::InvalidName);
    let other_extent = ChannelGrid { width: W + 1, ..grid() };
    assert_eq!(refused_save(&edit.records, &plane, &new_channel("d", StudioChannelKind::MaskedAreaAlpha, 8), &other_extent, &limits).0, ChannelError::ExtentMismatch);
    let (e, outcome) = refused_save(&records, &plane, &new_channel("d", StudioChannelKind::MaskedAreaAlpha, 12), &grid(), &limits);
    assert_eq!((e, outcome), (ChannelError::DepthUnsupported, Outcome::Failure(hsk_studio_observe::FailureCode::Unsupported)));
    assert_eq!(refused_save(&records, &plane, &new_channel("d", StudioChannelKind::MaskedAreaAlpha, 8), &ChannelGrid { tile_edge: 0, ..grid() }, &limits).0, ChannelError::TileEdge);
    // Hostile size (V4-02): the byte budget is checked before any cell is allocated.
    let huge = ChannelGrid { width: 300_000, height: 300_000, ..grid() };
    assert_eq!(refused_save(&records, &plane, &new_channel("d", StudioChannelKind::MaskedAreaAlpha, 32), &huge, &limits).0, ChannelError::BudgetExceeded);
    let small = SelectionLimits { max_bytes: 1024, ..limits };
    assert_eq!(refused_save(&records, &plane, &new_channel("d", StudioChannelKind::MaskedAreaAlpha, 8), &grid(), &small).0, ChannelError::BudgetExceeded);
    // An invalid channel id is a Folio record refusal.
    assert_eq!(refused_save(&records, &plane, &new_channel("bad id", StudioChannelKind::MaskedAreaAlpha, 8), &grid(), &limits).0, ChannelError::Record(hsk_studio_folio::Code::InvalidId));

    // Load: a missing tile, wrong size, a digest mismatch and a non-finite float sample.
    let c = edit.records.channel("c").unwrap();
    let cancel = CancellationToken::default();
    // (Payloads resolve by digest; identical cells share one, so every copy of tile 0's digest goes.)
    let mut payloads = edit.payloads.clone();
    payloads.retain(|p| p.sha256_hex != c.tiles[0].content_digest.digest);
    assert_eq!(load_selection(c, payloads.as_slice(), None, CombineMode::Replace, &limits, &cancel), Err(ChannelError::TileMissing));
    let mut payloads = edit.payloads.clone();
    payloads[0].bytes.pop();
    assert_eq!(load_selection(c, payloads.as_slice(), None, CombineMode::Replace, &limits, &cancel), Err(ChannelError::TileSize));
    let mut payloads = edit.payloads.clone();
    payloads[0].bytes[0] ^= 1;
    assert_eq!(load_selection(c, payloads.as_slice(), None, CombineMode::Replace, &limits, &cancel), Err(ChannelError::HashMismatch));
    // A NaN sample whose digest matches (a hostile but consistent store).
    let mut payloads = edit.payloads.clone();
    payloads[0].bytes[..4].copy_from_slice(&f32::NAN.to_le_bytes());
    let digest = hsk_studio_prism::hash_hex(&hsk_studio_prism::profile_hash(&payloads[0].bytes));
    payloads[0].sha256_hex = digest.clone();
    let mut hostile = c.clone();
    hostile.tiles[0].content_digest.digest = digest;
    assert_eq!(load_selection(&hostile, payloads.as_slice(), None, CombineMode::Replace, &limits, &cancel), Err(ChannelError::Nonfinite));
    // Canceled before any work.
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(load_selection(c, edit.payloads.as_slice(), None, CombineMode::Replace, &limits, &canceled), Err(ChannelError::Canceled));
    // An unknown channel through the attributed load is reported as a failure frame.
    let mut frames = Frames::default();
    let out = load_selection_attributed(&edit.records, "nope", edit.payloads.as_slice(), None, CombineMode::Replace, &limits, &cmd(), &cancel, &mut frames);
    assert_eq!(out.result.unwrap_err(), ChannelError::UnknownChannel);
    assert_eq!(out.delivery.unwrap().outcome, Outcome::Failure(hsk_studio_observe::FailureCode::Validation));
    assert_eq!(frames.0.len(), 1);
}

#[test]
fn tile_bytes_resolve_by_record_digest() {
    let plane = selection();
    let edit = save(&StudioRasterRecords::new(sdoc()), &plane, &new_channel("c", StudioChannelKind::MaskedAreaAlpha, 16), &none());
    let t: &TileRef = &edit.records.channel("c").unwrap().tiles[3];
    let payloads = edit.payloads.as_slice();
    assert_eq!(ChannelTileBytes::bytes(payloads, t), Some(edit.payloads[3].bytes.as_slice()));
    assert_eq!(t.artifact_manifest_id, format!("manifest-{}", &t.content_digest.digest[..16]));
    assert_eq!(t.object_key, "ch_0_1");
}

// ---- Slice 3: channel list operations (MT-050 AC-001 record half, Duplicate of AC-002).

/// Two channels `a` (masked-area alpha) and `b` (spot), both saved from the fixture selection.
fn two_channels() -> ChannelEdit {
    let plane = selection();
    let first = save(&StudioRasterRecords::new(sdoc()), &plane, &new_channel("a", StudioChannelKind::MaskedAreaAlpha, 16), &none());
    let second = save(&first.records, &plane, &new_channel("b", StudioChannelKind::SpotColour, 8), &none());
    let mut payloads = first.payloads;
    payloads.extend(second.payloads);
    ChannelEdit { records: second.records, payloads }
}
fn ok<T: std::fmt::Debug>(out: Attributed<T>) -> T {
    assert_eq!(out.delivery.as_ref().map(|r| r.outcome), Ok(Outcome::Success), "success frame for {:?}", out.result);
    out.result.unwrap()
}
fn err<T: std::fmt::Debug>(out: Attributed<T>) -> ChannelError {
    assert!(matches!(out.delivery.as_ref().map(|r| r.outcome), Ok(Outcome::Failure(_))), "failure frame");
    out.result.unwrap_err()
}
fn ids(r: &StudioRasterRecords) -> Vec<&str> {
    r.channels.iter().map(|c| c.channel_id.as_str()).collect()
}

#[test]
fn list_operations_return_new_record_sets() {
    let base = two_channels();
    let (cancel, mut sink) = (CancellationToken::default(), Frames::default());
    // Rename keeps id and tiles.
    let renamed = ok(rename_channel(&base.records, "a", "Saved hair", &cmd(), &cancel, &mut sink));
    assert_eq!(renamed.channel("a").unwrap().name, "Saved hair");
    assert_eq!(renamed.channel("a").unwrap().tiles, base.records.channel("a").unwrap().tiles);
    assert_eq!(base.records.channel("a").unwrap().name, "Alpha a", "input unchanged");
    // Reorder.
    let reordered = ok(reorder_channel(&base.records, "b", 0, &cmd(), &cancel, &mut sink));
    assert_eq!(ids(&reordered), ["b", "a"]);
    // Visibility.
    let shown = ok(set_channel_visibility(&base.records, "b", true, &cmd(), &cancel, &mut sink));
    assert!(shown.channel("b").unwrap().visible && !base.records.channel("b").unwrap().visible);
    // Duplicate: placed after the source, same content, tiles follow the new id.
    let dup = ok(duplicate_channel(&base.records, "a", "a-copy", "Alpha a copy", &cmd(), &cancel, &mut sink));
    assert_eq!(ids(&dup), ["a", "a-copy", "b"]);
    let (src, copy) = (dup.channel("a").unwrap(), dup.channel("a-copy").unwrap());
    assert!(copy.tiles.iter().all(|t| t.layer_id == "a-copy"));
    assert_eq!(
        copy.tiles.iter().map(|t| &t.content_digest).collect::<Vec<_>>(),
        src.tiles.iter().map(|t| &t.content_digest).collect::<Vec<_>>()
    );
    let limits = SelectionLimits::default();
    assert_eq!(
        load_selection(copy, base.payloads.as_slice(), None, CombineMode::Replace, &limits, &cancel),
        load_selection(src, base.payloads.as_slice(), None, CombineMode::Replace, &limits, &cancel)
    );
    // Delete.
    let deleted = ok(delete_channel(&base.records, "a", &cmd(), &cancel, &mut sink));
    assert_eq!(ids(&deleted), ["b"]);
    // Every operation emitted exactly one terminal frame.
    assert_eq!(sink.0.len(), 5);
    assert!(sink.0.iter().all(|(c, _)| *c == DeliveryClass::Terminal));
}

#[test]
fn list_operation_refusals_are_typed() {
    let base = two_channels();
    let (cancel, mut sink) = (CancellationToken::default(), Frames::default());
    let r = &base.records;
    assert_eq!(err(rename_channel(r, "nope", "x", &cmd(), &cancel, &mut sink)), ChannelError::UnknownChannel);
    assert_eq!(err(rename_channel(r, "a", &"n".repeat(MAX_CHANNEL_NAME_BYTES + 1), &cmd(), &cancel, &mut sink)), ChannelError::NameTooLong);
    assert_eq!(err(rename_channel(r, "a", "tab\there", &cmd(), &cancel, &mut sink)), ChannelError::InvalidName);
    assert_eq!(err(reorder_channel(r, "a", 2, &cmd(), &cancel, &mut sink)), ChannelError::IndexOutOfRange);
    assert_eq!(err(set_channel_visibility(r, "nope", true, &cmd(), &cancel, &mut sink)), ChannelError::UnknownChannel);
    assert_eq!(err(duplicate_channel(r, "a", "b", "copy", &cmd(), &cancel, &mut sink)), ChannelError::DuplicateChannel);
    assert_eq!(err(duplicate_channel(r, "a", "bad id", "copy", &cmd(), &cancel, &mut sink)), ChannelError::Record(hsk_studio_folio::Code::InvalidId));
    assert_eq!(err(delete_channel(r, "nope", &cmd(), &cancel, &mut sink)), ChannelError::UnknownChannel);
    let canceled = CancellationToken::default();
    canceled.cancel();
    let out = delete_channel(r, "a", &cmd(), &canceled, &mut sink);
    assert_eq!(out.result.unwrap_err(), ChannelError::Canceled);
    assert_eq!(out.delivery.unwrap().outcome, Outcome::Canceled);
}

#[test]
fn kind_conversion_inverts_samples_only_across_polarity() {
    let base = two_channels();
    let (cancel, mut sink) = (CancellationToken::default(), Frames::default());
    let limits = SelectionLimits::default();
    let before = load_selection(base.records.channel("a").unwrap(), base.payloads.as_slice(), None, CombineMode::Replace, &limits, &cancel).unwrap();
    // Masked-area -> selected-area: samples inverted, the loaded selection is unchanged.
    let converted = ok(convert_channel_kind(&base.records, "a", StudioChannelKind::SelectedAreaAlpha, base.payloads.as_slice(), &mut Mint, &limits, &cmd(), &cancel, &mut sink));
    let a = converted.records.channel("a").unwrap();
    assert_eq!(a.kind, StudioChannelKind::SelectedAreaAlpha);
    assert_eq!(converted.payloads.len(), a.tiles.len());
    let after = load_selection(a, converted.payloads.as_slice(), None, CombineMode::Replace, &limits, &cancel).unwrap();
    assert_eq!(after, before, "the selection a channel loads as survives a kind change");
    let old = base.records.channel("a").unwrap();
    let tone = |payloads: &[ChannelTilePayload], t: &TileRef| {
        let p = payloads.iter().find(|p| p.sha256_hex == t.content_digest.digest).unwrap();
        u16::from_le_bytes([p.bytes[0], p.bytes[1]])
    };
    assert_eq!(tone(&converted.payloads, &a.tiles[0]), u16::MAX - tone(&base.payloads, &old.tiles[0]));
    // Padding beyond the extent stays zero (column 2 spans x 256..384 of a 300-wide raster).
    let edge = converted.payloads.iter().find(|p| p.column == 2 && p.row == 0).unwrap();
    assert!(edge.bytes[(300 - 256) * 2..128 * 2].iter().all(|b| *b == 0));
    // And back: the original bytes return.
    let back = ok(convert_channel_kind(&converted.records, "a", StudioChannelKind::MaskedAreaAlpha, converted.payloads.as_slice(), &mut Mint, &limits, &cmd(), &cancel, &mut sink));
    assert_eq!(
        back.records.channel("a").unwrap().tiles.iter().map(|t| &t.content_digest).collect::<Vec<_>>(),
        old.tiles.iter().map(|t| &t.content_digest).collect::<Vec<_>>()
    );
    // Spot -> selected-area: same polarity, only the kind changes (no payloads).
    let spot = ok(convert_channel_kind(&base.records, "b", StudioChannelKind::SelectedAreaAlpha, base.payloads.as_slice(), &mut Mint, &limits, &cmd(), &cancel, &mut sink));
    assert!(spot.payloads.is_empty());
    assert_eq!(spot.records.channel("b").unwrap().tiles, base.records.channel("b").unwrap().tiles);
    // Same kind, unknown channel, missing bytes are typed.
    assert_eq!(err(convert_channel_kind(&base.records, "a", StudioChannelKind::MaskedAreaAlpha, base.payloads.as_slice(), &mut Mint, &limits, &cmd(), &cancel, &mut sink)), ChannelError::NoChange);
    assert_eq!(err(convert_channel_kind(&base.records, "z", StudioChannelKind::SpotColour, base.payloads.as_slice(), &mut Mint, &limits, &cmd(), &cancel, &mut sink)), ChannelError::UnknownChannel);
    assert_eq!(err(convert_channel_kind(&base.records, "a", StudioChannelKind::SpotColour, none().as_slice(), &mut Mint, &limits, &cmd(), &cancel, &mut sink)), ChannelError::TileMissing);
}
