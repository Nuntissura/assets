//! Lane SMB-PS-SOFT-PROOF-2 (feature `viewer`): soft proofing in the window, driven headlessly (one-frame
//! egui draws into a bare context, no window). VIEW > Proof and COLOR > Proof carry the spec leaves
//! (STU-SHL-025 / STU-SHL-030), every change is the Folio-validated session entry `proof` as one undo
//! step that survives save and reopen (STU-COL-240), the canvas shows Prism's `SoftProof::run` and the
//! gamut overlay while the document, the frame and exports stay unproofed (STU-COL-164), and
//! `state_json()["proof"]` reads the gamut warning as numbers (STU-COL-244). Oracles: Prism's own
//! `prepare_soft_proof` / `run` / `gamut_warning` on the same buffer, the session entry bytes, the run
//! history and the exported file bytes.
#![cfg(feature = "viewer")]

use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_folio::{self as folio, ContentDigest, ProofColourVision, ShippedProofProfile, StudioProofIntent, StudioProofProfile};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::export::ExportFormat;
use hsk_studio_harness_view::view::proof::{self, ProofCommand, ProofLeaf, ProofMenu, ProofMenuLeaf, ProofPage};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{ShellCommand, menu_entries};
use hsk_studio_prism::{self as prism, GamutWarningStyle, ProfileInput, ProofCondition, profile_hash};
use serde_json::Value;
use std::path::{Path, PathBuf};

const CMYK: &[u8] = include_bytes!("../../hsk-studio-prism/tests/oracle/profiles/hsk-synth-cmyk-v2.icc");
const W: usize = 4;

// ---- fixtures --------------------------------------------------------------------------------------

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-view-proof-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The 4 x 2 pixels: saturated sRGB green, blue, red and cyan (beyond the synthetic ink gamut) and
/// mid grey, white, black and a skin tone.
const PIXELS: [[u8; 3]; 8] = [[0, 255, 0], [0, 0, 255], [255, 0, 0], [0, 255, 255], [128, 128, 128], [255, 255, 255], [0, 0, 0], [200, 150, 120]];

/// One opaque RGB 8-bit PSD layer of [`PIXELS`] (independent byte assembly, raw channels).
fn psd() -> Vec<u8> {
    let (width, height) = (W as u32, 2u32);
    let plane = |c: usize| PIXELS.iter().map(|p| p[c]).collect::<Vec<u8>>();
    let channels: Vec<(i16, Vec<u8>)> = vec![(0, plane(0)), (1, plane(1)), (2, plane(2))];
    let mut v = b"8BPS".to_vec();
    v.extend(1u16.to_be_bytes());
    v.extend([0u8; 6]);
    v.extend(3u16.to_be_bytes());
    v.extend(height.to_be_bytes());
    v.extend(width.to_be_bytes());
    v.extend(8u16.to_be_bytes());
    v.extend(3u16.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    let mut info = 1i16.to_be_bytes().to_vec();
    for r in [0i32, 0, height as i32, width as i32] {
        info.extend(r.to_be_bytes());
    }
    info.extend((channels.len() as u16).to_be_bytes());
    for (id, data) in &channels {
        info.extend(id.to_be_bytes());
        info.extend(((2 + data.len()) as u32).to_be_bytes());
    }
    info.extend_from_slice(b"8BIMnorm");
    info.extend([255u8, 0, 0x08, 0]);
    let mut extra = 0u32.to_be_bytes().to_vec();
    extra.extend(0u32.to_be_bytes());
    let mut pascal = vec![5u8];
    pascal.extend_from_slice(b"chart");
    while pascal.len() % 4 != 0 {
        pascal.push(0);
    }
    extra.extend(pascal);
    info.extend((extra.len() as u32).to_be_bytes());
    info.extend(extra);
    for (_, data) in &channels {
        info.extend(0u16.to_be_bytes());
        info.extend_from_slice(data);
    }
    if info.len() % 2 == 1 {
        info.push(0);
    }
    let mut section = (info.len() as u32).to_be_bytes().to_vec();
    section.extend(info);
    section.extend(0u32.to_be_bytes());
    v.extend((section.len() as u32).to_be_bytes());
    v.extend(section);
    v.extend(0u16.to_be_bytes());
    v.extend(std::iter::repeat_n(10u8, (width * height * 3) as usize));
    v
}

fn psd_at(dir: &Path) -> PathBuf {
    let p = dir.join("chart.psd");
    std::fs::write(&p, psd()).unwrap();
    p
}

/// A viewer with the chart open and rendered (render-cpu).
fn opened(scratch: &Scratch) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&psd_at(&scratch.0)).unwrap();
    v.render_if_dirty();
    assert!(v.frame().is_some(), "{:?}", v.render_error());
    v
}

