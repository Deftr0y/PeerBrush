use peerbrush::{
    depth16, effects,
    engine::{Document, Engine, Scope},
    layer_clipboard, preview, psd, server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(19, 13, depth).unwrap();
    for y in 0..13 {
        for x in 0..19 {
            if depth == 16 {
                e.doc.layers[0].pixels.set16(
                    x,
                    y,
                    [12347 + x as u16 * 173, 33459 + y as u16 * 211, 51237, 42199],
                );
            } else {
                e.doc.layers[0]
                    .pixels
                    .set(x, y, [45 + x as u8, 95 + y as u8, 180, 164]);
            }
        }
    }
    e
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], None, None, "Effect").unwrap();
}
fn native(doc: &Document) -> Vec<u16> {
    if doc.bit_depth == 16 {
        depth16::render(doc).unwrap().words
    } else {
        doc.preview(None, 19, None, false)
            .unwrap()
            .2
            .into_iter()
            .map(u16::from)
            .collect()
    }
}
fn strip_resources(bytes: &[u8]) -> Vec<u8> {
    let pos = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
    [
        bytes[..pos].to_vec(),
        vec![0; 4],
        bytes[pos + 4 + len..].to_vec(),
    ]
    .concat()
}

#[test]
fn weight_endpoints_partial_native_samples_and_top_last_order() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let raw = native(&e.doc);
        let layer = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            json!({"op":"effect.add","layer":layer,"kind":"invert","weight":0}),
        );
        let effect = e.doc.layers[0].effects[0].id.clone();
        assert_eq!(native(&e.doc), raw);
        edit(
            &mut e,
            json!({"op":"effect.update","layer":layer,"effect":effect,"weight":1}),
        );
        let full = native(&e.doc);
        let max = if depth == 16 { 65535 } else { 255 };
        assert_eq!(full[0], max - raw[0]);
        edit(
            &mut e,
            json!({"op":"effect.update","layer":layer,"effect":effect,"weight":0.25}),
        );
        let partial = native(&e.doc);
        for ((old, applied), weighted) in raw
            .chunks_exact(4)
            .zip(full.chunks_exact(4))
            .zip(partial.chunks_exact(4))
        {
            for c in 0..3 {
                assert_eq!(
                    weighted[c],
                    (old[c] as f64 * 0.75 + applied[c] as f64 * 0.25).round() as u16
                );
            }
            assert_eq!(weighted[3], old[3]);
        }
        assert_eq!(
            e.doc.layers[0].pixels.rgba16(),
            fixture(depth).doc.layers[0].pixels.rgba16()
        );
        edit(
            &mut e,
            json!({"op":"effect.add","layer":layer,"kind":"invert"}),
        );
        assert_eq!(native(&e.doc)[0], max - partial[0]);
        if depth == 16 {
            assert!(partial.iter().any(|word| word % 257 != 0));
        }
    }
}

#[test]
fn spatial_weights_keep_faint_colors_and_ignore_hidden_transparent_rgb() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let layer = e.doc.layers[0].id.clone();
        e.doc.layers[0].pixels = peerbrush::raster::Raster::new_depth(19, 13, depth);
        for y in 0..13 {
            for x in 0..19 {
                if depth == 16 {
                    e.doc.layers[0].pixels.set16(x, y, [65535, 0, 65535, 0]);
                } else {
                    e.doc.layers[0].pixels.set(x, y, [255, 0, 255, 0]);
                }
            }
        }
        if depth == 16 {
            e.doc.layers[0].pixels.set16(9, 6, [0, 51237, 0, 42199]);
        } else {
            e.doc.layers[0].pixels.set(9, 6, [0, 201, 0, 164]);
        }
        edit(
            &mut e,
            json!({"op":"effect.add","layer":layer,"kind":"blur","settings":{"radius":1},"weight":1}),
        );
        let full = native(&e.doc);
        let effect = e.doc.layers[0].effects[0].id.clone();
        edit(
            &mut e,
            json!({"op":"effect.update","layer":layer,"effect":effect,"weight":0.5}),
        );
        let partial = native(&e.doc);
        let at = (6 * 19 + 8) * 4;
        assert!(partial[at + 3] > 0);
        assert_eq!(partial[at], 0);
        assert_eq!(partial[at + 2], 0);
        assert_eq!(partial[at + 1], full[at + 1]);
        assert_eq!(partial[at + 3], (full[at + 3] as f64 * 0.5).round() as u16);
    }
    assert_eq!(
        effects::weighted_pixel([12347, 30123, 49999, 0], [54321, 25001, 10003, 0], 0.5),
        [33334, 27562, 30001, 0]
    );
}

