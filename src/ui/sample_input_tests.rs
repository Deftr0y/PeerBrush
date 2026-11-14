use super::*;
use crate::{
    engine::{native::NativeDocument, Engine},
    large_document::DiskBudget,
};

fn command(doc: DocumentView<'_>, point: [f32; 2], mask: bool) -> Value {
    json!({"op":"paint", "document_id":doc.id(), "source_revision":doc.revision(),
        "layer":doc.layers().next().unwrap().id(), "mask":mask, "color":[9,18,27,183],
        "points":[point,[point[0]+1.,point[1]+1.]], "pressures":[0.3,0.8], "radius":3., "hardness":1.0})
}

#[test]
fn alternating_samples_and_strokes_observe_each_predecessor_and_keep_separate_history() {
    for depth in [8, 16] {
        for mask in [false, true] {
            let (app, _) = super::super::workspace_tests::fixture(depth);
            let shared = &app.shared;
            let project = &app.project_id;
            let before = {
                let mut engine = shared.lock().unwrap();
                let layer = app.selected.clone();
                engine.resident_state_mut().expect("resident fixture").document.layers[0]
                    .pixels
                    .set16(1, 1, [12347, 23459, 34571, 65535]);
                if mask {
                    engine
                        .edit(
                            "human",
                            &[json!({"op":"mask.add", "layer":layer,"value":255})],
                            None,
                            None,
                            "Mask",
                        )
                        .unwrap();
                }
                engine.resident_state().expect("resident fixture").document.clone()
            };
            let history = shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
            let mut input = Timeline::new(shared, project, (&before).into()).unwrap();
            let first = input
                .sample(
                    shared,
                    project,
                    (&before).into(),
                    [1.25, 1.25],
                    &app.selected,
                    false,
                    None,
                )
                .unwrap()
                .id;
            let first_stroke = command((&before).into(), [15., 12.], mask);
            input
                .stroke(
                    shared,
                    project,
                    (&before).into(),
                    &first_stroke,
                    "First sampled stroke",
                    true,
                )
                .unwrap();
            let second = input
                .sample(
                    shared,
                    project,
                    (&before).into(),
                    [15.25, 12.25],
                    &app.selected,
                    mask,
                    None,
                )
                .unwrap()
                .id;
            input
                .stroke(
                    shared,
                    project,
                    (&before).into(),
                    &command((&before).into(), [24., 16.], mask),
                    "Second sampled stroke",
                    true,
                )
                .unwrap();
            let mut samples = Vec::new();
            let mut strokes = 0;
            while input.pending() {
                let doc = shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                let ready = input
                    .start(shared, project, (&doc).into())
                    .unwrap()
                    .unwrap();
                let id = ready.id;
                assert_eq!(ready.revision, doc.revision);
                match ready.action {
                    Action::Sample {
                        point, layer, mask, ..
                    } => {
                        let pixel =
                            crate::render::sample::resident(&doc, point, mask.then_some(layer))
                                .unwrap();
                        assert!(input
                            .sampled(id, pixel, shared, project, (&doc).into())
                            .unwrap());
                        samples.push(pixel);
                        assert_eq!(
                            input.current_color(shared, project, (&doc).into()).unwrap(),
                            (id == second).then_some(pixel)
                        );
                        if id == first {
                            assert_eq!(doc.revision, before.revision);
                        }
                        if id == second {
                            assert_eq!(doc.revision, before.revision + 1);
                            assert_ne!(
                                pixel,
                                crate::render::sample::resident(
                                    &before,
                                    point,
                                    mask.then_some(app.selected.as_str())
                                )
                                .unwrap()
                            );
                        }
                    }
                    Action::Stroke { command, label, .. } => {
                        let source = samples.last().unwrap();
                        assert_eq!(command["color"][0], source[0]);
                        assert_eq!(command["color"][3], if mask { 183 } else { source[3] });
                        assert_eq!(command["pressures"], json!([0.3, 0.8]));
                        assert_eq!(command["source_revision"], doc.revision);
                        let expected =
                            Engine::preview_edits(doc.clone(), &[(*command).clone()]).unwrap();
                        let result = server::edit_shared(
                            shared,
                            "human",
                            &[(*command).clone()],
                            Some(ready.revision),
                            None,
                            label,
                            None,
                            Some(ready.document),
                        )
                        .unwrap();
                        let current = shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                        assert_eq!(
                            serde_json::to_value(&current.layers).unwrap(),
                            serde_json::to_value(&expected.layers).unwrap()
                        );
                        assert!(input
                            .committed(
                                id,
                                result.data()["revision"].as_u64().unwrap(),
                                shared,
                                project,
                                (&current).into()
                            )
                            .unwrap());
                        strokes += 1;
                    }
                }
            }
            let mut engine = shared.lock().unwrap();
            assert_eq!(strokes, 2);
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history + 2);
            assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, depth);
            let painted = engine.resident_state().expect("resident fixture").document.export_png().unwrap();
            engine.undo("human").unwrap();
            engine.undo("human").unwrap();
            assert_eq!(
                engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                before.export_png().unwrap()
            );
            engine.redo("human").unwrap();
            engine.redo("human").unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), painted);
        }
    }
}

