use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache, RasterCollector},
    Document, Engine, Layer,
};
use serde_json::json;

pub(super) fn document(depth: u16) -> Document {
    let mut doc = Document::new_depth(8, 8, depth).unwrap();
    doc.layers.clear();
    for (id, kind, parent, clip) in [
        ("b_child", "paint", Some("B"), None),
        ("outer", "group", None, None),
        ("a_clip", "paint", Some("A"), Some("a_base")),
        ("a_base", "paint", Some("A"), None),
        ("A", "group", Some("outer"), None),
        ("B", "group", Some("outer"), None),
        ("outside", "paint", None, None),
    ] {
        let mut layer = Layer::new(id, kind, 8, 8);
        layer.id = id.into();
        layer.parent = parent.map(str::to_owned);
        layer.clip_to = clip.map(str::to_owned);
        layer.pixels = crate::raster::Raster::new_depth(8, 8, depth);
        if kind == "paint" {
            layer
                .pixels
                .set16(doc.layers.len() as i32, 3, [12347, 23459, 34571, 49153]);
        }
        doc.layers.push(layer);
    }
    let doc = Engine::preview_edits(doc, &[
        json!({"op":"mask.add","layer":"a_base","value":137}),
        json!({"op":"effect.add","layer":"a_base","kind":"invert","weight":0.4}),
        json!({"op":"transform","layer":"a_base","scale_x":0.875,"scale_y":1.125,"selection_only":false}),
        json!({"op":"selection","rect":[1,1,6,7]}),
    ]).unwrap();
    doc
}

struct References(u32);
impl RasterCollector for References {
    fn resident(&mut self, raster: &crate::raster::Raster) -> Result<RasterRef36> {
        self.0 += 1;
        let id = self.0;
        let retained = raster
            .retained
            .as_ref()
            .map(|original| -> Result<_> {
                Ok(Box::new(crate::engine::scene::Retained36 {
                    pixels: self.resident(&original.pixels)?,
                    matrix: original.matrix,
                    projective: original.projective,
                    warps: original
                        .warps
                        .iter()
                        .map(crate::engine::scene::Warp36::capture)
                        .collect(),
                }))
            })
            .transpose()?;
        Ok(RasterRef36 {
            id,
            width: raster.width,
            height: raster.height,
            depth: raster.depth,
            retained,
        })
    }
    fn surface(&mut self, surface: &Surface) -> Result<RasterRef36> {
        self.0 += 1;
        Ok(RasterRef36 {
            id: self.0,
            width: surface.width(),
            height: surface.height(),
            depth: surface.depth(),
            retained: None,
        })
    }
    fn embedded_coverage(
        &mut self,
        value: &serde_json::Value,
    ) -> Result<crate::engine::scene::Coverage36> {
        use serde::Deserialize;
        let coverage = crate::selection::Coverage::deserialize(value).map_err(|e| e.to_string())?;
        coverage.validate()?;
        crate::engine::scene::Coverage36::from_capture(&coverage, self)
    }
}
pub(super) fn compare(native: &NativeDocument, expected: &Document) {
    let expected = crate::engine::scene::Document36::capture(expected, &mut References(0)).unwrap();
    assert_eq!(json!(native.metadata()), json!(expected));
}
pub(super) fn gate<'a>(leases: &'a [crate::engine::Lease]) -> Publication<'a> {
    Publication {
        actor: "human",
        leases,
        now: 0,
        closed: false,
        write_available: true,
    }
}

#[test]
fn selected_move_uses_one_coverage_per_gesture_across_native_layers() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(32, 32, depth).unwrap();
        let first = doc.layers[0].id.clone();
        let mut second = Layer::new("Second", "paint", 32, 32);
        second.pixels = crate::raster::Raster::new_depth(32, 32, depth);
        let second_id = second.id.clone();
        doc.layers.push(second);
        for layer in &mut doc.layers {
            layer.pixels.set16(3, 4, [12347, 23459, 34571, 65535]);
            layer.pixels.set16(12, 4, [34571, 23459, 12347, 65535]);
        }
        let commands = [
            json!({"op":"selection","rect":[2,3,6,7]}),
            json!({"op":"move","layer":first,"dx":8,"dy":0}),
            json!({"op":"move","layer":second_id,"dx":8,"dy":0}),
            // A repeated target begins a new gesture and sees the moved selection.
            json!({"op":"move","layer":first,"dx":8,"dy":0}),
        ];
        super::tests::exercise_edits(doc, &commands, true);
    }
}