/// [`opened`] with the synthetic CMYK profile as the document's CMYK working profile; its SCPF id.
fn opened_cmyk(scratch: &Scratch) -> (Viewer, String) {
    let mut v = opened(scratch);
    let id = v.set_document_cmyk_profile("Synthetic CMYK", CMYK).unwrap();
    v.render_if_dirty();
    (v, id)
}

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn did(text: &str) -> DomainId {
    DomainId::parse(text).unwrap()
}

/// Prism's soft proof of `condition` from the viewer's working space to the sRGB display.
fn prism_proof(condition: &ProofCondition<'_>) -> prism::SoftProof {
    let (doc, disp) = (did(proof::DOCUMENT_PROFILE_ID), did(proof::DISPLAY_PROFILE_ID));
    let working = prism::WORKING_PROFILE_LINEAR_SRGB;
    let srgb = hsk_studio_harness_view::view::develop::SRGB_V4_ICC;
    prism::prepare_soft_proof(
        ProfileInput { profile_id: &doc, bytes: working, expected_sha256: profile_hash(working) },
        condition,
        ProfileInput { profile_id: &disp, bytes: srgb, expected_sha256: profile_hash(srgb) },
        &token(),
    )
    .unwrap()
}

fn quantize(display: &[f32], frame: &[u8]) -> Vec<u8> {
    display.chunks_exact(3).zip(frame.chunks_exact(4)).flat_map(|(c, p)| c.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).chain([p[3]]).collect::<Vec<_>>()).collect()
}

fn frame_rgba(v: &Viewer) -> Vec<u8> {
    v.frame().unwrap().rgba8.clone()
}

fn entry(v: &Viewer) -> Option<Vec<u8>> {
    v.run().document_entry(proof::ENTRY).map(<[u8]>::to_vec)
}

fn undo_labels(v: &Viewer) -> Vec<String> {
    v.run().history()["undo"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_owned()).collect()
}

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1200.0))), events, ..Default::default() }
}

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    ctx.run(input(events), |ctx| v.ui(ctx))
}

#[test]
fn not_proven_lines() {
    println!("NOT_PROVEN: GPU shared-device display path with proof on (desktop window; the proof view uses the read-back frame)");
    println!("NOT_PROVEN: equality with Photoshop View > Proof Colors / Gamut Warning output and defaults (no D5 proof case)");
}

// ---- menus -----------------------------------------------------------------------------------------

#[test]
fn proof_menus_list_the_spec_leaves() {
    let v = Viewer::new(Prefer::Cpu);
    let bar = v.menu_bar();
    assert_eq!(bar.audit(), vec![]);
    let titles: Vec<&str> = bar.titles.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(titles, ["File", "Edit", "View", "Color", "Workspace", "Help"], "COLOR between VIEW and WORKSPACE ([STU-SHL-011])");
    let labels = |id: &str| bar.find(id).unwrap().children.iter().map(|c| c.label.clone()).collect::<Vec<_>>();
    // STU-SHL-025 VIEW > Proof (submenu id studio.color.proof) and STU-SHL-030 COLOR > Proof.
    assert_eq!(bar.find(proof::VIEW_MENU_ID).unwrap().label, "Proof");
    assert_eq!(labels(proof::VIEW_MENU_ID), ["Proof Setup…", "Proof Colours", "Gamut Warning", "Colour-Vision Proof…"]);
    assert_eq!(labels(proof::COLOR_MENU_ID), ["Proof Setup…", "Custom Proof Condition…", "Proof Colours", "Gamut Warning", "Colour-Vision Proof…", "Save Proof Preset…"]);
    assert!(labels("view").contains(&"Proof".to_string()) && labels("color") == ["Proof"]);
    // Leaf ids derived from the submenu ids (declared reading); no key bindings (the spec declares none).
    let leaves = ProofMenuLeaf::all();
    assert_eq!(leaves.len(), 10);
    for l in &leaves {
        let node = bar.find(&l.id()).unwrap_or_else(|| panic!("{} missing", l.id()));
        assert_eq!((node.label.as_str(), node.shortcut.as_deref()), (l.leaf.label(), None), "{}", l.id());
        assert_eq!(ShellCommand::from_id(&l.id()), Some(ShellCommand::Proof(*l)));
    }
    assert_eq!(ProofMenuLeaf { menu: ProofMenu::View, leaf: ProofLeaf::Colours }.id(), "studio.color.proof.colours");
    assert_eq!(ProofMenuLeaf { menu: ProofMenu::Color, leaf: ProofLeaf::SavePreset }.id(), "studio.color.proof_menu.save_preset");
    let entries = menu_entries();
    let mut ids: Vec<String> = entries.iter().map(|e| e.command.id()).collect();
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate command ids");
    assert!(entries.iter().filter(|e| matches!(e.command, ShellCommand::Proof(_))).all(|e| e.shortcut.is_none()));
    // Folio's shipped preset names are Prism's, in Prism's order.
    let dummy = did(proof::WORKING_RGB_PROFILE_ID);
    let working = prism::WORKING_PROFILE_LINEAR_SRGB;
    let input = ProfileInput { profile_id: &dummy, bytes: working, expected_sha256: profile_hash(working) };
    let prism_names: Vec<String> = prism::shipped_proof_conditions(input, input).into_iter().map(|c| c.name).collect();
    assert_eq!(prism_names, folio::SHIPPED_PROOF_PRESETS);
    // The projection agents read carries the same leaves.
    let nodes = v.state_json()["shell"]["menu"]["nodes"].as_array().unwrap().clone();
    assert!(nodes.iter().any(|n| n["id"] == "studio.color.proof.gamut_warning" && n["menu_path"] == serde_json::json!(["View", "Proof", "Gamut Warning"])));
    assert!(nodes.iter().any(|n| n["id"] == "studio.color.proof_menu.custom_condition" && n["menu_path"] == serde_json::json!(["Color", "Proof", "Custom Proof Condition…"])));
}

