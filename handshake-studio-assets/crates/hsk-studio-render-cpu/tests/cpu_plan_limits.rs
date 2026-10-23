//! Lane SMB-PIGMENT-FOLLOWUPS, PLR-070: the CPU backend hands a caller's `PlanLimits` to pigment's
//! shared admission (`Renderer::with_plan_limits`). A renderer without limits behaves exactly as
//! before (the maxima). Expected values are derived by hand from the plans built here.
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::AdjustmentOp;
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::{RenderError, Renderer};

mod common;
use common::*;

const BASE: [f32; 4] = [0.2, 0.4, 0.6, 1.0];

/// `0` solid | `1` placeholder | `2` isolated {Normal over 0, Normal over 1 (an adjustment
/// layer)} as the root: 3 nodes, 2 layers, depth 2.
fn small() -> (RenderPlan, PlanExtensions) {
    let one = rect(0, 0, 1, 1);
    (
        plan(
            one,
            vec![
                solid(one, BASE),
                placeholder(one),
                group(vec![
                    layer(0, StudioBlendMode::Normal, 1.0),
                    layer(1, StudioBlendMode::Normal, 1.0),
                ]),
            ],
            2,
        ),
        envelope(vec![one_layer(2, 1).with_adjustment(AdjustmentOp::Invert)]),
    )
}

fn v1(renderer: &Renderer, plan: &RenderPlan) -> (Result<RenderReceipt, BackendError>, Grid) {
    let mut grid = Grid::new(plan.target.extent);
    let result = renderer.execute(
        plan,
        &req(Exactness::Approximate, 1 << 20),
        &no_sources(),
        &CancellationToken::default(),
        &mut grid,
    );
    (result, grid)
}

/// A limit below what the plan needs is a typed `InvalidPlan` from every entry point (v1, envelope,
/// detailed) before any sink call; a limit that exactly fits renders; zero tightens everything.
#[test]
fn tightened_plan_limits_refuse_before_any_sink_call() {
    let (plan, ext) = small();
    let d = PlanLimits::default();
    let tighter = [
        (
            "max_nodes",
            PlanLimits { max_nodes: 2, ..d },
            PlanError::BudgetExceeded,
        ),
        (
            "max_layers",
            PlanLimits { max_layers: 1, ..d },
            PlanError::BudgetExceeded,
        ),
        (
            "max_depth",
            PlanLimits { max_depth: 1, ..d },
            PlanError::TooDeep,
        ),
        (
            "max_depth zero",
            PlanLimits { max_depth: 0, ..d },
            PlanError::TooDeep,
        ),
        (
            "max_nodes zero",
            PlanLimits { max_nodes: 0, ..d },
            PlanError::BudgetExceeded,
        ),
    ];
    for (what, limits, error) in tighter {
        let renderer = Renderer::new(1).unwrap().with_plan_limits(limits).unwrap();
        assert_eq!(renderer.plan_limits(), limits, "{what}");
        let refused = Err(BackendError::InvalidPlan(error));

        let (result, grid) = v1(&renderer, &plan);
        assert_eq!(result, refused, "{what}: execute");
        assert_eq!(grid.calls, 0, "{what}: execute");

        let (result, grid) = run_ext(
            &renderer,
            &plan,
            &ext,
            Exactness::Approximate,
            1 << 20,
            &no_sources(),
        );
        assert_eq!(result, refused, "{what}: execute_extended");
        assert_eq!(grid.calls, 0, "{what}: execute_extended");

        let mut grid = Grid::new(plan.target.extent);
        let detailed = renderer.execute_detailed(
            &plan,
            &req(Exactness::Approximate, 1 << 20),
            &no_sources(),
            &CancellationToken::default(),
            &mut grid,
        );
        assert_eq!(
            detailed,
            Err(RenderError::Backend(BackendError::InvalidPlan(error))),
            "{what}: execute_detailed"
        );
        assert_eq!(grid.calls, 0, "{what}: execute_detailed");
    }

    // Exactly enough renders, on both routes.
    let fits = PlanLimits {
        max_nodes: 3,
        max_layers: 2,
        max_depth: 2,
    };
    let renderer = Renderer::new(1).unwrap().with_plan_limits(fits).unwrap();
    let (result, grid) = v1(&renderer, &plan);
    result.expect("a plan that fits the tightened limits renders");
    near(grid.at(0, 0), BASE, 1e-6);
    let (result, grid) = run_ext(
        &renderer,
        &plan,
        &ext,
        Exactness::Approximate,
        1 << 20,
        &no_sources(),
    );
    result.expect("the envelope route renders too");
    near(grid.at(0, 0), [0.8, 0.6, 0.4, 1.0], 1e-6);

    // Limits only tighten: a field above its maximum is a configuration error, never clamped.
    let max = PlanLimits::MAXIMUM;
    for (what, limits) in [
        (
            "nodes",
            PlanLimits {
                max_nodes: max.max_nodes.saturating_add(1),
                ..d
            },
        ),
        (
            "layers",
            PlanLimits {
                max_layers: max.max_layers.saturating_add(1),
                ..d
            },
        ),
        (
            "depth",
            PlanLimits {
                max_depth: max.max_depth.saturating_add(1),
                ..d
            },
        ),
        (
            "usize::MAX",
            PlanLimits {
                max_nodes: usize::MAX,
                ..d
            },
        ),
    ] {
        assert_eq!(
            Renderer::new(1).unwrap().with_plan_limits(limits).err(),
            Some(RenderError::InvalidConfig("plan_limits")),
            "{what}"
        );
    }
    // The maxima themselves are accepted, and the builder composes with the thread setting.
    let renderer = Renderer::with_threads(8, 2)
        .unwrap()
        .with_plan_limits(max)
        .unwrap();
    assert_eq!((renderer.chunk(), renderer.threads()), (8, 2));
    assert_eq!(renderer.plan_limits(), max);
}

