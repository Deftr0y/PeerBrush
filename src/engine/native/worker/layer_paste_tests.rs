use super::*;
use super::{
    duplicate_tests::align_ids,
    grouping_tests::gate,
    import_tests::pixels,
    tests::{capture, Fixture},
};
use crate::{
    clipboard::native::layers::LayerCopyPlan,
    engine::{
        authority::DocumentAuthority,
        scene::{CapturedScene, LegacyTileCache},
        Document, Engine,
    },
    transform::native::Work,
};
use serde_json::json;

fn compare(native: &NativeDocument, expected: &Document, original: &Document) {
    let mut expected = expected.clone();
    align_ids(&mut expected, native, original);
    pixels(native, &expected, original);
}

pub(super) fn shared_large_document(depth: u16) -> Document {
    let mut original = Document::new_depth(8, 8, depth).unwrap();
    original.width = 16000;
    original.height = 512;
    let words = [12347, 23459, 34571, 65535, 54321, 32109, 10987, 0];
    let mut raster = crate::raster::Raster::new_depth(16000, 512, depth);
    if depth == 16 {
        let tile = std::sync::Arc::new(words.repeat(256 * 128));
        for x in 0..63 {
            for y in 0..2 { raster.samples16.insert((x, y), tile.clone()); }
        }
    } else {
        let tile = std::sync::Arc::new(words.map(crate::raster::project16).repeat(256 * 128));
        for x in 0..63 {
            for y in 0..2 { raster.tiles.insert((x, y), tile.clone()); }
        }
    }
    let mut template = original.layers.remove(0);
    template.pixels = raster;
    original.layers = (0..17).map(|i| {
        let mut layer = template.clone();
        layer.id = format!("shared-{i}");
        layer
    }).collect();
    original
}