#[test]
fn proof_menu_is_unavailable_without_a_document() {
    let mut v = Viewer::new(Prefer::Cpu);
    let bar = v.menu_bar();
    for l in ProofMenuLeaf::all() {
        let node = bar.find(&l.id()).unwrap();
        let a = node.availability.json();
        assert_eq!(a["availability_state"], "INAPPLICABLE_HERE", "{}", l.id());
        assert_eq!(a["reason_code"], "NO_SUCH_CONTAINER");
        assert_eq!(a["remedy_command_id"], "studio.document.open");
        let e = v.shell_command(ShellCommand::Proof(l)).unwrap_err();
        assert_eq!(e, node.availability.reason_line());
        assert!(e.contains("no open document to proof"), "{e}");
    }
    assert_eq!(v.proof_command(ProofCommand::SetProofColours(true)), Err("nothing open to proof".into()));
    assert!(v.proof_page().is_none());
    let s = v.state_json()["proof"].clone();
    assert_eq!((s["entry"].as_bool(), s["enabled"].as_bool(), s["canvas"].as_str()), (Some(false), Some(false), Some("frame")), "{s}");
    // Drawing the bar without a document commits nothing.
    let ctx = egui::Context::default();
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(v.run().ops().is_empty());
}

/// Feature `desktop`: View, View > Proof and Proof Colours clicked through AccessKit turn the proof view on.
#[cfg(feature = "desktop")]
#[test]
fn accesskit_click_toggles_proof_colours() {
    fn ak_find(out: &egui::FullOutput, pred: impl Fn(&str) -> bool) -> Option<(egui::accesskit::NodeId, bool)> {
        let tree = out.platform_output.accesskit_update.as_ref()?;
        tree.nodes.iter().find(|(_, n)| n.label().is_some_and(&pred)).map(|(id, n)| (*id, n.is_disabled()))
    }
    fn ak_click(id: egui::accesskit::NodeId) -> egui::Event {
        egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest { action: egui::accesskit::Action::Click, target: id, data: None })
    }
    let scratch = Scratch::new("accesskit");
    let mut v = opened(&scratch);
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    // Settle the layout before any pointer or AccessKit event (F-TL-2).
    for _ in 0..3 {
        let _ = frame(&mut v, &ctx, vec![]);
    }
    let out = frame(&mut v, &ctx, vec![]);
    let (view, disabled) = ak_find(&out, |l| l == "View").expect("View menu node");
    assert!(!disabled);
    let _ = frame(&mut v, &ctx, vec![ak_click(view)]);
    let out = frame(&mut v, &ctx, vec![]);
    let (sub, disabled) = ak_find(&out, |l| l.starts_with("Proof") && !l.contains("Colours") && !l.contains("Setup")).expect("Proof submenu node");
    assert!(!disabled);
    let _ = frame(&mut v, &ctx, vec![ak_click(sub)]);
    let out = frame(&mut v, &ctx, vec![]);
    let (leaf, disabled) = ak_find(&out, |l| l.contains("Proof Colours")).expect("Proof Colours node");
    assert!(!disabled);
    let _ = frame(&mut v, &ctx, vec![ak_click(leaf)]);
    let _ = frame(&mut v, &ctx, vec![]);
    let s = v.state_json()["proof"].clone();
    // No CMYK working profile in the document: the toggle selects working RGB (declared reading).
    assert_eq!((s["enabled"].as_bool(), s["active_condition"].as_str(), s["canvas"].as_str()), (Some(true), Some("working_rgb"), Some("proof")), "{s}");
    assert_eq!(undo_labels(&v).last().map(String::as_str), Some("proof_colours proof"));
}

// ---- document-scoped state ---------------------------------------------------------------------------

