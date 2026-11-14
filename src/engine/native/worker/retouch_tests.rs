use super::*;
use super::{
    grouping_tests::gate,
    tests::{capture, exercise_edits as exercise, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use serde_json::{json, Value};

fn command(op: &str, layer: &str, mask: bool) -> Value {
    json!({"op":op,"layer":layer,"mask":mask,"sample_merged":!mask,
        "points":[[1.25,2.5,0.7],[2.25,3.25,0.9],[4.25,4.5,0.6]],
        "source":[4.5,1.25],"radius":1.8,"spacing":0.8,"hardness":0.4,
        "flow":0.6,"opacity":0.7,"heal_radius":2})
}

#[test]
fn retouch_merged_and_composed_masks_match_original_depth_and_frame_expansion() {
    for depth in [8, 16] {
        for op in ["clone", "heal"] {
            for mask in [false, true] {
                let mut original = super::frame_tests::document(depth);
                if depth == 8 {
                    original.icc_profile = Some(std::sync::Arc::new(
                        moxcms::ColorProfile::new_srgb().encode().unwrap(),
                    ));
                }
                let cmd = command(op, "base", mask);
                for enabled in [false, true] {
                    original.layers[0].mask.as_mut().unwrap().enabled = enabled;
                    exercise(original.clone(), &[cmd.clone()], true);
                }
                // Batch step two must sample the first step's immutable result.
                exercise(original.clone(), &[cmd.clone(), cmd], true);
                let mut selected = Engine::preview_edits(
                    original,
                    &[json!({"op":"selection","kind":"ellipse","rect":[0,0,9,7],"feather":1})],
                )
                .unwrap();
                selected.selection_previous = selected.selection_coverage.clone();
                exercise(selected, &[command(op, "base", mask)], true);

                let mut nested = super::canvas_tests::document(depth);
                nested = Engine::preview_edits(
                    nested,
                    &[json!({"op":"effect.add","layer":"outside","kind":"drop_shadow"})],
                )
                .unwrap();
                let mut cmd = command(op, if mask { "a_base" } else { "outside" }, mask);
                cmd["source"] = json!([-0.25, 2.5]);
                exercise(nested.clone(), &[cmd.clone()], true);
                if mask {
                    // Foreign, hidden and locked masks are readable snapshots.
                    let mut source = nested
                        .layers
                        .iter()
                        .find(|l| l.id == "a_base")
                        .unwrap()
                        .clone();
                    source.id = "source".into();
                    source.parent = None;
                    source.clip_to = None;
                    source.visible = false;
                    source.locked = true;
                    source.x = -2;
                    nested.layers.push(source);
                    cmd["source_layer"] = json!("source");
                    exercise(nested, &[cmd], true);
                }
            }
        }
    }
}

#[test]
fn merged_retouch_samples_fractional_canvas_edges_and_tile_boundaries() {
    for depth in [8, 16] {
        let mut original = Document::new_depth(270, 9, depth).unwrap();
        original.layers[0].id = "base".into();
        for y in 0..9 {
            for x in 0..270 {
                original.layers[0].pixels.set16(
                    x,
                    y,
                    [
                        10001 + x as u16 * 113,
                        20003 + y as u16 * 717,
                        40007,
                        if x % 13 == 0 { 0 } else { 49153 },
                    ],
                );
            }
        }
        if depth == 8 {
            original.icc_profile = Some(std::sync::Arc::new(
                moxcms::ColorProfile::new_srgb().encode().unwrap(),
            ));
        }
        original = Engine::preview_edits(
            original,
            &[
                json!({"op":"mask.add","layer":"base","value":193}),
                json!({"op":"mask.step.add","layer":"base","kind":"invert","weight":0.43}),
                json!({"op":"effect.add","layer":"base","kind":"invert","weight":0.37}),
                json!({"op":"filter.add","kind":"blur","settings":{"radius":1.}}),
            ],
        )
        .unwrap();
        for op in ["clone", "heal"] {
            for source in [[-0.25, 1.5], [268.5, 7.75], [129.25, 4.5]] {
                let mut cmd = command(op, "base", false);
                cmd["points"] = json!([[254.75, 3.5], [258.25, 4.5]]);
                cmd["source"] = json!(source);
                exercise(original.clone(), &[cmd], true);
            }
        }
        if depth == 8 {
            original.icc_profile = None;
            let mut cmd = command("clone", "base", false);
            cmd["points"] = json!([[254.75, 3.5], [258.25, 4.5]]);
            cmd["source"] = json!([129.25, 4.5]);
            exercise(original, &[cmd], true);
        }
    }
}

#[test]
fn retouch_refusals_and_cancellation_retire_rendered_and_expanded_sources() {
    let original = super::frame_tests::document(16);
    for value in [
        json!({"sample_merged":true,"mask":true}),
        json!({"sample_merged":"yes"}),
        json!({"source_step":"missing"}),
        json!({"sample_merged":false,"mask":true,"source_layer":"missing"}),
    ] {
        let mut cmd = command("clone", "base", false);
        cmd.as_object_mut()
            .unwrap()
            .extend(value.as_object().unwrap().clone());
        exercise(original.clone(), &[cmd], false);
    }
    for mask in [false, true] {
        let cmd = command("clone", "base", mask);
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(512 * 1024 * 1024, 256 * 1024 * 1024);
        let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
        let mut staged = CapturedScene::from_parts(
            original.clone(),
            "retouch-cancel".into(),
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
        let baseline = runtime.disk().used().unwrap();
        let peak = std::cell::Cell::new(baseline);
        let preview = prepare(capture(&state, &[cmd.clone()]), &session, &|| {
            peak.set(peak.get().max(runtime.disk().used().unwrap()));
            Ok(())
        })
        .unwrap();
        preview
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert!(peak.get() > baseline);
        for threshold in [baseline + 1, peak.get()] {
            let refused = prepare(capture(&state, &[cmd.clone()]), &session, &|| {
                if runtime.disk().used().unwrap() >= threshold {
                    Err("Retouch interrupted".into())
                } else {
                    Ok(())
                }
            })
            .err()
            .unwrap();
            assert!(
                refused.reason.contains("Retouch interrupted"),
                "{}",
                refused.reason
            );
            assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            assert_eq!(state.native().unwrap().key(), native.key());
        }
        let previous = std::cell::Cell::new(baseline);
        let retired = std::cell::Cell::new(false);
        let refused = prepare(capture(&state, &[cmd.clone()]), &session, &|| {
            let used = runtime.disk().used().unwrap();
            if used < previous.replace(used) {
                retired.set(true);
            }
            if retired.get() {
                Err("Retouch interrupted after source retirement".into())
            } else {
                Ok(())
            }
        })
        .err()
        .expect("Source retirement must precede publication");
        assert!(
            refused
                .reason
                .contains("Retouch interrupted after source retirement"),
            "{}",
            refused.reason
        );
        assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        let prepared = prepare(capture(&state, &[cmd]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Retouch",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        let leases = [crate::engine::Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "Layer edit".into(),
            scopes: vec![crate::engine::Scope::layer("base")],
            expires: 100,
        }];
        let refused = state.commit_native(gate(&leases), commit).unwrap_err();
        assert!(refused.reason().contains("Reserved"));
        refused
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
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
    let cmd = command("clone", "base", true);
    let mut invalid = cmd.clone();
    invalid["source_layer"] = json!("missing");
    exercise(original, &[cmd, invalid], false);
    let mut large = Document::new_depth(4096, 4096, 16).unwrap();
    large.layers[0].id = "base".into();
    let error = Engine::preview_edits(large, &[command("clone", "base", false)])
        .err()
        .unwrap();
    assert!(error.contains("limited to 16 megapixels"));
}

#[test]
fn merged_clone_samples_the_native_canvas_corner_without_expanding_the_source() {
    use crate::raster::{backed::Region, Raster, NATIVE_EDGE};

    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let mut original = Document::new_depth(2, 2, depth).unwrap();
        original.width = NATIVE_EDGE;
        original.height = NATIVE_EDGE;
        original.layers[0].id = "base".into();
        let mut pixels = Raster::new_depth(NATIVE_EDGE, NATIVE_EDGE, depth);
        pixels.set16(15996, 15996, [12347, 23459, 34571, 65535]);
        let expected = pixels.get16(15996, 15996);
        original.layers[0].pixels = pixels;
        let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
        let mut staged =
            CapturedScene::from_parts(original, "large-merged-clone".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
            .unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let baseline = runtime.disk().used().unwrap();
        let clone = json!({"op":"clone","layer":"base","sample_merged":true,
            "points":[[260.5,260.5]],"source":[15996.5,15996.5],"radius":2.,
            "flow":1.,"opacity":1.,"hardness":1.});
        let prepared = prepare(capture(&state, &[clone]), &session, &|| Ok(())).unwrap();
        {
            let candidate = prepared.preview();
            let raster = candidate.metadata().layer(0).unwrap().pixels();
            let source = candidate.source(raster.slot()).unwrap();
            assert_eq!(source.physical_tiles().count(), 2);
            for (x, y) in [(260, 260), (15996, 15996)] {
                let area = Region::new([x, y, x + 1, y + 1]).unwrap();
                let sample = source.prepare_region(area, &mut || Ok(())).unwrap();
                assert_eq!(sample.pixel16(x, y).unwrap(), expected);
            }
        }
        prepared.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
    }
}
