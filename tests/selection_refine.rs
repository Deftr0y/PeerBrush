use peerbrush::{
    engine::{Document, Engine, Scope},
    psd,
    raster::Raster,
    selection,
};
use serde_json::{json, Value};
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], Some(e.doc.revision), None, "Refine")
        .unwrap();
}
fn fixture(depth: u16) -> (Engine, String) {
    let mut e = Engine::new();
    e.doc = Document::new_depth(520, 24, depth).unwrap();
    let layer = e.doc.layers[0].id.clone();
    for y in 0..24 {
        for x in 0..520 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                if x < 260 {
                    [113, 271, 539, 65535]
                } else {
                    [59817, 60719, 65123, 65535]
                },
            );
        }
    }
    (e, layer)
}

#[test]
fn new_masks_use_exact_document_selection_and_preserve_native_sources_history_psd() {
    for depth in [8, 16] {
        let mut e = Engine::new();
        e.doc = Document::new_depth(32, 24, depth).unwrap();
        let id = e.doc.layers[0].id.clone();
        e.doc.layers[0].x = -4;
        e.doc.layers[0].y = 3;
        for y in 0..24 {
            for x in 0..32 {
                e.doc.layers[0]
                    .pixels
                    .set16(x, y, [12347, 33559, 51237, 65535]);
            }
        }
        edit(
            &mut e,
            json!({"op":"selection","kind":"ellipse","rect":[2,4,22,20],"feather":2}),
        );
        edit(
            &mut e,
            json!({"op":"selection","kind":"rectangle","mode":"subtract","rect":[9,9,14,14]}),
        );
        let before = e.doc.clone();
        let selected = selection::current(&before).unwrap();
        let command = json!({"op":"mask.add","layer":id,"value":255,"document_id":before.id,"source_revision":before.revision});
        let draft = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
        let count = e.undo.len();
        edit(&mut e, command);
        assert_eq!(e.undo.len(), count + 1);
        let layer = &e.doc.layers[0];
        let mask = layer.mask.as_ref().unwrap();
        assert_eq!(mask.steps.len(), 1);
        assert_eq!(mask.steps[0].kind, "paint");
        assert_eq!(mask.steps[0].pixels.depth, depth);
        let mut soft = 0;
        for y in 0..24 {
            for x in 0..32 {
                let v = (selected.value(x - 4, y + 3) * 65535.).round() as u16;
                let v = if depth == 8 {
                    ((u32::from(v) + 128) / 257) as u16 * 257
                } else {
                    v
                };
                assert_eq!(mask.steps[0].pixels.get16(x, y), [v, v, v, 65535]);
                soft += usize::from(v > 0 && v < 65535);
            }
        }
        assert!(soft > 0);
        assert_eq!(mask.steps[0].pixels.get16(14, 7), [0, 0, 0, 65535]);
        assert_eq!(layer.pixels.rgba16(), before.layers[0].pixels.rgba16());
        assert_eq!((layer.x, layer.y), (-4, 3));
        assert_eq!(
            selection::current(&e.doc).unwrap().mask.rgba(),
            selected.mask.rgba()
        );
        assert_eq!(e.doc.export_png().unwrap(), draft.export_png().unwrap());
        let reopened = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        assert!(!reopened.read_only);
        assert_eq!(reopened.bit_depth, depth);
        assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
        assert_eq!(
            reopened.layers[0].mask.as_ref().unwrap().steps[0]
                .pixels
                .rgba16(),
            mask.steps[0].pixels.rgba16()
        );
        assert_eq!(
            reopened.layers[0].pixels.rgba16(),
            before.layers[0].pixels.rgba16()
        );
        e.undo("human").unwrap();
        assert!(e.doc.layers[0].mask.is_none());
        assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        e.redo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), draft.export_png().unwrap());
    }
}

