use peerbrush::{
    engine::{Document, Engine, Scope},
    preview, psd,
    raster::Raster,
    selection, server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn edit(e: &mut Engine, command: Value) {
    e.edit(
        "human",
        &[command],
        Some(e.doc.revision),
        None,
        "Edit bounds",
    )
    .unwrap();
}
fn fixture(depth: u16, mask: bool) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(96, 80, depth).unwrap();
    let l = &mut e.doc.layers[0];
    l.x = 28;
    l.y = 22;
    l.pixels = Raster::new_depth(24, 20, depth);
    for y in 0..20 {
        for x in 0..24 {
            l.pixels.set16(x, y, [12347, 23459, 34571, 65535]);
        }
    }
    let id = l.id.clone();
    if mask {
        edit(&mut e, json!({"op":"mask.add","layer":id,"value":255}));
        edit(
            &mut e,
            json!({"op":"paint","layer":id,"mask":true,"points":[[36,30]],"radius":3,"color":[80,80,80,255]}),
        );
    }
    edit(
        &mut e,
        json!({"op":"transform","layer":id,"pivot":[40,36],"scale_x":0.5,"scale_y":0.5,"selection_only":false}),
    );
    edit(
        &mut e,
        json!({"op":"move","layer":id,"dx":8,"dy":5,"selection_only":false}),
    );
    e.undo.clear();
    e
}
fn pixel(doc: &Document, x: i32, y: i32) -> [u16; 4] {
    let l = &doc.layers[0];
    l.pixels.get16(x - l.x, y - l.y)
}
fn assert_old_pixels(before: &Document, after: &Document) {
    let l = &before.layers[0];
    for y in l.y..l.y + l.pixels.height as i32 {
        for x in l.x..l.x + l.pixels.width as i32 {
            assert_eq!(pixel(before, x, y), pixel(after, x, y));
        }
    }
}

#[test]
fn scaled_moved_layers_paint_across_canvas_with_exact_live_preview_undo_and_reopen() {
    for depth in [8, 16] {
        let mut e = fixture(depth, true);
        let before = e.doc.clone();
        let id = before.layers[0].id.clone();
        let command = json!({"op":"paint","layer":id,"points":[[8,8],[85,8]],"radius":3,"hardness":1,"color":[20,100,245,255]});
        let live = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
        assert_eq!(e.undo.len(), 0);
        let mut cache = preview::Cache::default();
        let mut early = command.clone();
        early["points"] = json!([[8, 8]]);
        cache.edit(before.clone(), &[early], "outside").unwrap();
        let incremental = cache
            .edit(before.clone(), &[command.clone()], "outside")
            .unwrap();
        assert_eq!(
            incremental.export_png().unwrap(),
            live.export_png().unwrap()
        );
        edit(&mut e, command);
        assert!(pixel(&e.doc, 8, 8)[3] > 0, "paint clipped at depth {depth}");
        assert!(pixel(&e.doc, 85, 8)[3] > 0);
        assert_old_pixels(&before, &e.doc);
        assert_eq!(e.doc.export_png().unwrap(), live.export_png().unwrap());
        assert_eq!(e.undo.len(), 1);
        let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        assert!(!decoded.read_only);
        assert_eq!(decoded.bit_depth, depth);
        assert_eq!(decoded.export_png().unwrap(), e.doc.export_png().unwrap());
        e.undo("human").unwrap();
        assert_eq!(
            serde_json::to_value(&e.doc.layers).unwrap(),
            serde_json::to_value(&before.layers).unwrap()
        );
    }
}

