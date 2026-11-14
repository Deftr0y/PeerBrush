use super::effect_create_tests::compare_raster;
use super::grouping_tests::{compare, gate};
use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
    Document, Engine,
};
use serde_json::json;

pub(super) fn document(depth: u16) -> Document {
    let mut doc = Document::new_depth(9, 7, depth).unwrap();
    let layer = &mut doc.layers[0];
    layer.id = "base".into();
    layer.x = 3;
    layer.y = -1;
    layer.pixels = crate::raster::Raster::new_depth(5, 4, depth);
    for y in 0..4 {
        for x in 0..5 {
            layer
                .pixels
                .set16(x, y, [12347 + x as u16 * 911, 23459, 34571, 49153]);
        }
    }
    let mut original = crate::retained::Original::from(&layer.pixels);
    original.projective = Some([1., 0., 0., 0., 1., 0., 0.01, 0., 1.]);
    layer.pixels = original.render(5, 4).unwrap();
    let mut doc = Engine::preview_edits(doc, &[
        json!({"op":"mask.add","layer":"base","value":200}),
        json!({"op":"mask.step.add","layer":"base","kind":"blur","value":1}),
        json!({"op":"selection","kind":"ellipse","rect":[1,1,8,6],"feather":1}),
        json!({"op":"liquify.stroke","layer":"base","points":[[4.,1.],[6.,2.]],"radius":2.,"strength":0.2}),
        json!({"op":"selection.clear"}),
    ]).unwrap();
    let mask = doc.layers[0].mask.as_mut().unwrap();
    mask.linked = false;
    for (index, step) in mask.steps.iter_mut().enumerate() {
        step.pixels = crate::raster::Raster::new_depth(3 + index as u32, 6 - index as u32, depth);
        if step.kind == "paint" {
            // Deliberately extends beyond the eventual color/canvas union.
            step.pixels = crate::raster::Raster::new_depth(14, 12, depth);
            for y in 0..12 {
                for x in 0..14 {
                    step.pixels
                        .set16(x, y, [21931 + x as u16 * 977, 21931, 21931, 49153]);
                }
            }
        } else {
            step.pixels.set16(1, 1, [21931, 21931, 21931, 49153]);
        }
        step.pixels.retained = Some(Box::new(crate::retained::Original::from(&step.pixels)));
    }
    let effect = doc.layers[0]
        .effects
        .iter_mut()
        .find(|e| e.kind == "liquify")
        .unwrap();
    let mut settings = (*effect.settings).clone();
    let mut coverage: crate::selection::Coverage =
        serde_json::from_value(settings["strokes"][0]["coverage"].clone()).unwrap();
    coverage.mask.retained = Some(Box::new(crate::retained::Original::from(&coverage.mask)));
    settings["strokes"][0]["coverage"] = json!(coverage);
    effect.settings = settings.into();
    doc
}

fn compare_pixels(native: &NativeDocument, expected: &Document) {
    compare(native, expected);
    let layer = native.metadata().layer(0).unwrap();
    compare_raster(native, layer.pixels(), &expected.layers[0].pixels);
    for (step, expected) in layer
        .mask()
        .unwrap()
        .steps()
        .zip(&expected.layers[0].mask.as_ref().unwrap().steps)
    {
        compare_raster(native, step.pixels(), &expected.pixels);
    }
}

