use peerbrush::{
    engine::{Document, Engine},
    raster::Raster,
};
use serde_json::json;

fn document(depth: u16) -> Document {
    let mut doc = Document::new_depth(8, 8, depth).unwrap();
    let layer = &mut doc.layers[0];
    layer.id = "paint".into();
    layer.x = -1;
    layer.y = 2;
    layer.pixels = Raster::new_depth(6, 4, depth);
    for y in 0..4 {
        for x in 0..6 {
            if depth == 16 {
                layer.pixels.set16(x, y, [12347, 23459, 34571, 49153]);
            } else {
                layer.pixels.set(x, y, [41, 97, 153, 191]);
            }
        }
    }
    doc
}

#[test]
fn destructive_adjust_respects_rect_selection_intersection_and_native_alpha() {
    for depth in [8, 16] {
        for selected in [false, true] {
            let mut before = document(depth);
            if selected {
                before.selection = Some([2, 3, 4, 5]);
            }
            let after = Engine::preview_edits(
                before.clone(),
                &[json!({"op":"adjust","layer":"paint","rect":[1,2,3,4],"contrast":0})],
            )
            .unwrap();
            for y in 0..4 {
                for x in 0..6 {
                    let gx = x - 1;
                    let gy = y + 2;
                    let hit = (1..3).contains(&gx)
                        && (2..4).contains(&gy)
                        && (!selected || gx == 2 && gy == 3);
                    let mut expected = before.layers[0].pixels.get16(x, y);
                    if hit {
                        expected[..3].fill(if depth == 16 { 32768 } else { 127 * 257 });
                    }
                    assert_eq!(
                        after.layers[0].pixels.get16(x, y),
                        expected,
                        "depth={depth}, selected={selected}, at={gx},{gy}"
                    );
                }
            }
        }
    }
}

#[test]
fn regional_adjust_leaves_another_contributors_reserved_pixels_unchanged() {
    let mut engine = Engine::new();
    engine.resident_state_mut().expect("resident fixture").document = document(16);
    let before = engine.resident_state().expect("resident fixture").document.layers[0].pixels.get16(4, 2);
    engine
        .reserve(
            "artist",
            "Neighboring pixels",
            vec![peerbrush::engine::Scope {
                target: Some("paint".into()),
                rect: Some([3, 4, 5, 6]),
            }],
        )
        .unwrap();
    engine
        .edit(
            "agent",
            &[json!({"op":"adjust","layer":"paint","rect":[1,2,3,4],"contrast":0})],
            Some(0),
            None,
            "Local adjustment",
        )
        .unwrap();
    assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.get16(4, 2), before);
    assert_eq!(
        engine.resident_state().expect("resident fixture").document.layers[0].pixels.get16(2, 0),
        [32768, 32768, 32768, 49153]
    );
}

#[test]
fn destructive_adjust_polygon_and_soft_coverage_are_applied_once() {
    for depth in [8, 16] {
        let mut before = document(depth);
        before.selection = Some([0, 2, 4, 6]);
        before.selection_polygon = Some(vec![[0., 2.], [4., 2.], [0., 6.]]);
        let mut mask = Raster::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                mask.set(x, y, [77, 77, 77, 255]);
            }
        }
        before.selection_coverage = Some(peerbrush::selection::Coverage {
            mask,
            bounds: [0, 2, 4, 6],
            origin: [0, 2],
            contours: vec![],
        });
        let after = Engine::preview_edits(
            before.clone(),
            &[json!({"op":"adjust","layer":"paint","contrast":0})],
        )
        .unwrap();
        // Inside the bounding rectangle but outside the triangle must be exact.
        assert_eq!(
            after.layers[0].pixels.get16(4, 3),
            before.layers[0].pixels.get16(4, 3)
        );
        let old = before.layers[0].pixels.get16(1, 0);
        let new = after.layers[0].pixels.get16(1, 0);
        let amount = f64::from(77.0f32 / 255.0);
        for c in 0..3 {
            let target = if depth == 16 { 32768 } else { 127 * 257 };
            let blended =
                (f64::from(old[c]) * (1. - amount) + f64::from(target) * amount).round() as u16;
            let expected = if depth == 16 {
                blended
            } else {
                u16::from(peerbrush::raster::project16(blended)) * 257
            };
            assert_eq!(new[c], expected);
        }
        assert_eq!(new[3], old[3]);
    }
}

#[test]
fn default_adjust_is_exact_and_sparse_holes_padding_and_alpha_are_preserved() {
    for depth in [8, 16] {
        let mut before = document(depth);
        let raster = &mut before.layers[0].pixels;
        raster.width = 520;
        raster.set16(3, 3, [0; 4]);
        raster.set16(519, 0, [7777, 17777, 27777, 0]);
        let identical =
            Engine::preview_edits(before.clone(), &[json!({"op":"adjust","layer":"paint"})])
                .unwrap();
        assert_eq!(
            identical.layers[0].pixels.tiles,
            before.layers[0].pixels.tiles
        );
        assert_eq!(
            identical.layers[0].pixels.samples16,
            before.layers[0].pixels.samples16
        );
        let after = Engine::preview_edits(
            before.clone(),
            &[json!({"op":"adjust","layer":"paint","contrast":0})],
        )
        .unwrap();
        assert_eq!(after.layers[0].pixels.get16(300, 0), [0; 4]);
        assert_eq!(after.layers[0].pixels.get16(3, 3), [0; 4]);
        assert_eq!(after.layers[0].pixels.get16(519, 0)[3], 0);
        let padding = (5 * 256 + 8) * 4;
        if depth == 16 {
            assert_eq!(
                &after.layers[0].pixels.samples16[&(2, 0)][padding..padding + 4],
                &[0; 4]
            );
        } else {
            assert_eq!(
                &after.layers[0].pixels.tiles[&(2, 0)][padding..padding + 4],
                &[0; 4]
            );
        }
    }
}