#[test]
fn coalescing_retires_only_samples_without_a_dependent_stroke_and_rejects_late_replies() {
    let (app, _) = super::super::workspace_tests::fixture(16);
    let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    let view = (&doc).into();
    let mut input = Timeline::new(&app.shared, &app.project_id, view).unwrap();
    let first = input
        .sample(
            &app.shared,
            &app.project_id,
            view,
            [1., 1.],
            &app.selected,
            false,
            None,
        )
        .unwrap()
        .id;
    assert_eq!(
        input
            .start(&app.shared, &app.project_id, view)
            .unwrap()
            .unwrap()
            .id,
        first
    );
    let same = input
        .sample(
            &app.shared,
            &app.project_id,
            view,
            [1.5, 1.5],
            &app.selected,
            false,
            None,
        )
        .unwrap();
    assert_eq!(same.id, first);
    assert!(same.superseded.is_none());
    let second = input
        .sample(
            &app.shared,
            &app.project_id,
            view,
            [8., 8.],
            &app.selected,
            false,
            None,
        )
        .unwrap();
    assert_eq!(second.superseded, Some(first));
    assert!(!input
        .sampled(first, [255; 4], &app.shared, &app.project_id, view)
        .unwrap());
    input
        .stroke(
            &app.shared,
            &app.project_id,
            view,
            &command(view, [16., 12.], false),
            "Stroke",
            true,
        )
        .unwrap();
    let third = input
        .sample(
            &app.shared,
            &app.project_id,
            view,
            [12., 12.],
            &app.selected,
            false,
            None,
        )
        .unwrap();
    assert!(third.superseded.is_none());
    let ready = input
        .start(&app.shared, &app.project_id, view)
        .unwrap()
        .unwrap();
    assert_eq!(ready.id, second.id);
    assert!(input
        .sampled(
            second.id,
            [11, 22, 33, 255],
            &app.shared,
            &app.project_id,
            view
        )
        .unwrap());
    assert!(input
        .current_color(&app.shared, &app.project_id, view)
        .unwrap()
        .is_none());
    let ready = input
        .start(&app.shared, &app.project_id, view)
        .unwrap()
        .unwrap();
    let stroke_id = ready.id;
    let Action::Stroke { command, .. } = ready.action else {
        panic!("Dependent stroke was lost");
    };
    assert_eq!(command["color"], json!([11, 22, 33, 255]));
    assert!(input.failed(stroke_id));
    assert_eq!(input.events.len(), 2);
    assert!(input.start(&app.shared, &app.project_id, view).is_err());
}

#[test]
fn stale_sources_failed_receipts_and_bounds_preserve_queued_input() {
    for change in ["revision", "engine", "project", "document"] {
        let (app, _) = super::super::workspace_tests::fixture(16);
        let mut doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let mut input = Timeline::new(&app.shared, &app.project_id, (&doc).into()).unwrap();
        let id = input
            .sample(
                &app.shared,
                &app.project_id,
                (&doc).into(),
                [1., 1.],
                &app.selected,
                false,
                None,
            )
            .unwrap()
            .id;
        input
            .start(&app.shared, &app.project_id, (&doc).into())
            .unwrap()
            .unwrap();
        let other = Arc::new(std::sync::Mutex::new(Engine::new()));
        let shared = if change == "engine" {
            &other
        } else {
            &app.shared
        };
        let project = if change == "project" {
            "other"
        } else {
            app.project_id.as_str()
        };
        if change == "revision" {
            doc.revision += 1;
        }
        if change == "document" {
            doc.id = "other".into();
        }
        assert!(input
            .sampled(id, [255; 4], shared, project, (&doc).into())
            .is_err());
        assert!(input.current_color(shared, project, (&doc).into()).is_err());
        assert!(input.failure);
        assert_eq!(input.events.len(), 1);
    }
    let (app, _) = super::super::workspace_tests::fixture(16);
    let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    let mut input = Timeline::new(&app.shared, &app.project_id, (&doc).into()).unwrap();
    for _ in 0..EVENTS / 2 {
        input
            .sample(
                &app.shared,
                &app.project_id,
                (&doc).into(),
                [1., 1.],
                &app.selected,
                false,
                None,
            )
            .unwrap();
        input
            .stroke(
                &app.shared,
                &app.project_id,
                (&doc).into(),
                &command((&doc).into(), [16., 12.], false),
                "Stroke",
                true,
            )
            .unwrap();
    }
    assert_eq!(input.events.len(), EVENTS);
    assert!(input
        .sample(
            &app.shared,
            &app.project_id,
            (&doc).into(),
            [2., 2.],
            &app.selected,
            false,
            None
        )
        .is_err());
    assert_eq!(input.events.len(), EVENTS);
    let pool = input.pool.clone();
    assert!(pool.used() > 0);
    drop(input);
    assert_eq!(pool.used(), 0);
}