#[test]
fn frame_growth_reveals_retained_mask_before_color_paint_and_mask_erase() {
    use crate::raster::{backed::Region, Raster};
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let mut doc = Document::new_depth(9, 7, depth).unwrap();
        let layer = &mut doc.layers[0];
        let id = layer.id.clone();
        layer.x = 3;
        layer.y = 2;
        layer.pixels = Raster::new_depth(3, 2, depth);
        let mut full = Raster::new_depth(6, 4, depth);
        full.set16(0, 0, [0, 0, 0, 65535]);
        full.set16(1, 0, [12347, 12347, 12347, 65535]);
        full.set16(2, 1, [65535; 4]);
        let untouched = full.get16(1, 0);
        let mut original = crate::retained::Original::from(&full);
        original.matrix[4] = -2.;
        original.matrix[5] = -1.;
        let mut paint = Raster::new_depth(3, 2, depth);
        paint.retained = Some(Box::new(original));
        layer.mask = Some(crate::engine::Mask {
            enabled: true,
            linked: false,
            cache_key: crate::engine::id(),
            steps: vec![crate::engine::MaskStep {
                id: crate::engine::id(), kind: "paint".into(), enabled: true,
                weight: 1., value: 0., pixels: paint, settings: serde_json::Value::Null,
            }],
        });
        let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
        let mut staged = CapturedScene::from_parts(doc, "off-frame-mask".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native, 4).unwrap();
        for (erase, command) in [
            (false, json!({"op":"paint.fill","layer":id,"rect":[0,0,1,1],"color":[255,255,255,255]})),
            (true, json!({"op":"paint","layer":id,"mask":true,"points":[[1.5,1.5]],"radius":0.5,"hardness":1,"flow":1,"opacity":1,"erase":true,"color":[0,0,0,255]})),
        ] {
            let preview = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            let candidate = preview.preview();
            let layer = candidate.metadata().layer(0).unwrap();
            assert_eq!(layer.origin(), [0, 0]);
            let mask = layer.mask().unwrap().steps().next().unwrap().pixels();
            let sample = |slot, x, y| {
                let region = Region::new([x, y, x + 1, y + 1]).unwrap();
                candidate.source(slot).unwrap().prepare_region(region, &mut || Ok(()))
                    .unwrap().pixel16(x, y).unwrap()
            };
            assert_eq!(sample(mask.slot(), 2, 1), untouched);
            assert_eq!(sample(mask.slot(), 3, 2), [0; 4]);
            let at = if erase { [0; 4] } else { [0, 0, 0, 65535] };
            assert_eq!(sample(mask.slot(), 1, 1), at);
            let original = mask.retained().unwrap();
            assert_eq!(sample(original.pixels().slot(), 0, 0), at);
            preview.abort(&session, &|| session.cleanup_check()).unwrap();
        }
        assert_eq!(state.history_lengths(), [0, 0]);
    }
}

