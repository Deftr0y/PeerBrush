use peerbrush::{
    engine::{Document, Engine, Scope},
    psd,
    raster::Raster,
};
use serde_json::{json, Value};
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(32, 24, depth).unwrap();
    for y in 0..24 {
        for x in 0..32 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                [123 + x as u16 * 37, 271 + y as u16 * 113, 539, 65535],
            );
        }
    }
    e
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], Some(e.doc.revision), None, "Geometry")
        .unwrap();
}
#[test]
fn crop_preserves_off_canvas_sources_native_words_preview_undo_and_psd() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let before = e.doc.clone();
        let c = json!({"op":"crop","rect":[8,5,25,20]});
        let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
        edit(&mut e, c);
        assert_eq!((e.doc.width, e.doc.height), (17, 15));
        assert_eq!((e.doc.layers[0].x, e.doc.layers[0].y), (-8, -5));
        assert_eq!(e.doc.layers[0].pixels, before.layers[0].pixels);
        assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
        let image = e.doc.preview(None, 17, None, false).unwrap();
        assert_eq!(&image.2[..4], &before.layers[0].pixels.get(8, 5));
        let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        assert!(!decoded.read_only);
        assert_eq!(decoded.layers[0].pixels, before.layers[0].pixels);
        assert_eq!(decoded.export_png().unwrap(), e.doc.export_png().unwrap());
        assert_eq!(e.undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
    }
}
#[test]
fn all_canvas_anchors_move_frames_without_resampling_and_reveal_retained_pixels() {
    for (anchor, offset) in [
        ("top_left", [0, 0]),
        ("top", [4, 0]),
        ("top_right", [8, 0]),
        ("left", [0, 4]),
        ("center", [4, 4]),
        ("right", [8, 4]),
        ("bottom_left", [0, 8]),
        ("bottom", [4, 8]),
        ("bottom_right", [8, 8]),
    ] {
        let mut e = fixture(16);
        let before = e.doc.clone();
        edit(
            &mut e,
            json!({"op":"canvas.resize","width":40,"height":32,"anchor":anchor}),
        );
        assert_eq!([e.doc.layers[0].x, e.doc.layers[0].y], offset);
        assert_eq!(e.doc.layers[0].pixels, before.layers[0].pixels);
        let image = peerbrush::depth16::render(&e.doc).unwrap();
        assert_eq!(image.get(offset[0], offset[1]), [123, 271, 539, 65535]);
        e.undo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
    }
    let mut e = fixture(16);
    let before = e.doc.export_png().unwrap();
    edit(&mut e, json!({"op":"crop","rect":[8,5,25,20]}));
    edit(&mut e, json!({"op":"crop","rect":[-8,-5,24,19]}));
    assert_eq!(e.doc.export_png().unwrap(), before);
}
#[test]
fn resize_scales_hidden_layers_native_masks_spatial_effects_and_liquify_snapshots() {
    let mut e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    edit(&mut e, json!({"op":"mask.add","layer":layer,"value":255}));
    e.doc.layers[0]
        .mask
        .as_mut()
        .unwrap()
        .steps
        .last_mut()
        .unwrap()
        .pixels
        .set16(5, 5, [123, 123, 123, 65535]);
    edit(
        &mut e,
        json!({"op":"mask.step.add","layer":layer,"kind":"blur","value":1}),
    );
    edit(
        &mut e,
        json!({"op":"effect.add","layer":layer,"kind":"blur","settings":{"radius":1}}),
    );
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","rect":[4,4,16,16],"feather":2}),
    );
    edit(
        &mut e,
        json!({"op":"liquify.stroke","layer":layer,"points":[[10,10],[12,10]],"radius":3,"strength":0.2}),
    );
    let mut hidden = peerbrush::engine::Layer::new("Hidden", "paint", 4, 4);
    hidden.pixels = Raster::new_depth(4, 4, 16);
    hidden.pixels.set16(1, 1, [30123, 17271, 8539, 65535]);
    hidden.x = 20;
    hidden.y = 15;
    hidden.visible = false;
    e.doc.layers.push(hidden);
    let before = e.doc.clone();
    e.undo.clear();
    let c = json!({"op":"image.resize","width":64,"height":48});
    let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
    edit(&mut e, c);
    assert_eq!(e.doc.bit_depth, 16);
    assert_eq!((e.doc.width, e.doc.height), (64, 48));
    assert_eq!(e.doc.layers[0].pixels.get16(0, 0), [123, 271, 539, 65535]);
    assert_eq!(e.doc.layers[0].effects[0].settings["radius"], 2.);
    let liq = &e.doc.layers[0].effects[1].settings;
    assert_eq!(liq["strokes"][0]["points"], json!([[20., 20.], [24., 20.]]));
    assert_eq!(liq["strokes"][0]["radius"], 6.);
    let mask = e.doc.layers[0].mask.as_ref().unwrap();
    assert_eq!(mask.steps.last().unwrap().value, 2.);
    assert!(mask.steps.iter().all(|s| s.pixels.depth == 16));
    assert_eq!([e.doc.layers[1].x, e.doc.layers[1].y], [40, 30]);
    assert!(!e.doc.layers[1].visible);
    assert!(e.doc.selection.is_none());
    assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
    let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert!(!decoded.read_only);
    assert_eq!(decoded.export_png().unwrap(), e.doc.export_png().unwrap());
    assert_eq!(e.undo.len(), 1);
    e.undo("human").unwrap();
    assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
}
#[test]
fn nonproportional_resize_without_spatial_effects_keeps_native_edges_and_subbyte_interpolation() {
    let mut e = fixture(16);
    let before = e.doc.clone();
    edit(&mut e, json!({"op":"image.resize","width":64,"height":24}));
    assert_eq!(e.doc.layers[0].pixels.get16(0, 0), [123, 271, 539, 65535]);
    assert_eq!(
        e.doc.layers[0].pixels.get16(20, 7),
        before.layers[0].pixels.sample16(9.75, 7.)
    );
    assert_eq!(
        e.doc.layers[0].pixels.get16(63, 23),
        before.layers[0].pixels.get16(31, 23)
    );
}
#[test]
fn invalid_locked_reserved_and_stale_geometry_is_atomic() {
    let mut e = fixture(16);
    let before = e.doc.export_png().unwrap();
    for c in [
        json!({"op":"crop","rect":[i64::MIN,0,10,10]}),
        json!({"op":"crop","rect":[10,0,1,10]}),
        json!({"op":"canvas.resize","width":40,"height":32,"anchor":"unknown"}),
        json!({"op":"image.resize","width":8193,"height":32}),
        json!({"op":"image.resize","width":8192,"height":8192}),
        json!({"op":"image.resize","width":0,"height":32}),
        json!({"op":"canvas.resize","width":"40","height":32}),
        json!({"op":"crop","rect":[1.5,0,10,10]}),
        json!({"op":"crop","rect":[0,0,10,10],"source_revision":99}),
        json!({"op":"crop","rect":[0,0,10,10],"document_id":"different"}),
    ] {
        let rev = e.doc.revision;
        assert!(e.edit("human", &[c], None, None, "Geometry").is_err());
        assert_eq!(e.doc.revision, rev);
        assert_eq!(e.doc.export_png().unwrap(), before);
    }
    e.doc.layers[0].locked = true;
    assert!(e
        .edit(
            "human",
            &[json!({"op":"image.resize","width":64,"height":48})],
            None,
            None,
            "Resize"
        )
        .is_err());
    e.doc.layers[0].locked = false;
    let layer = e.doc.layers[0].id.clone();
    edit(
        &mut e,
        json!({"op":"effect.add","layer":layer,"kind":"blur"}),
    );
    let before = e.doc.export_png().unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"image.resize","width":64,"height":24})],
            None,
            None,
            "Resize"
        )
        .is_err());
    assert_eq!(e.doc.export_png().unwrap(), before);
    e.reserve(
        "agent",
        "Region",
        vec![Scope {
            target: Some(layer),
            rect: Some([1, 1, 3, 3]),
        }],
    )
    .unwrap();
    for c in [
        json!({"op":"crop","rect":[0,0,16,12]}),
        json!({"op":"canvas.resize","width":64,"height":48}),
        json!({"op":"image.resize","width":64,"height":48}),
    ] {
        assert!(e.edit("human", &[c], None, None, "Geometry").is_err());
    }
    assert_eq!(e.doc.export_png().unwrap(), before);
}

