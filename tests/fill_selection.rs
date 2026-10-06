use peerbrush::{
    engine::{Document, Engine, Layer, Scope},
    psd,
    raster::{Raster, TILE},
    selection, server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn canvas(width: u32, height: u32) -> (Engine, String) {
    let mut engine = Engine::new();
    engine.doc = Document::new(width, height).unwrap();
    let id = engine.doc.layers[0].id.clone();
    (engine, id)
}
fn edit(engine: &mut Engine, command: Value) {
    engine
        .edit("human", &[command], None, None, "Test edit")
        .unwrap();
}
fn pixel(engine: &Engine, x: i32, y: i32) -> [u8; 4] {
    let layer = &engine.doc.layers[0];
    layer.pixels.get(x - layer.x, y - layer.y)
}

#[test]
fn foreground_fill_covers_transparency_and_offset_layer_with_one_undoable_batch() {
    let (mut e, id) = canvas(12, 10);
    e.doc.layers[0].pixels = Raster::new(2, 2);
    e.doc.layers[0].x = 5;
    e.doc.layers[0].y = 3;
    e.doc.layers[0].pixels.set(0, 0, [20, 40, 90, 255]);
    let before = e.doc.clone();
    let command = json!({"op":"paint.fill","layer":id,"color":[233,84,32,255]});
    e.edit("artist", &[command], Some(0), None, "Foreground fill")
        .unwrap();
    assert_eq!(e.doc.layers[0].kind, "paint");
    assert_eq!((e.doc.layers[0].x, e.doc.layers[0].y), (0, 0));
    assert_eq!(
        (e.doc.layers[0].pixels.width, e.doc.layers[0].pixels.height),
        (12, 10)
    );
    for y in 0..10 {
        for x in 0..12 {
            assert_eq!(pixel(&e, x, y), [233, 84, 32, 255]);
        }
    }
    assert_eq!(e.undo.len(), 1);
    assert_eq!(e.ai_change.as_ref().unwrap().tool, "fill");
    let shared: server::Shared = Arc::new(Mutex::new(e));
    let observed = server::dispatch(&shared, "observe", &json!({"max_edge":32})).unwrap();
    let image = observed["images"][0]["data"].as_str().unwrap();
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let rendered = image::load_from_memory(&STANDARD.decode(image).unwrap())
        .unwrap()
        .to_rgba8();
    assert_eq!(rendered.get_pixel(11, 9).0, [233, 84, 32, 255]);
    let mut e = shared.lock().unwrap();
    e.undo("artist").unwrap();
    assert_eq!(
        e.doc.layers[0].pixels.rgba(),
        before.layers[0].pixels.rgba()
    );
    assert_eq!((e.doc.layers[0].x, e.doc.layers[0].y), (5, 3));
    e.redo("artist").unwrap();
    assert_eq!(pixel(&e, 11, 9), [233, 84, 32, 255]);
}

#[test]
fn selected_fill_preserves_pixels_outside_selection_and_masks_effects_off_canvas() {
    let (mut e, id) = canvas(8, 6);
    e.doc.layers[0].x = -2;
    e.doc.layers[0].pixels = Raster::new(5, 6);
    e.doc.layers[0].pixels.set(0, 0, [70, 80, 90, 255]);
    e.doc.layers[0].pixels.set(4, 2, [100, 110, 120, 255]);
    edit(&mut e, json!({"op":"mask.add","layer":id,"value":255}));
    edit(
        &mut e,
        json!({"op":"effect.add","layer":id,"kind":"invert"}),
    );
    let effect = e.doc.layers[0].effects[0].id.clone();
    let mask = e.doc.layers[0].mask.as_ref().unwrap().steps[0].id.clone();
    e.doc.selection = Some([3, 1, 7, 4]);
    edit(
        &mut e,
        json!({"op":"paint.fill","layer":id,"color":[30,60,100,255]}),
    );
    assert_eq!(pixel(&e, -2, 0), [70, 80, 90, 255]);
    assert_eq!(pixel(&e, 2, 2), [100, 110, 120, 255]);
    assert_eq!(pixel(&e, 3, 1), [30, 60, 100, 255]);
    assert_eq!(pixel(&e, 6, 3), [30, 60, 100, 255]);
    assert_eq!(pixel(&e, 7, 3), [0; 4]);
    assert_eq!(e.doc.layers[0].effects[0].id, effect);
    assert_eq!(e.doc.layers[0].mask.as_ref().unwrap().steps[0].id, mask);
    let view = e.doc.preview(None, 32, None, false).unwrap().2;
    assert_eq!(
        &view[((1 * 8 + 3) * 4)..((1 * 8 + 3) * 4 + 4)],
        &[225, 195, 155, 255]
    );
}

#[test]
fn mask_fill_is_gray_selection_clipped_cached_and_undoable() {
    let (mut e, id) = canvas(6, 6);
    edit(
        &mut e,
        json!({"op":"paint.fill","layer":id,"color":[120,180,240,255]}),
    );
    edit(&mut e, json!({"op":"mask.add","layer":id,"value":255}));
    let source = e.doc.layers[0].pixels.rgba();
    edit(
        &mut e,
        json!({"op":"mask.step.add","layer":id,"kind":"curves"}),
    );
    let before = e.doc.layers[0]
        .mask
        .as_ref()
        .unwrap()
        .prepare(6, 6)
        .unwrap();
    e.doc.selection = Some([2, 2, 4, 4]);
    edit(
        &mut e,
        json!({"op":"paint.fill","layer":id,"mask":true,"color":[0,180,230,255]}),
    );
    assert_eq!(e.doc.layers[0].pixels.rgba(), source);
    assert_eq!(e.doc.layers[0].mask_value(2, 2), 0.0);
    assert_eq!(e.doc.layers[0].mask_value(1, 2), 1.0);
    let after = e.doc.layers[0]
        .mask
        .as_ref()
        .unwrap()
        .prepare(6, 6)
        .unwrap();
    assert!(!Arc::ptr_eq(&before, &after));
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers[0].mask_value(2, 2), 1.0);
    assert!(Arc::ptr_eq(
        &before,
        &e.doc.layers[0]
            .mask
            .as_ref()
            .unwrap()
            .prepare(6, 6)
            .unwrap()
    ));
    e.redo("human").unwrap();
    assert_eq!(e.doc.layers[0].mask_value(2, 2), 0.0);
}

