use super::*;
use super::{
    grouping_tests::gate,
    import_tests::pixels,
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
    Document, Engine, Layer,
};
use serde_json::{json, Value};

#[test]
fn native_merge_spans_wide_and_tall_frames_at_original_depth_with_atomic_history() {
    use crate::raster::{backed::Region, CanvasLimits};
    for depth in [8, 16] {
        for [width, height] in [[16000, 2], [2, 16000]] {
            let bounds = [0, 0, i64::from(width), i64::from(height)];
            assert!(crate::merge::input::frame(Some(bounds), CanvasLimits::Resident).is_err());
            let oversized = [0, 0, if width == 16000 { 16001 } else { 2 },
                if height == 16000 { 16001 } else { 2 }];
            assert!(crate::merge::input::frame(Some(oversized), CanvasLimits::Native).is_err());
            let fixture = Fixture::new();
            let (runtime, _connection) =
                fixture.runtime_with_budgets(128 * 1024 * 1024, 16 * 1024 * 1024);
            let mut original = Document::new_depth(2, 2, depth).unwrap();
            original.width = width;
            original.height = height;
            original.layers[0].id = "first".into();
            let first = [12347, 23459, 34571, 65535];
            let last = [54321, 32109, 10987, 65535];
            original.layers[0].pixels.set16(0, 0, first);
            original.layers[0].pixels.set16(1, 0, [32109, 12347, 54321, 0]);
            let mut far = original.layers[0].clone();
            far.id = "last".into();
            far.x = width as i32 - 2;
            far.y = height as i32 - 2;
            far.pixels = crate::raster::Raster::new_depth(2, 2, depth);
            far.pixels.set16(1, 1, last);
            original.layers.push(far);
            let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
            let mut staged = CapturedScene::from_parts(original, "wide-merge".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(())).unwrap();
            let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
            let session = staged.session.take().unwrap();
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let command = json!({"op":"layer.merge","layers":["first","last"]});
            let disk = runtime.disk().used().unwrap();
            let check_pixels = |doc: &NativeDocument| {
                assert_eq!(doc.metadata().layers().len(), 1);
                let layer = doc.metadata().layer(0).unwrap();
                assert_eq!(layer.origin(), [0, 0]);
                let source = doc.source(layer.pixels().slot()).unwrap();
                assert_eq!((source.width(), source.height(), source.depth()), (width, height, depth));
                assert_eq!(source.physical_tiles().count(), 2);
                let view = source.prepare_region(Region::new(bounds.map(|v| v as i32)).unwrap(), &mut || Ok(())).unwrap();
                for y in 0..height as i32 {
                    for x in 0..width as i32 {
                        let expected = if (x, y) == (0, 0) { first }
                            else if (x, y) == (width as i32 - 1, height as i32 - 1) { last }
                            else { [0; 4] };
                        let expected = if depth == 16 { expected }
                            else { expected.map(|v| u16::from(crate::raster::project16(v)) * 257) };
                        assert_eq!(view.pixel16(x, y).unwrap(), expected);
                    }
                }
            };
            let preview = prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            check_pixels(preview.preview());
            preview.abort(&session, &|| session.cleanup_check()).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let cancelled = prepare(capture(&state, &[command.clone()]), &session, &|| {
                if runtime.disk().used().unwrap() > disk { Err("Cancel wide merge".into()) }
                else { Ok(()) }
            }).err().unwrap();
            assert!(cancelled.reason.contains("Cancel wide merge"));
            assert!(cancelled.cleanup_error.is_none());
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            let (base, history, delta) = prepared.into_parts();
            let commit = history.prepare_commit(&base, delta, HistoryRequest {
                actor: "human", label: "Merge wide layers", task: None, gesture: None,
            }, &|| Ok(())).unwrap();
            state.commit_native(gate(&[]), commit).unwrap().retire(&session, &|| session.cleanup_check()).unwrap();
            check_pixels(state.native().unwrap());
            for direction in [Direction::Undo, Direction::Redo] {
                let moved = state.native_history().unwrap().prepare_move(state.native().unwrap(), direction, "human", &|| Ok(())).unwrap();
                drop(state.move_native(gate(&[]), moved).unwrap());
                if matches!(direction, Direction::Redo) { check_pixels(state.native().unwrap()); }
                else {
                    for (before, after) in native.metadata().layers().zip(state.native().unwrap().metadata().layers()) {
                        assert_eq!(before.id(), after.id());
                        assert_eq!(native.source(before.pixels().slot()).unwrap().key(),
                            state.native().unwrap().source(after.pixels().slot()).unwrap().key());
                    }
                }
            }
        }
    }
}

