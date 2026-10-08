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
