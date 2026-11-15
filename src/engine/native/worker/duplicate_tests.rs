use super::*;
use super::{
    grouping_tests::{compare, gate},
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use serde_json::json;

fn timestamp(copy: bool) -> crate::psd::LayerMetadata {
    let mut descriptor = vec![];
    descriptor.extend(16u32.to_be_bytes());
    descriptor.extend(1u32.to_be_bytes());
    descriptor.extend(0u16.to_be_bytes());
    descriptor.extend(8u32.to_be_bytes());
    descriptor.extend(b"metadata");
    descriptor.extend(1u32.to_be_bytes());
    descriptor.extend(9u32.to_be_bytes());
    descriptor.extend(b"layerTime");
    descriptor.extend(b"doub");
    descriptor.extend(1720591532.2346468f64.to_be_bytes());
    descriptor.resize((descriptor.len() + 3) & !3, 0);
    let mut data = 1u32.to_be_bytes().to_vec();
    data.extend(b"8BIMcust");
    data.extend([u8::from(copy), 0, 0, 0]);
    data.extend((descriptor.len() as u32).to_be_bytes());
    data.extend(descriptor);
    crate::psd::LayerMetadata {
        key: "shmd".into(),
        data,
    }
}
fn document(depth: u16) -> Document {
    let mut doc = super::grouping_tests::document(depth);
    let source: crate::source::Source = serde_json::from_value(json!({"width":8,"height":8,"matrix":[1.,0.,0.,1.,0.,0.],
        "content":{"kind":"shape","shape":"ellipse","bounds":[0.25,0.5,7.75,7.5],"points":[],"closed":true,
        "fill":[12347,23459,34571,49153],"stroke":[54321,32109,10987,49153],"stroke_width":0.75}})).unwrap();
    let outside = doc.layers.iter_mut().find(|l| l.id == "outside").unwrap();
    outside.pixels = source.render(8, 8, depth).unwrap();
    outside.source = Some(Arc::new(source));
    outside.psd_metadata = vec![
        timestamp(false),
        crate::psd::LayerMetadata {
            key: "lyid".into(),
            data: 17u32.to_be_bytes().to_vec(),
        },
        crate::psd::LayerMetadata {
            key: "lclr".into(),
            data: vec![0, 4, 0, 0, 0, 0, 0, 0],
        },
    ];
    let child = doc.layers.iter_mut().find(|l| l.id == "a_base").unwrap();
    child.psd_metadata = vec![timestamp(true)];
    child.locked = true;
    crate::psd::validate(&doc).unwrap();
    doc
}
pub(super) fn align_ids(expected: &mut Document, actual: &NativeDocument, original: &Document) {
    let mapping: std::collections::HashMap<_, _> = expected
        .layers
        .iter()
        .zip(&actual.core.metadata.layers)
        .map(|(a, b)| {
            assert_eq!(a.name, b.name);
            if original.layers.iter().any(|old| old.id == a.id) {
                assert_eq!(a.id, b.id);
            }
            (a.id.clone(), b.id.clone())
        })
        .collect();
    for (layer, actual) in expected.layers.iter_mut().zip(&actual.core.metadata.layers) {
        layer.id = mapping[&layer.id].clone();
        layer.parent = layer.parent.as_ref().map(|id| mapping[id].clone());
        layer.clip_to = layer.clip_to.as_ref().map(|id| mapping[id].clone());
        for (effect, actual) in layer.effects.iter_mut().zip(&actual.effects) {
            effect.id = actual.id.clone();
        }
        if let (Some(mask), Some(actual)) = (&mut layer.mask, &actual.mask) {
            for (step, actual) in mask.steps.iter_mut().zip(&actual.steps) {
                step.id = actual.id.clone();
            }
        }
    }
}

#[test]
fn duplicate_native_forest_preserves_sources_remaps_ids_and_matches_rendered_history() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let original = document(depth);
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original.clone(), "duplicate".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        for (ids, names) in [
            (
                vec!["B", "A", "a_base", "A"],
                vec![
                    "b_child", "outer", "a_clip", "a_base", "A copy", "a_clip", "a_base", "B copy",
                    "b_child", "A", "B", "outside",
                ],
            ),
            (
                vec!["outside", "b_child"],
                vec![
                    "b_child copy",
                    "b_child",
                    "outer",
                    "a_clip",
                    "a_base",
                    "A",
                    "B",
                    "outside copy",
                    "outside",
                ],
            ),
        ] {
            let command = json!({"op":"layer.duplicate","layers":ids});
            let mut expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            assert_eq!(
                expected
                    .layers
                    .iter()
                    .map(|l| l.name.as_str())
                    .collect::<Vec<_>>(),
                names
            );
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let disk = runtime.disk().used().unwrap();
            let preview =
                prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            align_ids(&mut expected, preview.preview(), &original);
            compare(preview.preview(), &expected);
            let copied = preview
                .created_layers()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            for id in &copied {
                let layer = preview
                    .preview()
                    .core
                    .metadata
                    .layers
                    .iter()
                    .find(|l| l.id == *id)
                    .unwrap();
                let name = layer.name.strip_suffix(" copy").unwrap_or(&layer.name);
                let old = native
                    .core
                    .metadata
                    .layers
                    .iter()
                    .find(|l| l.name == name)
                    .unwrap();
                assert_eq!(
                    preview
                        .preview()
                        .source(preview.preview().layer_slot(id).unwrap())
                        .unwrap()
                        .key(),
                    native
                        .source(native.layer_slot(&old.id).unwrap())
                        .unwrap()
                        .key()
                );
                assert_ne!(layer.id, old.id);
                for (a, b) in layer.effects.iter().zip(&old.effects) {
                    assert_ne!(a.id, b.id);
                    assert!(a.settings.same_allocation(&b.settings));
                }
                if let (Some(a), Some(b)) = (&layer.mask, &old.mask) {
                    for (a, b) in a.steps.iter().zip(&b.steps) {
                        assert_ne!(a.id, b.id);
                        assert!(a.settings.same_allocation(&b.settings));
                    }
                }
                if let (Some(a), Some(b)) = (&layer.source, &old.source) {
                    assert!(a.content.same_allocation(&b.content));
                }
                assert_eq!(layer.locked, old.locked);
                assert_eq!(
                    &*layer.psd_metadata,
                    &crate::psd::metadata_for_duplicate(&old.psd_metadata)
                );
                assert!(layer.psd_metadata.iter().all(|entry| entry.key != "lyid"));
            }
            for (_, surface) in preview.preview().roster().iter() {
                assert!(native
                    .roster()
                    .iter()
                    .any(|(_, old)| old.key() == surface.key()));
            }
            let scene = crate::engine::scene::SceneSnapshot::from_native(
                crate::engine::scene::SceneIdentity {
                    project_id: "duplicate-preview".into(),
                    document_id: preview.preview().metadata().id().into(),
                    revision: preview.preview().metadata().revision(),
                },
                preview.preview().clone(),
                vec![],
                None,
                None,
            )
            .unwrap();
            let frame = crate::render::native_preview::test_frame(
                &scene,
                None,
                8,
                None,
                false,
                &mut || Ok(()),
            )
            .unwrap();
            assert_eq!(
                &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                expected.preview(None, 8, None, false).unwrap().2.as_slice()
            );
            drop((frame, scene));
            assert_eq!(runtime.disk().used().unwrap(), disk);
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(state.native().unwrap().key(), native.key());
            assert_eq!(state.history_lengths(), [0, 0]);
            let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            align_ids(&mut expected, prepared.preview(), &original);
            let (base, history, delta) = prepared.into_parts();
            let committed = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Duplicate",
                        task: None,
                        gesture: None,
                    },
                    &|| Ok(()),
                )
                .unwrap();
            state
                .commit_native(gate(&[]), committed)
                .unwrap()
                .retire(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(state.history_lengths(), [1, 0]);
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
                compare(state.native().unwrap(), &expected);
            }
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        drop((native, session, staged.seals, cache, runtime, connection));
    }
}

