use peerbrush::{
    effects::Image,
    engine::{Document, Engine},
    liquify,
    raster::Raster,
};
use serde_json::json;

fn gradient() -> Image {
    let mut bytes = vec![];
    for y in 0..64 {
        for x in 0..64 {
            bytes.extend_from_slice(&[x * 3, y * 3, 32, 255]);
        }
    }
    Image {
        width: 64,
        height: 64,
        bytes,
    }
}
fn warp() -> serde_json::Value {
    json!({"radius":12.0,"strength":1.0,"strokes":[{"mode":"push","points":[[24.0,32.0],[36.0,32.0]],"strength":1.0}]})
}

#[test]
fn push_deforms_pixels_in_drag_direction_and_preserves_every_pixel_outside_its_region() {
    let mut image = gradient();
    let source = image.bytes.clone();
    liquify::apply(&mut image, &warp()).unwrap();
    assert!(
        image.get(34, 32)[0] < 34 * 3,
        "Push-right samples pixels from the left"
    );
    for y in 0..64 {
        for x in 0..64 {
            if x < 10 || x > 50 || y < 18 || y > 46 {
                let at = (y * 64 + x) * 4;
                assert_eq!(&image.bytes[at..at + 4], &source[at..at + 4]);
            }
        }
    }
    assert!(image.bytes.chunks_exact(4).all(|p| p[3] == 255));
}
#[test]
fn expand_and_pinch_have_opposite_radial_results() {
    let mut expand = gradient();
    let mut pinch = gradient();
    for (mode, image) in [("expand", &mut expand), ("pinch", &mut pinch)] {
        liquify::apply(
            image,
            &json!({"strokes":[{"mode":mode,"points":[[32.0,32.0]],"radius":18.0,"strength":1.0}]}),
        )
        .unwrap();
    }
    assert!(expand.get(39, 32)[0] < 39 * 3);
    assert!(pinch.get(39, 32)[0] > 39 * 3);
}
#[test]
fn restore_moves_the_warp_back_toward_its_original_pixels() {
    let source = gradient();
    let mut warped = gradient();
    let mut restored = gradient();
    let settings = warp();
    liquify::apply(&mut warped, &settings).unwrap();
    let mut restore = settings.clone();
    restore["strokes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"mode":"restore","points":[[32.5,32.5]],"radius":20.0,"strength":1.0}));
    liquify::apply(&mut restored, &restore).unwrap();
    let distance = |image: &Image| {
        image
            .bytes
            .iter()
            .zip(&source.bytes)
            .map(|(a, b)| a.abs_diff(*b) as u64)
            .sum::<u64>()
    };
    assert!(distance(&restored) < distance(&warped));
    assert_eq!(restored.get(0, 0), source.get(0, 0));
}
#[test]
fn sampling_does_not_mix_hidden_colors_from_transparent_pixels() {
    let mut bytes = vec![0; 64 * 64 * 4];
    for y in 0..64 {
        for x in 0..64 {
            let at = (y * 64 + x) * 4;
            bytes[at..at + 4].copy_from_slice(
                if (25..=35).contains(&x) && (20..=44).contains(&y) {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 0]
                },
            );
        }
    }
    let mut image = Image {
        width: 64,
        height: 64,
        bytes,
    };
    let mut settings = warp();
    settings["strength"] = json!(0.7);
    liquify::apply(&mut image, &settings).unwrap();
    assert!(image.bytes.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
    for pixel in image.bytes.chunks_exact(4).filter(|p| p[3] > 0) {
        assert_eq!(&pixel[..3], &[255, 0, 0]);
    }
}
#[test]
fn polygon_selection_is_frozen_outside_even_with_an_inactive_unrestricted_stroke() {
    let source = gradient();
    let mut image = gradient();
    let polygon = vec![[32.0, 16.0], [48.0, 32.0], [32.0, 48.0], [16.0, 32.0]];
    let settings = json!({"strokes":[
        {"mode":"push","points":[[26.0,32.0],[36.0,32.0]],"radius":24.0,"strength":1.0,"selection":[16,16,48,48],"polygon":polygon},
        {"mode":"push","points":[[-400.0,-400.0],[-350.0,-400.0]],"radius":20.0,"strength":1.0}
    ]});
    liquify::apply(&mut image, &settings).unwrap();
    let mut changed_inside = false;
    for y in 0..64 {
        for x in 0..64 {
            let inside = peerbrush::selection::contains(&polygon, x as f32 + 0.5, y as f32 + 0.5);
            if !inside {
                assert_eq!(
                    image.get(x, y),
                    source.get(x, y),
                    "Pixel {x},{y} outside exact polygon changed"
                );
            } else {
                changed_inside |= image.get(x, y) != source.get(x, y);
            }
        }
    }
    assert!(changed_inside);
}
#[test]
fn adding_a_distant_smaller_brush_keeps_previous_warp_exactly_stable() {
    let mut large = gradient();
    let mut second = gradient();
    let settings = warp();
    liquify::apply(&mut large, &settings).unwrap();
    let mut changed = settings.clone();
    changed["strokes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"mode":"expand","points":[[3.0,3.0]],"radius":1.0,"strength":1.0}));
    liquify::apply(&mut second, &changed).unwrap();
    for y in 18..47 {
        for x in 10..51 {
            assert_eq!(large.get(x, y), second.get(x, y));
        }
    }
}
#[test]
fn identity_amount_and_empty_strokes_leave_source_bytes_exactly_intact() {
    for settings in [
        liquify::defaults(),
        json!({"radius":40.0,"strength":0.0,"strokes":warp()["strokes"]}),
    ] {
        let mut image = gradient();
        let source = image.bytes.clone();
        assert_eq!(liquify::working_bytes(64, 64, &settings).unwrap(), 0);
        liquify::apply(&mut image, &settings).unwrap();
        assert_eq!(image.bytes, source);
    }
}
#[test]
fn tiny_local_working_region_stays_small_on_an_eight_k_canvas_and_invalid_work_is_rejected() {
    assert!(liquify::working_bytes(8192, 4096, &warp()).unwrap() < 256 * 1024);
    let excessive = json!({"strokes":[{"mode":"push","radius":0.5,"points":[[0,0],[8191,4095],[0,0],[8191,4095]]}]});
    assert!(liquify::working_bytes(8192, 4096, &excessive).is_err());
    for invalid in [
        json!({"strokes":[{"mode":true,"points":[[1,2]]}]}),
        json!({"strokes":[{"mode":"push","points":[[1,2,3]]}]}),
        json!({"radius":0}),
        json!({"strength":2}),
    ] {
        assert!(liquify::validate(&invalid).is_err());
    }
}
#[test]
fn editable_liquify_effect_preview_and_commit_match_without_baking_source_pixels() {
    let mut engine = Engine::new();
    engine.doc = Document::new(64, 64).unwrap();
    let source = gradient();
    engine.doc.layers[0].pixels = Raster::from_rgba(64, 64, &source.bytes).unwrap();
    let layer = engine.doc.layers[0].id.clone();
    let commands = [json!({"op":"effect.add","layer":layer,"kind":"liquify","settings":warp()})];
    let preview = Engine::preview_edits(engine.doc.clone(), &commands).unwrap();
    let expected = preview.preview(None, 64, None, false).unwrap().2;
    engine
        .edit("human", &commands, None, None, "Liquify")
        .unwrap();
    assert_eq!(
        engine.doc.preview(None, 64, None, false).unwrap().2,
        expected
    );
    assert_eq!(engine.doc.layers[0].pixels.rgba(), source.bytes);
    let effect = engine.doc.layers[0].effects[0].id.clone();
    let mut modified = warp();
    modified["strokes"][0]["radius"] = json!(6.0);
    engine
        .edit(
            "human",
            &[json!({"op":"effect.update","layer":layer,"effect":effect,"settings":modified})],
            None,
            None,
            "Refine liquify",
        )
        .unwrap();
    assert_ne!(
        engine.doc.preview(None, 64, None, false).unwrap().2,
        expected
    );
    assert_eq!(engine.doc.layers[0].pixels.rgba(), source.bytes);
    engine.undo("human").unwrap();
    assert_eq!(
        engine.doc.preview(None, 64, None, false).unwrap().2,
        expected
    );
    engine.undo("human").unwrap();
    assert!(engine.doc.layers[0].effects.is_empty());
    assert_eq!(engine.doc.layers[0].pixels.rgba(), source.bytes);
}