#[test]
fn mask_creation_preserves_existing_masks_empty_selections_and_atomic_guards() {
    let mut e = Engine::new();
    e.doc = Document::new_depth(24, 16, 16).unwrap();
    let id = e.doc.layers[0].id.clone();
    edit(&mut e, json!({"op":"mask.add","layer":id,"value":123}));
    assert_eq!(e.doc.layers[0].mask.as_ref().unwrap().steps.len(), 2);
    let original = serde_json::to_value(&e.doc).unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"mask.add","layer":id})],
            None,
            None,
            "Existing"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), original);
    edit(&mut e, json!({"op":"mask.remove","layer":id}));
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","rect":[1,1,5,5]}),
    );
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","mode":"intersect","rect":[10,10,14,14]}),
    );
    let before = serde_json::to_value(&e.doc).unwrap();
    let count = e.undo.len();
    let command = json!({"op":"mask.add","layer":id});
    for target in ["@selection", id.as_str()] {
        let lease = e
            .reserve("other", "Keep work", vec![Scope::layer(target)])
            .unwrap();
        assert!(e
            .edit("human", &[command.clone()], None, None, "Reserved")
            .is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert_eq!(e.undo.len(), count);
        e.leases.retain(|l| l.id != lease.id);
    }
    e.doc.layers[0].locked = true;
    assert!(e
        .edit("human", &[command.clone()], None, None, "Locked")
        .is_err());
    e.doc.layers[0].locked = false;
    assert!(e
        .edit(
            "human",
            &[json!({"op":"mask.add","layer":id,"source_revision":0})],
            None,
            None,
            "Stale"
        )
        .is_err());
    edit(&mut e, command);
    for p in e.doc.layers[0].mask.as_ref().unwrap().steps[0]
        .pixels
        .rgba16()
        .chunks_exact(4)
    {
        assert_eq!(p, [0, 0, 0, 65535]);
    }
}
#[test]
fn native_masks_keep_sub_byte_samples_original_steps_and_exact_undo_psd_sources() {
    let (mut e, layer) = fixture(16);
    edit(&mut e, json!({"op":"mask.add","layer":layer,"value":0}));
    let paint = e.doc.layers[0]
        .mask
        .as_mut()
        .unwrap()
        .steps
        .last_mut()
        .unwrap();
    for y in 3..21 {
        for x in 240..280 {
            paint.pixels.set16(x, y, [123, 123, 123, 65535]);
        }
    }
    e.doc.layers[0].mask.as_mut().unwrap().cache_key = peerbrush::engine::id();
    let baseline = serde_json::to_value(&e.doc.layers[0]).unwrap();
    let png = e.doc.export_png().unwrap();
    edit(
        &mut e,
        json!({"op":"mask.refine","layer":layer,"feather":2}),
    );
    let mask = e.doc.layers[0].mask.as_ref().unwrap();
    assert_eq!(mask.steps.len(), 3);
    assert_eq!(mask.steps[1].pixels.get16(250, 10), [123, 123, 123, 65535]);
    assert_eq!(mask.steps[2].pixels.get16(250, 10), [123, 123, 123, 65535]);
    assert_eq!(mask.steps[2].pixels.get16(240, 10)[0], 74);
    assert!(mask.steps[2].pixels.get16(238, 10)[0] > 0);
    let saved = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert!(!saved.read_only);
    assert_eq!(saved.export_png().unwrap(), e.doc.export_png().unwrap());
    assert_eq!(
        saved.layers[0].mask.as_ref().unwrap().steps[2]
            .pixels
            .get16(240, 10)[0],
        74
    );
    e.undo("human").unwrap();
    assert_eq!(serde_json::to_value(&e.doc.layers[0]).unwrap(), baseline);
    assert_eq!(e.doc.export_png().unwrap(), png);
}
#[test]
fn feather_is_seamless_across_tiles_and_identity_refinement_preserves_coverage() {
    for depth in [8, 16] {
        let (mut e, _) = fixture(depth);
        let source = serde_json::to_value(&e.doc.layers).unwrap();
        edit(
            &mut e,
            json!({"op":"selection","kind":"rectangle","rect":[250,5,270,19]}),
        );
        let before = selection::current(&e.doc).unwrap();
        edit(&mut e, json!({"op":"selection.refine"}));
        assert!(
            selection::current(&e.doc).unwrap() == before,
            "Identity refinement changed coverage"
        );
        edit(&mut e, json!({"op":"selection.refine","feather":3}));
        let mask = selection::current(&e.doc).unwrap();
        for y in 0..24 {
            for x in 240..280 {
                let count = (-3..=3)
                    .flat_map(|dy| (-3..=3).map(move |dx| (x + dx, y + dy)))
                    .filter(|&(x, y)| (250..270).contains(&x) && (5..19).contains(&y))
                    .count();
                assert!(
                    (mask.mask.get(x, y)[0] as i32 - (count as f32 / 49. * 255.).round() as i32)
                        .abs()
                        <= 1,
                    "pixel ({x}, {y}), reference sample count {count}"
                );
            }
        }
        assert_eq!(serde_json::to_value(&e.doc.layers).unwrap(), source);
        assert_eq!(e.doc.selection_previous.as_ref().unwrap(), &before);
        let saved = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        assert_eq!(selection::current(&saved).unwrap(), mask);
    }
}
#[test]
fn guided_edge_improves_a_blurred_cutout_without_source_changes_or_tile_seams() {
    let (mut e, _) = fixture(16);
    e.doc = Document::new_depth(520, 96, 16).unwrap();
    let layer = e.doc.layers[0].id.clone();
    for y in 0..96 {
        for x in 0..520 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                if x < 260 {
                    [113, 271, 539, 65535]
                } else {
                    [59817, 60719, 65123, 65535]
                },
            );
        }
    }
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","rect":[260,0,520,96]}),
    );
    edit(&mut e, json!({"op":"selection.refine","feather":6}));
    let before = selection::current(&e.doc).unwrap();
    let source = e.doc.layers[0].pixels.clone();
    edit(
        &mut e,
        json!({"op":"selection.refine","edge":100,"edge_radius":8,"layer":layer}),
    );
    let after = selection::current(&e.doc).unwrap();
    let error = |m: &selection::Coverage| {
        (248..272)
            .map(|x| ((if x < 260 { 0. } else { 1. }) - m.value(x, 48)).powi(2))
            .sum::<f32>()
    };
    assert!(
        error(&after) < error(&before) * 0.75,
        "{} vs {}",
        error(&after),
        error(&before)
    );
    assert!(
        after.value(259, 48) < 0.25,
        "Dark-side edge coverage {}",
        after.value(259, 48)
    );
    assert!(
        after.value(260, 48) > 0.75,
        "Light-side edge coverage {}",
        after.value(260, 48)
    );
    assert_eq!(e.doc.layers[0].pixels, source);
}
#[test]
fn selection_to_mask_maps_document_coordinates_and_preserves_native_subtract_values() {
    let (mut e, layer) = fixture(16);
    edit(&mut e, json!({"op":"move","layer":layer,"dx":10,"dy":8}));
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","rect":[260,13,280,27]}),
    );
    edit(&mut e, json!({"op":"mask.from_selection","layer":layer}));
    let mask = e.doc.layers[0].mask.as_ref().unwrap();
    assert_eq!(mask.steps[0].pixels.get16(250, 5), [65535; 4]);
    assert_eq!(mask.steps[0].pixels.get16(249, 5), [0, 0, 0, 65535]);
    let original = e.doc.layers[0].mask.as_mut().unwrap();
    original.steps[0]
        .pixels
        .set16(251, 6, [123, 123, 123, 65535]);
    original.cache_key = peerbrush::engine::id();
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","rect":[300,13,320,27]}),
    );
    edit(
        &mut e,
        json!({"op":"mask.from_selection","layer":layer,"mode":"subtract"}),
    );
    assert_eq!(
        e.doc.layers[0].mask.as_ref().unwrap().steps[1]
            .pixels
            .get16(251, 6),
        [123, 123, 123, 65535]
    );
}
#[test]
fn invalid_locked_reserved_and_stale_refinements_leave_sources_and_history_intact() {
    let (mut e, layer) = fixture(16);
    edit(&mut e, json!({"op":"mask.add","layer":layer}));
    let baseline = serde_json::to_value(&e.doc).unwrap();
    let n = e.undo.len();
    for c in [
        json!({"op":"mask.refine","layer":layer,"feather":1000}),
        json!({"op":"mask.refine","layer":layer,"edge":true}),
        json!({"op":"selection.refine"}),
        json!({"op":"mask.refine","layer":layer,"document_id":"other"}),
        json!({"op":"mask.refine","layer":layer,"source_revision":0}),
    ] {
        assert!(e.edit("human", &[c], None, None, "Bad refine").is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), baseline);
        assert_eq!(e.undo.len(), n);
    }
    e.reserve("other", "Mask reserved", vec![Scope::layer(&layer)])
        .unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"mask.refine","layer":layer,"feather":2})],
            None,
            None,
            "Reserved"
        )
        .is_err());
    e.leases.clear();
    e.doc.layers[0].locked = true;
    assert!(e
        .edit(
            "human",
            &[json!({"op":"mask.refine","layer":layer,"feather":2})],
            None,
            None,
            "Locked"
        )
        .is_err());
}
#[test]
fn native_refinement_noop_keeps_every_mask_word() {
    let mut mask = Raster::new_depth(19, 13, 16);
    for y in 0..13 {
        for x in 0..19 {
            let v = (x * 129 + y * 73 + 1) as u16;
            mask.set16(x, y, [v; 4]);
        }
    }
    assert_eq!(
        selection::refine::run(&mask, None, &json!({})).unwrap(),
        mask
    );
}
