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

pub(super) fn document(depth: u16) -> Document {
    let mut doc = Engine::preview_edits(super::grouping_tests::document(depth),&[
        json!({"op":"selection","kind":"ellipse","rect":[1,1,7,7],"feather":1}),
        json!({"op":"liquify.stroke","layer":"a_base","points":[[2.,2.],[4.,4.]],"radius":2.,"strength":0.4}),
    ]).unwrap();
    let layer = doc.layers.iter_mut().find(|l| l.id == "outside").unwrap();
    layer.x = -3;
    layer.pixels = crate::raster::Raster::new_depth(780, 5, depth);
    layer.pixels.set16(1, 1, [12347, 23459, 34571, 49153]);
    layer.pixels.set16(779, 4, [54321, 32109, 10987, 0]);
    layer.pixels.retained = Some(Box::new(crate::retained::Original::from(&layer.pixels)));
    let source: crate::source::Source = serde_json::from_value(json!({
        "width":8,"height":8,"content":{"kind":"shape","shape":"ellipse",
        "bounds":[1.,1.,7.,7.],"points":[],"closed":true,"fill":[12347,23459,34571,49153],
        "stroke":[0,0,0,0],"stroke_width":0.}
    }))
    .unwrap();
    let vector = doc.layers.iter_mut().find(|l| l.id == "b_child").unwrap();
    vector.pixels = source.render(8, 8, depth).unwrap();
    vector.source = Some(source.into());
    let mask = doc
        .layers
        .iter_mut()
        .find(|l| l.id == "a_base")
        .unwrap()
        .mask
        .as_mut()
        .unwrap();
    mask.enabled = false;
    mask.linked = false;
    let pixels = &mut mask.steps[0].pixels;
    *pixels = crate::raster::Raster::new_depth(9, 5, depth);
    pixels.set16(8, 4, [11111, 22222, 33333, 44444]);
    pixels.retained = Some(Box::new(crate::retained::Original::from(pixels)));
    doc.layers.iter_mut().find(|l| l.id == "A").unwrap().locked = true;
    doc
}

