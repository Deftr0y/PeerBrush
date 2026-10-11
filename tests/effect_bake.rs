use peerbrush::{
    effects,
    engine::{Document, Engine, Layer, Scope},
    psd,
    raster::Raster,
    server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(32, 24, depth).unwrap();
    for y in 0..24 {
        for x in 0..32 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                [
                    10001 + x as u16 * 317,
                    20003 + y as u16 * 271,
                    40007,
                    if (8..12).contains(&x) { 0 } else { 65535 },
                ],
            );
        }
    }
    e
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], Some(e.doc.revision), None, "Bake test")
        .unwrap();
}
fn native(d: &Document) -> Vec<u16> {
    if d.bit_depth == 16 {
        peerbrush::depth16::render(d).unwrap().words
    } else {
        d.preview(None, 32, None, false)
            .unwrap()
            .2
            .into_iter()
            .map(|v| u16::from(v) * 257)
            .collect()
    }
}
fn assert_roundtrip(e: &mut Engine, before: &Document) {
    assert_eq!(e.undo.len(), 1);
    assert_eq!(native(&e.doc), native(before));
    let bytes = psd::encode(&e.doc).unwrap();
    let reopened = psd::decode(&bytes).unwrap();
    assert!(!reopened.read_only);
    assert_eq!(reopened.bit_depth, before.bit_depth);
    assert_eq!(native(&reopened), native(before));
    assert_eq!(
        serde_json::to_value(&reopened.layers[0].effects).unwrap(),
        serde_json::to_value(&e.doc.layers[0].effects).unwrap()
    );
    // Remove supplementary resource sources; retain ordinary layer information,
    // including Lr16 channels, and read the standard raster/mask result.
    let start = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let resources = u32::from_be_bytes(bytes[start..start + 4].try_into().unwrap()) as usize;
    let mut standard = bytes[..start].to_vec();
    standard.extend_from_slice(&0u32.to_be_bytes());
    standard.extend_from_slice(&bytes[start + 4 + resources..]);
    let standard = psd::decode(&standard).unwrap();
    assert!(
        native(&standard) == native(before),
        "Standard native PSD channels differ"
    );
    e.undo("human").unwrap();
    assert_eq!(
        serde_json::to_value(&e.doc.layers).unwrap(),
        serde_json::to_value(&before.layers).unwrap()
    );
    e.redo("human").unwrap();
    assert_eq!(native(&e.doc), native(before));
}
#[test]
fn color_prefix_keeps_suffix_masks_selection_and_native_psd_data() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let id = e.doc.layers[0].id.clone();
        e.edit(
            "human",
            &[
                json!({"op":"mask.add","layer":id,"value":193}),
                json!({"op":"effect.add","layer":id,"kind":"invert","weight":0.3}),
                json!({"op":"effect.add","layer":id,"kind":"blur","settings":{"radius":2.}}),
                json!({"op":"effect.add","layer":id,"kind":"posterize","settings":{"levels":7}}),
                json!({"op":"selection","rect":[3,4,16,20],"feather":1.}),
            ],
            None,
            None,
            "Setup",
        )
        .unwrap();
        e.doc.layers[0].effects[0].enabled = false;
        e.doc.layers[0].opacity = 181. / 255.;
        let before = e.doc.clone();
        e.undo.clear();
        edit(
            &mut e,
            json!({"op":"effect.bake","layer":id,"effect":before.layers[0].effects[1].id}),
        );
        assert_eq!(e.doc.layers[0].effects.len(), 1);
        assert_eq!(
            e.doc.layers[0].effects[0].id,
            before.layers[0].effects[2].id
        );
        assert_eq!(
            e.doc.layers[0].mask.as_ref().unwrap().steps[0].pixels,
            before.layers[0].mask.as_ref().unwrap().steps[0].pixels
        );
        assert_eq!(e.doc.selection_coverage, before.selection_coverage);
        assert!(e.doc.layers[0].pixels.retained.is_none());
        assert_roundtrip(&mut e, &before);
    }
}
#[test]
fn all_style_kinds_bake_expanded_document_frame_without_projection() {
    for depth in [8, 16] {
        for kind in effects::styles::KINDS {
            let mut e = fixture(depth);
            let id = e.doc.layers[0].id.clone();
            e.doc.layers[0].x = 3;
            e.doc.layers[0].y = 4;
            e.doc.layers[0].pixels = Raster::new_depth(16, 16, depth);
            for y in 0..16 {
                for x in 0..16 {
                    e.doc.layers[0]
                        .pixels
                        .set16(x, y, [12345, 23457, 34569, 65535]);
                }
            }
            edit(&mut e, json!({"op":"effect.add","layer":id,"kind":kind}));
            edit(
                &mut e,
                json!({"op":"effect.add","layer":id,"kind":"invert","weight":0.2}),
            );
            let before = e.doc.clone();
            e.undo.clear();
            edit(
                &mut e,
                json!({"op":"effect.bake","layer":id,"effect":before.layers[0].effects[0].id}),
            );
            assert_eq!((e.doc.layers[0].x, e.doc.layers[0].y), (0, 0));
            assert_eq!(
                (e.doc.layers[0].pixels.width, e.doc.layers[0].pixels.height),
                (32, 24)
            );
            assert_roundtrip(&mut e, &before);
        }
    }
}
#[test]
fn mask_prefix_preserves_color_and_remaining_spatial_pipeline() {
    for depth in [8, 16] {
        for spatial in [false, true] {
            let mut e = fixture(depth);
            let id = e.doc.layers[0].id.clone();
            edit(&mut e, json!({"op":"mask.add","layer":id,"value":255}));
            edit(
                &mut e,
                json!({"op":"paint","layer":id,"mask":true,"points":[[16,12]],"radius":6.,"hardness":1.,"color":[0,0,0,255]}),
            );
            edit(
                &mut e,
                json!({"op":"mask.step.add","layer":id,"kind":"invert"}),
            );
            if spatial {
                edit(
                    &mut e,
                    json!({"op":"mask.step.add","layer":id,"kind":"blur","value":2.}),
                );
            }
            let before = e.doc.clone();
            e.undo.clear();
            let at = before.layers[0].mask.as_ref().unwrap().steps.len() - 1;
            edit(
                &mut e,
                json!({"op":"mask.step.bake","layer":id,"step":before.layers[0].mask.as_ref().unwrap().steps[at].id}),
            );
            assert_eq!(e.doc.layers[0].pixels, before.layers[0].pixels);
            let m = e.doc.layers[0].mask.as_ref().unwrap();
            assert_eq!(m.steps.len(), 1);
            assert_eq!(m.steps[0].kind, "paint");
            assert_eq!(m.steps[0].pixels.depth, depth);
            assert_roundtrip(&mut e, &before);
        }
    }
}
#[test]
fn folder_bake_preserves_external_properties_and_restores_hidden_children() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let id = e.doc.layers[0].id.clone();
        let mut child = e.doc.layers[0].clone();
        child.id = peerbrush::engine::id();
        child.parent = Some(id.clone());
        let mut hidden = Layer::new("Hidden source", "paint", 32, 24);
        hidden.parent = Some(id.clone());
        hidden.visible = false;
        e.doc.layers[0].kind = "group".into();
        e.doc.layers[0].pixels = Raster::new_depth(32, 24, depth);
        e.doc.layers.extend([child, hidden]);
        edit(&mut e, json!({"op":"mask.add","layer":id,"value":211}));
        edit(
            &mut e,
            json!({"op":"effect.add","layer":id,"kind":"invert","weight":0.4}),
        );
        e.doc.layers[0].opacity = 211. / 255.;
        let before = e.doc.clone();
        e.undo.clear();
        edit(
            &mut e,
            json!({"op":"effect.bake","layer":id,"effect":before.layers[0].effects[0].id}),
        );
        assert_eq!(e.doc.layers.len(), 1);
        assert_eq!(e.doc.layers[0].kind, "paint");
        assert_roundtrip(&mut e, &before);
    }
}
#[test]
fn bake_guards_are_atomic_and_cover_hidden_folder_children() {
    for depth in [8, 16] {
        for guard in ["lock", "reserve", "stale", "read_only", "unknown", "frame"] {
            let mut e = fixture(depth);
            let id = e.doc.layers[0].id.clone();
            let kind = if guard == "frame" { "stroke" } else { "invert" };
            edit(&mut e, json!({"op":"effect.add","layer":id,"kind":kind}));
            let mut c =
                json!({"op":"effect.bake","layer":id,"effect":e.doc.layers[0].effects[0].id});
            if matches!(guard, "lock" | "reserve") {
                let mut child = Layer::new("Hidden locked child", "paint", 32, 24);
                child.parent = Some(id.clone());
                child.visible = false;
                child.locked = guard == "lock";
                if guard == "reserve" {
                    e.reserve("other", "Reserved", vec![Scope::layer(&child.id)])
                        .unwrap();
                }
                e.doc.layers[0].kind = "group".into();
                e.doc.layers.push(child);
            }
            if guard == "stale" {
                c["source_revision"] = json!(e.doc.revision + 1);
            }
            if guard == "read_only" {
                e.doc.read_only = true;
            }
            if guard == "unknown" {
                c["effect"] = json!("missing");
            }
            if guard == "frame" {
                edit(&mut e, json!({"op":"mask.add","layer":id}));
                e.doc.layers[0].x = 4;
            }
            e.undo.clear();
            let before = serde_json::to_value(&e.doc).unwrap();
            assert!(
                e.edit("human", &[c], None, None, "Rejected bake").is_err(),
                "{guard}"
            );
            assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
            assert!(e.undo.is_empty());
        }
    }
}
#[test]
fn protocol_bake_returns_actual_png_coordinates_and_rejects_stale_revision() {
    let mut e = fixture(16);
    let id = e.doc.layers[0].id.clone();
    edit(
        &mut e,
        json!({"op":"effect.add","layer":id,"kind":"invert"}),
    );
    let c = json!({"op":"effect.bake","layer":id,"effect":e.doc.layers[0].effects[0].id});
    let revision = e.doc.revision;
    let shared = Arc::new(Mutex::new(e));
    let p = json!({"actor":"artist","commands":[c],"expected_revision":revision});
    let r = server::dispatch(&shared, "edit", &p).unwrap();
    assert_eq!(r["images"][0]["document_rect"], json!([0, 0, 32, 24]));
    assert_eq!(r["images"][0]["mime_type"], "image/png");
    assert!(r["images"][0]["data"]
        .as_str()
        .unwrap()
        .starts_with("iVBOR"));
    assert!(server::dispatch(&shared, "edit", &p).is_err());
    let e = shared.lock().unwrap();
    assert_eq!(e.undo.len(), 2);
    assert_eq!(e.doc.bit_depth, 16);
}

