use peerbrush::{
    clipboard::Image,
    effects,
    engine::{Document, Engine, Layer, Scope},
    preview, psd, selection,
};
use serde_json::{json, Value};
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], None, None, "Selection regression")
        .unwrap();
}

#[test]
fn selected_clear_preserves_native_samples_holes_preview_psd_and_one_step_undo() {
    for depth in [8, 16] {
        let (mut e, id) = fixture(depth);
        e.doc.layers[0].x = -2;
        edit(
            &mut e,
            json!({"op":"selection","kind":"ellipse","rect":[2,2,22,22],"feather":2}),
        );
        edit(
            &mut e,
            json!({"op":"selection","kind":"rectangle","mode":"subtract","rect":[9,9,15,15]}),
        );
        let before = e.doc.clone();
        let coverage = selection::current(&before).unwrap();
        let command = json!({"op":"paint.clear_selection","layer":id});
        let lease = e
            .reserve(
                "other",
                "Keep selected pixels",
                vec![Scope {
                    target: Some(id.clone()),
                    rect: Some([3, 3, 6, 6]),
                }],
            )
            .unwrap();
        // A caller-supplied rectangle cannot understate the actual clearing scope.
        assert!(e
            .edit(
                "human",
                &[json!({"op":"paint.clear_selection","layer":id,"rect":[0,0,1,1]})],
                None,
                None,
                "Clear"
            )
            .is_err());
        e.leases.retain(|l| l.id != lease.id);
        let preview = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
        let history = e.undo.len();
        edit(&mut e, command);
        assert_eq!(e.undo.len(), history + 1);
        assert_eq!(e.doc.selection, before.selection);
        assert_eq!(e.doc.layers.len(), before.layers.len());
        assert_eq!(
            e.doc.layers[0].pixels.rgba16(),
            preview.layers[0].pixels.rgba16()
        );
        let mut soft = 0;
        for y in 0..24 {
            for x in 0..32 {
                let old = before.layers[0].pixels.get16(x, y);
                let factor = coverage.value(x - 2, y);
                let mut expected = old;
                if depth == 16 {
                    expected[3] = (old[3] as f64 * (1. - factor as f64)).round() as u16;
                } else {
                    expected[3] =
                        ((old[3] / 257) as f64 * (1. - factor as f64)).round() as u16 * 257;
                }
                if expected[3] == 0 {
                    expected = [0; 4];
                }
                assert_eq!(
                    e.doc.layers[0].pixels.get16(x, y),
                    expected,
                    "depth {depth}, {x},{y}"
                );
                soft += usize::from(factor > 0. && factor < 1.);
            }
        }
        assert!(soft > 0);
        let saved = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        assert_eq!(saved.bit_depth, depth);
        assert_eq!(saved.export_png().unwrap(), e.doc.export_png().unwrap());
        e.undo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        e.redo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
    }
}