#[test]
fn weighted_masks_and_regional_strokes_match_full_render_at_both_depths() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let layer = e.doc.layers[0].id.clone();
        edit(&mut e, json!({"op":"mask.add","layer":layer}));
        let fill = e.doc.layers[0].mask.as_ref().unwrap().steps[0].id.clone();
        edit(
            &mut e,
            json!({"op":"mask.step.update","layer":layer,"step":fill,"value":0,"weight":0.25}),
        );
        assert!((e.doc.layers[0].mask_value_raw(3, 4) - 0.75).abs() < 1e-6);
        edit(
            &mut e,
            json!({"op":"mask.step.add","layer":layer,"kind":"invert","weight":0.5}),
        );
        assert!((e.doc.layers[0].mask_value_raw(3, 4) - 0.5).abs() < 1e-6);
        edit(
            &mut e,
            json!({"op":"mask.step.add","layer":layer,"kind":"gaussian","settings":{"radius":1},"weight":0.5}),
        );
        edit(
            &mut e,
            json!({"op":"effect.add","layer":layer,"kind":"blur","settings":{"radius":1},"weight":0.4}),
        );
        let mut cache = preview::Cache::default();
        for end in [6., 9., 12.] {
            let draft=Engine::preview_edits(e.doc.clone(),&[json!({"op":"paint","layer":layer,"points":[[4,4],[end,6]],"radius":2,"color":[230,70,10,255]})]).unwrap();
            let rendered = cache
                .render(
                    &draft,
                    "weighted-stroke",
                    Some([1, 1, 16, 10]),
                    19,
                    None,
                    false,
                )
                .unwrap();
            assert_eq!(
                rendered.bytes,
                draft.preview(None, 19, None, false).unwrap().2,
                "depth {depth}"
            );
        }
    }
}

#[test]
fn shared_protocol_validates_weights_atomically_and_enforces_reservations() {
    let e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    let shared = Arc::new(Mutex::new(e));
    for weight in [
        json!(-0.1),
        json!(1.1),
        json!(null),
        json!("half"),
        json!([]),
    ] {
        let error=server::dispatch(&shared,"edit",&json!({"actor":"human","commands":[{"op":"mask.add","layer":layer},{"op":"effect.add","layer":layer,"kind":"invert","weight":weight}]})).unwrap_err();
        assert!(error.contains("weight"));
        let e = shared.lock().unwrap();
        assert!(e.doc.layers[0].mask.is_none());
        assert!(e.undo.is_empty());
        assert_eq!(e.doc.revision, 0);
    }
    {
        let mut e = shared.lock().unwrap();
        e.reserve("agent", "Working", vec![Scope::layer(&layer)])
            .unwrap();
    }
    assert!(server::dispatch(&shared,"edit",&json!({"actor":"human","commands":[{"op":"effect.add","layer":layer,"kind":"invert","weight":0.5}]})).unwrap_err().contains("Reserved"));
    let mut e = shared.lock().unwrap();
    e.leases.clear();
    edit(
        &mut e,
        json!({"op":"effect.add","layer":layer,"kind":"invert","weight":0.5}),
    );
    let effect = e.doc.layers[0].effects[0].id.clone();
    let before = native(&e.doc);
    let draft = Engine::preview_edits(
        e.doc.clone(),
        &[json!({"op":"effect.update","layer":layer,"effect":effect,"weight":0.2})],
    )
    .unwrap();
    assert_ne!(native(&draft), before);
    assert_eq!(e.undo.len(), 1);
    assert_eq!(e.doc.revision, 1);
    edit(
        &mut e,
        json!({"op":"effect.update","layer":layer,"effect":effect,"weight":0.2}),
    );
    assert_eq!(native(&e.doc), native(&draft));
    e.undo("human").unwrap();
    assert_eq!(native(&e.doc), before);
    e.redo("human").unwrap();
    assert_eq!(native(&e.doc), native(&draft));
}