#[test]
fn selecting_a_preset_writes_the_document_entry_and_undo_restores() {
    let scratch = Scratch::new("select");
    let (mut v, _) = opened_cmyk(&scratch);
    assert!(entry(&v).is_none());
    let revision = v.state_json()["revision"].clone();
    let steps = undo_labels(&v).len();
    // Proof Setup… opens the window on the setup page with one row per preset.
    v.shell_command(ShellCommand::Proof(ProofMenuLeaf { menu: ProofMenu::View, leaf: ProofLeaf::Setup })).unwrap();
    assert_eq!(v.proof_page(), Some(ProofPage::Setup));
    let ctx = egui::Context::default();
    let _ = frame(&mut v, &ctx, vec![]);
    for name in [proof::widget::WINDOW.to_string(), proof::widget::preset(0), proof::widget::preset(5), proof::widget::GAMUT_OPACITY.into(), proof::widget::CLOSE.into()] {
        assert!(v.drawn().contains(&name), "{name} not drawn: {:?}", v.drawn());
    }
    assert_eq!(v.state_json()["proof"]["window"], "setup");
    // Selecting a preset: the Folio-validated entry, ONE undo step, no document revision.
    v.proof_command(ProofCommand::Select("working_cmyk".into())).unwrap();
    let r = v.proof_records().unwrap().unwrap();
    assert_eq!((r.active_condition.as_deref(), r.proof_colours), (Some("working_cmyk"), true));
    let bytes = entry(&v).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&r).unwrap());
    assert_eq!(undo_labels(&v).len(), steps + 1);
    assert_eq!(undo_labels(&v).last().map(String::as_str), Some("proof_select proof"));
    assert_eq!(v.state_json()["revision"], revision, "proof state is not a document revision");
    // A second step, then undo restores each previous state in turn.
    v.proof_command(ProofCommand::SelectColourVision(ProofColourVision::Tritanopia)).unwrap();
    assert_eq!(v.proof_records().unwrap().unwrap().active_condition.as_deref(), Some("tritanopia"));
    v.undo().unwrap();
    assert_eq!(entry(&v), Some(bytes.clone()));
    v.undo().unwrap();
    assert!(entry(&v).is_none() && v.proof_records().unwrap().is_none());
    assert_eq!(v.state_json()["proof"]["canvas"], "frame");
    v.redo().unwrap();
    assert_eq!(entry(&v), Some(bytes));
    // Unknown names and Folio refusals change nothing.
    let before = entry(&v);
    let steps = undo_labels(&v).len();
    assert!(v.proof_command(ProofCommand::Select("press".into())).unwrap_err().contains("no proof condition"));
    let e = v.proof_command(ProofCommand::SetGamutStyle { colour: [0.0, 0.0, 0.0], opacity: 101 }).unwrap_err();
    assert!(e.contains("gamut_warning/opacity"), "{e}");
    let e = v.proof_command(ProofCommand::SetGamutStyle { colour: [1.5, 0.0, 0.0], opacity: 50 }).unwrap_err();
    assert!(e.contains("gamut_warning/colour"), "{e}");
    assert_eq!((entry(&v), undo_labels(&v).len()), (before, steps));
    v.proof_close_page();
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(!v.drawn().contains(&proof::widget::WINDOW.to_string()));
}

