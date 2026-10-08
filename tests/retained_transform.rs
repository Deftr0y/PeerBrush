use peerbrush::{
    engine::{Document, Engine, Scope},
    layer_clipboard, psd,
    raster::Raster,
};
use serde_json::{json, Value};
fn fixture(depth: u16, mask: bool) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(180, 160, depth).unwrap();
    let l = &mut e.doc.layers[0];
    l.x = 31;
    l.y = 27;
    l.pixels = Raster::new_depth(64, 48, depth);
    for y in 0..48 {
        for x in 0..64 {
            let a = if (x + y) % 11 == 0 { 0 } else { 65535 };
            l.pixels.set16(
                x,
                y,
                [12345 + (x * 347) as u16, 23457 + (y * 283) as u16, 34569, a],
            );
        }
    }
    if mask {
        let id = l.id.clone();
        edit(&mut e, json!({"op":"mask.add","layer":id,"value":255}));
        edit(
            &mut e,
            json!({"op":"paint","layer":id,"mask":true,"points":[[48,44],[60,53]],"radius":8,"color":[0,0,0,255],"opacity":0.71}),
        );
    }
    e.undo.clear();
    e
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], Some(e.doc.revision), None, "Transform")
        .unwrap();
}
fn transform(e: &mut Engine, angle: f64, sx: f64, sy: f64) {
    let id = e.doc.layers[0].id.clone();
    edit(
        e,
        json!({"op":"transform","layer":id,"angle":angle,"scale_x":sx,"scale_y":sy,"pivot":[63.,51.],"selection_only":false}),
    );
}
#[test]
fn repeated_rotation_and_scale_sample_original_native_words_and_masks() {
    for depth in [8, 16] {
        for mask in [false, true] {
            let mut e = fixture(depth, mask);
            let before = e.doc.export_png().unwrap();
            let original = e.doc.layers[0].pixels.clone();
            let before_mask = e.doc.layers[0]
                .mask
                .as_ref()
                .map(|m| m.steps.last().unwrap().pixels.clone());
            for _ in 0..3 {
                transform(&mut e, 31., 1., 1.);
                transform(&mut e, -31., 1., 1.);
                transform(&mut e, 0., 0.1, 0.1);
                transform(&mut e, 0., 10., 10.);
            }
            assert_eq!(
                e.doc.export_png().unwrap(),
                before,
                "depth {depth}, mask {mask}"
            );
            let l = &e.doc.layers[0];
            assert_eq!(l.x, 31);
            assert_eq!(l.y, 27);
            assert_eq!(
                l.pixels.retained.as_ref().unwrap().pixels.as_ref(),
                &original
            );
            for y in 0..48 {
                for x in 0..64 {
                    assert_eq!(l.pixels.get16(x, y), original.get16(x, y));
                }
            }
            if let Some(before_mask) = before_mask {
                let m = l.mask.as_ref().unwrap().steps.last().unwrap();
                assert_eq!(
                    m.pixels.retained.as_ref().unwrap().pixels.as_ref(),
                    &before_mask
                );
                assert_eq!(m.pixels.rgba16(), before_mask.rgba16());
            }
        }
    }
}
#[test]
fn accumulated_affine_matches_single_transform_and_live_preview_then_one_undo() {
    let mut e = fixture(16, true);
    let baseline = e.doc.export_png().unwrap();
    transform(&mut e, 17., 1., 1.);
    transform(&mut e, 23., 1., 1.);
    let repeated = e.doc.export_png().unwrap();
    let mut once = fixture(16, true);
    transform(&mut once, 40., 1., 1.);
    assert_eq!(repeated, once.doc.export_png().unwrap());
    let id = e.doc.layers[0].id.clone();
    let command =
        json!({"op":"transform","layer":id,"angle":-40.,"pivot":[63,51],"selection_only":false});
    let live = Engine::preview_edits(e.doc.clone(), &[command.clone()]).unwrap();
    assert_eq!(live.export_png().unwrap(), baseline);
    let history = e.undo.len();
    edit(&mut e, command);
    assert_eq!(e.undo.len(), history + 1);
    e.undo("human").unwrap();
    assert_eq!(e.doc.export_png().unwrap(), repeated);
    e.redo("human").unwrap();
    assert_eq!(e.doc.export_png().unwrap(), baseline);
}
#[test]
fn painting_rebases_only_changed_source_and_later_transforms_preserve_it() {
    for mask in [false, true] {
        let mut e = fixture(16, true);
        transform(&mut e, 22., 1., 1.);
        let id = e.doc.layers[0].id.clone();
        let old_color = e.doc.layers[0].pixels.retained.clone();
        let old_mask = e.doc.layers[0]
            .mask
            .as_ref()
            .unwrap()
            .steps
            .last()
            .unwrap()
            .pixels
            .retained
            .clone();
        edit(
            &mut e,
            json!({"op":"paint","layer":id,"mask":mask,"points":[[60,51]],"radius":6,"color":[0,100,255,255]}),
        );
        let painted = e.doc.export_png().unwrap();
        let l = &e.doc.layers[0];
        if mask {
            assert_eq!(l.pixels.retained, old_color);
            assert!(l
                .mask
                .as_ref()
                .unwrap()
                .steps
                .last()
                .unwrap()
                .pixels
                .retained
                .is_none());
        } else {
            assert!(l.pixels.retained.is_none());
            assert_eq!(
                l.mask
                    .as_ref()
                    .unwrap()
                    .steps
                    .last()
                    .unwrap()
                    .pixels
                    .retained,
                old_mask
            );
        }
        transform(&mut e, 12., 1., 1.);
        transform(&mut e, -12., 1., 1.);
        assert_eq!(e.doc.export_png().unwrap(), painted);
        e.undo("human").unwrap();
        e.undo("human").unwrap();
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers[0].pixels.retained, old_color);
        assert_eq!(
            e.doc.layers[0]
                .mask
                .as_ref()
                .unwrap()
                .steps
                .last()
                .unwrap()
                .pixels
                .retained,
            old_mask
        );
    }
}
#[test]
fn psd_clipboard_masks_and_effects_keep_originals_for_later_inverse() {
    let mut e = fixture(16, true);
    let id = e.doc.layers[0].id.clone();
    edit(
        &mut e,
        json!({"op":"effect.add","layer":id,"kind":"hsl","settings":{"hue":15.,"saturation":0.,"lightness":0.}}),
    );
    let before = e.doc.export_png().unwrap();
    transform(&mut e, 33., 0.5, 0.5);
    let bytes = psd::encode(&e.doc).unwrap();
    let decoded = psd::decode(&bytes).unwrap();
    assert!(!decoded.read_only);
    assert!(decoded.layers[0].pixels.retained.is_some());
    assert_eq!(e.doc.export_png().unwrap(), decoded.export_png().unwrap());
    e.doc = decoded;
    transform(&mut e, -33., 2., 2.);
    assert!(
        e.doc.export_png().unwrap() == before,
        "PNG16 pixels differ after inverse placement"
    );
    let snapshot = layer_clipboard::copy(&e.doc, &[id]).unwrap();
    let mut pasted = Document::new_depth(180, 160, 16).unwrap();
    layer_clipboard::paste(&mut pasted, &snapshot, "").unwrap();
    assert!(pasted.layers.iter().any(|l| l.pixels.retained.is_some()));
    let copied = pasted
        .layers
        .iter()
        .find(|l| l.pixels.retained.is_some())
        .unwrap();
    assert!(copied
        .mask
        .as_ref()
        .unwrap()
        .steps
        .last()
        .unwrap()
        .pixels
        .retained
        .is_some());
}
#[test]
fn reservations_locks_invalid_nested_sources_and_failed_batches_are_atomic() {
    let mut e = fixture(16, false);
    transform(&mut e, 19., 1., 1.);
    let before = e.doc.export_png().unwrap();
    let retained = e.doc.layers[0].pixels.retained.clone();
    let id = e.doc.layers[0].id.clone();
    let rev = e.doc.revision;
    for c in [
        json!({"op":"transform","layer":id,"scale_x":0}),
        json!({"op":"transform","layer":id,"pivot":[100001,0]}),
        json!({"op":"transform","layer":id,"scale_x":20,"scale_y":20,"pivot":[100000,100000]}),
    ] {
        assert!(e.edit("human", &[c], Some(rev), None, "Invalid").is_err());
        assert_eq!(e.doc.export_png().unwrap(), before);
        assert_eq!(e.doc.layers[0].pixels.retained, retained);
    }
    e.reserve(
        "agent",
        "Retained transform",
        vec![Scope {
            target: Some(id.clone()),
            rect: None,
        }],
    )
    .unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"transform","layer":id,"angle":10})],
            Some(rev),
            None,
            "Reserved"
        )
        .is_err());
    e.leases.clear();
    e.doc.layers[0].locked = true;
    assert!(e
        .edit(
            "human",
            &[json!({"op":"transform","layer":id,"angle":10})],
            Some(rev),
            None,
            "Locked"
        )
        .is_err());
    e.doc.layers[0].locked = false;
    let mut bad = e.doc.clone();
    let r = bad.layers[0].pixels.retained.as_mut().unwrap();
    r.pixels.retained = retained;
    assert!(psd::validate(&bad).unwrap_err().contains("Nested"));
}
#[test]
fn native_depth_conversion_and_geometry_do_not_leave_stale_originals() {
    let mut e = fixture(8, true);
    transform(&mut e, 18., 1., 1.);
    let id = e.doc.layers[0].id.clone();
    edit(&mut e, json!({"op":"document.settings","bit_depth":16}));
    assert_eq!(e.doc.layers[0].pixels.depth, 16);
    if let Some(s) = &e.doc.layers[0].pixels.retained {
        assert_eq!(s.pixels.depth, 16);
    }
    psd::validate(&e.doc).unwrap();
    edit(&mut e, json!({"op":"crop","rect":[10,10,150,140]}));
    let before = e.doc.export_png().unwrap();
    transform(&mut e, 7., 1., 1.);
    transform(&mut e, -7., 1., 1.);
    assert_eq!(e.doc.export_png().unwrap(), before);
    edit(&mut e, json!({"op":"image.resize","width":70,"height":65}));
    assert!(e.doc.layers[0].pixels.retained.is_none());
    assert!(e.doc.layers[0]
        .mask
        .as_ref()
        .unwrap()
        .steps
        .iter()
        .all(|s| s.pixels.retained.is_none()));
    edit(
        &mut e,
        json!({"op":"transform","layer":id,"angle":90.,"selection_only":false}),
    );
    assert!(e.doc.layers[0].pixels.retained.is_some());
}