#[test]
fn mask_edits_expand_without_resampling_the_unchanged_color_original() {
    let mut e = fixture(16, true);
    let before = e.doc.clone();
    let original = before.layers[0]
        .pixels
        .retained
        .as_ref()
        .unwrap()
        .pixels
        .clone();
    let id = before.layers[0].id.clone();
    edit(
        &mut e,
        json!({"op":"paint","layer":id,"mask":true,"points":[[8,8]],"radius":3,"hardness":1,"color":[0,0,0,255]}),
    );
    assert_old_pixels(&before, &e.doc);
    let l = &e.doc.layers[0];
    assert_eq!(l.pixels.retained.as_ref().unwrap().pixels, original);
    let mask = l.mask.as_ref().unwrap().steps.last().unwrap();
    assert_eq!(mask.pixels.get16(8 - l.x, 8 - l.y), [0, 0, 0, 65535]);
    assert!(mask.pixels.retained.is_none());
    let painted = e.doc.export_png().unwrap();
    edit(
        &mut e,
        json!({"op":"transform","layer":id,"pivot":[40,36],"angle":20,"selection_only":false}),
    );
    edit(
        &mut e,
        json!({"op":"transform","layer":id,"pivot":[40,36],"angle":-20,"selection_only":false}),
    );
    assert_eq!(e.doc.export_png().unwrap(), painted);
}

#[test]
fn soft_selections_keep_holes_and_native_samples_outside_the_edit() {
    let mut e = fixture(16, false);
    let id = e.doc.layers[0].id.clone();
    let mut mask = Raster::new(12, 12);
    for y in 0..12 {
        for x in 0..12 {
            if x != 5 {
                mask.set(x, y, [128; 4]);
            }
        }
    }
    let coverage = selection::from_mask(mask).unwrap().local(-4, -4);
    e.doc.selection = Some(coverage.bounds);
    e.doc.selection_coverage = Some(coverage);
    let before = e.doc.clone();
    edit(
        &mut e,
        json!({"op":"paint","layer":id,"points":[[8,8]],"radius":7,"hardness":1,"color":[25,120,240,255]}),
    );
    assert!(pixel(&e.doc, 8, 8)[3] > 0 && pixel(&e.doc, 8, 8)[3] < 65535);
    assert_eq!(pixel(&e.doc, 9, 8), [0; 4]);
    assert_eq!(pixel(&e.doc, 2, 8), [0; 4]);
    assert_old_pixels(&before, &e.doc);
}

#[test]
fn fills_shapes_gradients_and_clone_share_document_editing_bounds() {
    for depth in [8, 16] {
        for op in ["fill", "paint.fill", "shape", "gradient", "clone"] {
            let mut e = fixture(depth, false);
            let before = e.doc.clone();
            let l = &before.layers[0];
            let command = if op == "clone" {
                json!({"op":op,"layer":l.id,"source":[l.x+4,l.y+4],"points":[[8,8]],"radius":2,"hardness":1})
            } else {
                json!({"op":op,"layer":l.id,"rect":[4,4,16,16],"color":[20,100,245,255],"color2":[240,10,50,255]})
            };
            edit(&mut e, command);
            assert!(pixel(&e.doc, 8, 8)[3] > 0, "{op} clipped at depth {depth}");
            assert_old_pixels(&before, &e.doc);
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        }
    }
}

#[test]
fn bounds_expansion_rejects_locked_reserved_and_invalid_edits_atomically() {
    let mut e = fixture(16, true);
    let id = e.doc.layers[0].id.clone();
    let command =
        json!({"op":"paint","layer":id,"points":[[8,8]],"radius":3,"color":[20,100,245,255]});
    e.doc.layers[0].locked = true;
    let before = serde_json::to_value(&e.doc).unwrap();
    assert!(e
        .edit("human", &[command.clone()], None, None, "Locked")
        .is_err());
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
    e.doc.layers[0].locked = false;
    let lease = e
        .reserve(
            "agent",
            "Reserved",
            vec![Scope {
                target: Some(id.clone()),
                rect: Some([0, 0, 16, 16]),
            }],
        )
        .unwrap();
    let before = serde_json::to_value(&e.doc).unwrap();
    assert!(e
        .edit("human", &[command.clone()], None, None, "Reserved")
        .is_err());
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
    e.leases.retain(|l| l.id != lease.id);
    let mut invalid = command;
    invalid["radius"] = json!(-1);
    assert!(e.edit("human", &[invalid], None, None, "Invalid").is_err());
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
    assert!(e.undo.is_empty());
}