#[test]
fn weights_survive_clipboard_psd_and_standard_pixels_are_current() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let layer = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            json!({"op":"effect.add","layer":layer,"kind":"invert","weight":0.25}),
        );
        edit(&mut e, json!({"op":"mask.add","layer":layer}));
        let fill = e.doc.layers[0].mask.as_ref().unwrap().steps[0].id.clone();
        edit(
            &mut e,
            json!({"op":"mask.step.update","layer":layer,"step":fill,"value":0,"weight":0.25}),
        );
        let expected = native(&e.doc);
        let source = e.doc.layers[0].pixels.rgba16();
        let snapshot = layer_clipboard::copy(&e.doc, &[layer.clone()]).unwrap();
        let bytes = psd::encode(&e.doc).unwrap();
        let reopened = psd::decode(&bytes).unwrap();
        assert!(!reopened.read_only);
        assert_eq!(native(&reopened), expected);
        assert_eq!(reopened.layers[0].pixels.rgba16(), source);
        assert_eq!(reopened.layers[0].effects[0].weight, 0.25);
        assert_eq!(
            reopened.layers[0].mask.as_ref().unwrap().steps[0].weight,
            0.25
        );
        let standard = psd::decode(&strip_resources(&bytes)).unwrap();
        assert!(!standard.read_only);
        assert_eq!(native(&standard), expected);
        let mut target = fixture(depth);
        let target_layer = target.doc.layers[0].id.clone();
        let result = target
            .paste_layers("human", &snapshot, &target_layer, None, None)
            .unwrap();
        let pasted_id = result["created_roots"][0].as_str().unwrap();
        let pasted = target
            .doc
            .layers
            .iter()
            .find(|l| l.id == pasted_id)
            .unwrap();
        assert_eq!(pasted.effects[0].weight, 0.25);
        assert_eq!(pasted.mask.as_ref().unwrap().steps[0].weight, 0.25);
        assert_eq!(pasted.pixels.rgba16(), source);
    }
}

#[test]
fn older_effect_and_mask_sources_default_to_full_weight() {
    let effect: effects::Effect =
        serde_json::from_value(json!({"id":"old","kind":"invert","enabled":true,"settings":{}}))
            .unwrap();
    assert_eq!(effect.weight, 1.0);
    let mut e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    edit(&mut e, json!({"op":"mask.add","layer":layer}));
    let mut value = serde_json::to_value(&e.doc.layers[0].mask.as_ref().unwrap().steps[0]).unwrap();
    value.as_object_mut().unwrap().remove("weight");
    let step: peerbrush::engine::MaskStep = serde_json::from_value(value).unwrap();
    assert_eq!(step.weight, 1.0);
    assert!(effects::validate_weight(f32::NAN).is_err());
    assert!(effects::validate_weight(f32::INFINITY).is_err());
}

#[test]
fn selective_task_undo_restores_weights_and_preserves_later_human_settings() {
    let mut e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    edit(
        &mut e,
        json!({"op":"effect.add","layer":layer,"kind":"levels"}),
    );
    edit(&mut e, json!({"op":"mask.add","layer":layer}));
    let effect = e.doc.layers[0].effects[0].id.clone();
    let step = e.doc.layers[0].mask.as_ref().unwrap().steps[0].id.clone();
    let task = e.reserve("agent", "Weight", vec![]).unwrap().id;
    e.edit(
        "agent",
        &[
            json!({"op":"effect.update","layer":layer,"effect":effect,"weight":0.25}),
            json!({"op":"mask.step.update","layer":layer,"step":step,"weight":0.5}),
        ],
        None,
        Some(&task),
        "Weights",
    )
    .unwrap();
    edit(
        &mut e,
        json!({"op":"effect.update","layer":layer,"effect":effect,"settings":{"gamma":1.3}}),
    );
    edit(
        &mut e,
        json!({"op":"mask.step.update","layer":layer,"step":step,"value":100}),
    );
    e.undo_task("human", "agent", &task).unwrap();
    assert_eq!(e.doc.layers[0].effects[0].weight, 1.0);
    assert_eq!(e.doc.layers[0].effects[0].settings["gamma"], json!(1.3));
    let mask = e.doc.layers[0].mask.as_ref().unwrap();
    assert_eq!(mask.steps[0].weight, 1.0);
    assert_eq!(mask.steps[0].value, 100.);
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers[0].effects[0].weight, 0.25);
    assert_eq!(e.doc.layers[0].mask.as_ref().unwrap().steps[0].weight, 0.5);
}