#[test]
fn native_input_keeps_admitted_commands_pressure_and_manual_override_until_retirement() {
    let pool = ResidentBudget::new(BYTES).unwrap();
    let doc = NativeDocument::new_blank(
        32,
        24,
        16,
        &pool,
        &DiskBudget::new(BYTES as u64, 4).unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    let shared = Arc::new(std::sync::Mutex::new(Engine::new()));
    let layer = doc.metadata().layers().next().unwrap().id().to_owned();
    let baseline = pool.used();
    let mut input = Timeline::new(&shared, "native", (&doc).into()).unwrap();
    let first = input
        .sample(
            &shared,
            "native",
            (&doc).into(),
            [1., 1.],
            &layer,
            false,
            None,
        )
        .unwrap()
        .id;
    let original = command((&doc).into(), [16., 12.], false);
    input
        .stroke(
            &shared,
            "native",
            (&doc).into(),
            &original,
            "Manual color",
            false,
        )
        .unwrap();
    input
        .start(&shared, "native", (&doc).into())
        .unwrap()
        .unwrap();
    input
        .sampled(first, [211, 89, 47, 255], &shared, "native", (&doc).into())
        .unwrap();
    let ready = input
        .start(&shared, "native", (&doc).into())
        .unwrap()
        .unwrap();
    let id = ready.id;
    let Action::Stroke {
        command, endpoint, ..
    } = ready.action
    else {
        panic!("Missing stroke");
    };
    assert_eq!(endpoint, ([17., 13.], 0.8));
    assert!(command.owner().uses_budget(&pool));
    assert_eq!(*command, original);
    let preview = input.preview(&shared, "native", (&doc).into()).unwrap();
    assert_eq!(preview.commands.len(), 1);
    assert!(preview.commands[0].owner().uses_budget(&pool));
    assert_eq!(*preview.commands[0], original);
    assert!(!input
        .committed(first, 1, &shared, "native", (&doc).into())
        .unwrap());
    assert!(input.failed(id));
    assert_eq!(input.events.len(), 1);
    drop(input);
    assert!(
        pool.used() > baseline,
        "Dispatched command must retain its real allocation"
    );
    drop(command);
    drop(preview);
    assert_eq!(pool.used(), baseline);
    let full = pool
        .reserve_bytes(pool.limit() - pool.used(), pool.limit())
        .unwrap();
    assert!(Timeline::new(&shared, "native", (&doc).into()).is_err());
    drop(full);
    drop(doc);
    assert_eq!(pool.used(), 0);
}

#[test]
fn late_queue_replies_and_intervening_edits_cannot_replay_a_committed_stroke() {
    let (app, _) = super::super::workspace_tests::fixture(16);
    let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    let view = (&doc).into();
    let mut old = Timeline::new(&app.shared, &app.project_id, view).unwrap();
    let obsolete = old
        .sample(
            &app.shared,
            &app.project_id,
            view,
            [1., 1.],
            &app.selected,
            false,
            None,
        )
        .unwrap()
        .id;
    old.start(&app.shared, &app.project_id, view).unwrap();
    drop(old);
    let mut input = Timeline::new(&app.shared, &app.project_id, view).unwrap();
    let current = input
        .sample(
            &app.shared,
            &app.project_id,
            view,
            [2., 2.],
            &app.selected,
            false,
            None,
        )
        .unwrap()
        .id;
    assert_ne!(obsolete, current);
    input
        .stroke(
            &app.shared,
            &app.project_id,
            view,
            &command(view, [16., 12.], false),
            "First",
            true,
        )
        .unwrap();
    input
        .stroke(
            &app.shared,
            &app.project_id,
            view,
            &command(view, [24., 16.], false),
            "Second",
            true,
        )
        .unwrap();
    input.start(&app.shared, &app.project_id, view).unwrap();
    assert!(!input
        .sampled(obsolete, [255; 4], &app.shared, &app.project_id, view)
        .unwrap());
    input
        .sampled(
            current,
            [211, 89, 47, 255],
            &app.shared,
            &app.project_id,
            view,
        )
        .unwrap();
    let ready = input
        .start(&app.shared, &app.project_id, view)
        .unwrap()
        .unwrap();
    let id = ready.id;
    let Action::Stroke { command, label, .. } = ready.action else {
        panic!("Missing stroke");
    };
    let receipt = server::edit_shared(
        &app.shared,
        "human",
        &[(*command).clone()],
        Some(ready.revision),
        None,
        label,
        None,
        Some(ready.document),
    )
    .unwrap();
    let revision = receipt.data()["revision"].as_u64().unwrap();
    let later = {
        let mut engine = app.shared.lock().unwrap();
        engine
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":app.selected,"name":"Later edit"})],
                None,
                None,
                "Later edit",
            )
            .unwrap();
        engine.resident_state().expect("resident fixture").document.clone()
    };
    assert!(input
        .committed(id, revision, &app.shared, &app.project_id, (&later).into())
        .is_err());
    assert_eq!(input.events.len(), 1);
    assert_ne!(input.events.front().unwrap().id, id);
    assert!(input.failure);
    assert!(input
        .start(&app.shared, &app.project_id, (&later).into())
        .is_err());
}