#[test]
fn duplicate_and_transform_address_the_new_root_in_one_native_batch() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let original = document(depth);
        let old = original.layers.iter().find(|layer| layer.id == "outside").unwrap();
        let id = crate::engine::id();
        let commands = [
            json!({"op":"layer.duplicate","layers":["outside"],"root_ids":{"outside":id}}),
            json!({"op":"move","layer":id,"dx":7,"dy":4,"selection_only":false}),
        ];
        let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(original.clone(), "repeat-duplicate".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let mut state = DocumentAuthority::from_native(native, 4).unwrap();
        for invalid in [vec![commands[1].clone()], vec![commands[1].clone(), commands[0].clone()], vec![
            json!({"op":"layer.duplicate","layers":["outside"],"root_ids":{"unrelated":id}}),
            commands[1].clone(),
        ]] {
            let document = state.native().unwrap();
            let descriptor = CommandDescriptor::from_normalized(document, document.metadata().revision(), &invalid, &|| Ok(())).unwrap();
            let failure = state.capture_native(descriptor, "human", &[], 0, false, true).err().unwrap();
            assert!(failure.contains("Unknown native editor scope target"));
        }
        let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
        let actual = prepared.preview().metadata().layers().find(|layer| layer.id() == id).unwrap();
        assert_eq!(actual.origin(), [old.x + 7, old.y + 4]);
        align_ids(&mut expected, prepared.preview(), &original);
        compare(prepared.preview(), &expected);
        assert_eq!(state.history_lengths(), [0, 0]);
        let (base, history, delta) = prepared.into_parts();
        let committed = history.prepare_commit(&base, delta, HistoryRequest {
            actor: "human", label: "Duplicate with Transform", task: None, gesture: None,
        }, &|| Ok(())).unwrap();
        state.commit_native(gate(&[]), committed).unwrap().retire(&session, &|| Ok(())).unwrap();
        assert_eq!(state.history_lengths(), [1, 0]);
        let undone = state.native_history().unwrap().prepare_move(
            state.native().unwrap(), Direction::Undo, "human", &|| Ok(()),
        ).unwrap();
        drop(state.move_native(gate(&[]), undone).unwrap());
        assert!(state.native().unwrap().metadata().layers().all(|layer| layer.id() != id));
        assert_eq!(state.history_lengths(), [0, 1]);
    }
}