#[test]
fn transformed_off_frame_masks_keep_revealed_samples_when_paint_rebases_the_source() {
    use crate::raster::{backed::Region, Raster};
    for depth in [8, 16] {
        for placement in ["fractional", "perspective", "warp"] {
            let fixture = Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let mut doc = Document::new_depth(9, 7, depth).unwrap();
            let layer = &mut doc.layers[0];
            let id = layer.id.clone();
            layer.x = 3;
            layer.y = 3;
            layer.pixels = Raster::new_depth(3, 2, depth);
            let mut raw = Raster::new_depth(11, 4, depth);
            for y in 0..4 {
                for x in 0..11 {
                    let word = 12347 + x as u16 * 257 + y as u16 * 171;
                    raw.set16(x, y, [word, word, word, 65535]);
                }
            }
            let mut original = crate::retained::Original::from(&raw);
            original.matrix[4] = -2.25;
            original.matrix[5] = -1.;
            match placement {
                "perspective" => {
                    original.projective = Some([1., 0., 0., 0., 1., 0., 0.005, 0., 1.])
                }
                "warp" => original.warps.push(crate::warp::Stage {
                    before: crate::projective::Map::IDENTITY.matrix(),
                    grid: crate::warp::Grid::preset([0., 0., 11., 4.], "arc", 0.03).unwrap(),
                }),
                _ => {}
            }
            let mut world = original.clone();
            world.matrix[4] += 3.;
            world.matrix[5] += 3.;
            let expected = world.render(14, 8).unwrap();
            let exposed = expected.get16(2, 2);
            let off_canvas = expected.get16(10, 2);
            assert!(exposed[3] > 0 && off_canvas[3] > 0, "{placement}");
            let mut current = Raster::new_depth(3, 2, depth);
            current.retained = Some(Box::new(original));
            layer.mask = Some(crate::engine::Mask {
                enabled: true,
                linked: false,
                cache_key: crate::engine::id(),
                steps: vec![crate::engine::MaskStep {
                    id: crate::engine::id(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 1.,
                    value: 0.,
                    pixels: current,
                    settings: serde_json::Value::Null,
                }],
            });
            let commands = [
                json!({"op":"paint.fill","layer":id,"rect":[0,0,1,1],"color":[255,255,255,255]}),
                json!({"op":"paint.fill","layer":id,"mask":true,"rect":[3,3,4,4],"color":[81,81,81,255]}),
            ];
            let resident = Engine::preview_edits(doc.clone(), &commands).unwrap();
            let current = &resident.layers[0].mask.as_ref().unwrap().steps[0].pixels;
            assert_eq!(current.get16(2, 2), exposed, "{placement}");
            assert_eq!(current.get16(2, 0), [0; 4]);
            assert_eq!(current.get16(4, 3), [0; 4]);
            assert_eq!(current.get16(3, 3), [81 * 257, 81 * 257, 81 * 257, 65535]);
            let baked = current.retained.as_ref().unwrap();
            assert!(baked.projective.is_none() && baked.warps.is_empty());
            assert_eq!(
                baked.pixels.get16(10 - baked.matrix[4] as i32, 2 - baked.matrix[5] as i32),
                off_canvas,
                "{placement}"
            );

            let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
            let mut staged =
                CapturedScene::from_parts(doc, "transformed-mask".into(), vec![], None, None)
                    .stage(&runtime, &cache, &|| Ok(()))
                    .unwrap();
            let native =
                NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
                    .unwrap();
            let session = staged.session.take().unwrap();
            let state = DocumentAuthority::from_native(native, 4).unwrap();
            let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            let candidate = preview.preview();
            let layer = candidate.metadata().layer(0).unwrap();
            let mask = layer.mask().unwrap().steps().next().unwrap().pixels();
            let original = mask.retained().unwrap();
            assert_eq!(original.matrix(), baked.matrix);
            compare_raster(candidate, mask, current);
            compare_raster(candidate, original.pixels(), &baked.pixels);
            let x = 10 - original.matrix()[4] as i32;
            let y = 2 - original.matrix()[5] as i32;
            let region = Region::new([x, y, x + 1, y + 1]).unwrap();
            assert_eq!(
                candidate
                    .source(original.pixels().slot())
                    .unwrap()
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(x, y)
                    .unwrap(),
                off_canvas
            );
            preview.abort(&session, &|| session.cleanup_check()).unwrap();
            assert_eq!(state.history_lengths(), [0, 0]);
        }
    }
}

#[test]
fn expanded_operators_preserve_linked_planes_clips_previews_and_atomic_history() {
    for depth in [8, 16] {
        for soft in [false, true] {
            let root = Fixture::new();
            let (runtime, connection) =
                root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
            let mut original = document(depth);
            if soft {
                original = Engine::preview_edits(
                    original,
                    &[json!({"op":"selection","kind":"ellipse","rect":[0,0,9,7],"feather":1})],
                )
                .unwrap();
            }
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "expanded-operators".into(),
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
            let old_layer = native.metadata().layer(0).unwrap();
            let old_clip = old_layer
                .effects()
                .find(|e| e.kind() == "liquify")
                .unwrap()
                .clips()
                .next()
                .unwrap()
                .1;
            for op in [
                "paint",
                "smudge",
                "clone",
                "heal",
                "paint.fill",
                "shape",
                "gradient",
                "pixels.replace",
            ] {
                for mask in [false, true] {
                    if op == "pixels.replace" && mask {
                        continue;
                    }
                    let disk = runtime.disk().used().unwrap();
                    let mut command = json!({"op":op,"layer":"base","mask":mask,
                        "rect":[0,0,9,7],"kind":"ellipse","color":[37,83,127,193],
                        "points":[[1.25,2.5,0.7],[2.25,3.25,0.9],[4.25,4.5,0.6]],
                        "source":[4.5,1.25],"radius":1.8,"spacing":0.8,"hardness":0.4,
                        "flow":0.6,"opacity":0.7,"load":0.3,"pickup":0.4,"wetness":0.8,"heal_radius":2});
                    if op == "pixels.replace" {
                        command = super::replacement_tests::command(depth, [0, 0, 9, 7]);
                    }
                    if mask && matches!(op, "clone" | "heal") {
                        command["source"] = json!([8.5, 1.25]);
                        command["source_step"] = json!(
                            original.layers[0]
                                .mask
                                .as_ref()
                                .unwrap()
                                .steps
                                .iter()
                                .find(|s| s.kind == "paint")
                                .unwrap()
                                .id
                        );
                    }
                    let expected =
                        Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
                    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                    let preview =
                        prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
                    compare_pixels(preview.preview(), &expected);
                    let layer = preview.preview().metadata().layer(0).unwrap();
                    assert_eq!(layer.origin(), [0, -1]);
                    assert_eq!([layer.pixels().width(), layer.pixels().height()], [9, 8]);
                    let clip = layer
                        .effects()
                        .find(|e| e.kind() == "liquify")
                        .unwrap()
                        .clips()
                        .next()
                        .unwrap()
                        .1;
                    assert_eq!(
                        preview.preview().source(clip.mask().slot()).unwrap().key(),
                        native.source(old_clip.mask().slot()).unwrap().key()
                    );
                    assert_eq!(
                        preview
                            .preview()
                            .source(clip.mask().retained().unwrap().pixels().slot())
                            .unwrap()
                            .key(),
                        native
                            .source(old_clip.mask().retained().unwrap().pixels().slot())
                            .unwrap()
                            .key()
                    );
                    assert_eq!(
                        clip.origin(),
                        [old_clip.origin()[0] + 3, old_clip.origin()[1]]
                    );
                    let scene = Arc::new(
                        SceneSnapshot::from_native(
                            SceneIdentity {
                                project_id: "expanded-preview".into(),
                                document_id: expected.id.clone(),
                                revision: expected.revision,
                            },
                            preview.preview().clone(),
                            vec![],
                            None,
                            None,
                        )
                        .unwrap(),
                    );
                    let read = runtime.begin_read(&scene).unwrap();
                    let request = crate::render::native_preview::Request::capture(
                        scene, read, None, 9, None, false,
                    )
                    .unwrap();
                    let frame = request.render(&mut || Ok(())).unwrap();
                    assert_eq!(
                        &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                        expected.preview(None, 9, None, false).unwrap().2.as_slice()
                    );
                    drop((frame, request));
                    preview
                        .abort(&session, &|| session.cleanup_check())
                        .unwrap();
                    assert_eq!(runtime.disk().used().unwrap(), disk);
                    assert_eq!(state.history_lengths(), [0, 0]);
                    if op != "gradient" || !mask {
                        continue;
                    }
                    let prepared =
                        prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
                    let (base, history, delta) = prepared.into_parts();
                    let commit = history
                        .prepare_commit(
                            &base,
                            delta,
                            HistoryRequest {
                                actor: "human",
                                label: "Expanded mask",
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
                        compare_pixels(state.native().unwrap(), &expected);
                    }
                }
            }
            drop((native, session, staged.seals, cache, runtime, connection));
        }
    }
}

#[test]
fn expanded_fill_and_editable_source_masks_keep_old_visual_extent_and_originals() {
    for depth in [8, 16] {
        for kind in ["paint", "fill", "source"] {
            let root = Fixture::new();
            let (runtime, connection) =
                root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
            let mut original = document(depth);
            let layer = &mut original.layers[0];
            layer.x = -2;
            layer.y = 2;
            if kind == "fill" {
                layer.kind = "fill".into();
                layer.color = [93, 151, 211, 173];
            } else if kind == "source" {
                let source = crate::source::parse(&json!({"source":{"width":5,"height":4,
                    "content":{"kind":"shape","shape":"rectangle","bounds":[0.,0.,5.,4.],"points":[],"closed":true,"fill":[12347,23459,34571,49153],"stroke":[0,0,0,0],"stroke_width":0.}}})).unwrap();
                layer.pixels = source.render(5, 4, depth).unwrap();
                layer.source = Some(Arc::new(source));
            }
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "expanded-masks".into(),
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
            for op in ["paint", "paint.fill", "shape", "gradient"] {
                for alpha in [0, 193] {
                    let disk = runtime.disk().used().unwrap();
                    let command = json!({"op":op,"layer":"base","mask":true,"rect":[0,0,9,7],"color":[37,83,127,alpha],"points":[[4.,1.],[6.,3.]],"radius":1.2});
                    let expected =
                        Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
                    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                    let prepared =
                        prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
                    compare_pixels(prepared.preview(), &expected);
                    prepared
                        .abort(&session, &|| session.cleanup_check())
                        .unwrap();
                    assert_eq!(runtime.disk().used().unwrap(), disk);
                }
            }
            drop((native, session, staged.seals, cache, runtime, connection));
        }
    }
}

#[test]
fn expanded_failure_and_late_reservations_retire_all_ancillary_planes() {
    let root = Fixture::new();
    let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let original = document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged =
        CapturedScene::from_parts(original, "expanded-refusal".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let valid = json!({"op":"gradient","layer":"base","mask":true,"rect":[0,0,9,7],"color":[37,83,127,193]});
    let failed = prepare(
        capture(
            &state,
            &[valid.clone(), json!({"op":"shape","layer":"missing"})],
        ),
        &session,
        &|| Ok(()),
    )
    .err()
    .unwrap();
    assert!(failed.cleanup_error.is_none(), "{}", failed.reason);
    assert_eq!(runtime.disk().used().unwrap(), disk);
    let prepared = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Expanded mask",
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
        scopes: vec![crate::engine::Scope::layer("base")],
        expires: 100,
    }];
    let refused = state.commit_native(gate(&leases), commit).unwrap_err();
    assert!(refused.reason().contains("Reserved"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    let failed = prepare(capture(&state, &[valid.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > disk + 1024 * 1024 {
            Err("Interrupted expanded planes".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(failed.cleanup_error.is_none());
    let cancelled = capture(&state, &[valid.clone()]);
    cancelled.cancellation().cancel();
    assert!(prepare(cancelled, &session, &|| Ok(())).is_err());
    let captured = capture(&state, &[valid]);
    let pool = runtime.resident();
    let remaining = pool.limit() - pool.used();
    let held = pool.reserve_bytes(remaining, remaining).unwrap();
    assert!(prepare(captured, &session, &|| Ok(())).is_err());
    drop(held);
    assert_eq!(runtime.disk().used().unwrap(), disk);
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
