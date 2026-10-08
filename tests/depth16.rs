use peerbrush::{
    depth16::{self, color, Image16},
    effects,
    engine::{id, Document, Engine, Layer, Mask, MaskStep},
    raster::{blend16, Raster},
};
use serde_json::json;
use std::{collections::HashSet, sync::Arc};
fn image(pixels: &[[u16; 4]]) -> Image16 {
    Image16 {
        width: pixels.len() as u32,
        height: 1,
        words: pixels.iter().flatten().copied().collect(),
    }
}
fn paint_mask(width: u32, height: u32, value: u16) -> Mask {
    let words: Vec<u16> = (0..width * height)
        .flat_map(|_| [value, value, value, 65535])
        .collect();
    Mask {
        enabled: true,
        cache_key: id(),
        steps: vec![
            MaskStep {
                id: id(),
                kind: "fill".into(),
                enabled: true,
                value: 0.0,
                pixels: Raster::new_depth(width, height, 16),
                settings: json!({}),
            },
            MaskStep {
                id: id(),
                kind: "paint".into(),
                enabled: true,
                value: 255.0,
                pixels: Raster::from_rgba16(width, height, &words).unwrap(),
                settings: json!({}),
            },
        ],
    }
}
#[test]
fn render_crop_and_clipboard_preview_retain_values_hidden_by_display_projection() {
    let mut doc = Document::new_depth(3, 1, 16).unwrap();
    let pixels = [
        [12345, 12346, 30001, 65535],
        [12347, 12348, 30002, 65535],
        [65001, 101, 30003, 1],
    ];
    doc.layers[0].pixels =
        Raster::from_rgba16(3, 1, &pixels.iter().flatten().copied().collect::<Vec<_>>()).unwrap();
    assert_eq!(
        depth16::render(&doc).unwrap().words,
        pixels.iter().flatten().copied().collect::<Vec<_>>()
    );
    let crop = depth16::render_crop(&doc, [1, 0, 3, 1]).unwrap();
    assert_eq!(crop.get(0, 0), pixels[1]);
    assert_eq!(crop.get(1, 0), pixels[2]);
    let (_, _, native, rect) =
        depth16::preview16(&doc, None, 3, Some(&doc.layers[0].id), false).unwrap();
    assert_eq!(rect, [0, 0, 3, 1]);
    assert_eq!(native, doc.layers[0].pixels.rgba16());
    let display = doc.preview(None, 3, None, false).unwrap().2;
    assert_eq!(&display[8..12], &[0, 0, 0, 0]);
    assert_ne!(doc.layers[0].pixels.get16(2, 0), [0; 4]);
}
#[test]
fn all_numeric_effects_operate_on_native_words_and_keep_alpha_and_hidden_colors() {
    let source = [
        [12345, 30123, 49999, 123],
        [77, 1234, 5678, 0],
        [65535, 0, 0, 65535],
    ];
    for kind in ["levels", "curves", "adjust", "hsl", "color_balance"] {
        let mut im = image(&source);
        color::apply(&mut im, kind, &effects::defaults(kind)).unwrap();
        assert_eq!(im.words, image(&source).words, "{kind} identity");
    }
    let mut im = image(&source);
    color::apply(&mut im, "invert", &json!({})).unwrap();
    assert_eq!(
        im.get(0, 0),
        [65535 - 12345, 65535 - 30123, 65535 - 49999, 123]
    );
    assert_eq!(im.get(1, 0), source[1]);
    color::apply(&mut im, "invert", &json!({})).unwrap();
    assert_eq!(im.words, image(&source).words);
    let mut gradient = image(
        &(0..8192)
            .map(|n| [12000 + n, 18000 + n, 20000 + n, 65535])
            .collect::<Vec<_>>(),
    );
    color::apply(&mut gradient, "levels", &json!({"gamma":1.6})).unwrap();
    assert!(
        gradient
            .words
            .chunks_exact(4)
            .map(|p| p[0])
            .collect::<HashSet<_>>()
            .len()
            > 4000
    );
    assert!(gradient.words.chunks_exact(4).any(|p| p[0] % 257 != 0));
    let mut primary = image(&[[65535, 0, 0, 32123]]);
    color::apply(&mut primary, "hsl", &json!({"hue":120})).unwrap();
    assert_eq!(primary.get(0, 0), [0, 65535, 0, 32123]);
    let mut gray = image(&source);
    color::apply(&mut gray, "grayscale", &json!({})).unwrap();
    let expected = (12345.0_f64 * 0.2126 + 30123.0 * 0.7152 + 49999.0 * 0.0722).round() as u16;
    assert_eq!(gray.get(0, 0), [expected, expected, expected, 123]);
}
fn brute_blur(input: &Image16, sigma: f32) -> Vec<u16> {
    let (w, h) = (input.width as usize, input.height as usize);
    let mut values: Vec<[u64; 4]> = input
        .words
        .chunks_exact(4)
        .map(|p| {
            [
                u64::from(p[0]) * u64::from(p[3]),
                u64::from(p[1]) * u64::from(p[3]),
                u64::from(p[2]) * u64::from(p[3]),
                u64::from(p[3]) * 65535,
            ]
        })
        .collect();
    for radius in effects::gaussian_radii(sigma) {
        if radius == 0 {
            continue;
        }
        for vertical in [false, true] {
            let source = values.clone();
            let count = (radius * 2 + 1) as u64;
            for y in 0..h {
                for x in 0..w {
                    for c in 0..4 {
                        let mut sum = 0;
                        for d in -(radius as isize)..=radius as isize {
                            let sx = if vertical {
                                x
                            } else {
                                (x as isize + d).clamp(0, w as isize - 1) as usize
                            };
                            let sy = if vertical {
                                (y as isize + d).clamp(0, h as isize - 1) as usize
                            } else {
                                y
                            };
                            sum += source[sy * w + sx][c];
                        }
                        values[y * w + x][c] = (sum + count / 2) / count;
                    }
                }
            }
        }
    }
    values
        .iter()
        .flat_map(|v| {
            let alpha = ((v[3] + 32767) / 65535).min(65535) as u16;
            let rgb = std::array::from_fn::<_, 3, _>(|c| {
                if alpha == 0 || v[3] == 0 {
                    0
                } else {
                    ((v[c] * 65535 + v[3] / 2) / v[3]).min(65535) as u16
                }
            });
            [rgb[0], rgb[1], rgb[2], alpha]
        })
        .collect()
}
#[test]
fn native_gaussian_matches_independent_premultiplied_reference_including_faint_alpha() {
    for (w, h) in [(1, 1), (1, 7), (9, 1), (9, 7)] {
        let words: Vec<u16> = (0..w * h)
            .flat_map(|n| {
                [
                    12001 + (n * 317) as u16,
                    45678,
                    32123,
                    [0, 1, 33, 12345, 65535][n as usize % 5],
                ]
            })
            .collect();
        let source = Image16 {
            width: w,
            height: h,
            words,
        };
        for sigma in [0.5, 1.0, 8.0, 64.0] {
            let mut actual = source.clone();
            color::apply(&mut actual, "blur", &json!({"radius":sigma})).unwrap();
            assert_eq!(
                actual.words,
                brute_blur(&source, sigma),
                "{w}x{h}, sigma{sigma}"
            );
        }
    }
    let mut edge = image(&[[65535, 30001, 10001, 1], [0, 0, 0, 0], [0, 0, 0, 0]]);
    color::apply(&mut edge, "blur", &json!({"radius":1.0})).unwrap();
    for p in edge.words.chunks_exact(4).filter(|p| p[3] > 0) {
        assert_eq!(p[0], 65535);
        assert!(p[1].abs_diff(30001) <= 1);
    }
    assert!(
        color::working_bytes(4096, 4096, "blur", &json!({"radius":64})).unwrap()
            <= depth16::WORKING_BUDGET
    );
}
#[test]
fn local_and_spatial_masks_keep_native_precision_and_disabled_raw_source() {
    let mut doc = Document::new_depth(3, 1, 16).unwrap();
    for x in 0..3 {
        doc.layers[0]
            .pixels
            .set16(x, 0, [12345, 30001, 50003, 65535]);
    }
    doc.layers[0].mask = Some(paint_mask(3, 1, 12346));
    assert!(
        depth16::prepare_masks(&doc).unwrap()[0].is_none(),
        "local masks stay lazy during drags"
    );
    assert_eq!(
        depth16::render(&doc).unwrap().get(0, 0),
        [12345, 30001, 50003, 12346]
    );
    let spatial = MaskStep {
        id: id(),
        kind: "gaussian".into(),
        enabled: true,
        value: 1.0,
        pixels: Raster::new_depth(3, 1, 16),
        settings: json!({"radius":1.0}),
    };
    doc.layers[0].mask.as_mut().unwrap().steps.push(spatial);
    doc.layers[0].mask.as_mut().unwrap().cache_key = id();
    let masks = depth16::prepare_masks(&doc).unwrap();
    assert_eq!(masks[0].as_ref().unwrap().get(0, 0), 12346);
    doc.layers[0].mask.as_mut().unwrap().enabled = false;
    assert_eq!(depth16::render(&doc).unwrap().get(0, 0)[3], 65535);
    assert_eq!(
        depth16::mask_image(&doc, 0).unwrap().unwrap().get(0, 0),
        [12346, 12346, 12346, 65535]
    );
}
#[test]
fn clipped_adjustments_apply_native_base_mask_opacity_and_folder_scope_once() {
    let mut doc = Document::new_depth(1, 1, 16).unwrap();
    let mut group = Layer::new("Folder", "group", 1, 1);
    group.pixels.promote16();
    group.opacity = 0.75;
    group.mask = Some(paint_mask(1, 1, 50001));
    let mut base = doc.layers.remove(0);
    base.parent = Some(group.id.clone());
    base.opacity = 0.5;
    base.mask = Some(paint_mask(1, 1, 30001));
    base.pixels.set16(0, 0, [12345, 30123, 49999, 31001]);
    let mut adjustment = Layer::new("Invert", "adjustment", 1, 1);
    adjustment.parent = Some(group.id.clone());
    adjustment.clip_to = Some(base.id.clone());
    adjustment.pixels.promote16();
    adjustment.effects.push(effects::Effect {
        id: id(),
        kind: "invert".into(),
        enabled: true,
        settings: json!({}),
    });
    let mut background = Layer::new("Backdrop", "paint", 1, 1);
    background.pixels = Raster::from_rgba16(1, 1, &[33001, 22001, 11001, 65535]).unwrap();
    doc.layers = vec![group, adjustment, base, background];
    let unit = [65535 - 12345, 65535 - 30123, 65535 - 49999, 31001];
    let inside = blend16([0; 4], unit, 0.5 * 30001.0 / 65535.0, "normal");
    let expected = blend16(
        [33001, 22001, 11001, 65535],
        inside,
        0.75 * 50001.0 / 65535.0,
        "normal",
    );
    assert_eq!(depth16::render(&doc).unwrap().get(0, 0), expected);
    assert_eq!(
        depth16::layer_image(&doc, 1).unwrap().get(0, 0),
        unit,
        "isolated adjustment retains native base alpha before masking"
    );
}
#[test]
fn native_effect_cache_distinguishes_live_drafts_and_keeps_history_sources_untouched() {
    let mut engine = Engine::new();
    engine.doc = Document::new_depth(4, 1, 16).unwrap();
    let id = engine.doc.layers[0].id.clone();
    for x in 0..4 {
        engine.doc.layers[0]
            .pixels
            .set16(x, 0, [12345 + x as u16, 30001, 50003, 65535]);
    }
    engine
        .edit(
            "human",
            &[json!({"op":"effect.add","layer":id,"kind":"levels","settings":{"gamma":1.4}})],
            None,
            None,
            "Levels",
        )
        .unwrap();
    let source = engine.doc.layers[0].pixels.rgba16();
    let baseline = depth16::render(&engine.doc).unwrap().words;
    let cached = depth16::prepare(&engine.doc).unwrap()[0]
        .as_ref()
        .unwrap()
        .clone();
    let effect = engine.doc.layers[0].effects[0].id.clone();
    let draft = Engine::preview_edits(
        engine.doc.clone(),
        &[json!({"op":"effect.update","layer":id,"effect":effect,"settings":{"gamma":2.1}})],
    )
    .unwrap();
    assert_eq!(draft.revision, engine.doc.revision);
    assert_ne!(depth16::render(&draft).unwrap().words, baseline);
    assert_eq!(depth16::render(&engine.doc).unwrap().words, baseline);
    let again = depth16::prepare(&engine.doc).unwrap()[0]
        .as_ref()
        .unwrap()
        .clone();
    assert!(Arc::ptr_eq(&cached, &again));
    assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
}
#[test]
fn native_liquify_and_bloom_never_round_sources_through_bytes() {
    let mut image = Image16 {
        width: 64,
        height: 64,
        words: (0..64 * 64)
            .flat_map(|n| {
                [
                    30000 + (n % 64) as u16,
                    40000 + (n / 64) as u16,
                    12345,
                    65535,
                ]
            })
            .collect(),
    };
    let source = image.clone();
    let settings =
        json!({"strokes":[{"mode":"push","points":[[24,32],[36,32]],"radius":12,"strength":1}]});
    color::apply(&mut image, "liquify", &settings).unwrap();
    assert!(image.get(34, 32)[0] < source.get(34, 32)[0]);
    assert_eq!(image.get(0, 0), source.get(0, 0));
    assert!(image.words.chunks_exact(4).any(|p| p[0] % 257 != 0));
    let mut light = super_image();
    let hidden = light.get(1, 0);
    color::apply(
        &mut light,
        "bloom",
        &json!({"threshold":0.7,"spread":0,"strength":0.5}),
    )
    .unwrap();
    assert!(light.get(0, 0)[0] > 50001);
    assert_eq!(light.get(1, 0), hidden);
    assert!(light.get(0, 0)[0] % 257 != 0);
}
fn super_image() -> Image16 {
    image(&[[50001, 51002, 52003, 65535], [11, 12, 13, 0]])
}