fn exercise(original: Document, commands: &[Value]) {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(512 * 1024 * 1024, 256 * 1024 * 1024);
    let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
    let mut staged =
        CapturedScene::from_parts(original.clone(), "merge".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = Arc::new(staged.session.take().unwrap());
    let baseline = runtime.disk().used().unwrap();
    for command in commands {
        let commands = command
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![command.clone()]);
        let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
        expected.revision = original.revision + 1;
        let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
        pixels(preview.preview(), &expected, &original);
        for old in native.metadata().layers() {
            if let Some(kept) = preview
                .preview()
                .metadata()
                .layers()
                .find(|l| l.id() == old.id())
            {
                assert_eq!(
                    native.source(old.pixels().slot()).unwrap().key(),
                    preview
                        .preview()
                        .source(kept.pixels().slot())
                        .unwrap()
                        .key()
                );
                if let Some(mask) = old.mask() {
                    for (a, b) in mask.steps().zip(kept.mask().unwrap().steps()) {
                        assert_eq!(
                            native.source(a.pixels().slot()).unwrap().key(),
                            preview.preview().source(b.pixels().slot()).unwrap().key()
                        );
                    }
                }
            }
        }
        if let Some(old) = native.metadata().selection_coverage() {
            let kept = preview.preview().metadata().selection_coverage().unwrap();
            assert_eq!(
                native.source(old.mask().slot()).unwrap().key(),
                preview.preview().source(kept.mask().slot()).unwrap().key()
            );
        }
        let scene = SceneSnapshot::from_native(
            SceneIdentity {
                project_id: "merge-preview".into(),
                document_id: preview.preview().metadata().id().into(),
                revision: preview.preview().metadata().revision(),
            },
            preview.preview().clone(),
            vec![],
            None,
            None,
        )
        .unwrap();
        let request = crate::render::native_preview::Request::capture(
            Arc::new(scene),
            session.clone(),
            None,
            8192,
            None,
            false,
        )
        .unwrap();
        let actual = request.render(&mut || Ok(())).unwrap();
        assert_eq!(
            &*actual.copy_rgba8(&mut || Ok(())).unwrap(),
            expected
                .preview(None, 8192, None, false)
                .unwrap()
                .2
                .as_slice()
        );
        drop((actual, request));
        preview
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Merge",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        state
            .commit_native(gate(&[]), commit)
            .unwrap()
            .retire(&*session, &|| session.cleanup_check())
            .unwrap();
        for (direction, mut expected) in [
            (Direction::Undo, original.clone()),
            (Direction::Redo, expected),
        ] {
            let moved = state
                .native_history()
                .unwrap()
                .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
                .unwrap();
            drop(state.move_native(gate(&[]), moved).unwrap());
            expected.revision = state.view().revision();
            pixels(state.native().unwrap(), &expected, &original);
        }
        drop(state);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
    drop((native, session, staged.seals, cache, runtime, connection));
}

#[test]
fn merge_matches_native_samples_masks_styles_clipping_filters_and_nested_folder_history() {
    for depth in [8, 16] {
        let doc = super::canvas_tests::document(depth);
        exercise(
            doc.clone(),
            &[
                json!({"op":"layer.merge","layer":"a_clip"}),
                json!({"op":"layer.merge","layers":["A","a_base","A"],"name":"Flattened"}),
                json!({"op":"layer.merge","layers":["A","B"]}),
                json!({"op":"layer.merge","layers":["outer","outside"]}),
                json!([
                    {"op":"layer.merge","layer":"A"},
                    {"op":"layer.merge","layer":"B"},
                    {"op":"layer.merge","layer":"outer"}
                ]),
            ],
        );
        let mut folder = Engine::preview_edits(doc,&[
            json!({"op":"adjustment.add","layer":"B","kind":"invert"}),
            json!({"op":"mask.add","layer":"outer","value":181}),
            json!({"op":"effect.add","layer":"outer","kind":"drop_shadow","settings":{"size":2.,"distance":1.}}),
            json!({"op":"layer.update","layer":"outer","opacity":0.67,"blend":"screen"}),
        ]).unwrap();
        folder
            .layers
            .iter_mut()
            .find(|l| l.id == "b_child")
            .unwrap()
            .visible = false;
        exercise(folder, &[json!({"op":"layer.merge","layer":"outer"})]);
    }
}

#[test]
fn merge_preserves_off_canvas_native16_support_and_ignores_hidden_padding() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(8, 6, depth).unwrap();
        let top = &mut doc.layers[0];
        top.id = "top".into();
        top.x = -3;
        top.y = -2;
        top.pixels = crate::raster::Raster::new_depth(512, 5, depth);
        top.pixels.set16(500, 4, [65535; 4]);
        top.pixels.width = 270;
        top.pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
        top.pixels.set16(269, 4, [54321, 32109, 10987, 1]);
        top.pixels.set16(268, 4, [54321, 32109, 10987, 0]);
        let mut bottom = Layer::new("Bottom", "paint", 8, 6);
        bottom.id = "bottom".into();
        bottom.pixels = crate::raster::Raster::new_depth(8, 6, depth);
        doc.layers.push(bottom);
        doc.selection = Some([1, 1, 4, 4]);
        exercise(doc.clone(), &[json!({"op":"layer.merge","layer":"top"})]);
        for l in &mut doc.layers {
            l.pixels = crate::raster::Raster::new_depth(1, 1, depth);
        }
        exercise(doc, &[json!({"op":"layer.merge","layer":"top"})]);
    }
}