#[test]
fn native_settings_refuse_loss_of_a_disabled_native16_pattern() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
    let encoded =
        STANDARD.encode(crate::raster::png16(1, 1, &[12347, 23459, 34571, 49153]).unwrap());
    let original = Engine::preview_edits(document(16),&[
        json!({"op":"effect.add","layer":"outside","kind":"pattern_overlay","enabled":false,"weight":0.,
            "settings":{"pattern":"bitmap","bitmap_png":encoded}}),
    ]).unwrap();
    let command = json!({"op":"document.settings","bit_depth":8});
    assert!(Engine::preview_edits(original.clone(), &[command.clone()])
        .unwrap_err()
        .contains("16-bit bitmap patterns"));
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged =
        CapturedScene::from_parts(original, "settings-pattern".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let refused = prepare(capture(&state, &[command]), &session, &|| Ok(()))
        .err()
        .unwrap();
    assert!(refused.reason.contains("16-bit bitmap patterns"));
    assert!(refused.cleanup_error.is_none());
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
fn pixels(native: &NativeDocument, expected: &Document) {
    compare(native, expected);
    for (layer, expected) in native.metadata().layers().zip(&expected.layers) {
        compare_raster(native, layer.pixels(), &expected.pixels);
        if let Some(mask) = layer.mask() {
            for (step, expected) in mask.steps().zip(&expected.mask.as_ref().unwrap().steps) {
                compare_raster(native, step.pixels(), &expected.pixels);
            }
        }
    }
}

#[test]
fn native_settings_convert_all_planes_preserve_effect_clips_and_undo_atomically() {
    for depth in [8, 16] {
        for target in [depth, 24 - depth] {
            for paint_after in [false, true] {
                let fixture = Fixture::new();
                let (runtime, connection) =
                    fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
                let original = document(depth);
                let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
                let mut staged = CapturedScene::from_parts(
                    original.clone(),
                    "native-settings".into(),
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
                let mut commands = vec![
                    json!({"op":"document.settings","width":12,"height":11,"bit_depth":target}),
                ];
                if paint_after {
                    commands.push(
                        json!({"op":"shape","layer":"outside","rect":[0,0,2,2],"color":"#765432"}),
                    );
                }
                let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
                expected.revision = original.revision + 1;
                let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                let disk = runtime.disk().used().unwrap();
                let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
                pixels(preview.preview(), &expected);
                assert!(preview.preview().metadata().selection().is_none());
                let old = native
                    .metadata()
                    .layers()
                    .find(|l| l.id() == "outside")
                    .unwrap();
                let changed = preview
                    .preview()
                    .metadata()
                    .layers()
                    .find(|l| l.id() == "outside")
                    .unwrap();
                let old_source = native.source(old.pixels().slot()).unwrap();
                let source = preview.preview().source(changed.pixels().slot()).unwrap();
                assert_eq!(
                    source
                        .physical_tiles()
                        .map(|t| t.coordinate())
                        .collect::<Vec<_>>(),
                    vec![(0, 0), (3, 0)]
                );
                if target == depth && !paint_after {
                    assert_eq!(source.key(), old_source.key());
                }
                if target != depth {
                    assert!(changed.pixels().retained().is_none());
                }
                let input = original
                    .layers
                    .iter()
                    .find(|l| l.id == "outside")
                    .unwrap()
                    .pixels
                    .get16(779, 4);
                let projected = if target == 8 {
                    input.map(|v| u16::from(crate::raster::project16(v)) * 257)
                } else {
                    input
                };
                let region = source
                    .prepare_region(
                        crate::raster::backed::Region::new([779, 4, 780, 5]).unwrap(),
                        &mut || Ok(()),
                    )
                    .unwrap();
                assert_eq!(region.pixel16(779, 4).unwrap(), projected);
                drop(region);
                preview
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
                assert_eq!(runtime.disk().used().unwrap(), disk);
                if !paint_after {
                    let prepared =
                        prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
                    let (base, history, delta) = prepared.into_parts();
                    let commit = history
                        .prepare_commit(
                            &base,
                            delta,
                            HistoryRequest {
                                actor: "human",
                                label: "Document settings",
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
fn global_light_updates_linked_native_effects_without_staging_pixels_or_selection() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut original = document(16);
    original.psd_global_angle = Some(180u32.to_be_bytes());
    for (layer_id, kind, linked) in [("a_base", "inner_shadow", true), ("outside", "drop_shadow", true), ("b_child", "inner_shadow", false)] {
        let mut settings = crate::effects::defaults(kind);
        settings["angle"] = json!(0.);
        if layer_id == "a_base" { settings["size"] = json!(0.); }
        if linked { settings["global_light"] = json!([180, 45.]); }
        let layer = original.layers.iter_mut().find(|layer| layer.id == layer_id).unwrap();
        layer.effects.push(crate::effects::Effect {
            id: crate::engine::id(), kind: kind.into(), enabled: layer_id != "outside", weight: 1., settings: settings.into(),
        });
    }
    let commands = [json!({"op":"document.global_light","angle":45})];
    let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
    expected.revision = original.revision + 1;
    assert_eq!(expected.selection_coverage, original.selection_coverage);
    assert_eq!(expected.psd_global_angle, Some(45u32.to_be_bytes()));
    for layer in &expected.layers {
        let Some(effect) = layer.effects.last() else { continue };
        if effect.settings.get("global_light").is_some() {
            assert_eq!(effect.settings["global_light"], json!([45, 45.]));
            assert_eq!(effect.settings["angle"], -135.);
            assert_eq!(effect.settings["pixel_snap"], layer.id == "a_base");
        } else if effect.kind == "inner_shadow" {
            assert_eq!(effect.settings["angle"], 0.);
        }
    }
    let mut staged = CapturedScene::from_parts(original.clone(), "global-light".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(())).unwrap();
    let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native, 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
    pixels(prepared.preview(), &expected);
    assert_eq!(prepared.scopes().len(), 1);
    assert!(prepared.scopes()[0].target.is_none() && prepared.scopes()[0].rect.is_none());
    assert_eq!(state.history_lengths(), [0, 0]);
    let (base, history, delta) = prepared.into_parts();
    let commit = history.prepare_commit(&base, delta, HistoryRequest {
        actor: "human", label: "Global Light", task: None, gesture: None,
    }, &|| Ok(())).unwrap();
    state.commit_native(gate(&[]), commit).unwrap()
        .retire(&session, &|| session.cleanup_check()).unwrap();
    assert_eq!(state.history_lengths(), [1, 0]);
    assert_eq!(runtime.disk().used().unwrap(), disk);
    for (direction, mut expected) in [(Direction::Undo, original), (Direction::Redo, expected)] {
        let moved = state.native_history().unwrap()
            .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(())).unwrap();
        drop(state.move_native(gate(&[]), moved).unwrap());
        expected.revision = state.view().revision();
        pixels(state.native().unwrap(), &expected);
    }
    drop((state, base, session, staged.seals, cache, runtime, connection));
}

#[test]
fn settings_refusal_cancel_and_late_reservation_preserve_the_whole_document() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
    let original = document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "settings-refusal".into(),
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
    let command = json!({"op":"document.settings","width":12,"bit_depth":8});
    for invalid in [
        json!({"op":"document.settings","width":0}),
        json!({"op":"document.settings","height":u64::MAX}),
        json!({"op":"document.settings","bit_depth":12}),
        json!({"op":"document.settings","rect":[1]}),
        json!({"op":"document.global_light","angle":360}),
        json!({"op":"document.global_light","angle":45,"rect":[0,0,4,4]}),
    ] {
        assert!(Engine::preview_edits(original.clone(), &[invalid.clone()]).is_err());
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
            Err("settings interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(refused.reason.contains("settings interrupted"));
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
                label: "Settings",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let leases = [crate::engine::Lease {
        id: "late".into(),
        owner: "other".into(),
        description: "Layer".into(),
        scopes: vec![crate::engine::Scope::layer("outside")],
        expires: 100,
    }];
    let refused = state.commit_native(gate(&leases), commit).unwrap_err();
    assert!(refused.reason().contains("Reserved"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    let captured = capture(&state, &[command]);
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