/// A renderer that was never given limits admits and refuses exactly at the shared maxima: 65 536
/// nodes render, 65 537 are refused with the budget error (the shared default admission).
#[test]
fn default_plan_limits_are_unchanged() {
    assert_eq!(Renderer::default().plan_limits(), PlanLimits::MAXIMUM);
    assert_eq!(
        Renderer::new(4).unwrap().plan_limits(),
        PlanLimits::default()
    );
    assert_eq!(
        Renderer::with_threads(4, 2).unwrap().plan_limits(),
        PlanLimits::MAXIMUM
    );

    let one = rect(0, 0, 1, 1);
    // `count` nodes: `count - 1` solids no layer references and the root group over node 0.
    let nodes = |count: usize| {
        let mut nodes = vec![solid(one, BASE)];
        nodes.extend((2..count).map(|_| solid(one, BASE)));
        nodes.push(group(vec![layer(0, StudioBlendMode::Normal, 1.0)]));
        plan(one, nodes, u32::try_from(count.saturating_sub(1)).unwrap())
    };
    for limits in [None, Some(PlanLimits::default())] {
        let mut renderer = Renderer::new(1).unwrap();
        if let Some(limits) = limits {
            renderer = renderer.with_plan_limits(limits).unwrap();
        }
        let (result, grid) = v1(&renderer, &nodes(PlanLimits::MAXIMUM.max_nodes));
        result.expect("the maximum node count renders");
        near(grid.at(0, 0), BASE, 1e-6);

        let (result, grid) = v1(
            &renderer,
            &nodes(PlanLimits::MAXIMUM.max_nodes.saturating_add(1)),
        );
        assert_eq!(
            result,
            Err(BackendError::InvalidPlan(PlanError::BudgetExceeded))
        );
        assert_eq!(grid.calls, 0);
    }
}
