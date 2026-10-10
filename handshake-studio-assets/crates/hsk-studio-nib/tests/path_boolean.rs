#[path = "../examples/nib-consumer.rs"]
mod consumer;
use consumer::*;
use hsk_studio_accord::{Length, Unit};
use hsk_studio_nib::*;
use hsk_studio_observe::{DeliveryError, MAX_FRAME_BYTES};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

fn bounded_case(case: impl FnOnce()) {
    let start = Instant::now();
    case();
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "individual case exceeded30seconds"
    );
}
fn reject(request: Request<'_>, context: &Context<'_>, scene: &Scene) -> Error {
    let mut observer = scene.observer();
    let mut sink = Collector::new(None);
    match prepare(request, context, scene, &scene.meter, &scene.cancel) {
        Ok(_) => panic!("unexpected prepared proposal"),
        Err(r) => {
            assert_eq!(r.inspection.disposition, ProposalDisposition::Rejected);
            assert_eq!(r.inspection.counts.retained_owned_bytes, 0);
            emit_rejection(&request, r, &mut observer, &scene.cancel, &mut sink)
                .expect("actual rejection diagnostic");
            let (disposition, _, counts, bytes) = detail(&sink.frames[0][..sink.lengths[0]]);
            assert_eq!(disposition, 3);
            assert_eq!(&counts[2..6], &[0; 4]);
            assert_eq!(bytes[2], 0);
            r.error()
        }
    }
}
fn detail(frame: &[u8]) -> (u8, u8, [u64; 8], [u64; 3]) {
    assert_eq!(&frame[..4], b"HSKO");
    assert_eq!(frame[4], 4);
    assert_eq!(frame[7], 0);
    let mut at = 48;
    for _ in 0..7 {
        let len = u16::from_be_bytes(frame[at..at + 2].try_into().unwrap()) as usize;
        at += 2 + len;
        assert!(at <= frame.len());
    }
    let disposition = frame[at];
    let code = frame[at + 1];
    at += 3;
    let mut counts = [0; 8];
    for count in &mut counts {
        *count = u64::from_be_bytes(frame[at..at + 8].try_into().unwrap());
        at += 8;
    }
    let mut bytes = [0; 3];
    for count in &mut bytes {
        *count = u64::from_be_bytes(frame[at..at + 8].try_into().unwrap());
        at += 8;
    }
    (disposition, code, counts, bytes)
}
fn proof(network: &Network<'_>, expected: f64, style: &hsk_studio_accord::DomainId) {
    assert!((area(network) - expected).abs() < 1e-10);
    assert_eq!(network.winding_rule(), Winding::NonZero);
    let mut signed_total = 0.0;
    for (i, l) in network.loops().iter().enumerate() {
        let refs = network
            .loop_segment_indices(i)
            .expect("public directed loop");
        assert!(refs.len() >= 4);
        let mut signed = 0.0;
        for (j, &index) in refs.iter().enumerate() {
            let s = &network.segments()[index];
            let next = &network.segments()[refs[(j + 1) % refs.len()]];
            assert_eq!(s.end(), next.start());
            assert_ne!(s.start(), s.end());
            assert!(s.incoming_tangent().is_none() && s.outgoing_tangent().is_none());
            let a = network.vertices()[s.start()];
            let b = network.vertices()[s.end()];
            assert!(a.x == b.x || a.y == b.y);
            signed += a.x.value() * b.y.value() - b.x.value() * a.y.value();
        }
        signed /= 2.0;
        assert_eq!(signed > 0.0, l.is_outer());
        signed_total += signed;
        let first = network.vertices()[network.segments()[refs[0]].start()];
        for &index in refs {
            let p = network.vertices()[network.segments()[index].start()];
            assert!((first.x.value(), first.y.value()) <= (p.x.value(), p.y.value()));
        }
    }
    assert!((signed_total - expected).abs() < 1e-10);
    for region in network.regions() {
        assert_eq!(region.fill_style_id(), style);
        assert_eq!(region.winding_rule(), Winding::NonZero);
        let loops = &network.loops()[region.loop_range()];
        assert!(loops[0].is_outer());
        assert!(loops[1..].iter().all(|l| !l.is_outer()));
    }
    for pair in network.vertices().windows(2) {
        assert!((pair[0].x.value(), pair[0].y.value()) < (pair[1].x.value(), pair[1].y.value()));
    }
}
type Signature = (Vec<(f64, f64)>, Vec<(usize, usize)>, Vec<Vec<usize>>);
fn signature(network: &Network<'_>) -> Signature {
    (
        network
            .vertices()
            .iter()
            .map(|p| (p.x.value(), p.y.value()))
            .collect(),
        network
            .segments()
            .iter()
            .map(|s| (s.start(), s.end()))
            .collect(),
        (0..network.loops().len())
            .map(|i| network.loop_segment_indices(i).unwrap().to_vec())
            .collect(),
    )
}