#[test]
fn custom_condition_and_save_preset_round_trip_through_save_and_reopen() {
    let scratch = Scratch::new("custom");
    let (mut v, profile_id) = opened_cmyk(&scratch);
    // COLOR > Proof > Custom Proof Condition… opens the draft page; the draft is the page's fields.
    v.shell_command(ShellCommand::Proof(ProofMenuLeaf { menu: ProofMenu::Color, leaf: ProofLeaf::CustomCondition })).unwrap();
    assert_eq!(v.proof_page(), Some(ProofPage::Custom));
    let ctx = egui::Context::default();
    let _ = frame(&mut v, &ctx, vec![]);
    for name in [proof::widget::CUSTOM_NAME, proof::widget::CUSTOM_PROFILE, proof::widget::CUSTOM_PROOF_INTENT, proof::widget::CUSTOM_DISPLAY_INTENT, proof::widget::CUSTOM_BPC, proof::widget::CUSTOM_PAPER, proof::widget::CUSTOM_INK, proof::widget::CUSTOM_APPLY] {
        assert!(v.drawn().contains(&name.to_string()), "{name} not drawn");
    }
    let digest = ContentDigest { algorithm: "sha256".into(), digest: prism::hash_hex(&profile_hash(CMYK)) };
    {
        let d = v.proof_draft_mut().unwrap();
        d.name = "Press coated".into();
        d.proof_profile = StudioProofProfile::Icc { profile_id: profile_id.clone(), content_digest: digest.clone() };
        d.proof_intent = StudioProofIntent::Perceptual;
        d.display_intent = StudioProofIntent::AbsoluteColorimetric;
        d.black_point_compensation = false;
        d.simulate_paper_color = true;
        d.simulate_black_ink = true;
    }
    v.proof_apply_draft().unwrap();
    let r = v.proof_records().unwrap().unwrap();
    assert_eq!((r.active_condition.as_deref(), r.proof_colours, r.conditions.len()), (Some("Press coated"), true, 1));
    // The six fields reach Prism unchanged: the proof receipt names both intents and both flags.
    let s = v.state_json()["proof"].clone();
    assert_eq!(s["condition"]["proof_intent"], "perceptual");
    assert_eq!(s["receipt"]["condition"]["display_intent"], "absolute_colorimetric", "{s}");
    assert_eq!((s["receipt"]["condition"]["simulate_paper_color"].as_bool(), s["receipt"]["condition"]["simulate_black_ink"].as_bool(), s["receipt"]["condition"]["black_point_compensation"].as_bool()), (Some(true), Some(true), Some(false)));
    assert_eq!(s["receipt"]["proof_profile_id"], profile_id.as_str());
    // Save Proof Preset…: the active condition under a new name; a taken name is refused.
    v.shell_command(ShellCommand::Proof(ProofMenuLeaf { menu: ProofMenu::Color, leaf: ProofLeaf::SavePreset })).unwrap();
    assert_eq!(v.proof_page(), Some(ProofPage::SavePreset));
    v.proof_set_save_name("Press copy");
    v.proof_command(ProofCommand::SavePreset("Press copy".into())).unwrap();
    assert!(v.proof_command(ProofCommand::SavePreset("Press copy".into())).unwrap_err().contains("exists"));
    assert!(v.proof_command(ProofCommand::SavePreset("working_rgb".into())).unwrap_err().contains("conditions/2/name"));
    v.proof_command(ProofCommand::SetGamutStyle { colour: [1.0, 0.0, 1.0], opacity: 35 }).unwrap();
    v.proof_command(ProofCommand::SetGamutWarning(true)).unwrap();
    let saved = v.proof_records().unwrap().unwrap();
    assert_eq!(saved.conditions[1].name, "Press copy");
    assert_eq!(saved.conditions[1].proof_profile, saved.conditions[0].proof_profile);
    assert_eq!(saved.active_condition.as_deref(), Some("Press coated"));
    // Save and reopen: the same record set, the same proof view.
    let path = scratch.0.join("proofed.handshake");
    v.save(&path, false).unwrap();
    let view_before = v.proof_frame().unwrap().unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&path).unwrap();
    assert_eq!(w.proof_records().unwrap().unwrap(), saved);
    assert_eq!(w.proof_frame().unwrap().unwrap(), view_before);
    let s = w.state_json()["proof"].clone();
    assert_eq!((s["conditions"].clone(), s["gamut_warning"]["opacity"].as_u64()), (serde_json::json!(["Press coated", "Press copy"]), Some(35)));
    // The preset menu lists the shipped presets then the saved conditions.
    w.proof_open_page(ProofPage::Setup);
    let ctx = egui::Context::default();
    let _ = frame(&mut w, &ctx, vec![]);
    assert!(w.drawn().contains(&proof::widget::preset(7)) && !w.drawn().contains(&proof::widget::preset(8)), "{:?}", w.drawn());
    // A shipped-profile condition with its own intents is savable too.
    w.proof_command(ProofCommand::Custom(folio::StudioProofConditionRecord {
        name: "CMYK perceptual".into(),
        proof_profile: StudioProofProfile::Shipped { preset: ShippedProofProfile::WorkingCmyk },
        proof_intent: StudioProofIntent::Perceptual,
        display_intent: StudioProofIntent::RelativeColorimetric,
        black_point_compensation: true,
        simulate_paper_color: false,
        simulate_black_ink: false,
    }))
    .unwrap();
    assert_eq!(w.state_json()["proof"]["receipt"]["condition"]["proof_intent"], "perceptual");
}

// ---- canvas ------------------------------------------------------------------------------------------