#[test]
fn native_layer_paste_shares_large_repeated_sources_without_a_resident_frame_estimate() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 8 * 1024 * 1024);
        let original = shared_large_document(depth);
        let words = [12347, 23459, 34571, 65535, 54321, 32109, 10987, 0];
        let ids: Vec<_> = original.layers.iter().map(|l| l.id.clone()).collect();
        let cache = LegacyTileCache::new(&runtime.resident(), 4).unwrap();
        let mut staged = CapturedScene::from_parts(original, "large-layer-copy".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let source = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let clipboard = LayerCopyPlan::capture(&source, &ids, &mut Work::new(100_000).unwrap(), &|| Ok(())).unwrap();
        let oversized = super::super::materialize_layer_clipboard(&clipboard, [8, 8], &|| Ok(()));
        assert!(oversized.err().unwrap().contains("dimensions"));
        let referenced_bytes: usize = source.metadata().layers().map(|l| {
            source.source(l.pixels().slot()).unwrap().physical_tiles()
                .map(|t| t.reference().spec().bytes()).sum::<usize>()
        }).sum();
        assert!(referenced_bytes > 512 * 1024 * 1024);
        let destination = NativeDocument::new_blank(16000, 512, depth, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let mut state = DocumentAuthority::from_native(destination.clone(), 4).unwrap();
        let command = json!({"op":"layer.paste","layer":destination.metadata().layer(0).unwrap().id()});
        let disk = runtime.disk().used().unwrap();
        let preview = prepare_layer_paste(capture(&state, &[command]), &clipboard, &session, &|| Ok(())).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        for (old, new) in source.metadata().layers().zip(preview.preview().metadata().layers().skip(1).take(17)) {
            let before = source.source(old.pixels().slot()).unwrap();
            let after = preview.preview().source(new.pixels().slot()).unwrap();
            assert_eq!(before.key(), after.key());
            assert_ne!(old.id(), new.id());
            assert_eq!((after.width(), after.height(), after.depth()), (16000, 512, depth));
            for (x, y) in [(0, 0), (1, 0), (15999, 511)] {
                let expected: [u16; 4] = words[if x % 2 == 0 { 0..4 } else { 4..8 }].try_into().unwrap();
                let expected = if depth == 16 { expected } else { expected.map(|v| u16::from(crate::raster::project16(v)) * 257) };
                let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
                assert_eq!(after.prepare_region(region, &mut || Ok(())).unwrap().pixel16(x, y).unwrap(), expected);
            }
        }
        let (base, history, delta) = preview.into_parts();
        let commit = history.prepare_commit(&base, delta, HistoryRequest {
            actor: "human", label: "Paste shared layers", task: None, gesture: None,
        }, &|| Ok(())).unwrap();
        state.commit_native(gate(&[]), commit).unwrap().retire(&session, &|| session.cleanup_check()).unwrap();
        drop((clipboard, source, cache, staged.seals));
        for (direction, count) in [(Direction::Undo, 1), (Direction::Redo, 19)] {
            let moved = state.native_history().unwrap().prepare_move(state.native().unwrap(), direction, "human", &|| Ok(())).unwrap();
            drop(state.move_native(gate(&[]), moved).unwrap());
            assert_eq!(state.native().unwrap().metadata().layers().len(), count);
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
    }
}

#[test]
fn typed_layer_paste_shares_sources_or_promotes_exact_channels_across_documents_and_history() {
    for (depth, incoming) in [(8, 8), (8, 16), (16, 8), (16, 16)] {
        for target in ["outside", "B", "b_child", "", "empty"] {
            let fixture = Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(512 * 1024 * 1024, 256 * 1024 * 1024);
            let mut source_doc = super::settings_tests::document(incoming);
            let mut original = super::settings_tests::document(depth);
            source_doc.psd_global_angle = Some(180u32.to_be_bytes());
            original.psd_global_angle = Some(if depth == incoming { 180u32 } else { 0 }.to_be_bytes());
            let mut settings = crate::effects::defaults("inner_shadow");
            settings["angle"] = json!(0.);
            settings["global_light"] = json!([180, 45.]);
            source_doc.layers.iter_mut().find(|layer| layer.id == "outside").unwrap().effects.push(crate::effects::Effect {
                id: crate::engine::id(), kind: "inner_shadow".into(), enabled: true, weight: 1., settings: settings.into(),
            });
            if target == "empty" {
                original.layers.clear();
            }
            let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
            let mut source_stage = CapturedScene::from_parts(
                source_doc.clone(),
                "clipboard-source".into(),
                vec![],
                None,
                None,
            )
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
            let source = NativeDocument::from_pending(
                source_stage.scene.into_provisional_pending().unwrap(),
            )
            .unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "clipboard-destination".into(),
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
            let ids = vec!["outside".into(), "outer".into()];
            let clipboard =
                LayerCopyPlan::capture(&source, &ids, &mut Work::new(100_000).unwrap(), &|| Ok(()))
                    .unwrap();
            let snapshot = crate::layer_clipboard::copy(&source_doc, &ids).unwrap();
            let mut resident = Engine::new();
            resident.resident_state_mut().expect("resident fixture").document = original.clone();
            let response = resident
                .paste_layers("human", &snapshot, target, None, None)
                .unwrap();
            let expected = resident.into_resident_document().unwrap();
            let first = expected
                .layers
                .iter()
                .position(|l| l.id == response["created_roots"][0].as_str().unwrap())
                .unwrap();
            let command = json!({"op":"layer.paste","layer":target});
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let disk = runtime.disk().used().unwrap();
            let preview = prepare_layer_paste(
                capture(&state, &[command.clone()]),
                &clipboard,
                &session,
                &|| Ok(()),
            )
            .unwrap();
            compare(preview.preview(), &expected, &original);
            assert!(preview.preview().metadata().selection_coverage().is_none());
            for (j, &index) in clipboard.preorder().iter().enumerate() {
                let old = &source.core.metadata.layers[index];
                let layer = &preview.preview().core.metadata.layers[first + j];
                assert_ne!(layer.id, old.id);
                assert_eq!(layer.locked, old.locked);
                if old.id == "outside" {
                    let effect = layer.effects.last().unwrap();
                    assert_eq!(effect.settings.get("global_light").is_some(), depth == incoming);
                    assert_eq!(effect.settings["angle"], 0.);
                }
                if depth <= incoming {
                    assert_eq!(
                        preview.preview().roster().get(&layer.pixels).unwrap().key(),
                        source.roster().get(&old.pixels).unwrap().key()
                    );
                }
                for (new, old) in layer.effects.iter().zip(&old.effects) {
                    assert_ne!(new.id, old.id);
                    assert!(new.settings.same_allocation(&old.settings));
                }
                if let (Some(new), Some(old)) = (&layer.source, &old.source) {
                    assert!(new.content.same_allocation(&old.content));
                }
            }
            if depth == incoming {
                assert_eq!(runtime.disk().used().unwrap(), disk);
            }
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let prepared = prepare_layer_paste(
                capture(&state, &[command]),
                &clipboard,
                &session,
                &|| Ok(()),
            )
            .unwrap();
            let (base, history, delta) = prepared.into_parts();
            let commit = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Paste layers",
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
            // A clipboard can be released after publication; pasted sources have
            // their own shared owners, independent of the copied project's view.
            drop((
                clipboard,
                source,
                source_stage.session.take(),
                source_stage.seals,
                cache,
            ));
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
            drop((state, native, session, staged.seals, runtime, connection));
        }
    }
}

#[test]
fn resident_layer_clipboard_publishes_exact_native_sources_in_one_undo_entry() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 64 * 1024 * 1024);
    let source = super::settings_tests::document(16);
    let clipboard =
        crate::layer_clipboard::copy(&source, &["outside".into(), "outer".into()]).unwrap();
    let original = Document::new_depth(8, 8, 8).unwrap();
    let target = &original.layers[0].id;
    let mut resident = Engine::from_document(original.clone());
    resident
        .paste_layers("human", &clipboard, target, None, None)
        .unwrap();
    let mut expected = resident.into_resident_document().unwrap();
    let cache = LegacyTileCache::new(&runtime.resident(), 4).unwrap();
    let staged =
        CapturedScene::from_parts(original.clone(), "clipboard-target".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = runtime.begin().unwrap();
    let mut state = DocumentAuthority::from_native(native, 4).unwrap();
    let command = json!({"op":"layer.paste","layer":target});
    let disk = runtime.disk().used().unwrap();
    let preview = prepare_resident_layer_paste(
        capture(&state, std::slice::from_ref(&command)),
        &clipboard,
        &session,
        &|| Ok(()),
    )
    .unwrap();
    compare(preview.preview(), &expected, &original);
    assert!(runtime.disk().used().unwrap() > disk);
    preview.abort(&session, &|| session.cleanup_check()).unwrap();
    assert_eq!(runtime.disk().used().unwrap(), disk);
    assert_eq!(state.history_lengths(), [0, 0]);

    let prepared =
        prepare_resident_layer_paste(capture(&state, &[command]), &clipboard, &session, &|| Ok(()))
            .unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Paste layers",
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
    drop((clipboard, source));
    assert_eq!(state.history_lengths(), [1, 0]);
    expected.revision = state.view().revision();
    compare(state.native().unwrap(), &expected, &original);

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
}

