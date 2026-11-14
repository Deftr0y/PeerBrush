use super::*;
use crate::{
    engine::native::worker::{
        self,
        tests::{capture, Fixture},
    },
    engine::{
        authority::DocumentAuthority,
        scene::{CapturedScene, LegacyTileCache},
        Document, Layer, Mask, MaskStep,
    },
    raster::Raster,
};
use serde_json::json;

#[test]
fn large_contiguous_color_mask_budget_covers_every_native_sample() {
    let fixture = Fixture::new();
    let (runtime,connection) = fixture.runtime_with_budgets(128*1024*1024,128*1024*1024);
    let session = runtime.begin().unwrap();
    let native = NativeDocument::new_blank(16000,4200,16,&runtime.resident(),&runtime.disk(),&|| Ok(())).unwrap();
    let guide = Surface::zero(16000,4200,8).unwrap();
    let command = json!({"point":[15999,4199],"contiguous":true,"tolerance":0});
    let parameters = Parameters::new(&command,[0,0],[16000,4200]).unwrap();
    let output = crate::retained::transaction::stage(&native,&session,&|| Ok(()),|store,check| {
        Ok(sample(&guide,&parameters,store.clone(),check)?.mask().clone())
    }).unwrap();
    for [x,y] in [[0,0],[15999,4199],[8192,2048]] {
        let view = output.surface().prepare_region(Region::new([x,y,x+1,y+1]).unwrap(),&mut || Ok(())).unwrap();
        assert_eq!(view.pixel(x,y).unwrap(),[255;4]);
    }
    output.abort(&session,&|| session.cleanup_check()).unwrap();
    assert_eq!(runtime.disk().used().unwrap(),0);
    drop((native,session,runtime,connection));
}

fn document(depth: u16, kind: &str) -> Document {
    let mut doc = Document::new_depth(270, 4, depth).unwrap();
    let layer = &mut doc.layers[0];
    layer.x = -2;
    layer.y = 1;
    layer.opacity = 0.41;
    for y in 0..4 {
        for x in 0..270 {
            let pixel = if x < 40 || (x > 249 && y > 0) {
                [12347, 33219, 49107, 51123]
            } else {
                [51011, 21001, 7157, 65535]
            };
            if depth == 16 {
                layer.pixels.set16(x, y, pixel);
            } else {
                layer
                    .pixels
                    .set(x, y, pixel.map(|v| ((u32::from(v) + 128) / 257) as u8));
            }
        }
    }
    let mut pixels = Raster::new_depth(270, 4, depth);
    for y in 0..4 {
        for x in 0..270 {
            if depth == 16 {
                pixels.set16(x, y, [21719, 21719, 21719, 65535]);
            } else {
                pixels.set(x, y, [83, 83, 83, 255]);
            }
        }
    }
    layer.mask = Some(Mask {
        enabled: false,
        linked: false,
        cache_key: crate::engine::id(),
        steps: vec![MaskStep {
            id: crate::engine::id(),
            kind: "paint".into(),
            enabled: true,
            weight: 1.,
            value: 0.,
            pixels,
            settings: json!(null),
        }],
    });
    match kind {
        "fill" => {
            layer.kind = "fill".into();
            layer.color = [73, 137, 213, 197];
            layer.mask = None;
        }
        "effect" => layer.effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "invert".into(),
            enabled: true,
            weight: 0.71,
            settings: json!({}).into(),
        }),
        "group" => {
            let mut child = Layer::new("Child", "paint", 270, 4);
            child.pixels = layer.pixels.clone();
            child.parent = Some(layer.id.clone());
            child.x = layer.x;
            child.y = layer.y;
            layer.kind = "group".into();
            layer.pixels = Raster::new_depth(270, 4, depth);
            doc.layers.push(child);
        }
        _ => {}
    }
    doc.selection = Some([100, 0, 102, 1]);
    doc
}
fn gate<'a>(leases: &'a [crate::engine::Lease]) -> Publication<'a> {
    Publication {
        actor: "human",
        leases,
        now: 0,
        closed: false,
        write_available: true,
    }
}