#[test]
fn proof_colours_change_the_displayed_texture_not_the_document() {
    let scratch = Scratch::new("texture");
    let (mut v, profile_id) = opened_cmyk(&scratch);
    let unproofed = frame_rgba(&v);
    let document = v.run().snapshot().unwrap().encoded_bytes().to_vec();
    assert_eq!(v.proof_frame().unwrap(), None, "no proof view yet");
    v.proof_command(ProofCommand::Select("working_cmyk".into())).unwrap();
    let shown = v.proof_frame().unwrap().unwrap();
    // Oracle: Prism's own run over the same buffer with the same condition.
    let cmyk_id = did(&profile_id);
    let condition = ProofCondition::working_cmyk(ProfileInput { profile_id: &cmyk_id, bytes: CMYK, expected_sha256: profile_hash(CMYK) });
    let sp = prism_proof(&condition);
    let source = proof::proof_source(&unproofed);
    let run = sp.run(&source, W, true, &token()).unwrap();
    assert_eq!(shown, quantize(&run.pixels, &unproofed));
    assert_ne!(shown, unproofed, "the proof simulates; it does not pass through");
    // Neither the frame nor the document changed.
    assert_eq!(frame_rgba(&v), unproofed);
    assert_eq!(v.run().snapshot().unwrap().encoded_bytes(), document.as_slice());
    let s = v.state_json()["proof"].clone();
    assert_eq!(s["receipt"]["output_sha256"], prism::hash_hex(&run.receipt.output_sha256), "{s}");
    assert_eq!(s["receipt"]["proof_profile_id"], profile_id.as_str());
    assert_eq!(s["read_back"], false);
    // The canvas draws the proof texture; toggled off it draws the frame again.
    let ctx = egui::Context::default();
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(v.drawn().contains(&"view.canvas".to_string()));
    assert_eq!(v.state_json()["proof"]["canvas"], "proof");
    v.shell_command(ShellCommand::Proof(ProofMenuLeaf { menu: ProofMenu::View, leaf: ProofLeaf::Colours })).unwrap();
    assert_eq!(v.proof_frame().unwrap(), None);
    let _ = frame(&mut v, &ctx, vec![]);
    assert_eq!(v.state_json()["proof"]["canvas"], "frame");
    // Toggle off keeps the active condition; colour-vision proofs follow Prism the same way.
    v.proof_command(ProofCommand::SelectColourVision(ProofColourVision::Deuteranopia)).unwrap();
    let cvd = prism_proof(&ProofCondition::colour_vision(prism::ColourVision::Deuteranopia)).run(&source, W, true, &token()).unwrap();
    assert_eq!(v.proof_frame().unwrap().unwrap(), quantize(&cvd.pixels, &unproofed));
    // Without a CMYK working profile working_cmyk is refused with a typed reason, the canvas unproofed.
    let s2 = Scratch::new("texture-nocmyk");
    let mut plain = opened(&s2);
    plain.proof_command(ProofCommand::Select("working_cmyk".into())).unwrap();
    assert!(plain.proof_frame().unwrap_err().contains("proof_no_cmyk_working_profile"));
    let _ = frame(&mut plain, &ctx, vec![]);
    let s = plain.state_json()["proof"].clone();
    assert_eq!(s["canvas"], "frame");
    assert!(s["preview_error"].as_str().unwrap().contains("proof_no_cmyk_working_profile"), "{s}");
}

#[test]
fn export_stays_unproofed_while_proof_is_on() {
    let scratch = Scratch::new("export");
    let (mut v, _) = opened_cmyk(&scratch);
    let plain = scratch.0.join("plain.png");
    v.export(&plain, ExportFormat::Png, false).unwrap();
    v.proof_command(ProofCommand::Select("working_cmyk".into())).unwrap();
    v.proof_command(ProofCommand::SetGamutWarning(true)).unwrap();
    assert!(v.proof_frame().unwrap().is_some());
    let proofed = scratch.0.join("while-proof.png");
    v.export(&proofed, ExportFormat::Png, false).unwrap();
    assert_eq!(std::fs::read(&plain).unwrap(), std::fs::read(&proofed).unwrap(), "STU-COL-164: no proof in an export");
    let jpeg_plain = scratch.0.join("plain.jpg");
    let jpeg = scratch.0.join("while-proof.jpg");
    v.export(&jpeg, ExportFormat::Jpeg { quality: 90 }, false).unwrap();
    v.proof_command(ProofCommand::SetProofColours(false)).unwrap();
    v.proof_command(ProofCommand::SetGamutWarning(false)).unwrap();
    v.export(&jpeg_plain, ExportFormat::Jpeg { quality: 90 }, false).unwrap();
    assert_eq!(std::fs::read(&jpeg_plain).unwrap(), std::fs::read(&jpeg).unwrap());
}