#[test]
fn native_clipboard_round_trips_editable_layers_into_a_resident_tab() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 64 * 1024 * 1024);
    let mut source = super::settings_tests::document(16);
    let ids = ["outside".into(), "outer".into()];
    let expected = crate::layer_clipboard::copy(&source, &ids).unwrap();
    source.width = 16000;
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let staged = CapturedScene::from_parts(source, "native-clipboard".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let plan =
        LayerCopyPlan::capture(&native, &ids, &mut Work::new(100_000).unwrap(), &|| Ok(()))
            .unwrap();
    let (converted, charge) =
        super::super::materialize_layer_clipboard(&plan, [8, 8], &|| Ok(())).unwrap();
    assert_eq!(json!(converted), json!(expected));
    drop((converted, charge));

    let snapshot = crate::clipboard::LayerSnapshot::Native(plan);
    let original = Document::new_depth(8, 8, 8).unwrap();
    let shared = std::sync::Arc::new(std::sync::Mutex::new(Engine::from_document(original.clone())));
    let reply = crate::server::paste_layers_shared(
        &shared,
        "human",
        &snapshot,
        &original.layers[0].id,
        Some(original.revision),
        None,
        Some(&original.id),
        Some((&original).into()),
    )
    .unwrap();
    assert_eq!(reply.data()["applied"], 1);
    drop((snapshot, native));
    let mut engine = shared.lock().unwrap();
    assert_eq!(engine.history_lengths(), [1, 0]);
    let assert_pasted = |doc: &Document| {
        assert_eq!(doc.bit_depth, 16);
        let paint = doc
            .layers
            .iter()
            .find(|layer| layer.name == "outside copy")
            .unwrap();
        assert_eq!(paint.pixels.get16(779, 4), [54321, 32109, 10987, 0]);
        assert_eq!(
            paint.pixels.retained.as_ref().unwrap().pixels.get16(779, 4),
            [54321, 32109, 10987, 0]
        );
    };
    assert_pasted(engine.resident().unwrap());
    engine.undo("human").unwrap();
    assert_eq!(engine.resident().unwrap().layers.len(), 1);
    assert_eq!(engine.resident().unwrap().bit_depth, 8);
    engine.redo("human").unwrap();
    assert_pasted(engine.resident().unwrap());
}

