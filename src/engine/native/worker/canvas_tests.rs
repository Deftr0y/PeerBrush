use super::*;
use super::{
    grouping_tests::gate,
    import_tests::pixels,
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;

pub(super) fn document(depth: u16) -> Document {
    let mut doc = super::settings_tests::document(depth);
    doc.layers.iter_mut().find(|l| l.id == "A").unwrap().locked = false;
    let bitmap = if depth == 16 {
        crate::raster::png16(1, 1, &[12347, 23459, 34571, 49153]).unwrap()
    } else {
        crate::raster::png(1, 1, &[47, 91, 135, 191]).unwrap()
    };
    let mut doc = Engine::preview_edits(doc,&[
        json!({"op":"liquify.stroke","layer":"B","points":[[2.,2.],[4.,4.]],"radius":2.,"strength":0.4}),
        json!({"op":"effect.add","layer":"outside","kind":"pattern_overlay","settings":{"pattern":"bitmap","bitmap_png":STANDARD.encode(bitmap),"phase_x":1.25,"phase_y":-0.5}}),
        json!({"op":"filter.add","kind":"blur","settings":{"radius":1.}}),
    ]).unwrap();
    doc.selection_previous = doc.selection_coverage.clone();
    doc
}

#[test]
fn canvas_anchors_and_crop_preserve_native_owners_and_remap_effects_in_one_history_step() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(512 * 1024 * 1024, 128 * 1024 * 1024);
        let original = document(depth);
        let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
        let mut staged = CapturedScene::from_parts(
            original.clone(),
            "canvas-geometry".into(),
            vec![],
            None,
            None,
        )
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let disk = runtime.disk().used().unwrap();
        let mut commands = vec![
            json!({"op":"crop","rect":[-3,-2,9,10]}),
            json!({"op":"crop","rect":[2,1,7,6]}),
            json!({"op":"crop","rect":[0,0,6,4]}),
            json!({"op":"canvas.resize","width":8,"height":8}),
        ];
        commands.extend(
            [
                "top_left",
                "top",
                "top_right",
                "left",
                "center",
                "right",
                "bottom_left",
                "bottom",
                "bottom_right",
            ]
            .map(|a| json!({"op":"canvas.resize","width":13,"height":5,"anchor":a})),
        );
        for command in commands {
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let commands = [command];
            let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
            expected.revision = original.revision + 1;
            let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            pixels(preview.preview(), &expected, &original);
            for layer in native.metadata().layers() {
                let after = preview
                    .preview()
                    .metadata()
                    .layers()
                    .find(|l| l.id() == layer.id())
                    .unwrap();
                assert_eq!(
                    native.source(layer.pixels().slot()).unwrap().key(),
                    preview
                        .preview()
                        .source(after.pixels().slot())
                        .unwrap()
                        .key()
                );
                if let Some(mask) = layer.mask() {
                    for (before, after) in mask.steps().zip(after.mask().unwrap().steps()) {
                        assert_eq!(
                            native.source(before.pixels().slot()).unwrap().key(),
                            preview
                                .preview()
                                .source(after.pixels().slot())
                                .unwrap()
                                .key()
                        );
                    }
                }
                for (before, after) in layer.effects().zip(after.effects()) {
                    for ((_, before), (_, after)) in before.clips().zip(after.clips()) {
                        assert_eq!(
                            native.source(before.mask().slot()).unwrap().key(),
                            preview.preview().source(after.mask().slot()).unwrap().key()
                        );
                    }
                }
            }
            assert_eq!(runtime.disk().used().unwrap(), disk);
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            let (base, history, delta) = prepared.into_parts();
            let commit = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Canvas geometry",
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
                pixels(state.native().unwrap(), &expected, &original);
            }
            assert_eq!(runtime.disk().used().unwrap(), disk);
            drop(state);
        }
        drop((native, session, staged.seals, cache, runtime, connection));
    }
}

#[test]
fn canvas_refusals_and_late_leases_preserve_the_entire_document() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(512 * 1024 * 1024, 128 * 1024 * 1024);
    let original = document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "canvas-refusal".into(),
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
    let valid = json!({"op":"crop","rect":[2,1,7,6]});
    for commands in [
        vec![json!({"op":"crop","rect":[0,0,0,2]})],
        vec![json!({"op":"crop","rect":[0.5,0,2,2]})],
        vec![json!({"op":"crop","rect":[-100001,0,-99999,2]})],
        vec![json!({"op":"canvas.resize","width":0,"height":8})],
        vec![json!({"op":"canvas.resize","width":8193,"height":8})],
        vec![json!({"op":"canvas.resize","width":8,"height":8,"anchor":null})],
        vec![json!({"op":"canvas.resize","width":8,"height":8,"anchor":"invalid"})],
        vec![
            json!({"op":"layer.update","layer":"A","locked":true}),
            valid.clone(),
        ],
        vec![json!({"op":"crop","rect":[99999,0,100000,1]})],
        vec![json!({"op":"canvas.resize","width":8192,"height":8192,"anchor":"top_left"})],
    ] {
        assert!(Engine::preview_edits(original.clone(), &commands).is_err());
        let refused = prepare(capture(&state, &commands), &session, &|| Ok(()))
            .err()
            .unwrap();
        assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let locked_no_shift = [
        json!({"op":"layer.update","layer":"A","locked":true}),
        json!({"op":"canvas.resize","width":10,"height":9,"anchor":"top_left"}),
    ];
    let mut expected = Engine::preview_edits(original.clone(), &locked_no_shift).unwrap();
    expected.revision = original.revision + 1;
    let preview = prepare(capture(&state, &locked_no_shift), &session, &|| Ok(())).unwrap();
    pixels(preview.preview(), &expected, &original);
    preview
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    let calls = std::cell::Cell::new(0);
    let refused = prepare(capture(&state, &[valid.clone()]), &session, &|| {
        calls.set(calls.get() + 1);
        if calls.get() == 80 {
            Err("canvas cancelled".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(refused.reason.contains("canvas cancelled"));
    assert!(refused.cleanup_error.is_none());
    let prepared = prepare(capture(&state, &[valid]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Crop",
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