#[test]
fn gamut_overlay_marks_out_of_gamut_pixels_with_the_configured_colour() {
    let scratch = Scratch::new("overlay");
    let (mut v, profile_id) = opened_cmyk(&scratch);
    let unproofed = frame_rgba(&v);
    v.proof_command(ProofCommand::Select("working_cmyk".into())).unwrap();
    v.proof_command(ProofCommand::SetProofColours(false)).unwrap();
    v.proof_command(ProofCommand::SetGamutStyle { colour: [1.0, 0.0, 1.0], opacity: 100 }).unwrap();
    v.proof_command(ProofCommand::SetGamutWarning(true)).unwrap();
    let shown = v.proof_frame().unwrap().unwrap();
    // Oracle: Prism's gamut warning on the same buffer.
    let cmyk_id = did(&profile_id);
    let condition = ProofCondition::working_cmyk(ProfileInput { profile_id: &cmyk_id, bytes: CMYK, expected_sha256: profile_hash(CMYK) });
    let sp = prism_proof(&condition);
    let source = proof::proof_source(&unproofed);
    let warning = sp.gamut_warning(&source, W, GamutWarningStyle::new([1.0, 0.0, 1.0], 100).unwrap(), &token()).unwrap();
    assert!(warning.mask[0] && warning.mask[1], "sRGB green and blue are beyond the synthetic ink gamut: {:?}", warning.mask);
    assert!(!warning.mask[4], "mid grey is in gamut");
    for (i, marked) in warning.mask.iter().enumerate() {
        let (got, frame_px) = (&shown[i * 4..i * 4 + 4], &unproofed[i * 4..i * 4 + 4]);
        if *marked {
            assert_eq!(got, &[255, 0, 255, frame_px[3]], "pixel {i} marked");
        } else {
            assert_eq!(got, frame_px, "pixel {i} unmarked stays as shown");
        }
    }
    // Opacity 50: (1 - a) * pixel + a * colour over what is shown.
    v.proof_command(ProofCommand::SetGamutStyle { colour: [1.0, 0.0, 1.0], opacity: 50 }).unwrap();
    let half = v.proof_frame().unwrap().unwrap();
    let base: Vec<f32> = unproofed.chunks_exact(4).flat_map(|p| [f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0]).collect();
    let w50 = sp.gamut_warning(&source, W, GamutWarningStyle::new([1.0, 0.0, 1.0], 50).unwrap(), &token()).unwrap();
    assert_eq!(half, quantize(&w50.apply_overlay(&base).unwrap(), &unproofed));
    // With Proof Colours on the overlay sits on the proof preview.
    v.proof_command(ProofCommand::SetProofColours(true)).unwrap();
    let both = v.proof_frame().unwrap().unwrap();
    let preview = sp.run(&source, W, true, &token()).unwrap();
    assert_eq!(both, quantize(&w50.apply_overlay(&preview.pixels).unwrap(), &unproofed));
    // The overlay never reaches the frame.
    assert_eq!(frame_rgba(&v), unproofed);
}

// ---- inspection ------------------------------------------------------------------------------------

#[test]
fn state_json_reports_area_fraction_and_regions() {
    let scratch = Scratch::new("state");
    let (mut v, profile_id) = opened_cmyk(&scratch);
    v.proof_command(ProofCommand::Select("working_cmyk".into())).unwrap();
    v.proof_command(ProofCommand::SetGamutWarning(true)).unwrap();
    let cmyk_id = did(&profile_id);
    let condition = ProofCondition::working_cmyk(ProfileInput { profile_id: &cmyk_id, bytes: CMYK, expected_sha256: profile_hash(CMYK) });
    let unproofed = frame_rgba(&v);
    let warning = prism_proof(&condition).gamut_warning(&proof::proof_source(&unproofed), W, GamutWarningStyle::default(), &token()).unwrap();
    // Read headlessly with no UI frame: the state computes the view itself.
    let s = v.state_json()["proof"].clone();
    let g = &s["gamut_warning"];
    assert_eq!(g["area_fraction"].as_f64(), Some(warning.area_fraction), "{s}");
    assert!(warning.area_fraction > 0.0 && warning.area_fraction < 1.0);
    assert_eq!(g["out_of_gamut_pixels"].as_u64(), Some(warning.out_of_gamut_pixels));
    assert_eq!(g["pixel_count"].as_u64(), Some(8));
    assert_eq!(g["region_count"].as_u64(), Some(warning.region_count));
    assert_eq!(g["regions_truncated"].as_bool(), Some(false));
    let regions = g["regions"].as_array().unwrap();
    assert_eq!(regions.len(), warning.regions.len());
    for (got, want) in regions.iter().zip(&warning.regions) {
        assert_eq!((got["x0"].as_u64(), got["y0"].as_u64(), got["x1"].as_u64(), got["y1"].as_u64(), got["pixels"].as_u64()), (Some(want.x0 as u64), Some(want.y0 as u64), Some(want.x1 as u64), Some(want.y1 as u64), Some(want.pixels)));
    }
    let b = warning.bounds.unwrap();
    assert_eq!((g["bounds"]["x0"].as_u64(), g["bounds"]["x1"].as_u64()), (Some(b.x0 as u64), Some(b.x1 as u64)));
    assert_eq!((g["method"].as_str(), g["gamut_intent"].as_str()), (Some("round_trip"), Some("relative_colorimetric")));
    assert_eq!(g["threshold_delta_e76"].as_f64(), Some(prism::GAMUT_ROUND_TRIP_DELTA_E76));
    assert_eq!((g["opacity"].as_u64(), g["colour"].clone()), (Some(100), serde_json::json!([0.5, 0.5, 0.5])));
    // The active condition and its six fields; the receipt's profile ids and hashes.
    assert_eq!(s["active_condition"], "working_cmyk");
    let c = s["condition"].as_object().unwrap();
    for field in ["name", "proof_profile", "proof_intent", "display_intent", "black_point_compensation", "simulate_paper_color", "simulate_black_ink"] {
        assert!(c.contains_key(field), "{field}");
    }
    let r = &s["receipt"];
    assert_eq!(r["document_profile_id"], proof::DOCUMENT_PROFILE_ID);
    assert_eq!(r["display_profile_id"], proof::DISPLAY_PROFILE_ID);
    assert_eq!(r["proof_profile_sha256"], prism::hash_hex(&profile_hash(CMYK)));
    assert_eq!(r["document_sha256"], prism::hash_hex(&profile_hash(prism::WORKING_PROFILE_LINEAR_SRGB)));
    assert_eq!(r["pixel_count"].as_u64(), Some(8));
    assert_eq!(s["presets"], serde_json::json!(folio::SHIPPED_PROOF_PRESETS));
    assert!(s["not_proven"].as_array().unwrap().iter().all(|l| l.as_str().unwrap().starts_with("NOT_PROVEN:")));
    // The cache the canvas fills reports the same numbers.
    let ctx = egui::Context::default();
    let _ = frame(&mut v, &ctx, vec![]);
    assert_eq!(v.state_json()["proof"]["gamut_warning"], s["gamut_warning"]);
}