#[test]
fn protocol_edits_outside_transformed_content_use_the_same_engine_bounds() {
    let shared = Arc::new(Mutex::new(fixture(16, false)));
    let (project, document, revision, layer) = {
        let e = shared.lock().unwrap();
        (
            e.project_id.clone(),
            e.doc.id.clone(),
            e.doc.revision,
            e.doc.layers[0].id.clone(),
        )
    };
    server::dispatch(&shared,"edit",&json!({"actor":"human","project_id":project,"document_id":document,"expected_revision":revision,"commands":[{"op":"paint","layer":layer,"points":[[8,8]],"radius":3,"color":[20,100,245,255]}],"feedback":"request"})).unwrap();
    assert!(pixel(&shared.lock().unwrap().doc, 8, 8)[3] > 0);
}

#[test]
fn expanding_storage_preserves_off_canvas_pixels_and_hidden_rgb_at_both_depths() {
    for depth in [8, 16] {
        let mut e = Engine::new();
        e.doc = Document::new_depth(96, 80, depth).unwrap();
        let l = &mut e.doc.layers[0];
        l.x = -6;
        l.y = 24;
        l.pixels = Raster::new_depth(20, 16, depth);
        l.pixels.set16(1, 1, [12347, 23459, 34571, 0]);
        l.pixels.set16(2, 2, [45679, 56781, 60001, 65535]);
        let before = e.doc.clone();
        let id = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            json!({"op":"paint","layer":id,"points":[[85,8]],"radius":3,"color":[20,100,245,255]}),
        );
        assert_old_pixels(&before, &e.doc);
        assert_ne!(pixel(&e.doc, -5, 25), [0; 4]);
        assert!(pixel(&e.doc, 85, 8)[3] > 0);
        e.undo("human").unwrap();
        assert_eq!(
            serde_json::to_value(&e.doc.layers).unwrap(),
            serde_json::to_value(&before.layers).unwrap()
        );
    }
}

#[test]
fn expanding_storage_translates_liquify_points_polygons_and_coverage_together() {
    let mut e = fixture(16, false);
    let id = e.doc.layers[0].id.clone();
    let mut coverage = Raster::new(12, 10);
    coverage.set(2, 4, [128; 4]);
    let coverage = selection::from_mask(coverage).unwrap();
    edit(
        &mut e,
        json!({"op":"effect.add","layer":id,"kind":"liquify","settings":{"strokes":[{"mode":"push","points":[[2,4],[5,4]],"radius":3,"strength":0.5,"selection":[0,0,12,10],"polygon":[[0,0],[12,0],[12,10],[0,10]],"coverage":coverage}]}}),
    );
    let before = e.doc.clone();
    let old = &before.layers[0];
    edit(
        &mut e,
        json!({"op":"paint","layer":id,"points":[[8,8]],"radius":3,"color":[20,100,245,255]}),
    );
    let l = &e.doc.layers[0];
    let (dx, dy) = (old.x - l.x, old.y - l.y);
    let stroke = &l.effects[0].settings["strokes"][0];
    let points: Vec<[f32; 2]> = serde_json::from_value(stroke["points"].clone()).unwrap();
    assert_eq!(
        points,
        vec![
            [(2 + dx) as f32, (4 + dy) as f32],
            [(5 + dx) as f32, (4 + dy) as f32]
        ]
    );
    let first: [f32; 2] = serde_json::from_value(stroke["polygon"][0].clone()).unwrap();
    assert_eq!(first, [dx as f32, dy as f32]);
    assert_eq!(stroke["selection"], json!([dx, dy, 12 + dx, 10 + dy]));
    let shifted: selection::Coverage = serde_json::from_value(stroke["coverage"].clone()).unwrap();
    assert_eq!(shifted.value(2 + dx, 4 + dy), coverage.value(2, 4));
    e.undo("human").unwrap();
    assert_eq!(
        serde_json::to_value(&e.doc.layers).unwrap(),
        serde_json::to_value(&before.layers).unwrap()
    );
}

#[test]
fn invisible_strokes_do_not_expand_sources_or_discard_native_originals() {
    for field in ["opacity", "flow"] {
        let mut e = fixture(16, true);
        let before = serde_json::to_value(&e.doc.layers).unwrap();
        let mut command = json!({"op":"paint","layer":e.doc.layers[0].id,"points":[[8,8]],"radius":3,"color":[20,100,245,255]});
        command[field] = json!(0);
        edit(&mut e, command);
        assert_eq!(serde_json::to_value(&e.doc.layers).unwrap(), before);
    }
}