#[test]
fn fill_respects_locks_read_only_and_selective_reservations_without_partial_changes() {
    let (mut e, id) = canvas(8, 8);
    let fill = json!({"op":"paint.fill","layer":id,"color":[255,50,20,255]});
    e.doc.layers[0].locked = true;
    assert!(e
        .edit("human", &[fill.clone()], None, None, "Fill")
        .is_err());
    e.doc.layers[0].locked = false;
    e.doc.read_only = true;
    assert!(e
        .edit("human", &[fill.clone()], None, None, "Fill")
        .is_err());
    e.doc.read_only = false;
    e.reserve(
        "other",
        "Corner",
        vec![Scope {
            target: Some(id.clone()),
            rect: Some([0, 0, 2, 2]),
        }],
    )
    .unwrap();
    assert!(e
        .edit("human", &[fill.clone()], None, None, "Fill")
        .is_err());
    assert!(e.undo.is_empty());
    assert!(e.doc.layers[0].pixels.tiles.is_empty());
    e.doc.selection = Some([4, 4, 7, 7]);
    e.edit("human", &[fill], None, None, "Fill selection elsewhere")
        .unwrap();
    assert_eq!(pixel(&e, 4, 4), [255, 50, 20, 255]);
    assert_eq!(pixel(&e, 0, 0), [0; 4]);
    let before = serde_json::to_value(&e.doc).unwrap();
    let history = e.undo.len();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"paint.fill","layer":id,"color":[300,0,0,255]})],
            None,
            None,
            "Bad color"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
    assert_eq!(e.undo.len(), history);
}

#[test]
fn tiled_fill_keeps_partial_tile_padding_transparent_after_expansion() {
    let (mut e, id) = canvas(TILE + 1, TILE + 1);
    edit(
        &mut e,
        json!({"op":"paint.fill","layer":id,"color":[40,90,180,255]}),
    );
    let expanded = peerbrush::transform::selection(
        &mut e.doc.layers[0],
        [0, 0, 1, 1],
        &json!({"op":"move","dx":300,"dy":300}),
    )
    .unwrap();
    assert_eq!(expanded, [300, 300, 301, 301]);
    assert_eq!(pixel(&e, 300, 300), [40, 90, 180, 255]);
    assert_eq!(
        pixel(&e, 280, 280),
        [0; 4],
        "padding from the original edge tile must not become real pixels"
    );
}

#[test]
fn rotation_preserves_polygon_and_later_move_leaves_bounding_box_corners_untouched() {
    let (mut e, id) = canvas(32, 24);
    edit(
        &mut e,
        json!({"op":"paint.fill","layer":id,"color":[190,50,140,255]}),
    );
    e.doc.selection = Some([4, 4, 12, 8]);
    edit(
        &mut e,
        json!({"op":"paint.fill","layer":id,"color":[30,180,220,255]}),
    );
    edit(
        &mut e,
        json!({"op":"transform","layer":id,"angle":45,"pivot":[8,6]}),
    );
    let bounds = e.doc.selection.unwrap();
    let polygon = selection::polygon(&e.doc).unwrap().to_vec();
    assert_eq!(polygon.len(), 4);
    assert!(!selection::contains(
        &polygon,
        bounds[0] as f32 + 0.5,
        bounds[1] as f32 + 0.5
    ));
    let protected = (bounds[0], bounds[1]);
    let before = pixel(&e, protected.0, protected.1);
    assert_eq!(before, [190, 50, 140, 255]);
    edit(&mut e, json!({"op":"move","layer":id,"dx":12,"dy":3}));
    assert_eq!(pixel(&e, protected.0, protected.1), before);
    let moved = selection::polygon(&e.doc).unwrap();
    for (before, after) in polygon.iter().zip(moved) {
        assert!((after[0] - before[0] - 12.0).abs() < 0.001);
        assert!((after[1] - before[1] - 3.0).abs() < 0.001);
    }
    assert_eq!(
        e.doc.selection.unwrap(),
        [bounds[0] + 12, bounds[1] + 3, bounds[2] + 12, bounds[3] + 3]
    );
}