#[test]
fn selected_clear_folder_and_mask_enforce_locks_reservations_and_protocol_guards() {
    for depth in [8, 16] {
        let (mut e, id) = fixture(depth);
        let mut folder = Layer::new("Folder", "group", 32, 24);
        let root = folder.id.clone();
        folder.visible = false;
        e.doc.layers[0].parent = Some(root.clone());
        e.doc.layers.insert(0, folder);
        edit(
            &mut e,
            json!({"op":"selection","kind":"rectangle","rect":[2,3,5,7]}),
        );
        let before = e.doc.clone();
        let command = json!({"op":"paint.clear_selection","layer":root});
        e.doc.layers[1].locked = true;
        assert!(e
            .edit("human", &[command.clone()], None, None, "Clear")
            .is_err());
        assert_eq!(
            e.doc.layers[1].pixels.rgba16(),
            before.layers[1].pixels.rgba16()
        );
        e.doc.layers[1].locked = false;
        let lease = e
            .reserve(
                "other",
                "Preserve child",
                vec![Scope {
                    target: Some(id.clone()),
                    rect: Some([2, 3, 5, 7]),
                }],
            )
            .unwrap();
        assert!(e
            .edit("human", &[command.clone()], None, None, "Clear")
            .is_err());
        e.leases.retain(|l| l.id != lease.id);
        let shared = std::sync::Arc::new(std::sync::Mutex::new(e));
        let doc = shared.lock().unwrap().doc.clone();
        let result=peerbrush::server::dispatch(&shared,"edit",&json!({"actor":"test-agent","document_id":doc.id,"expected_revision":doc.revision,"commands":[command],"feedback":"always"})).unwrap();
        assert!(result["images"]
            .as_array()
            .is_some_and(|images| !images.is_empty()));
        assert_eq!(
            shared.lock().unwrap().doc.layers[1].pixels.get16(2, 3),
            [0; 4]
        );
        assert_eq!(
            shared.lock().unwrap().doc.layers[1].pixels.get16(9, 9),
            before.layers[1].pixels.get16(9, 9)
        );
        let mut e = shared.lock().unwrap();
        e.undo("test-agent").unwrap();
        edit(&mut e, json!({"op":"mask.add","layer":id}));
        let mask = e.doc.layers[1].mask.as_mut().unwrap();
        mask.steps.push(peerbrush::engine::MaskStep {
            id: peerbrush::engine::id(),
            kind: "paint".into(),
            enabled: true,
            value: 255.,
            weight: 1.,
            pixels: peerbrush::raster::Raster::new_depth(32, 24, depth),
            settings: json!({}),
        });
        let pixels = &mut mask.steps.last_mut().unwrap().pixels;
        pixels.set16(2, 3, [10001, 10001, 10001, 65535]);
        pixels.set16(9, 9, [20003, 20003, 20003, 65535]);
        let before = e.doc.clone();
        edit(
            &mut e,
            json!({"op":"paint.clear_selection","layer":id,"mask":true}),
        );
        assert_eq!(
            e.doc.layers[1].pixels.rgba16(),
            before.layers[1].pixels.rgba16()
        );
        let pixels = &e.doc.layers[1]
            .mask
            .as_ref()
            .unwrap()
            .steps
            .last()
            .unwrap()
            .pixels;
        assert_eq!(pixels.get16(2, 3), [0; 4]);
        assert_eq!(
            pixels.get16(9, 9),
            before.layers[1]
                .mask
                .as_ref()
                .unwrap()
                .steps
                .last()
                .unwrap()
                .pixels
                .get16(9, 9)
        );
        e.undo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        e.doc.selection = None;
        e.doc.selection_coverage = None;
        assert!(e
            .edit(
                "human",
                &[json!({"op":"paint.clear_selection","layer":id})],
                None,
                None,
                "No selection"
            )
            .is_err());
    }
}
fn fixture(depth: u16) -> (Engine, String) {
    let mut e = Engine::new();
    e.doc = Document::new_depth(32, 24, depth).unwrap();
    let layer = &mut e.doc.layers[0];
    for y in 0..24 {
        for x in 0..32 {
            layer
                .pixels
                .set16(x, y, [10001 + x as u16, 20003 + y as u16, 30007, 65535]);
        }
    }
    let id = layer.id.clone();
    (e, id)
}
#[test]
fn empty_intersection_remains_active_and_blocks_all_pixels_then_reselects() {
    let (mut e, id) = fixture(16);
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","rect":[2,2,8,8]}),
    );
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","mode":"intersect","rect":[20,16,25,20]}),
    );
    assert_eq!(e.doc.selection, Some([0; 4]));
    let before = e.doc.layers[0].pixels.rgba16();
    edit(
        &mut e,
        json!({"op":"paint.fill","layer":id,"color":[255,0,0,255]}),
    );
    assert_eq!(e.doc.layers[0].pixels.rgba16(), before);
    edit(&mut e, json!({"op":"selection.clear"}));
    assert!(e.doc.selection.is_none());
    edit(&mut e, json!({"op":"selection.reselect"}));
    assert_eq!(e.doc.selection, Some([0; 4]));
}
#[test]
fn holes_soft_edges_clipboard_and_live_preview_match_both_native_depths() {
    for depth in [8, 16] {
        let (mut e, id) = fixture(depth);
        edit(
            &mut e,
            json!({"op":"selection","kind":"ellipse","rect":[2,2,22,22],"feather":2}),
        );
        edit(
            &mut e,
            json!({"op":"selection","kind":"rectangle","mode":"subtract","rect":[9,9,15,15]}),
        );
        let coverage = selection::current(&e.doc).unwrap();
        assert_eq!(coverage.value(12, 12), 0.);
        assert!(coverage.contours.len() >= 2);
        let copied = Image::copy(&e.doc, &id, false, false).unwrap();
        let area = copied.origin.unwrap();
        let index = (((12 - area[1]) as u32 * copied.width + (12 - area[0]) as u32) * 4) as usize;
        assert_eq!(copied.bytes[index + 3], 0);
        if depth == 16 {
            let samples = copied.samples16.unwrap();
            let i = (((6 - area[1]) as u32 * copied.width + (12 - area[0]) as u32) * 4) as usize;
            assert_eq!(samples[i], 10013);
        }
        let baseline = e.doc.clone();
        let command = json!({"op":"paint","layer":id,"points":[[4.,12.],[20.,12.]],"radius":6.,"color":[255,0,0,255]});
        let rendered = preview::Cache::default()
            .edit(baseline.clone(), &[command.clone()], "gesture")
            .unwrap();
        edit(&mut e, command);
        assert_eq!(
            rendered.layers[0].pixels.rgba16(),
            e.doc.layers[0].pixels.rgba16()
        );
        assert_eq!(
            e.doc.layers[0].pixels.get16(12, 12),
            baseline.layers[0].pixels.get16(12, 12)
        );
        assert_eq!(
            e.doc.layers[0].pixels.get16(0, 0),
            baseline.layers[0].pixels.get16(0, 0)
        );
        e.undo("human").unwrap();
        assert_eq!(
            e.doc.layers[0].pixels.rgba16(),
            baseline.layers[0].pixels.rgba16()
        );
    }
}
#[test]
fn selected_transform_preserves_native_words_holes_and_shared_multilayer_geometry() {
    let (mut e, id) = fixture(16);
    let mut second = Layer::new("Second", "paint", 32, 24);
    second.pixels = e.doc.layers[0].pixels.clone();
    let other = second.id.clone();
    e.doc.layers.push(second);
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","rect":[2,2,10,10]}),
    );
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","mode":"subtract","rect":[4,4,7,7]}),
    );
    e.edit(
        "human",
        &[
            json!({"op":"move","layer":id,"dx":12,"dy":0}),
            json!({"op":"move","layer":other,"dx":12,"dy":0}),
        ],
        None,
        None,
        "Move selected pixels",
    )
    .unwrap();
    let coverage = selection::current(&e.doc).unwrap();
    assert_eq!(coverage.bounds, [14, 2, 22, 10]);
    assert_eq!(coverage.value(17, 5), 0.);
    for layer in &e.doc.layers {
        assert_eq!(
            layer.pixels.get16(15 - layer.x, 3 - layer.y),
            [10004, 20006, 30007, 65535]
        );
        assert_eq!(
            layer.pixels.get16(5 - layer.x, 5 - layer.y),
            [10006, 20008, 30007, 65535]
        );
    }
}
#[test]
fn visibility_does_not_conflict_with_ai_pixels_or_unrelated_structure() {
    let (mut e, id) = fixture(16);
    let rev = e.doc.revision;
    let lease = e
        .reserve("agent", "Reserved paint", vec![Scope::layer(&id)])
        .unwrap();
    edit(
        &mut e,
        json!({"op":"layer.update","layer":id,"visible":false}),
    );
    e.edit(
        "agent",
        &[json!({"op":"paint","layer":id,"points":[[4,4]],"radius":2,"color":[255,0,0,255]})],
        Some(rev),
        Some(&lease.id),
        "AI stroke",
    )
    .unwrap();
    assert!(!e.doc.layers[0].visible);
    edit(&mut e, json!({"op":"layer.add","name":"Unreserved"}));
    let free = e.doc.layers[0].id.clone();
    edit(&mut e, json!({"op":"layer.reorder","layer":free,"index":1}));
    assert!(e
        .edit(
            "human",
            &[json!({"op":"layer.update","layer":id,"visible":true,"opacity":0.5})],
            None,
            None,
            "Mixed command"
        )
        .is_err());
    assert_eq!(e.leases.len(), 1);
}
#[test]
fn group_reservations_cover_descendants_and_only_affected_structural_operations() {
    let (mut e, id) = fixture(8);
    let group = Layer::new("Reserved folder", "group", 32, 24);
    let gid = group.id.clone();
    e.doc.layers[0].parent = Some(gid.clone());
    e.doc.layers.insert(0, group);
    e.reserve("agent", "Folder", vec![Scope::layer(&gid)])
        .unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"layer.delete","layer":id})],
            None,
            None,
            "Delete reserved child"
        )
        .is_err());
    edit(
        &mut e,
        json!({"op":"layer.update","layer":id,"visible":false}),
    );
    edit(&mut e, json!({"op":"layer.add","name":"Outside"}));
    let free = e.doc.layers[0].id.clone();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"layer.parent","layer":free,"parent":gid})],
            None,
            None,
            "Enter reserved folder"
        )
        .is_err());
}
#[test]
fn liquify_snapshots_the_selected_hole_into_chosen_effect_and_psd() {
    let (mut e, id) = fixture(16);
    edit(
        &mut e,
        json!({"op":"effect.add","layer":id,"kind":"liquify"}),
    );
    let first = e.doc.layers[0].effects[0].id.clone();
    edit(
        &mut e,
        json!({"op":"effect.add","layer":id,"kind":"liquify"}),
    );
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","rect":[2,2,30,22]}),
    );
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","rect":[12,8,20,16],"mode":"subtract"}),
    );
    edit(
        &mut e,
        json!({"op":"liquify.stroke","layer":id,"effect":first,"points":[[8,12],[24,12]],"radius":7,"strength":1}),
    );
    assert_eq!(
        e.doc.layers[0].effects[0].settings["strokes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        e.doc.layers[0].effects[1].settings["strokes"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let before = e.doc.layers[0].pixels.get16(16, 12);
    let output = peerbrush::depth16::preview16(&e.doc, None, 32, None, false).unwrap();
    let i = ((12 * 32 + 16) * 4) as usize;
    assert_eq!(&output.2[i..i + 4], &before);
    let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert_eq!(decoded.bit_depth, 16);
    assert!(!decoded.read_only);
    assert_eq!(
        decoded.layers[0].effects[0].settings,
        e.doc.layers[0].effects[0].settings
    );
}
#[test]
fn canvas_and_depth_conversion_are_explicit_undoable_and_keep_original_native_words() {
    let (mut e, _) = fixture(16);
    let old = e.doc.layers[0].pixels.get16(1, 1);
    edit(
        &mut e,
        json!({"op":"document.settings","width":40,"height":20,"bit_depth":16}),
    );
    assert_eq!(e.doc.layers[0].pixels.get16(1, 1), old);
    edit(&mut e, json!({"op":"document.settings","bit_depth":8}));
    assert_eq!(e.doc.layers[0].pixels.depth, 8);
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers[0].pixels.get16(1, 1), old);
    assert_eq!(e.doc.bit_depth, 16);
    let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert_eq!(
        (decoded.width, decoded.height, decoded.bit_depth),
        (40, 20, 16)
    );
    assert_eq!(decoded.layers[0].pixels.get16(1, 1), old);
}
#[test]
fn smooth_curves_remain_bounded_and_old_sources_keep_linear_interpolation() {
    let mut smooth = effects::defaults("curves");
    smooth["points"] = json!([[0., 0.], [0.3, 0.7], [0.7, 0.8], [1., 1.]]);
    let mut old = smooth.clone();
    old.as_object_mut().unwrap().remove("interpolation");
    let old = effects::normalized("curves", &old).unwrap();
    assert!(
        (effects::curve_value64(&smooth, 0.2) - effects::curve_value64(&old, 0.2)).abs() > 0.01
    );
    let mut previous = 0.;
    for i in 0..=65535 {
        let value = effects::curve_value64(&smooth, i as f64 / 65535.);
        assert!((previous..=1.).contains(&value));
        previous = value;
    }
}
#[test]
fn morphology_wand_and_invalid_object_bounds_are_transactional() {
    let (mut e, _) = fixture(8);
    edit(
        &mut e,
        json!({"op":"selection","kind":"rectangle","rect":[5,5,10,10]}),
    );
    edit(
        &mut e,
        json!({"op":"selection.modify","mode":"expand","radius":2}),
    );
    assert_eq!(e.doc.selection, Some([3, 3, 12, 12]));
    let rev = e.doc.revision;
    assert!(e
        .edit(
            "human",
            &[json!({"op":"selection","kind":"object","rect":[0,0,0,0]})],
            None,
            None,
            "Invalid"
        )
        .is_err());
    assert_eq!(e.doc.revision, rev);
    edit(
        &mut e,
        json!({"op":"selection","kind":"wand","point":[0,0],"tolerance":255,"sample_merged":true}),
    );
    assert_eq!(e.doc.selection, Some([0, 0, 32, 24]));
}