#[test]
fn rectangle_union_difference_winding() {
    bounded_case(|| {
        let scene = Scene::new([0., 0., 10., 10.], [5., 0., 15., 10.]).unwrap();
        let original = scene.bytes.clone();
        for (op, expected) in [
            (Operation::Unite, 150.),
            (Operation::BackMinusFront, 50.),
            (Operation::FrontMinusBack, 50.),
        ] {
            let mut first = None;
            for permuted in [false, true] {
                scene.with_request(op, permuted, |request, context| {
                    let prepared = prepare(request, &context, &scene, &scene.meter, &scene.cancel)
                        .unwrap_or_else(|r| panic!("{:?}", r.error()));
                    proof(prepared.geometry(), expected, &scene.styles[2]);
                    let result = signature(prepared.geometry());
                    if let Some(ref previous) = first {
                        assert_eq!(&result, previous);
                    } else {
                        first = Some(result);
                    }
                    let receipt = prepared.precision_receipt();
                    assert!(receipt.exact && !receipt.geometry_loss && !receipt.style_loss);
                    assert_eq!(receipt.maximum_error_pt, 0.);
                    assert_eq!(
                        prepared
                            .source_provenance()
                            .map(|o| o.path.path_id)
                            .collect::<Vec<_>>(),
                        vec![&scene.paths[0], &scene.paths[1]]
                    );
                    let clone = prepared.clone();
                    let charged = scene.meter.current.load(Ordering::SeqCst);
                    assert!(charged > scene.meter.input_bytes);
                    let mut observer = scene.observer();
                    let mut diagnostics = scene.observer();
                    let mut fallback = Collector::new(None);
                    let mut port = Port {
                        context: &context,
                        sink: Collector::new(None),
                        reconciliation: Reconciliation::Accepted,
                        exclusive_error: None,
                    };
                    let outcome = finalize(
                        prepared,
                        &mut port,
                        &mut observer,
                        &mut diagnostics,
                        &scene.cancel,
                        &mut fallback,
                    );
                    match outcome {
                        Finalized::Accepted {
                            proposal,
                            inspection,
                        } => {
                            assert_eq!(inspection.disposition, ProposalDisposition::Accepted);
                            assert_eq!(proposal.revision(), 41);
                            proof(proposal.geometry(), expected, &scene.styles[2]);
                            drop(proposal);
                        }
                        other => panic!("{:?}", other.inspection()),
                    }
                    assert_eq!(scene.meter.current.load(Ordering::SeqCst), charged);
                    drop(clone);
                    assert_eq!(
                        scene.meter.current.load(Ordering::SeqCst),
                        scene.meter.input_bytes
                    );
                    assert_eq!(port.sink.count, 1);
                    let frame = &port.sink.frames[0][..port.sink.lengths[0]];
                    assert_eq!(&frame[..4], b"HSKO");
                    assert!(frame.len() <= MAX_FRAME_BYTES);
                    assert!(
                        !frame
                            .windows(b"original-private-paint".len())
                            .any(|s| s == b"original-private-paint")
                    );
                    assert_eq!(fallback.count, 0);
                });
            }
        }
        assert_eq!(scene.bytes, original);
        let cases = [
            (
                [0., 0., 10., 10.],
                [20., 0., 30., 10.],
                Operation::Unite,
                200.,
                2,
                2,
            ),
            (
                [0., 0., 20., 20.],
                [5., 5., 15., 15.],
                Operation::BackMinusFront,
                300.,
                2,
                1,
            ),
            (
                [0., 0., 10., 10.],
                [10., 10., 20., 20.],
                Operation::Unite,
                200.,
                2,
                2,
            ),
        ];
        for (a, b, op, expected, loops, regions) in cases {
            let s = Scene::new(a, b).unwrap();
            s.with_request(op, false, |request, context| {
                let p = prepare(request, &context, &s, &s.meter, &s.cancel)
                    .unwrap_or_else(|r| panic!("{:?}", r.error()));
                proof(p.geometry(), expected, &s.styles[2]);
                assert_eq!(p.geometry().loops().len(), loops);
                assert_eq!(p.geometry().regions().len(), regions);
                print_network(p.geometry());
            });
            assert_eq!(s.meter.current.load(Ordering::SeqCst), s.meter.input_bytes);
        }
        let mut reversed = Scene::new([0., 0., 10., 10.], [5., 0., 15., 10.]).unwrap();
        reversed.anchors[0].reverse();
        reversed.winding = [Winding::EvenOdd; 2];
        reversed.refresh();
        reversed.with_request(Operation::Unite, false, |request, context| {
            let p = prepare(
                request,
                &context,
                &reversed,
                &reversed.meter,
                &reversed.cancel,
            )
            .unwrap_or_else(|r| panic!("{:?}", r.error()));
            proof(p.geometry(), 150., &reversed.styles[2]);
        });
        let empty = Scene::new([0., 0., 10., 10.], [0., 0., 10., 10.]).unwrap();
        empty.with_request(Operation::BackMinusFront, false, |request, context| {
            assert_eq!(
                reject(request, &context, &empty),
                Error::EmptyPathfinderResult
            )
        });
    });
}