#[test]
fn merge_refusals_cancellation_batch_rollback_and_late_child_reservations_are_atomic() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(512 * 1024 * 1024, 256 * 1024 * 1024);
    let original = super::canvas_tests::document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
    let mut staged =
        CapturedScene::from_parts(original.clone(), "merge-refusal".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let baseline = runtime.disk().used().unwrap();
    for commands in [
        vec![json!({"op":"layer.merge","layers":[]})],
        vec![json!({"op":"layer.merge","layers":["missing"]})],
        vec![json!({"op":"layer.merge","layers":["a_base"]})],
        vec![json!({"op":"layer.merge","layers":["A","outside"]})],
        vec![
            json!({"op":"layer.update","layer":"A","locked":true}),
            json!({"op":"layer.merge","layer":"a_clip"}),
        ],
        vec![
            json!({"op":"layer.update","layer":"outer","blend":"pass_through"}),
            json!({"op":"layer.merge","layer":"outer"}),
        ],
        vec![
            json!({"op":"layer.update","layer":"A","visible":false}),
            json!({"op":"layer.merge","layer":"A"}),
        ],
        vec![
            json!({"op":"layer.merge","layer":"A"}),
            json!({"op":"layer.merge","layer":"missing"}),
        ],
    ] {
        assert!(Engine::preview_edits(original.clone(), &commands).is_err());
        let refused = prepare(capture(&state, &commands), &session, &|| Ok(()))
            .err()
            .unwrap();
        assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
    let command = json!({"op":"layer.merge","layer":"outer"});
    let refused = prepare(capture(&state, &[command.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > baseline {
            Err("Merge interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(refused.reason.contains("Merge interrupted"));
    assert!(refused.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Merge",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let leases = [crate::engine::Lease {
        id: "late".into(),
        owner: "other".into(),
        description: "Child edit".into(),
        scopes: vec![crate::engine::Scope::layer("a_base")],
        expires: 100,
    }];
    let refused = state.commit_native(gate(&leases), commit).unwrap_err();
    assert!(refused.reason().contains("Reserved"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
    drop((
        state,
        native,
        session,
        staged.seals,
        cache,
        runtime,
        connection,
    ));
}