#[test]
fn grouping_preserves_native_roster_and_visible_subtree_order_through_atomic_history() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let original = document(depth);
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(
            original.clone(),
            "native-grouping".into(),
            vec![],
            None,
            None,
        )
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        for (ids, order, parent) in [
            (
                vec![],
                vec![
                    "Folder", "b_child", "outer", "a_clip", "a_base", "A", "B", "outside",
                ],
                None,
            ),
            (
                vec!["b_child", "a_base", "a_clip"],
                vec![
                    "outer", "Folder", "a_clip", "a_base", "b_child", "A", "B", "outside",
                ],
                Some("outer"),
            ),
            (
                vec!["A", "a_base", "outside", "A"],
                vec![
                    "b_child", "Folder", "A", "a_clip", "a_base", "outside", "outer", "B",
                ],
                None,
            ),
        ] {
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let command = json!({"op":"group.create_selected","layers":ids,"name":"Folder"});
            let mut expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            assert_eq!(
                expected
                    .layers
                    .iter()
                    .map(|l| l.name.as_str())
                    .collect::<Vec<_>>(),
                order
            );
            let baseline = runtime.disk().used().unwrap();
            let prepared =
                prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            let group = prepared.created_layers().next().unwrap().to_owned();
            assert_eq!(prepared.created_layers().count(), 1);
            let old_group = expected
                .layers
                .iter()
                .find(|l| l.name == "Folder")
                .unwrap()
                .id
                .clone();
            for layer in &mut expected.layers {
                if layer.id == old_group {
                    layer.id = group.clone();
                }
                if layer.parent.as_deref() == Some(&old_group) {
                    layer.parent = Some(group.clone());
                }
            }
            compare(prepared.preview(), &expected);
            let scene = crate::engine::scene::SceneSnapshot::from_native(
                crate::engine::scene::SceneIdentity {
                    project_id: "group-preview".into(),
                    document_id: prepared.preview().metadata().id().into(),
                    revision: prepared.preview().metadata().revision(),
                },
                prepared.preview().clone(),
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
            let folder = prepared
                .preview()
                .metadata()
                .layers()
                .find(|l| l.id() == group)
                .unwrap();
            assert_eq!(folder.parent(), parent);
            assert_eq!(folder.pixels().depth(), depth);
            assert!(prepared
                .preview()
                .source(folder.pixels().slot())
                .unwrap()
                .is_known_zero());
            let mut actual_keys = prepared
                .preview()
                .roster()
                .iter()
                .map(|(_, s)| s.key())
                .collect::<Vec<_>>();
            let mut expected_keys = native
                .roster()
                .iter()
                .map(|(_, s)| s.key())
                .collect::<Vec<_>>();
            expected_keys.push(Surface::zero(8, 8, depth).unwrap().key());
            actual_keys.sort();
            expected_keys.sort();
            assert_eq!(actual_keys, expected_keys);
            for layer in native.metadata().layers() {
                assert_eq!(
                    prepared
                        .preview()
                        .source(prepared.preview().layer_slot(layer.id()).unwrap())
                        .unwrap()
                        .key(),
                    native.source(layer.pixels().slot()).unwrap().key()
                );
            }
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            let (base, history, delta) = prepared.into_parts();
            let committed = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Folder",
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
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            let preview = prepare(
                capture(&state, &[json!({"op":"group.create_selected","layers":[]})]),
                &session,
                &|| Ok(()),
            )
            .unwrap();
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(state.history_lengths(), [1, 0]);
        }
        drop((native, session, staged.seals, cache, runtime, connection));
    }
}

#[test]
fn grouping_refuses_partial_clips_locked_descendants_invalid_ids_and_late_reservations() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let original = document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "native-group-refusal".into(),
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
    let valid = json!({"op":"group.create_selected","layers":["A","B"]});
    let baseline = runtime.disk().used().unwrap();
    for command in [
        json!({"op":"group.create_selected","layers":["a_base"]}),
        json!({"op":"group.create_selected","layers":["a_clip"]}),
        json!({"op":"group.create_selected","layers":["missing"]}),
        json!({"op":"group.create_selected","layers":[""]}),
        json!({"op":"group.create_selected","layers":"A"}),
        json!({"op":"group.create_selected","layers":vec!["A";101]}),
    ] {
        let failed = prepare(
            capture(&state, &[valid.clone(), command]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failed.cleanup_error.is_none(), "{}", failed.reason);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
    for locked in ["a_base", "A", "outer"] {
        let failed = prepare(
            capture(
                &state,
                &[
                    json!({"op":"layer.update","layer":locked,"locked":true}),
                    valid.clone(),
                ],
            ),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failed.reason.contains("Unlock"), "{}", failed.reason);
        assert!(failed.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
    for reserved in ["a_base", "outer"] {
        let prepared = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let committed = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Folder",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        let leases = [crate::engine::Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "Artist work".into(),
            scopes: vec![crate::engine::Scope::layer(reserved)],
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
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
    assert_eq!(runtime.disk().used().unwrap(), baseline);
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
fn grouping_keeps_the_same_layer_and_nesting_limits_in_both_document_engines() {
    for (count, depth, accepted) in [
        (99, 0, true),
        (100, 0, false),
        (17, 15, true),
        (17, 16, false),
    ] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(64 * 1024 * 1024, 16 * 1024 * 1024);
        let mut original = Document::new_depth(4, 2, 16).unwrap();
        original.layers.clear();
        for i in 0..count {
            let mut layer = Layer::new(
                &format!("Layer {i}"),
                if depth > 0 && i < 16 {
                    "group"
                } else {
                    "paint"
                },
                4,
                2,
            );
            layer.id = format!("node{i}");
            layer.pixels = crate::raster::Raster::new_depth(4, 2, 16);
            if depth > 0 && i > 0 {
                layer.parent = Some(format!("node{}", if i == 16 { depth - 1 } else { i - 1 }));
            }
            original.layers.push(layer);
        }
        crate::psd::validate(&original).unwrap();
        let target = if depth > 0 { "node16" } else { "node0" };
        let command = json!({"op":"group.create_selected","layer":target});
        assert_eq!(
            Engine::preview_edits(original.clone(), &[command.clone()]).is_ok(),
            accepted
        );
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original, "group-limits".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let baseline = runtime.disk().used().unwrap();
        match prepare(capture(&state, &[command]), &session, &|| Ok(())) {
            Ok(prepared) => {
                assert!(accepted);
                assert_eq!(prepared.preview().metadata().layers().len(), count + 1);
                prepared
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
            }
            Err(failed) => {
                assert!(!accepted);
                assert!(
                    failed
                        .reason
                        .contains(if depth > 0 { "nesting" } else { "100 layers" }),
                    "{}",
                    failed.reason
                );
                assert!(failed.cleanup_error.is_none());
            }
        }
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
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