#[test]
fn editable_source_bake_is_explicit_and_inactive_prefix_keeps_source() {
    for depth in [8, 16] {
        for enabled in [false, true] {
            let mut e = fixture(depth);
            let source = peerbrush::source::Source {
                width: 32,
                height: 24,
                matrix: [1., 0., 0., 1., 0., 0.],
                content: peerbrush::source::Content::Shape {
                    shape: "rectangle".into(),
                    bounds: [4., 4., 24., 20.],
                    points: vec![],
                    closed: true,
                    fill: [12345, 23457, 34569, 65535],
                    stroke: [0; 4],
                    stroke_width: 0.,
                },
            };
            edit(&mut e, json!({"op":"source.add","source":source}));
            let id = e.doc.layers[0].id.clone();
            edit(
                &mut e,
                json!({"op":"effect.add","layer":id,"kind":"invert"}),
            );
            e.doc.layers[0].effects[0].enabled = enabled;
            let before = e.doc.clone();
            e.undo.clear();
            edit(
                &mut e,
                json!({"op":"effect.bake","layer":id,"effect":before.layers[0].effects[0].id}),
            );
            assert_eq!(e.doc.layers[0].source.is_some(), !enabled);
            assert_roundtrip(&mut e, &before);
        }
    }
}
#[test]
fn fractional_mask_prefix_that_changes_samples_is_refused_atomically() {
    let mut e = fixture(16);
    let id = e.doc.layers[0].id.clone();
    for y in 0..24 {
        for x in 0..32 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                [12345, 23457, 34569, 1000 + x as u16 * 1500 + y as u16 * 600],
            );
        }
    }
    edit(&mut e, json!({"op":"mask.add","layer":id,"value":193}));
    edit(
        &mut e,
        json!({"op":"mask.step.add","layer":id,"kind":"levels","value":2.3,"weight":0.37}),
    );
    edit(
        &mut e,
        json!({"op":"mask.step.add","layer":id,"kind":"blur","value":2.}),
    );
    edit(
        &mut e,
        json!({"op":"mask.step.add","layer":id,"kind":"invert","weight":0.3}),
    );
    let c = json!({"op":"mask.step.bake","layer":id,"step":e.doc.layers[0].mask.as_ref().unwrap().steps[3].id});
    let before = serde_json::to_value(&e.doc).unwrap();
    let count = e.undo.len();
    let error = e
        .edit("human", &[c], None, None, "Prefix bake")
        .unwrap_err();
    assert!(error.contains("native samples"));
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
    assert_eq!(e.undo.len(), count);
    let before = e.doc.clone();
    e.undo.clear();
    let last = e.doc.layers[0]
        .mask
        .as_ref()
        .unwrap()
        .steps
        .last()
        .unwrap()
        .id
        .clone();
    edit(
        &mut e,
        json!({"op":"mask.step.bake","layer":id,"step":last}),
    );
    assert_roundtrip(&mut e, &before);
}
