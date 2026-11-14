use super::*;
use super::{
    effect_create_tests::compare_raster,
    grouping_tests::{compare, gate},
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use serde_json::json;

fn document(depth: u16) -> Document {
    let mut doc = Document::new_depth(270, 6, depth).unwrap();
    let layer = &mut doc.layers[0];
    layer.id = "base".into();
    layer.x = -2;
    layer.y = 1;
    for y in 0..6 {
        for x in 0..270 {
            layer.pixels.set16(
                x,
                y,
                [11111, 22222, 33333, if x % 5 == 0 { 0 } else { 44444 }],
            );
        }
    }
    layer.pixels.retained = Some(Box::new(crate::retained::Original::from(&layer.pixels)));
    doc
}
fn png(fixture: &Fixture, depth: u16) -> std::path::PathBuf {
    let path = fixture.path().join(format!("patch-{depth}.png"));
    let pixel = [12347, 23459, 34571, 49153];
    let bytes = if depth == 16 {
        crate::raster::png16(270, 6, &pixel.repeat(270 * 6)).unwrap()
    } else {
        crate::raster::png(270, 6, &pixel.map(crate::raster::project16).repeat(270 * 6)).unwrap()
    };
    std::fs::write(&path, bytes).unwrap();
    path
}
fn pixels(native: &NativeDocument, expected: &Document) {
    compare(native, expected);
    compare_raster(
        native,
        native.metadata().layers().next().unwrap().pixels(),
        &expected.layers[0].pixels,
    );
}

#[test]
fn wide_png_patches_reach_the_native_canvas_corner_and_retire_the_input() {
    for (depth, incoming) in [(8, 8), (16, 8), (16, 16)] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let native = NativeDocument::new_blank(16000, 16000, depth, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let layer = native.metadata().layers().next().unwrap().id().to_owned();
        let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let path = fixture.path().join("wide-patch.png");
        let words = [12347, 23459, 34571, 49153];
        let bytes = if incoming == 16 {
            super::import_tests::native_png16(16000, 3, &words.repeat(16000 * 3))
        } else {
            crate::raster::png(16000, 3, &words.map(crate::raster::project16).repeat(16000 * 3)).unwrap()
        };
        std::fs::write(&path, bytes).unwrap();
        let command = json!({"op":"image.patch","path":path,"layer":layer,"x":0,"y":15997,"rect":[15998,15998,16000,16000]});
        let disk = runtime.disk().used().unwrap();
        let expected = if incoming == 16 { words } else { words.map(|v| u16::from(crate::raster::project16(v)) * 257) };
        let inspect = |document: &NativeDocument, applied| {
            assert_eq!((document.metadata().width(), document.metadata().height(), document.metadata().depth()), (16000, 16000, depth));
            let pixels = document.metadata().layers().next().unwrap().pixels();
            let source = document.source(pixels.slot()).unwrap();
            let area = crate::raster::backed::Region::new([15997, 15997, 16000, 16000]).unwrap();
            let view = source.prepare_region(area, &mut || Ok(())).unwrap();
            assert_eq!(view.pixel16(15997, 15998).unwrap(), [0; 4]);
            assert_eq!(view.pixel16(15998, 15997).unwrap(), [0; 4]);
            assert_eq!(view.pixel16(15999, 15999).unwrap(), if applied { expected } else { [0; 4] });
            assert_eq!(source.physical_tiles().count(), usize::from(applied));
        };
        let preview = prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
        inspect(preview.preview(), true);
        // Only the changed destination tile survives preparation, not the 63 source tiles.
        assert!(runtime.disk().used().unwrap() - disk < 2 * 1024 * 1024);
        preview.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let refused = prepare(capture(&state, &[command.clone()]), &session, &|| {
            if runtime.disk().used().unwrap() > disk { Err("Cancel wide PNG input".into()) } else { Ok(()) }
        }).err().unwrap();
        assert!(refused.reason.contains("Cancel wide PNG input"));
        assert!(refused.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(state.history_lengths(), [0, 0]);
        let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history.prepare_commit(&base, delta, HistoryRequest {
            actor: "human", label: "Wide patch", task: None, gesture: None,
        }, &|| Ok(())).unwrap();
        state.commit_native(gate(&[]), commit).unwrap().retire(&session, &|| session.cleanup_check()).unwrap();
        inspect(state.native().unwrap(), true);
        for (direction, applied) in [(Direction::Undo, false), (Direction::Redo, true)] {
            let moved = state.native_history().unwrap().prepare_move(state.native().unwrap(), direction, "human", &|| Ok(())).unwrap();
            drop(state.move_native(gate(&[]), moved).unwrap());
            inspect(state.native().unwrap(), applied);
        }
    }
}

#[test]
fn png_patch_matches_native_depth_selection_and_clipped_layer_frame_with_history() {
    for (depth, source_depth) in [(8, 8), (16, 8), (16, 16)] {
        for mode in 0..5 {
            let fixture = Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
            let mut original = document(depth);
            match mode {
                1 => original.selection = Some([250, 2, 266, 5]),
                2 => {
                    original.selection = Some([250, 2, 266, 5]);
                    original.selection_polygon = Some(vec![[250., 2.], [266., 2.], [250., 5.]]);
                }
                3 => {
                    original.selection = Some([250, 2, 266, 5]);
                    original.selection_polygon = Some(vec![[250., 2.], [266., 2.], [250., 5.]]);
                    let mut mask = crate::raster::Raster::new(270, 6);
                    for y in 0..6 {
                        for x in 0..270 {
                            if x % 3 != 1 {
                                mask.set(x, y, [77, 77, 77, 255]);
                            }
                        }
                    }
                    original.selection_coverage = Some(crate::selection::Coverage {
                        mask,
                        bounds: [250, 2, 266, 5],
                        origin: [0, 0],
                        contours: vec![],
                    });
                }
                4 => original.selection = Some([0; 4]),
                _ => {}
            }
            let path = png(&fixture, source_depth);
            let command = json!({"op":"image.patch","layer":"base","path":path,"x":-1,"y":0,"rect":[0,0,267,5]});
            let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            assert_eq!([expected.layers[0].x, expected.layers[0].y], [-2, 1]);
            assert_eq!(
                expected.layers[0].pixels.get16(0, 0),
                original.layers[0].pixels.get16(0, 0)
            );
            if mode == 0 {
                let source = if source_depth == 16 {
                    [12347, 23459, 34571, 49153]
                } else {
                    [12347, 23459, 34571, 49153]
                        .map(|v| u16::from(crate::raster::project16(v)) * 257)
                };
                let before = original.layers[0].pixels.get16(252, 1);
                let sample = if depth == 16 {
                    crate::raster::blend16(before, source, 1., "normal")
                } else {
                    crate::raster::blend(
                        before.map(crate::raster::project16),
                        source.map(crate::raster::project16),
                        1.,
                        "normal",
                    )
                    .map(|v| u16::from(v) * 257)
                };
                assert_eq!(expected.layers[0].pixels.get16(252, 1), sample);
            }
            if mode == 3 {
                // Coverage overrides the polygon but never its declared bounds.
                assert_ne!(
                    expected.layers[0].pixels.get16(266, 3),
                    original.layers[0].pixels.get16(266, 3)
                );
                assert_eq!(
                    expected.layers[0].pixels.get16(255, 2),
                    original.layers[0].pixels.get16(255, 2)
                );
                assert_eq!(
                    expected.layers[0].pixels.get16(4, 2),
                    original.layers[0].pixels.get16(4, 2)
                );
            }
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "image-patch".into(),
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
            let disk = runtime.disk().used().unwrap();
            let preview =
                prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            pixels(preview.preview(), &expected);
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            if mode == 3 {
                let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
                let (base, history, delta) = prepared.into_parts();
                let commit = history
                    .prepare_commit(
                        &base,
                        delta,
                        HistoryRequest {
                            actor: "human",
                            label: "Image patch",
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
                for (direction, mut expected) in
                    [(Direction::Undo, original), (Direction::Redo, expected)]
                {
                    let moved = state
                        .native_history()
                        .unwrap()
                        .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
                        .unwrap();
                    drop(state.move_native(gate(&[]), moved).unwrap());
                    expected.revision = state.view().revision();
                    pixels(state.native().unwrap(), &expected);
                }
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

#[test]
fn patch_refusals_and_cancellation_leave_no_source_or_history() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let original = document(8);
    let path = png(&fixture, 8);
    let deep = png(&fixture, 16);
    let broken = fixture.path().join("broken.png");
    let mut bytes = std::fs::read(&path).unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    std::fs::write(&broken, bytes).unwrap();
    let command = json!({"op":"image.patch","layer":"base","path":path});
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "patch-refusals".into(),
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
    for fields in [
        json!({"path":deep}),
        json!({"path":broken}),
        json!({"x":1.5}),
        json!({"x":i64::MAX}),
        json!({"mask":true}),
    ] {
        let mut invalid = command.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        if invalid["path"] != json!(broken) {
            assert!(Engine::preview_edits(original.clone(), &[invalid.clone()]).is_err());
        }
        let refused = prepare(
            capture(&state, &[command.clone(), invalid]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let refused = prepare(capture(&state, &[command.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > disk {
            Err("patch interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(refused.reason.contains("patch interrupted"));
    assert!(refused.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), disk);
    let prepared = prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Patch",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let leases = [crate::engine::Lease {
        id: "late".into(),
        owner: "other".into(),
        description: "Paint".into(),
        scopes: vec![crate::engine::Scope::layer("base")],
        expires: 100,
    }];
    let refused = state.commit_native(gate(&leases), commit).unwrap_err();
    assert!(refused.reason().contains("Reserved"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    let captured = capture(&state, &[command]);
    let pool = runtime.resident();
    let available = pool.limit() - pool.used();
    let held = pool.reserve_bytes(available, available).unwrap();
    assert!(prepare(captured, &session, &|| Ok(())).is_err());
    drop(held);
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
    assert_eq!(runtime.disk().used().unwrap(), disk);
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