#[test]
fn brush_fill_shape_and_gradient_clip_to_exact_rotated_geometry() {
    for op in ["paint.fill", "fill", "shape", "gradient", "paint"] {
        let (mut e, id) = canvas(16, 16);
        let polygon = vec![[8.0, 2.0], [14.0, 8.0], [8.0, 14.0], [2.0, 8.0]];
        edit(&mut e, json!({"op":"selection","polygon":polygon}));
        let command = if op == "paint" {
            json!({"op":op,"layer":id,"points":[[8,8]],"radius":30,"hardness":1,"color":[240,100,30,255]})
        } else {
            json!({"op":op,"layer":id,"rect":[0,0,16,16],"color":[240,100,30,255]})
        };
        edit(&mut e, command);
        assert!(pixel(&e, 8, 8)[3] > 0, "{op} should paint inside selection");
        for y in 0..16 {
            for x in 0..16 {
                if !selection::contains(&polygon, x as f32 + 0.5, y as f32 + 0.5) {
                    assert_eq!(
                        pixel(&e, x, y),
                        [0; 4],
                        "{op} painted outside exact geometry at {x},{y}"
                    );
                }
            }
        }
    }
}

#[test]
fn one_multilayer_rotation_transforms_original_selection_once_for_all_layers() {
    let (mut e, top) = canvas(20, 20);
    e.doc.selection = Some([4, 4, 10, 8]);
    edit(
        &mut e,
        json!({"op":"paint.fill","layer":top,"color":[30,150,220,255]}),
    );
    let mut bottom = Layer::new("Second", "paint", 20, 20);
    bottom.pixels = e.doc.layers[0].pixels.clone();
    let below = bottom.id.clone();
    e.doc.layers.push(bottom);
    e.edit(
        "human",
        &[
            json!({"op":"transform","layer":top,"angle":45,"pivot":[7,6]}),
            json!({"op":"transform","layer":below,"angle":45,"pivot":[7,6]}),
        ],
        None,
        None,
        "Rotate two layers",
    )
    .unwrap();
    assert_eq!(e.doc.layers[0].pixels.rgba(), e.doc.layers[1].pixels.rgba());
    let points = selection::polygon(&e.doc).unwrap();
    let s = 45.0f32.to_radians().sin();
    let c = 45.0f32.to_radians().cos();
    for (before, after) in selection::rectangle([4, 4, 10, 8]).iter().zip(points) {
        let expected = [
            7.0 + (before[0] - 7.0) * c - (before[1] - 6.0) * s,
            6.0 + (before[0] - 7.0) * s + (before[1] - 6.0) * c,
        ];
        assert!((after[0] - expected[0]).abs() < 0.001 && (after[1] - expected[1]).abs() < 0.001);
    }
}

#[test]
fn geometry_is_undoable_serializable_and_reset_by_rectangle_selection() {
    let (mut e, id) = canvas(16, 16);
    e.doc.selection = Some([4, 4, 10, 8]);
    edit(
        &mut e,
        json!({"op":"paint.fill","layer":id,"color":[30,150,220,255]}),
    );
    edit(&mut e, json!({"op":"transform","layer":id,"angle":30}));
    let polygon = e.doc.selection_polygon.clone();
    let bounds = e.doc.selection;
    let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert_eq!(decoded.selection_polygon, polygon);
    assert_eq!(decoded.selection, bounds);
    let state = e.state();
    assert_eq!(state["document"]["selection_polygon"], json!(polygon));
    edit(&mut e, json!({"op":"selection","rect":[0,0,3,3]}));
    assert!(e.doc.selection_polygon.is_none());
    e.undo("human").unwrap();
    assert_eq!(e.doc.selection_polygon, polygon);
    e.redo("human").unwrap();
    assert!(e.doc.selection_polygon.is_none());
    edit(&mut e, json!({"op":"selection","rect":null}));
    assert!(e.doc.selection.is_none() && e.doc.selection_polygon.is_none());
}

#[test]
fn invalid_selection_transform_rolls_back_pixels_geometry_and_history() {
    let (mut e, id) = canvas(12, 12);
    edit(
        &mut e,
        json!({"op":"selection","polygon":[[6,2],[10,6],[6,10],[2,6]]}),
    );
    let before = serde_json::to_value(&e.doc).unwrap();
    let count = e.undo.len();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"transform","layer":id,"scale_x":0})],
            None,
            None,
            "Bad transform"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
    assert_eq!(e.undo.len(), count);
    assert!(e
        .edit(
            "human",
            &[json!({"op":"selection","polygon":[[0,0],[1,1],[2,2]]})],
            None,
            None,
            "Bad polygon"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
}