#[test]
fn proportional_rounding_and_group_world_controls_survive_crop_and_resize() {
    let mut e = fixture(16);
    let mut group = peerbrush::engine::Layer::new("Folder", "group", 32, 24);
    let root = group.id.clone();
    group.pixels = Raster::new_depth(32, 24, 16);
    e.doc.layers[0].parent = Some(root.clone());
    e.doc.layers.push(group);
    edit(
        &mut e,
        json!({"op":"liquify.stroke","layer":root,"points":[[10,10],[12,10]],"radius":3,"strength":0.2}),
    );
    edit(&mut e, json!({"op":"crop","rect":[4,2,28,22]}));
    assert_eq!(
        e.doc.layers[1].effects[0].settings["strokes"][0]["points"],
        json!([[6., 8.], [8., 8.]])
    );
    edit(&mut e, json!({"op":"image.resize","width":101,"height":84}));
    let radius = e.doc.layers[1].effects[0].settings["strokes"][0]["radius"]
        .as_f64()
        .unwrap();
    assert!((radius - 3. * ((101.0_f64 / 24.) * (84. / 20.)).sqrt()).abs() < 0.00001);
    let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert!(!decoded.read_only);
    assert_eq!(decoded.export_png().unwrap(), e.doc.export_png().unwrap());
}