#[test]
fn duplication_refuses_incomplete_clips_locked_roots_failed_batches_and_late_reservations() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let original = document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged =
        CapturedScene::from_parts(original, "duplicate-refusal".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let valid = json!({"op":"layer.duplicate","layers":["A","outside"]});
    let disk = runtime.disk().used().unwrap();
    for invalid in [
        json!({"op":"layer.duplicate","layers":[]}),
        json!({"op":"layer.duplicate","layers":["missing"]}),
        json!({"op":"layer.duplicate","layers":["a_clip"]}),
        json!({"op":"layer.duplicate","layers":["a_base","a_clip"]}),
        json!({"op":"layer.duplicate","layers":vec!["A";101]}),
        json!({"op":"layer.duplicate","layers":"A"}),
    ] {
        let failed = prepare(
            capture(&state, &[valid.clone(), invalid]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failed.cleanup_error.is_none(), "{}", failed.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(state.history_lengths(), [0, 0]);
        assert_eq!(state.native().unwrap().key(), native.key());
    }
    for id in ["A", "outer"] {
        let failed = prepare(
            capture(
                &state,
                &[
                    json!({"op":"layer.update","layer":id,"locked":true}),
                    valid.clone(),
                ],
            ),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failed.reason.to_lowercase().contains("locked"));
        assert!(failed.cleanup_error.is_none());
    }
    for target in ["A", "a_base", "outer", "outside"] {
        let prepared = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let committed = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Duplicate",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        let leases = [crate::engine::Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "Human work".into(),
            scopes: vec![crate::engine::Scope::layer(target)],
            expires: 100,
        }];
        let refused = state.commit_native(gate(&leases), committed).unwrap_err();
        assert!(refused.reason().contains("Reserved"));
        refused
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
    }
    let capture = capture(&state, &[valid]);
    capture.cancellation().cancel();
    assert!(prepare(capture, &session, &|| Ok(())).is_err());
    assert_eq!(runtime.disk().used().unwrap(), disk);
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

#[test]
fn duplicate_limits_and_header_admission_preserve_the_original_owner() {
    for (count, nested, accepted) in [(99, false, true), (100, false, false), (17, true, true)] {
        let fixture = Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let mut original = Document::new_depth(4, 2, 16).unwrap();
        original.layers.clear();
        for i in 0..count {
            let mut layer = crate::engine::Layer::new(
                &format!("Layer {i}"),
                if nested && i < 16 { "group" } else { "paint" },
                4,
                2,
            );
            layer.id = format!("node{i}");
            layer.pixels = crate::raster::Raster::new_depth(4, 2, 16);
            if nested && i > 0 {
                layer.parent = Some(format!("node{}", i - 1));
            }
            original.layers.push(layer);
        }
        let command = json!({"op":"layer.duplicate","layer":if nested {"node16"} else {"node0"}});
        assert_eq!(
            Engine::preview_edits(original.clone(), &[command.clone()]).is_ok(),
            accepted
        );
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original, "duplicate-limits".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let disk = runtime.disk().used().unwrap();
        match prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())) {
            Ok(prepared) => {
                assert!(accepted);
                assert_eq!(prepared.preview().metadata().layers().len(), count + 1);
                prepared
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
            }
            Err(failed) => {
                assert!(!accepted);
                assert!(failed.reason.contains("100 layers"));
                assert!(failed.cleanup_error.is_none());
            }
        }
        let foreign = ResidentBudget::new(32 * 1024 * 1024).unwrap();
        assert!(native
            .core
            .metadata
            .copy_runtime_with_layers(&[0], &foreign, &|| Ok(()))
            .is_err());
        assert_eq!(foreign.used(), 0);
        assert!(native
            .core
            .metadata
            .copy_runtime_with_layers(&[count], native.budget(), &|| Ok(()))
            .is_err());
        let edit = capture(&state, &[command]);
        let pool = runtime.resident();
        let remaining = pool.limit() - pool.used();
        let held = pool.reserve_bytes(remaining, remaining).unwrap();
        assert!(native
            .core
            .metadata
            .copy_runtime_with_layers(&[0], &pool, &|| Ok(()))
            .is_err());
        let failed = prepare(edit, &session, &|| Ok(())).err().unwrap();
        assert!(failed.cleanup_error.is_none());
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
}
