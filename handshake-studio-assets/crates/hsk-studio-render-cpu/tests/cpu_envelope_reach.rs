//! Lane SMB-PIGMENT-FOLLOWUPS, PLR-069 on the CPU backend: an envelope entry addressed to a group
//! the root does not reach is neither admitted as capability (pigment) nor prepared (this backend),
//! like the layers `prepare` visits. The entry renders nothing, so it asks nothing of the backend
//! and charges nothing to `max_working_bytes`; the same entry on a reachable group is refused or
//! charged exactly as before. Only API that exists before the lane is used, so this file also
//! compiles on the old tree and the tests are red there. Expected values are derived by hand.
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::AdjustmentOp;
use hsk_studio_pigment::effect_math::{DropShadowOp, EffectOp};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const BASE: [f32; 4] = [0.2, 0.4, 0.6, 1.0];

/// `0` solid | `1` placeholder | `2` isolated {Normal over 0, Normal over 1 (an adjustment
/// layer)} | `3` the root: {Normal over 0}, plus {Normal over 2} when `reach` is set. Without
/// `reach` nothing references node 2.
fn adjustment_plan(reach: bool) -> RenderPlan {
    let one = rect(0, 0, 1, 1);
    let mut root = vec![layer(0, StudioBlendMode::Normal, 1.0)];
    if reach {
        root.push(layer(2, StudioBlendMode::Normal, 1.0));
    }
    plan(
        one,
        vec![
            solid(one, BASE),
            placeholder(one),
            group(vec![
                layer(0, StudioBlendMode::Normal, 1.0),
                layer(1, StudioBlendMode::Normal, 1.0),
            ]),
            group(root),
        ],
        3,
    )
}

/// The v1 route of `plan`: its receipt and the delivered grid.
fn run_v1(renderer: &Renderer, plan: &RenderPlan, budget: u64) -> (RenderReceipt, Grid) {
    let mut grid = Grid::new(plan.target.extent);
    let receipt = renderer
        .execute(
            plan,
            &req(Exactness::Approximate, budget),
            &no_sources(),
            &CancellationToken::default(),
            &mut grid,
        )
        .expect("the plain plan renders");
    (receipt, grid)
}

/// An adjustment entry on an unreachable group is not prepared: with the working set of the plain
/// plan as the whole budget it still renders the same pixel, charges nothing, and leaves no trace
/// in the ledger. On a reachable group the same entry is charged and the same budget refuses it.
#[test]
fn unreachable_envelope_entries_are_neither_admitted_nor_prepared() {
    let renderer = Renderer::new(1).unwrap();
    let invert = envelope(vec![one_layer(2, 1).with_adjustment(AdjustmentOp::Invert)]);

    // Unreachable: the plain plan reaches the root and the solid only.
    let plain = adjustment_plan(false);
    let (baseline, grid) = run_v1(&renderer, &plain, 1 << 20);
    near(grid.at(0, 0), BASE, 1e-6);
    assert_eq!(baseline.nodes_executed, 2);
    let budget = baseline.peak_working_bytes;
    let (result, grid) = run_ext(
        &renderer,
        &plain,
        &invert,
        Exactness::Approximate,
        budget,
        &no_sources(),
    );
    let receipt = result.expect("an entry on an unreachable group asks nothing and costs nothing");
    near(grid.at(0, 0), BASE, 1e-6);
    assert_eq!(receipt.peak_working_bytes, budget);
    assert_eq!(receipt.nodes_executed, 2);
    assert!(
        receipt
            .exactness
            .operators
            .iter()
            .all(|o| o.key != "adjustment_layer" && o.key != "invert"),
        "{:?}",
        receipt.exactness.operators
    );
    // The receipt still binds the whole envelope (the digest covers every entry).
    assert_eq!(receipt.plan_digest, invert.digest_with(&plain).unwrap());

    // Reachable: the entry is prepared and its tables are charged, so the working set of the
    // plain plan is no longer enough, and nothing reaches the sink.
    let reached = adjustment_plan(true);
    let (baseline, _) = run_v1(&renderer, &reached, 1 << 20);
    let (result, grid) = run_ext(
        &renderer,
        &reached,
        &invert,
        Exactness::Approximate,
        baseline.peak_working_bytes,
        &no_sources(),
    );
    assert!(
        matches!(result, Err(BackendError::BudgetExceeded { .. })),
        "{result:?}"
    );
    assert_eq!(grid.calls, 0);
    // With room for the tables it renders and reports the operators.
    let (result, grid) = run_ext(
        &renderer,
        &reached,
        &invert,
        Exactness::Approximate,
        1 << 20,
        &no_sources(),
    );
    let receipt = result.expect("a reachable entry renders");
    near(grid.at(0, 0), [0.8, 0.6, 0.4, 1.0], 1e-6);
    assert!(
        receipt
            .exactness
            .operators
            .iter()
            .any(|o| o.key == "adjustment_layer")
    );
}

/// This backend declares nested effects Unsupported: two effect entries, the second on a group
/// whose content holds the first, are refused before any sink call when the root reaches them, and
/// admitted (rendering the plain pixel) when no layer does.
#[test]
fn unreachable_nested_effect_entries_admit_and_reachable_ones_are_refused() {
    let one = rect(0, 0, 1, 1);
    // 0 solid | 1 isolated {0} (inner) | 2 isolated {1} (outer) | 3 root {0}, plus {2} if reached
    let nested = |reach: bool| {
        let mut root = vec![layer(0, StudioBlendMode::Normal, 1.0)];
        if reach {
            root.push(layer(2, StudioBlendMode::Normal, 1.0));
        }
        plan(
            one,
            vec![
                solid(one, BASE),
                group(vec![layer(0, StudioBlendMode::Normal, 1.0)]),
                group(vec![layer(1, StudioBlendMode::Normal, 1.0)]),
                group(root),
            ],
            3,
        )
    };
    let shadow = || {
        vec![EffectOp::DropShadow(DropShadowOp::new(
            [0.0; 3],
            StudioBlendMode::Normal,
            0.5,
        ))]
    };
    let ext = envelope(vec![
        one_layer(1, 0).with_effects(shadow()),
        one_layer(2, 0).with_effects(shadow()),
    ]);
    let renderer = Renderer::new(1).unwrap();

    let (result, grid) = run_ext(
        &renderer,
        &nested(false),
        &ext,
        Exactness::Approximate,
        1 << 20,
        &no_sources(),
    );
    let receipt = result.expect("entries on unreachable groups ask nothing");
    near(grid.at(0, 0), BASE, 1e-6);
    assert!(
        receipt
            .exactness
            .operators
            .iter()
            .all(|o| o.key != ExtOperator::EffectsNested.key() && o.key != "drop_shadow"),
        "{:?}",
        receipt.exactness.operators
    );

    let (result, grid) = run_ext(
        &renderer,
        &nested(true),
        &ext,
        Exactness::Approximate,
        1 << 20,
        &no_sources(),
    );
    assert_eq!(
        result,
        Err(BackendError::Unsupported {
            operator: ExtOperator::EffectsNested.key(),
            exactness: Exactness::Unsupported,
        })
    );
    assert_eq!(grid.calls, 0);
}
