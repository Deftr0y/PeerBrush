use peerbrush::{
    clipboard::{self, Image},
    engine::{self, Document, Engine, Layer, Mask, MaskStep},
    layer_clipboard,
    raster::Raster,
};
use serde_json::json;

fn selected_document() -> (Document, String) {
    let mut doc = Document::new(32, 24).unwrap();
    let layer = &mut doc.layers[0];
    layer.x = 2;
    layer.y = -1;
    layer.opacity = 0.75;
    for y in 0..24 {
        for x in 0..32 {
            layer
                .pixels
                .set(x, y, [20 + x as u8, 50 + y as u8, 180, 217]);
        }
    }
    layer.mask = Some(Mask {
        enabled: true,
        cache_key: engine::id(),
        steps: vec![MaskStep {
            id: engine::id(),
            kind: "fill".into(),
            enabled: true,
            value: 128.,
            pixels: Raster::new(32, 24),
            settings: json!({}),
        }],
    });
    let target = layer.id.clone();
    let mut backdrop = Layer::new("Backdrop", "paint", 32, 24);
    for y in 0..24 {
        for x in 0..32 {
            backdrop.pixels.set(x, y, [91, 71, 50, 255]);
        }
    }
    doc.layers.push(backdrop);
    doc.selection = Some([-3, 1, 15, 18]);
    doc.selection_polygon = Some(vec![[6.2, 1.35], [14.5, 9.65], [6.2, 17.95], [-2.1, 9.65]]);
    (doc, target)
}

#[test]
fn rotated_pixel_copy_preserves_inside_color_mask_and_merged_pixels_and_zeroes_outside() {
    let (doc, target) = selected_document();
    let mut rectangle = doc.clone();
    rectangle.selection_polygon = None;
    for (mask, merged) in [(false, false), (true, false), (false, true)] {
        let expected = Image::copy(&rectangle, &target, mask, merged).unwrap();
        let copied = clipboard::copy_document_pixels(&doc, &target, mask, merged).unwrap();
        assert_eq!(
            (copied.width, copied.height, copied.origin),
            (15, 17, Some([0, 1]))
        );
        let mut kept = 0;
        let mut removed = 0;
        for y in 0..copied.height {
            for x in 0..copied.width {
                let at = ((y * copied.width + x) * 4) as usize;
                let inside = (x as f32 + 0.5 - 6.2).abs() + (y as f32 + 1.5 - 9.65).abs() < 8.3;
                if inside {
                    assert_eq!(
                        &copied.bytes[at..at + 4],
                        &expected.bytes[at..at + 4],
                        "mask {mask}, merged {merged}, ({x},{y})"
                    );
                    kept += 1;
                } else {
                    assert_eq!(
                        &copied.bytes[at..at + 4],
                        &[0; 4],
                        "unselected RGBA leaked at ({x},{y})"
                    );
                    removed += 1;
                }
            }
        }
        assert!(kept > 0 && removed > 0);
    }
}

#[test]
fn both_pixel_and_whole_layer_paste_clear_polygon_and_pixel_undo_restores_it() {
    let (doc, target) = selected_document();
    let image = Image::copy(&doc, &target, false, false).unwrap();
    let mut engine = Engine::new();
    engine.doc = doc.clone();
    engine
        .edit(
            "human",
            &[image.command(&target).unwrap()],
            None,
            None,
            "Paste",
        )
        .unwrap();
    assert!(engine.doc.selection.is_none());
    assert!(engine.doc.selection_polygon.is_none());
    engine.undo("human").unwrap();
    assert_eq!(engine.doc.selection, doc.selection);
    assert_eq!(engine.doc.selection_polygon, doc.selection_polygon);

    let snapshot = layer_clipboard::copy(&doc, &[target.clone()]).unwrap();
    let mut pasted = doc;
    layer_clipboard::paste(&mut pasted, &snapshot, &target).unwrap();
    assert!(pasted.selection.is_none());
    assert!(pasted.selection_polygon.is_none());
}