#[test]
fn feathered_identity_transform_is_exact_and_invalid_geometry_never_clears_coverage() {
    let (mut e, id) = fixture(16);
    edit(
        &mut e,
        json!({"op":"selection","kind":"ellipse","rect":[2,2,22,22],"feather":3}),
    );
    let before = e.doc.layers[0].pixels.rgba16();
    edit(
        &mut e,
        json!({"op":"transform","layer":id,"scale_x":1,"scale_y":1,"angle":0}),
    );
    assert_eq!(e.doc.layers[0].pixels.rgba16(), before);
    let coverage = selection::current(&e.doc).unwrap();
    for command in [
        json!({"op":"selection","kind":"unknown"}),
        json!({"op":"selection","kind":"polygon","polygon":[[1,1],[2]]}),
        json!({"op":"selection","kind":"rectangle","rect":[0,0,1e20,8]}),
    ] {
        let revision = e.doc.revision;
        assert!(e
            .edit("human", &[command], None, None, "Invalid selection")
            .is_err());
        assert_eq!(e.doc.revision, revision);
        assert_eq!(
            selection::current(&e.doc).unwrap().mask.rgba(),
            coverage.mask.rgba()
        );
    }
    let mut invalid = coverage;
    invalid.origin[0] = i32::MIN;
    assert!(invalid.validate().is_err());
}
