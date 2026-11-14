use super::*;
use super::{
    effect_create_tests::compare_raster,
    grouping_tests::{compare, gate},
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine, Layer,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;

pub(super) fn command(depth: u16, rect: [i32; 4]) -> serde_json::Value {
    let mut bytes = Vec::new();
    for y in rect[1]..rect[3] {
        for x in rect[0]..rect[2] {
            let pixel = [
                12347 + x as u16 * 37,
                23459 + y as u16 * 23,
                34571,
                if x % 3 == 0 { 0 } else { 49153 },
            ];
            for word in pixel {
                if depth == 16 {
                    bytes.extend(word.to_le_bytes());
                } else {
                    bytes.push(crate::raster::project16(word));
                }
            }
        }
    }
    json!({"op":"pixels.replace","layer":"base","rect":rect,"bit_depth":depth,"rgba":STANDARD.encode(bytes)})
}
fn document(depth: u16, expanded: bool) -> Document {
    let mut doc = Document::new_depth(270, 6, depth).unwrap();
    let layer = &mut doc.layers[0];
    layer.id = "base".into();
    layer.x = if expanded { 4 } else { -2 };
    layer.y = if expanded { 2 } else { -1 };
    layer.pixels = crate::raster::Raster::new_depth(
        if expanded { 3 } else { 274 },
        if expanded { 2 } else { 8 },
        depth,
    );
    for y in 0..layer.pixels.height as i32 {
        for x in 0..layer.pixels.width as i32 {
            layer.pixels.set16(
                x,
                y,
                [11111, 22222, 33333, if x % 5 == 0 { 0 } else { 44444 }],
            );
        }
    }
    layer.pixels.retained = Some(Box::new(crate::retained::Original::from(&layer.pixels)));
    Engine::preview_edits(doc, &[json!({"op":"mask.add","layer":"base","value":137})]).unwrap()
}
fn pixels(native: &NativeDocument, expected: &Document) {
    compare(native, expected);
    for (actual, expected) in native.metadata().layers().zip(&expected.layers) {
        compare_raster(native, actual.pixels(), &expected.pixels);
        if let Some(mask) = actual.mask() {
            for (a, e) in mask.steps().zip(&expected.mask.as_ref().unwrap().steps) {
                compare_raster(native, a.pixels(), &e.pixels);
            }
        }
    }
}

#[test]
fn native_replacement_matches_literal_channels_coverage_frames_and_undo() {
    for depth in [8, 16] {
        for expanded in [false, true] {
            for mode in 0..5 {
                let fixture = Fixture::new();
                let (runtime, connection) =
                    fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
                let mut original = document(depth, expanded);
                match mode {
                    1 => original.selection = Some([250, 1, 268, 5]),
                    2 => {
                        original.selection = Some([250, 1, 268, 5]);
                        original.selection_polygon = Some(vec![[250., 1.], [268., 1.], [250., 5.]]);
                    }
                    3 => {
                        original.selection = Some([250, 1, 268, 5]);
                        original.selection_polygon = Some(vec![[250., 1.], [268., 1.], [250., 5.]]);
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
                            bounds: [250, 1, 268, 5],
                            origin: [0, 0],
                            contours: vec![],
                        });
                    }
                    4 => original.selection = Some([0; 4]),
                    _ => {}
                }
                let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
                let mut staged = CapturedScene::from_parts(
                    original.clone(),
                    "pixel-replacement".into(),
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
                let command = command(depth, [0, 0, 270, 6]);
                let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
                if !expanded && mode == 0 {
                    assert_eq!(
                        expected.layers[0].pixels.get16(3, 2),
                        if depth == 16 {
                            [12384, 23482, 34571, 49153]
                        } else {
                            [12384, 23482, 34571, 49153]
                                .map(|v| u16::from(crate::raster::project16(v)) * 257)
                        }
                    );
                }
                if mode == 3 && !expanded {
                    assert_ne!(
                        expected.layers[0].pixels.get16(268, 4),
                        original.layers[0].pixels.get16(268, 4)
                    );
                    assert_eq!(
                        expected.layers[0].pixels.get16(255, 3),
                        original.layers[0].pixels.get16(255, 3)
                    );
                    assert_eq!(
                        expected.layers[0].pixels.get16(4, 2),
                        original.layers[0].pixels.get16(4, 2)
                    );
                }
                let disk = runtime.disk().used().unwrap();
                let preview =
                    prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
                pixels(preview.preview(), &expected);
                preview
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
                assert_eq!(runtime.disk().used().unwrap(), disk);
                if mode == 3 {
                    let prepared =
                        prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
                    let (base, history, delta) = prepared.into_parts();
                    let commit = history
                        .prepare_commit(
                            &base,
                            delta,
                            HistoryRequest {
                                actor: "human",
                                label: "Pixels",
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
}

#[test]
fn replacement_refusals_preserve_sources_and_a_sixteen_folder_target_is_editable() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let mut original = document(16, false);
    let mut leaf = original.layers.remove(0);
    let mut parent = None;
    for i in 0..16 {
        let mut group = Layer::new("Folder", "group", 270, 6);
        group.pixels = crate::raster::Raster::new_depth(270, 6, 16);
        group.id = format!("g{i}");
        group.parent = parent;
        parent = Some(group.id.clone());
        original.layers.push(group);
    }
    leaf.parent = parent;
    original.layers.push(leaf);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "replacement-refusal".into(),
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
    let valid = command(16, [0, 0, 270, 6]);
    let expected = Engine::preview_edits(original.clone(), &[valid.clone()]).unwrap();
    let disk = runtime.disk().used().unwrap();
    let preview = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
    pixels(preview.preview(), &expected);
    preview
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    for patch in [
        json!({"bit_depth":8}),
        json!({"rgba":"AA=="}),
        json!({"rgba":"!".repeat(valid["rgba"].as_str().unwrap().len())}),
        json!({"rect":[-1,0,2,2]}),
        json!({"rect":[0,0,0,0]}),
        json!({"mask":true}),
        json!({"layer":"g0"}),
    ] {
        let mut invalid = valid.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let failure = prepare(
            capture(&state, &[valid.clone(), invalid]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failure.cleanup_error.is_none(), "{}", failure.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let failure = prepare(capture(&state, &[valid.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > disk {
            Err("replacement interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(failure.reason.contains("replacement interrupted"));
    assert!(failure.cleanup_error.is_none());
    let cancelled = capture(&state, &[valid.clone()]);
    cancelled.cancellation().cancel();
    assert!(prepare(cancelled, &session, &|| Ok(())).is_err());
    let prepared = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Pixels",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let leases = [crate::engine::Lease {
        id: "late".into(),
        owner: "other".into(),
        description: "Folder".into(),
        scopes: vec![crate::engine::Scope::layer("g0")],
        expires: 100,
    }];
    let refused = state.commit_native(gate(&leases), commit).unwrap_err();
    assert!(refused.reason().contains("Reserved"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    let captured = capture(&state, &[valid]);
    let pool = runtime.resident();
    let remaining = pool.limit() - pool.used();
    let held = pool.reserve_bytes(remaining, remaining).unwrap();
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