#[test]
fn typed_clipboard_binding_cancellation_and_late_leases_refuse_without_leaking_promotions() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(512 * 1024 * 1024, 256 * 1024 * 1024);
    let source_doc = super::settings_tests::document(16);
    let original = super::settings_tests::document(8);
    let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
    let mut source_stage = CapturedScene::from_parts(
        source_doc,
        "paste-refusal-source".into(),
        vec![],
        None,
        None,
    )
    .stage(&runtime, &cache, &|| Ok(()))
    .unwrap();
    let source =
        NativeDocument::from_pending(source_stage.scene.into_provisional_pending().unwrap())
            .unwrap();
    let mut staged =
        CapturedScene::from_parts(original, "paste-refusal-target".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let clipboard = LayerCopyPlan::capture(
        &source,
        &["outer".into()],
        &mut Work::new(100_000).unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let command = json!({"op":"layer.paste","layer":"outside"});
    let disk = runtime.disk().used().unwrap();
    let missing = prepare(capture(&state, &[command.clone()]), &session, &|| Ok(()))
        .err()
        .unwrap();
    assert!(missing.reason.contains("typed clipboard"));
    for commands in [
        vec![command.clone(), command.clone()],
        vec![json!({"op":"layer.update","layer":"outside","opacity":0.5})],
    ] {
        assert!(
            prepare_layer_paste(capture(&state, &commands), &clipboard, &session, &|| Ok(()))
                .is_err()
        );
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    for target in ["a_base", "missing"] {
        let refused = prepare_layer_paste(
            capture(&state, &[json!({"op":"layer.paste","layer":target})]),
            &clipboard,
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(refused.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let refused = prepare_layer_paste(
        capture(&state, &[command.clone()]),
        &clipboard,
        &session,
        &|| {
            if runtime.disk().used().unwrap() > disk {
                Err("layer paste interrupted".into())
            } else {
                Ok(())
            }
        },
    )
    .err()
    .unwrap();
    assert!(refused.reason.contains("layer paste interrupted"));
    assert!(refused.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), disk);
    let foreign_fixture = Fixture::new();
    let (foreign_runtime, foreign_connection) =
        foreign_fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let foreign_cache = LegacyTileCache::new(&foreign_runtime.resident(), 4).unwrap();
    let foreign_stage = CapturedScene::from_parts(
        crate::engine::Document::new(2, 2).unwrap(),
        "foreign-paste-source".into(),
        vec![],
        None,
        None,
    )
    .stage(&foreign_runtime, &foreign_cache, &|| Ok(()))
    .unwrap();
    let foreign =
        NativeDocument::from_pending(foreign_stage.scene.into_provisional_pending().unwrap())
            .unwrap();
    let foreign_clipboard = LayerCopyPlan::capture(
        &foreign,
        &[foreign.metadata().layers().next().unwrap().id().into()],
        &mut Work::new(100_000).unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    let refused = prepare_layer_paste(
        capture(&state, &[command.clone()]),
        &foreign_clipboard,
        &session,
        &|| Ok(()),
    )
    .err()
    .unwrap();
    assert!(refused.reason.contains("another source service"));
    assert!(refused.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), disk);
    drop((
        foreign_clipboard,
        foreign,
        foreign_stage.session,
        foreign_stage.seals,
        foreign_cache,
        foreign_runtime,
        foreign_connection,
    ));
    let prepared =
        prepare_layer_paste(
            capture(&state, &[command]),
            &clipboard,
            &session,
            &|| Ok(()),
        )
        .unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Paste layers",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let leases = [crate::engine::Lease {
        id: "late".into(),
        owner: "other".into(),
        description: "Promoted source".into(),
        scopes: vec![crate::engine::Scope::layer("b_child")],
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
        clipboard,
        native,
        source,
        session,
        staged.seals,
        source_stage.session.take(),
        source_stage.seals,
        cache,
        runtime,
        connection,
    ));
}