#[test]
fn hostile_proof_entries_read_as_folio_diagnostics() {
    use hsk_studio_harness_core::harness::{Harness, References, Selection, default_actor};
    let scratch = Scratch::new("hostile");
    let doc_id = {
        let v = opened(&scratch);
        v.run().snapshot().unwrap().document().document_id.clone()
    };
    let valid = |f: &dyn Fn(&mut Value)| {
        let mut r = serde_json::to_value(folio::StudioProofRecords::new(doc_id.clone())).unwrap();
        r["active_condition"] = "working_rgb".into();
        f(&mut r);
        serde_json::to_vec(&r).unwrap()
    };
    let cases: Vec<(&str, Vec<u8>, &str, &str)> = vec![
        ("junk", b"{\"schema_id\":".to_vec(), "invalid_json", "proof_records"),
        ("duplicate key", format!(r#"{{"schema_id":"hsk.studio.proof_records@1","document_id":"{doc_id}","document_id":"{doc_id}"}}"#).into_bytes(), "duplicate_field", "proof_records"),
        ("other document", valid(&|r| r["document_id"] = "SDOC-019abcde-0000-7000-8000-0000000000ff".into()), "invalid_field", "document_id"),
        ("opacity", valid(&|r| r["gamut_warning"]["opacity"] = 101.into()), "invalid_field", "gamut_warning/opacity"),
        ("dangling", valid(&|r| r["active_condition"] = "press".into()), "dangling_reference", "active_condition"),
        ("matrix", valid(&|r| r["conditions"] = serde_json::json!([{"name":"m","proof_profile":{"kind":"operator_matrix","matrix":[9.0,0,0,0,1,0,0,0,1]},"proof_intent":"perceptual","display_intent":"perceptual","black_point_compensation":true,"simulate_paper_color":false,"simulate_black_ink":false}])), "invalid_field", "conditions/0/proof_profile/matrix"),
        ("deep", format!("{}{}", "[".repeat(200), "]".repeat(200)).into_bytes(), "budget_exceeded", "input_depth"),
    ];
    for (what, bytes, code, target) in cases {
        let harness = Harness::new(Selection { providers: vec![], references: References::TrustAll }).unwrap();
        let cancel = CancellationToken::default();
        let mut run = harness.run(default_actor(), &cancel);
        assert!(run.open_import(&psd()));
        run.set_document_entry(proof::ENTRY, Some(bytes.clone())).unwrap();
        let path = scratch.0.join(format!("hostile-{}.handshake", what.replace(' ', "-")));
        assert!(run.save_to(&path), "{what}");
        let mut v = Viewer::new(Prefer::Cpu);
        v.open(&path).unwrap();
        v.render_if_dirty();
        let e = v.proof_records().unwrap_err();
        let s = v.state_json()["proof"].clone();
        assert_eq!((s["error"]["code"].as_str(), s["error"]["target"].as_str()), (Some(code), Some(target)), "{what}: {e:?} {s}");
        assert_eq!((s["entry"].as_bool(), s["enabled"].as_bool(), s["canvas"].as_str()), (Some(true), Some(false), Some("frame")), "{what}");
        // Every write is refused and the stored bytes stay byte-identical; the canvas stays unproofed.
        assert!(v.proof_command(ProofCommand::SetProofColours(true)).unwrap_err().contains(code), "{what}");
        assert!(v.shell_command(ShellCommand::Proof(ProofMenuLeaf { menu: ProofMenu::View, leaf: ProofLeaf::GamutWarning })).is_err(), "{what}");
        assert_eq!(entry(&v), Some(bytes), "{what}");
        assert!(v.proof_frame().unwrap_err().contains(code), "{what}: the canvas proof view is off with the folio reason");
        let ctx = egui::Context::default();
        let _ = frame(&mut v, &ctx, vec![]);
        assert_eq!(v.state_json()["proof"]["canvas"], "frame", "{what}");
    }
}
