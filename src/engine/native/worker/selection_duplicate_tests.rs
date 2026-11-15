use super::*;
use super::{
    duplicate_tests::align_ids,
    grouping_tests::gate,
    import_tests::pixels,
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use serde_json::json;

fn compare(actual: &NativeDocument, expected: &Document, original: &Document) {
    let mut expected = expected.clone();
    align_ids(&mut expected, actual, original);
    pixels(actual, &expected, original);
}

#[test]
fn selected_duplicate_keeps_the_document_profile_coordinates_and_samples() {
    for depth in [8, 16] {
        let mut original = Document::new_depth(12, 8, depth).unwrap();
        let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        original.icc_profile = Some(profile.into());
        original.selection = Some([3, 2, 7, 6]);
        let source = &mut original.layers[0];
        let id = source.id.clone();
        source.pixels.set16(3, 2, [13107, 39321, 19661, 65535]);
        let sample = source.pixels.get16(3, 2);
        let command = json!({"op":"layer.duplicate_selection","layer":id});
        let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
        let copy = &expected.layers[0];
        assert_eq!([copy.x, copy.y], [3, 2]);
        assert_eq!(copy.pixels.depth, depth);
        assert_eq!(copy.pixels.get16(0, 0), sample);
        assert_eq!(expected.icc_profile, original.icc_profile);
        assert_eq!(expected.selection, original.selection);
        if depth == 8 {
            let clipboard = crate::clipboard::Image::copy(&original, &id, false, false).unwrap();
            assert_ne!(&clipboard.bytes[..3], &sample.map(crate::raster::project16)[..3]);
        }
        super::tests::exercise_edits(original, &[command], true);
    }
}

#[test]
fn selected_mask_duplicate_uses_rendered_gray_and_keeps_the_clipping_unit() {
    for depth in [8, 16] {
        let mut original = Document::new_depth(12, 8, depth).unwrap();
        let base = original.layers[0].id.clone();
        let mut foreground = crate::engine::Layer::new("Clipped", "paint", 12, 8);
        foreground.clip_to = Some(base.clone());
        let target = foreground.id.clone();
        original.layers.insert(0, foreground);
        original = Engine::preview_edits(original, &[json!({"op":"mask.add","layer":target,"value":103})]).unwrap();
        original.layers[0].pixels.set16(3, 2, [13107, 39321, 19661, 65535]);
        original.selection = Some([3, 2, 7, 6]);
        let command = json!({"op":"layer.duplicate_selection","layer":target,"mask":true});
        let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
        let copy = &expected.layers[0];
        assert_eq!([copy.x, copy.y], [3, 2]);
        assert_eq!(copy.pixels.depth, depth);
        assert_eq!(copy.pixels.get16(0, 0), [26471, 26471, 26471, 65535]);
        assert!(copy.clip_to.is_none() && copy.mask.is_none());
        assert_eq!(expected.layers[1].id, target);
        assert_eq!(expected.layers[1].clip_to.as_deref(), Some(base.as_str()));
        assert_eq!(expected.selection, original.selection);
        super::tests::exercise_edits(original, &[command], true);
    }
}

#[test]
fn sparse_large_selected_duplicate_keeps_native_mask_opacity_coordinates_and_history() {
    for (depth, height, column) in [
        (8, 4, false), (16, 4, false), (8, 16000, false), (16, 16000, false),
        (8, 16000, true), (16, 16000, true),
    ] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 32 * 1024 * 1024);
        let mut original = Document::new_depth(2, 2, depth).unwrap();
        original.width = 16000;
        original.height = height;
        original.selection = Some([1, 1, 16000, height as i32 - 1]);
        let target = &mut original.layers[0];
        target.id = "target".into();
        target.opacity = 0.5;
        target.pixels = crate::raster::Raster::new_depth(16000, height, depth);
        let first = [12347, 23459, 34571, 65535];
        let last = [54321, 32109, 10987, 65535];
        target.pixels.set16(1, 1, first);
        if column {
            for y in 2..height as i32 - 1 { target.pixels.set16(1, y, first); }
        }
        target.pixels.set16(2, 1, [32109, 12347, 54321, 0]);
        target.pixels.set16(15999, height as i32 - 2, last);
        target.mask = Some(crate::engine::Mask {
            enabled: true, linked: true, cache_key: crate::engine::id(),
            steps: vec![crate::engine::MaskStep {
                id: "fill-mask".into(), kind: "fill".into(), enabled: true, weight: 1., value: 128.,
                settings: serde_json::Value::Null,
                pixels: crate::raster::Raster::new_depth(16000, height, depth),
            }],
        });
        let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
        let mut staged = CapturedScene::from_parts(original, "wide-selection-copy".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let command = json!({"op":"layer.duplicate_selection","layer":"target"});
        let disk = runtime.disk().used().unwrap();
        let verify = |doc: &NativeDocument| {
            assert_eq!(doc.metadata().selection(), Some([1, 1, 16000, height as i32 - 1]));
            assert_eq!(doc.metadata().layers().len(), 2);
            let layer = doc.metadata().layer(0).unwrap();
            assert_ne!(layer.id(), "target");
            assert_eq!(layer.origin(), [1, 1]);
            let source = doc.source(layer.pixels().slot()).unwrap();
            assert_eq!((source.width(), source.height(), source.depth()), (15999, height - 2, depth));
            assert_eq!(source.physical_tiles().count(), if column { (height - 2).div_ceil(256) as usize + 1 } else { 2 });
            for tile in source.physical_tiles() {
                let (tx, ty) = tile.coordinate();
                let [left, top, right, bottom] = [tx * 256, ty * 256,
                    ((tx + 1) * 256).min(15999), ((ty + 1) * 256).min(height - 2)].map(|v| v as i32);
                let view = source.prepare_region(crate::raster::backed::Region::new([left, top, right, bottom]).unwrap(), &mut || Ok(())).unwrap();
                for y in top..bottom {
                    for x in left..right {
                        let mut expected = if x == 0 && (column || y == 0) { first }
                            else if (x, y) == (15998, height as i32 - 3) { last } else { [0; 4] };
                        if depth == 8 { expected = expected.map(|v| u16::from(crate::raster::project16(v)) * 257); }
                        if expected[3] != 0 { expected[3] = 16448; }
                        assert_eq!(view.pixel16(x, y).unwrap(), expected);
                    }
                }
            }
            assert!(!source.has_physical_tiles([8000, 0, 8001, height as i32 - 2]));
            let before = native.metadata().layer(0).unwrap();
            let after = doc.metadata().layer(1).unwrap();
            assert_eq!(native.source(before.pixels().slot()).unwrap().key(), doc.source(after.pixels().slot()).unwrap().key());
        };
        let preview = prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
        verify(preview.preview());
        preview.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let stopped = prepare(capture(&state, &[command.clone()]), &session, &|| {
            if runtime.disk().used().unwrap() > disk { Err("Cancel wide selection copy".into()) }
            else { Ok(()) }
        }).err().unwrap();
        assert!(stopped.reason.contains("Cancel wide selection copy"), "{}", stopped.reason);
        assert!(stopped.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history.prepare_commit(&base, delta, HistoryRequest {
            actor: "human", label: "Duplicate wide selection", task: None, gesture: None,
        }, &|| Ok(())).unwrap();
        state.commit_native(gate(&[]), commit).unwrap().retire(&session, &|| session.cleanup_check()).unwrap();
        for direction in [Direction::Undo, Direction::Redo] {
            let moved = state.native_history().unwrap().prepare_move(state.native().unwrap(), direction, "human", &|| Ok(())).unwrap();
            drop(state.move_native(gate(&[]), moved).unwrap());
            if matches!(direction, Direction::Redo) { verify(state.native().unwrap()); }
            else { assert_eq!(state.native().unwrap().metadata().layers().len(), 1); }
        }
    }
}

#[test]
fn selected_duplicate_keeps_pixels_generated_outside_sparse_paint_bands() {
    for depth in [8, 16] {
        for fill in [false, true] {
            let mut original = Document::new_depth(300, 600, depth).unwrap();
            original.selection = Some([0, 0, 300, 600]);
            let layer = &mut original.layers[0];
            layer.id = "target".into();
            if fill {
                layer.kind = "fill".into();
                layer.color = [13, 57, 123, 255];
            } else {
                layer.pixels.set16(150, 255, [12347, 33559, 51237, 65535]);
                layer.effects.push(crate::effects::Effect {
                    id: "outline".into(), kind: "stroke".into(), enabled: true, weight: 1.,
                    settings: json!({"size":6.,"opacity":1.,"color":[1.,0.,0.]}).into(),
                });
            }
            let command = json!({"op":"layer.duplicate_selection","layer":"target"});
            let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            assert!(expected.layers[0].pixels.get16(150, 256)[3] > 0);
            super::tests::exercise_edits(original, &[command], true);
        }
    }
}

#[test]
fn selected_duplicate_keeps_precision_rendered_pixels_clipping_selection_and_shared_history() {
    for depth in [8, 16] {
        for target in ["a_clip", "a_base", "outer", "B", "b_child", "outside"] {
            for selected in [false, true] {
                if !selected && matches!(target, "a_clip" | "a_base") {
                    continue; // Whole-layer duplication requires a complete clipping unit.
                }
                let fixture = Fixture::new();
                let (runtime, connection) =
                    fixture.runtime_with_budgets(512 * 1024 * 1024, 128 * 1024 * 1024);
                let mut original = super::settings_tests::document(depth);
                original
                    .layers
                    .iter_mut()
                    .find(|l| l.id == "A")
                    .unwrap()
                    .locked = false;
                original
                    .layers
                    .iter_mut()
                    .find(|l| l.id == "outside")
                    .unwrap()
                    .pixels
                    .set16(6, 3, [12347, 23459, 34571, 49153]);
                original.selection_previous = original.selection_coverage.clone();
                if !selected {
                    original.selection = None;
                    original.selection_polygon = None;
                    original.selection_coverage = None;
                }
                let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
                let mut staged = CapturedScene::from_parts(
                    original.clone(),
                    "selected-duplicate".into(),
                    vec![],
                    None,
                    None,
                )
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
                let native =
                    NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
                        .unwrap();
                let session = staged.session.take().unwrap();
                let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                let command = json!({"op":"layer.duplicate_selection","layer":target,"document_id":original.id,"source_revision":original.revision});
                // Supplied `layers` never replaces this command's single active target.
                let mut command = command;
                command["layers"] = json!(["missing"]);
                let commands = [command];
                let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
                expected.revision = original.revision + 1;
                let disk = runtime.disk().used().unwrap();
                let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
                compare(preview.preview(), &expected, &original);
                for before in native.metadata().layers() {
                    let after = preview
                        .preview()
                        .metadata()
                        .layers()
                        .find(|l| l.id() == before.id())
                        .unwrap();
                    assert_eq!(
                        native.source(before.pixels().slot()).unwrap().key(),
                        preview
                            .preview()
                            .source(after.pixels().slot())
                            .unwrap()
                            .key()
                    );
                }
                if !selected {
                    assert_eq!(runtime.disk().used().unwrap(), disk);
                }
                preview
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
                assert_eq!(runtime.disk().used().unwrap(), disk);
                assert_eq!(state.history_lengths(), [0, 0]);
                let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
                let (base, history, delta) = prepared.into_parts();
                let commit = history
                    .prepare_commit(
                        &base,
                        delta,
                        HistoryRequest {
                            actor: "human",
                            label: "Duplicate selection",
                            task: None,
                            gesture: None,
                        },
                        &|| Ok(()),
                    )
                    .unwrap();
                state
                    .commit_native(gate(&[]), commit)
                    .unwrap()
                    .retire(&session, &|| session.cleanup_check())
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
                    compare(state.native().unwrap(), &expected, &original);
                }
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
        }
    }
}

#[test]
fn empty_locked_stale_cancelled_and_late_reserved_selection_duplication_has_no_partial_result() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(512 * 1024 * 1024, 128 * 1024 * 1024);
    let original = super::settings_tests::document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "selected-duplicate-refusal".into(),
        vec![],
        None,
        None,
    )
    .stage(&runtime, &cache, &|| Ok(()))
    .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let valid = json!({"op":"layer.duplicate_selection","layer":"b_child"});
    for command in [
        json!({"op":"layer.duplicate_selection","layer":"outside"}),
        json!({"op":"layer.duplicate_selection","layer":"a_base"}),
        json!({"op":"layer.duplicate_selection","layer":"missing"}),
    ] {
        assert!(Engine::preview_edits(original.clone(), &[command.clone()]).is_err());
        let refused = prepare(
            capture(&state, &[valid.clone(), command]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
    }
    for fields in [
        json!({"document_id":"stale"}),
        json!({"source_revision": original.revision + 1}),
    ] {
        let mut stale = valid.clone();
        stale
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(Engine::preview_edits(original.clone(), &[stale.clone()]).is_err());
        assert!(
            CommandDescriptor::from_normalized(&native, original.revision, &[stale], &|| Ok(()))
                .is_err()
        );
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let refused = prepare(capture(&state, &[valid.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > disk {
            Err("duplicate interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(refused.reason.contains("duplicate interrupted"));
    assert!(refused.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), disk);
    let prepared = prepare(capture(&state, &[valid]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Duplicate selection",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let leases = [crate::engine::Lease {
        id: "late".into(),
        owner: "other".into(),
        description: "Selection".into(),
        scopes: vec![crate::engine::Scope::layer("@selection")],
        expires: 100,
    }];
    let refused = state.commit_native(gate(&leases), commit).unwrap_err();
    assert!(refused.reason().contains("Reserved"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(runtime.disk().used().unwrap(), disk);
    assert_eq!(state.history_lengths(), [0, 0]);
    assert_eq!(state.native().unwrap().key(), native.key());
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