#[test]
fn nonfinite_degenerate_budget_cancel_stale() {
    bounded_case(|| {
        assert!(Scene::new([f64::NAN, 0., 10., 10.], [5., 0., 15., 10.]).is_err());
        for malformed in [
            "nonfinite",
            "unknown",
            "missing",
            "duplicate",
            "version",
            "nullable",
            "trailing",
        ] {
            let mut s = Scene::new([0., 0., 10., 10.], [5., 0., 15., 10.]).unwrap();
            let text = String::from_utf8(s.bytes[0].clone()).unwrap();
            s.bytes[0] = match malformed {
                "nonfinite" => text
                    .replacen("\"value\":0", "\"value\":1e999", 1)
                    .into_bytes(),
                "unknown" => text.replacen('{', "{\"extra\":0,", 1).into_bytes(),
                "missing" => text.replace("\"closed\":true,", "").into_bytes(),
                "version" => text.replace("vector_path@1", "vector_path@2").into_bytes(),
                "nullable" => text
                    .replace(
                        &format!(",\"fill_style_id\":\"{}\"", s.styles[0].as_str()),
                        "",
                    )
                    .into_bytes(),
                "duplicate" => text.replacen('{', "{\"closed\":true,", 1).into_bytes(),
                _ => format!("{text} trailing").into_bytes(),
            };
            s.precharge();
            let before = s.bytes.clone();
            s.with_request(Operation::Unite, false, |request, context| {
                let _ = reject(request, &context, &s);
            });
            assert_eq!(s.bytes, before);
            assert_eq!(s.meter.current.load(Ordering::SeqCst), s.meter.input_bytes);
        }
        let degenerate = Scene::new([0., 0., 0., 10.], [5., 0., 15., 10.]).unwrap();
        degenerate.with_request(Operation::Unite, false, |request, context| {
            assert!(matches!(
                reject(request, &context, &degenerate),
                Error::InvalidGeometry | Error::UnsupportedApproximation
            ))
        });
        let mut open = Scene::new([0., 0., 10., 10.], [5., 0., 15., 10.]).unwrap();
        open.closed[0] = false;
        open.refresh();
        open.with_request(Operation::Unite, false, |request, context| {
            assert_eq!(reject(request, &context, &open), Error::InvalidGeometry)
        });
        let mut units = Scene::new([0., 0., 10., 10.], [5., 0., 15., 10.]).unwrap();
        units.anchors[0][0].position.x = Length::new(0., Unit::Millimetres).unwrap();
        units.refresh();
        units.with_request(Operation::Unite, false, |request, context| {
            assert_eq!(reject(request, &context, &units), Error::ValueMismatch);
        });
        let scene = Scene::new([0., 0., 10., 10.], [5., 0., 15., 10.]).unwrap();
        scene.with_request(Operation::Unite, false, |request, context| {
            let mut r = request;
            r.options.remove_redundant_points = true;
            assert_eq!(reject(r, &context, &scene), Error::UnsupportedOptions);
            r = request;
            r.options.precision = Length::new(1., Unit::Millimetres).unwrap();
            assert_eq!(reject(r, &context, &scene), Error::InvalidRequest);
            r = request;
            r.style_reads = &[];
            assert_eq!(reject(r, &context, &scene), Error::UnavailableStyle);
            for cap in 0..13 {
                r = request;
                match cap {
                    0 => r.limits.input_bytes = 1,
                    1 => r.limits.anchors = 7,
                    2 => r.limits.output_vertices = 0,
                    3 => r.limits.output_segments = 0,
                    4 => r.limits.output_loops = 0,
                    5 => r.limits.intersections = 0,
                    6 => r.limits.work_units = 1,
                    7 => r.limits.requested_allocation_bytes = scene.meter.input_bytes,
                    8 => r.limits.output_regions = 0,
                    9 => r.limits.sweep_events = 0,
                    10 => r.limits.segments = 7,
                    11 => r.limits.operands = 1,
                    _ => r.limits.recursion_depth = 4,
                };
                assert_eq!(reject(r, &context, &scene), Error::BudgetExceeded);
                assert_eq!(
                    scene.meter.current.load(Ordering::SeqCst),
                    scene.meter.input_bytes
                );
            }
        });
        scene.missing.store(true, Ordering::SeqCst);
        scene.with_request(Operation::Unite, false, |r, c| {
            assert_eq!(reject(r, &c, &scene), Error::AbsentRead)
        });
        scene.missing.store(false, Ordering::SeqCst);
        scene.unsupported_style.store(true, Ordering::SeqCst);
        scene.with_request(Operation::Unite, false, |r, c| {
            assert_eq!(reject(r, &c, &scene), Error::UnsupportedStyle)
        });
        scene.unsupported_style.store(false, Ordering::SeqCst);
        scene.geometry_revision[0].store(11, Ordering::SeqCst);
        scene.with_request(Operation::Unite, false, |r, c| {
            assert_eq!(reject(r, &c, &scene), Error::StaleRevision)
        });
        scene.geometry_revision[0].store(10, Ordering::SeqCst);
        scene.epoch.store(2, Ordering::SeqCst);
        scene.with_request(Operation::Unite, false, |r, c| {
            assert_eq!(reject(r, &c, &scene), Error::StaleEpoch)
        });
        scene.epoch.store(1, Ordering::SeqCst);
        scene.meter.denied.store(true, Ordering::SeqCst);
        scene.with_request(Operation::Unite, false, |r, c| {
            assert_eq!(reject(r, &c, &scene), Error::LeaseUnavailable)
        });
        scene.meter.denied.store(false, Ordering::SeqCst);
        for mode in [
            DeliveryError::Rejected,
            DeliveryError::Saturated,
            DeliveryError::Unavailable,
            DeliveryError::Indeterminate,
        ] {
            scene.with_request(Operation::Unite, false, |request, context| {
                let p = prepare(request, &context, &scene, &scene.meter, &scene.cancel)
                    .unwrap_or_else(|r| panic!("{:?}", r.error()));
                let mut observer = scene.observer();
                let mut diagnostics = scene.observer();
                let mut fallback = Collector::new(None);
                let mut port = Port {
                    context: &context,
                    sink: Collector::new(Some(mode)),
                    reconciliation: Reconciliation::Accepted,
                    exclusive_error: None,
                };
                let outcome = finalize(
                    p,
                    &mut port,
                    &mut observer,
                    &mut diagnostics,
                    &scene.cancel,
                    &mut fallback,
                );
                assert_eq!(outcome.inspection().delivery, Some(mode));
                match outcome {
                    Finalized::ReconciliationRequired { token, inspection } => {
                        assert_eq!(mode, DeliveryError::Indeterminate);
                        assert_eq!(token.inspection().counts, inspection.counts);
                        let retained = token.clone();
                        let charged = scene.meter.current.load(Ordering::SeqCst);
                        let mut fresh = scene.observer();
                        let mut pending_observer = scene.observer();
                        let mut pending_sink = Collector::new(None);
                        emit_pending(
                            &token,
                            &mut pending_observer,
                            &scene.cancel,
                            &mut pending_sink,
                        )
                        .expect("pending transition");
                        assert_eq!(
                            detail(&pending_sink.frames[0][..pending_sink.lengths[0]]).0,
                            4
                        );
                        print_frames(&pending_sink);
                        port.sink.mode = None;
                        let mut resolved_diagnostics = scene.observer();
                        let resolved = reconcile(
                            token,
                            &mut port,
                            &mut fresh,
                            &mut resolved_diagnostics,
                            &scene.cancel,
                            &mut fallback,
                        );
                        assert!(matches!(resolved, Finalized::Accepted { .. }));
                        assert_eq!(resolved.inspection().delivery, None);
                        drop(resolved);
                        assert_eq!(scene.meter.current.load(Ordering::SeqCst), charged);
                        drop(retained);
                    }
                    Finalized::Rejected { error, .. } => assert_eq!(
                        error,
                        match mode {
                            DeliveryError::Rejected => Error::DeliveryRejected,
                            DeliveryError::Unavailable => Error::DeliveryUnavailable,
                            DeliveryError::Saturated => Error::DeliverySaturated,
                            DeliveryError::Indeterminate =>
                                panic!("indeterminate outcome lost token"),
                        }
                    ),
                    _ => panic!("sink failure accepted"),
                }
            });
            assert_eq!(
                scene.meter.current.load(Ordering::SeqCst),
                scene.meter.input_bytes
            );
        }
        scene.with_request(Operation::Unite, false, |request, context| {
            let p = prepare(request, &context, &scene, &scene.meter, &scene.cancel)
                .unwrap_or_else(|r| panic!("{:?}", r.error()));
            scene.target_revision.store(41, Ordering::SeqCst);
            let mut observer = scene.observer();
            let mut diagnostics = scene.observer();
            let mut fallback = Collector::new(None);
            let mut port = Port {
                context: &context,
                sink: Collector::new(None),
                reconciliation: Reconciliation::Rejected,
                exclusive_error: None,
            };
            let outcome = finalize(
                p,
                &mut port,
                &mut observer,
                &mut diagnostics,
                &scene.cancel,
                &mut fallback,
            );
            assert!(matches!(
                outcome,
                Finalized::Rejected {
                    error: Error::StaleRevision,
                    ..
                }
            ));
            scene.target_revision.store(40, Ordering::SeqCst);
        });
        scene.with_request(Operation::Unite, false, |request, context| {
            let mut observer = scene.observer();
            let mut diagnostics = scene.observer();
            let mut fallback = Collector::new(None);
            let p = prepare(request, &context, &scene, &scene.meter, &scene.cancel)
                .unwrap_or_else(|r| panic!("{:?}", r.error()));
            let mut port = Port {
                context: &context,
                sink: Collector::new(None),
                reconciliation: Reconciliation::Rejected,
                exclusive_error: Some(Error::UnsupportedFinalization),
            };
            let outcome = finalize(
                p,
                &mut port,
                &mut observer,
                &mut diagnostics,
                &scene.cancel,
                &mut fallback,
            );
            assert!(matches!(
                outcome,
                Finalized::Rejected {
                    error: Error::UnsupportedFinalization,
                    ..
                }
            ));
            assert_eq!(port.sink.count, 0);
            assert_eq!(fallback.count, 1);
            let (disposition, _, counts, bytes) =
                detail(&fallback.frames[0][..fallback.lengths[0]]);
            assert_eq!(disposition, 3);
            assert_eq!(&counts[2..6], &[0; 4]);
            assert_eq!(bytes[2], 0);
            print_frames(&fallback);
        });
        scene.with_request(Operation::Unite, false, |request, context| {
            let mut observer = scene.observer();
            let mut diagnostics = scene.observer();
            let mut fallback = Collector::new(Some(DeliveryError::Indeterminate));
            let p = prepare(request, &context, &scene, &scene.meter, &scene.cancel)
                .unwrap_or_else(|r| panic!("{:?}", r.error()));
            let mut port = Port {
                context: &context,
                sink: Collector::new(None),
                reconciliation: Reconciliation::Rejected,
                exclusive_error: Some(Error::UnsupportedFinalization),
            };
            let outcome = finalize(
                p,
                &mut port,
                &mut observer,
                &mut diagnostics,
                &scene.cancel,
                &mut fallback,
            );
            match outcome {
                Finalized::Rejected { error, inspection } => {
                    assert_eq!(error, Error::UnsupportedFinalization);
                    assert_eq!(
                        inspection.diagnostic_delivery,
                        Some(DeliveryError::Indeterminate)
                    );
                    assert_eq!(inspection.counts.retained_owned_bytes, 0);
                }
                _ => panic!("diagnostic delivery erased source refusal"),
            }
            assert_eq!(port.sink.count, 0);
            assert_eq!(fallback.count, 1);
        });
        scene.cancel.cancel();
        scene.with_request(Operation::Unite, false, |request, context| {
            assert_eq!(reject(request, &context, &scene), Error::Cancelled)
        });
        assert_eq!(
            scene.meter.current.load(Ordering::SeqCst),
            scene.meter.input_bytes
        );
    });
}