#[test]
fn color_masks_match_resident_native_words_stack_semantics_and_atomic_history() {
    for depth in [8, 16] {
        for kind in ["paint", "effect", "fill", "group"] {
            let fixture = Fixture::new();
            let (runtime, connection) = fixture.runtime();
            let original = document(depth, kind);
            let target = original.layers[0].id.clone();
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "color-mask".into(),
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
            for mode in ["replace", "add", "subtract"] {
                for contiguous in [true, false] {
                    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                    // The right patch crosses a tile boundary in the layer frame
                    // and its connected coverage starts away from both axes.
                    let point = if kind == "paint" && contiguous { [264, 3] } else { [0, 2] };
                    let command = json!({"op":"mask.from_color", "layer":target, "point":point, "tolerance":0.12, "contiguous":contiguous, "mode":mode});
                    let expected =
                        crate::engine::Engine::preview_edits(original.clone(), &[command.clone()])
                            .unwrap();
                    let baseline = runtime.disk().used().unwrap();
                    let preview =
                        worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(()))
                            .unwrap();
                    super::super::mask::tests::compare(
                        &native,
                        preview.preview(),
                        &expected,
                        &target,
                    );
                    preview
                        .abort(&session, &|| session.cleanup_check())
                        .unwrap();
                    assert_eq!(runtime.disk().used().unwrap(), baseline);
                    if kind == "paint" && mode == "add" && !contiguous {
                        let prepared =
                            worker::prepare(capture(&state, &[command.clone()]), &session, &|| {
                                Ok(())
                            })
                            .unwrap();
                        let (base, history, delta) = prepared.into_parts();
                        let commit = history
                            .prepare_commit(
                                &base,
                                delta,
                                HistoryRequest {
                                    actor: "human",
                                    label: "Color mask",
                                    task: None,
                                    gesture: None,
                                },
                                &|| Ok(()),
                            )
                            .unwrap();
                        let leases = [crate::engine::Lease {
                            id: "late".into(),
                            owner: "other".into(),
                            description: "Mask".into(),
                            scopes: vec![crate::engine::Scope::layer(&target)],
                            expires: 100,
                        }];
                        let refused = state.commit_native(gate(&leases), commit).unwrap_err();
                        assert!(refused.reason().contains("Reserved"));
                        refused
                            .abort(&session, &|| session.cleanup_check())
                            .unwrap();
                        assert_eq!(runtime.disk().used().unwrap(), baseline);
                        let prepared =
                            worker::prepare(capture(&state, &[command]), &session, &|| Ok(()))
                                .unwrap();
                        let (base, history, delta) = prepared.into_parts();
                        let commit = history
                            .prepare_commit(
                                &base,
                                delta,
                                HistoryRequest {
                                    actor: "human",
                                    label: "Color mask",
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
                        for direction in [Direction::Undo, Direction::Redo] {
                            let moved = state
                                .native_history()
                                .unwrap()
                                .prepare_move(state.native().unwrap(), direction, "human", &|| {
                                    Ok(())
                                })
                                .unwrap();
                            drop(state.move_native(gate(&[]), moved).unwrap());
                            super::super::mask::tests::compare(
                                &native,
                                state.native().unwrap(),
                                if matches!(direction, Direction::Undo) {
                                    &original
                                } else {
                                    &expected
                                },
                                &target,
                            );
                        }
                    }
                }
            }
            drop((native, staged.seals, session, cache, runtime, connection));
        }
    }
}

#[test]
fn color_mask_refusals_retire_sampled_sources_and_replacement_accepts_a_full_stack() {
    use std::cell::Cell;
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let mut original = document(16, "paint");
    let target = original.layers[0].id.clone();
    let mut template = original.layers[0].mask.as_ref().unwrap().steps[0].clone();
    template.kind = "fill".into();
    template.value = 0.37;
    template.pixels = Raster::new_depth(1, 1, 16);
    original.layers[0].mask.as_mut().unwrap().steps = (0..32)
        .map(|_| {
            let mut step = template.clone();
            step.id = crate::engine::id();
            step
        })
        .collect();
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "color-mask-refusal".into(),
        vec![],
        None,
        None,
    )
    .stage(&runtime, &cache, &|| Ok(()))
    .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let command = json!({"op":"mask.from_color", "layer":target, "point":[0,2]});
    let baseline = runtime.disk().used().unwrap();
    let expected =
        crate::engine::Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
    let peak = Cell::new(baseline);
    let sampled_seal = Cell::new(None);
    let preview = worker::prepare(capture(&state, &[command.clone()]), &session, &|| {
        let disk = runtime.disk().used().unwrap();
        if sampled_seal.get().is_none() {
            if disk < peak.get() {
                // The guide retires after the sampled coverage is sealed.
                sampled_seal.set(Some(peak.get()));
            } else {
                peak.set(disk);
            }
        }
        Ok(())
    })
    .unwrap();
    super::super::mask::tests::compare(&native, preview.preview(), &expected, &target);
    preview
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    let sealed_bytes = sampled_seal.get().unwrap();
    assert!(sealed_bytes > baseline);
    let refused = worker::prepare(capture(&state, &[command.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() >= sealed_bytes {
            Err("Color coverage seal interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(refused.reason.contains("Color coverage seal interrupted"));
    assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    for patch in [
        json!({"mode":"add"}),
        json!({"mode":"intersect"}),
        json!({"point":[i64::MAX,2]}),
        json!({"point":[i64::MIN,2]}),
        json!({"tolerance":-1}),
    ] {
        let mut invalid = command.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let failure = worker::prepare(capture(&state, &[invalid]), &session, &|| Ok(()))
            .err()
            .unwrap();
        assert!(failure.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
    let interrupted = worker::prepare(capture(&state, &[command.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > baseline {
            Err("Color mask interrupted after staging".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(interrupted.reason.contains("interrupted after staging"));
    assert!(interrupted.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    let failed = worker::prepare(
        capture(
            &state,
            &[
                command,
                json!({"op":"mask.from_color", "layer":target, "point":[999,2]}),
            ],
        ),
        &session,
        &|| Ok(()),
    )
    .err()
    .unwrap();
    assert!(failed.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
    drop((
        state,
        native,
        staged.seals,
        session,
        cache,
        runtime,
        connection,
    ));
}
