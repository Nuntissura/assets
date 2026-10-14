//! SMB-HARNESS-UI-3 slice 3 (feature `viewer`): the active selection is saved in the package
//! (session document entry `selection`) and restored on reopen; its byte format refuses hostile
//! input before allocating.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::selection_store::{decode, encode};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::selection::{CombineMode, CoveragePlane, SelectionLimits};

#[test]
fn selection_is_saved_with_the_document_and_restored_on_reopen() {
    let scratch = Scratch::new("selection-persist");
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(32, 32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.select_ellipse(2.5, 3.0, 20.0, 14.0).unwrap();
    v.set_combine(CombineMode::Diminish);
    v.select_rect(8.0, 0.0, 4.0, 32.0).unwrap();
    let saved = v.selection().unwrap().clone();
    assert!(saved.data.iter().any(|c| *c > 0 && *c < u16::MAX), "antialiased edge");
    let out = scratch.0.join("selected.handshake");
    v.save(&out, false).unwrap();

    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    let restored = again.selection().expect("selection restored on reopen");
    assert_eq!((restored.bounds, &restored.data), (saved.bounds, &saved.data), "exact coverage");
    assert_eq!(again.ant_segments(), v.ant_segments(), "same outline");
    assert!(again.run().snapshot().is_some());

    // Saving without a selection removes it from the document.
    again.clear_selection();
    let cleared = scratch.0.join("cleared.handshake");
    again.save(&cleared, false).unwrap();
    let mut third = Viewer::new(Prefer::Cpu);
    third.open(&cleared).unwrap();
    assert!(third.selection().is_none());
    assert!(third.run().document_entry("selection").is_none());
}

fn header(x: i64, y: i64, w: u32, h: u32, runs: u32) -> Vec<u8> {
    let mut b = b"HSKSEL01".to_vec();
    b.extend_from_slice(&x.to_le_bytes());
    b.extend_from_slice(&y.to_le_bytes());
    b.extend_from_slice(&w.to_le_bytes());
    b.extend_from_slice(&h.to_le_bytes());
    b.extend_from_slice(&runs.to_le_bytes());
    b
}

fn run(b: &mut Vec<u8>, n: u32, v: u16) {
    b.extend_from_slice(&n.to_le_bytes());
    b.extend_from_slice(&v.to_le_bytes());
}

#[test]
fn hostile_selection_bytes_are_refused_before_allocation() {
    let limits = SelectionLimits::default();
    // Round trip.
    let mut plane = CoveragePlane::filled(Rect { x: -3, y: 5, width: 7, height: 3 }, 0, &limits).unwrap();
    plane.data[4] = 40_000;
    plane.data[5] = u16::MAX;
    let bytes = encode(&plane);
    let back = decode(&bytes, &limits).unwrap();
    assert_eq!((back.bounds, back.data), (plane.bounds, plane.data));
    // dims_u32_max: refused by the size limit before any allocation.
    let mut huge = header(0, 0, u32::MAX, u32::MAX, 1);
    run(&mut huge, u32::MAX, 1);
    assert_eq!(decode(&huge, &limits).unwrap_err(), "selection_over_budget");
    // aggregate_over_budget: runs that add up past the plane.
    let mut over = header(0, 0, 2, 2, 2);
    run(&mut over, 3, 1);
    run(&mut over, 3, 1);
    assert_eq!(decode(&over, &limits).unwrap_err(), "selection_runs");
    // Too few pixels, a zero-length run, a truncated body, trailing bytes, the wrong magic.
    let mut short = header(0, 0, 2, 2, 1);
    run(&mut short, 3, 1);
    assert_eq!(decode(&short, &limits).unwrap_err(), "selection_runs");
    let mut zero = header(0, 0, 2, 2, 2);
    run(&mut zero, 0, 1);
    run(&mut zero, 4, 1);
    assert_eq!(decode(&zero, &limits).unwrap_err(), "selection_runs");
    assert_eq!(decode(&bytes[..bytes.len() - 1], &limits).unwrap_err(), "selection_length");
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(decode(&trailing, &limits).unwrap_err(), "selection_length");
    assert_eq!(decode(b"HSKSEL0", &limits).unwrap_err(), "selection_format");
    assert_eq!(decode(&header(0, 0, 1, 1, u32::MAX), &limits).unwrap_err(), "selection_length");
    // Bounds whose end overflows.
    let mut edge = header(i64::MAX, 0, 2, 1, 1);
    run(&mut edge, 2, 1);
    assert_eq!(decode(&edge, &limits).unwrap_err(), "selection_bounds");
}