#[test]
fn standard_psd_keeps_current_raster_appearance_and_versions_supplementary_originals() {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    for depth in [8, 16] {
        let mut e = fixture(depth, false);
        transform(&mut e, 27., 0.5, 0.5);
        let bytes = psd::encode(&e.doc).unwrap();
        let start = bytes.windows(4).position(|w| w == b"PBR1").unwrap() + 4;
        let mut definitions = Vec::new();
        ZlibDecoder::new(&bytes[start..])
            .read_to_end(&mut definitions)
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&definitions).unwrap()["format"],
            9
        );
        let color_len = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
        let offset = 30 + color_len;
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let mut standard = bytes[..offset].to_vec();
        standard.extend_from_slice(&0u32.to_be_bytes());
        standard.extend_from_slice(&bytes[offset + 4 + len..]);
        let decoded = psd::decode(&standard).unwrap();
        assert!(decoded.layers.iter().all(|l| l.pixels.retained.is_none()));
        assert_eq!(
            decoded.layers[0].pixels.rgba16(),
            e.doc.layers[0].pixels.rgba16()
        );
        assert!(decoded.export_png().unwrap() == e.doc.export_png().unwrap());
    }
}

#[test]
fn task_compensation_preserves_later_human_metadata_and_refuses_pixel_source_conflicts() {
    let mut e = fixture(16, true);
    transform(&mut e, 13., 1., 1.);
    let before = e.doc.export_png().unwrap();
    let original = e.doc.layers[0].pixels.retained.clone();
    let id = e.doc.layers[0].id.clone();
    let task = e.reserve("agent", "Transform artwork", vec![]).unwrap().id;
    e.edit(
        "agent",
        &[json!({"op":"transform","layer":id,"angle":19,"pivot":[63,51],"selection_only":false})],
        Some(e.doc.revision),
        Some(&task),
        "Transform artwork",
    )
    .unwrap();
    edit(
        &mut e,
        json!({"op":"layer.update","layer":id,"name":"Human title"}),
    );
    e.undo_task("human", "agent", &task).unwrap();
    assert!(e.doc.export_png().unwrap() == before);
    assert_eq!(e.doc.layers[0].name, "Human title");
    assert_eq!(e.doc.layers[0].pixels.retained, original);
    let next = e.reserve("agent", "Another transform", vec![]).unwrap().id;
    e.edit(
        "agent",
        &[json!({"op":"transform","layer":id,"angle":27,"pivot":[63,51],"selection_only":false})],
        Some(e.doc.revision),
        Some(&next),
        "Another transform",
    )
    .unwrap();
    edit(
        &mut e,
        json!({"op":"paint","layer":id,"points":[[60,51]],"radius":6,"color":[0,100,255,255]}),
    );
    let human = e.doc.export_png().unwrap();
    let rev = e.doc.revision;
    assert!(e.undo_task("human", "agent", &next).is_err());
    assert_eq!(e.doc.revision, rev);
    assert!(e.doc.export_png().unwrap() == human);
}

#[test]
fn empty_large_sources_stay_sparse_and_identical_writes_keep_originals() {
    let mut e = Engine::new();
    e.doc = Document::new_depth(8192, 4096, 16).unwrap();
    let id = e.doc.layers[0].id.clone();
    edit(
        &mut e,
        json!({"op":"transform","layer":id,"scale_x":0.5,"scale_y":0.5,"selection_only":false}),
    );
    assert_eq!(e.doc.layers[0].pixels.stored_bytes(), 0);
    assert!(e.doc.layers[0].pixels.retained.is_some());
    for depth in [8, 16] {
        let mut e = fixture(depth, false);
        transform(&mut e, 23., 1., 1.);
        let r = &mut e.doc.layers[0].pixels;
        let original = r.retained.clone();
        let mut p = r.get16(12, 12);
        r.set16(12, 12, p);
        assert_eq!(r.retained, original);
        p[0] = p[0].wrapping_add(if depth == 16 { 1 } else { 257 });
        r.set16(12, 12, p);
        assert!(r.retained.is_none());
    }
}
