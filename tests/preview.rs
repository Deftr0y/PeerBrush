use peerbrush::{
    effects::Effect,
    engine::{id, Document, Engine, Layer, Mask, MaskStep},
    preview::{self, Cache},
    raster::Raster,
};
use serde_json::json;

#[test]
fn dirty_stroke_cache_matches_full_render_for_fractional_scales_isolation_and_taper() {
    for (width, height, edge) in [(67, 43, 29), (63, 47, 128), (1024, 513, 384)] {
        let mut source = Document::new(width, height).unwrap();
        let mut backdrop = Layer::new("Backdrop", "fill", width, height);
        backdrop.color = [40, 80, 180, 255];
        source.layers.push(backdrop);
        source.layers[0].mask = Some(Mask {
            enabled: true,
            cache_key: id(),
            steps: vec![
                MaskStep {
                    id: id(),
                    kind: "fill".into(),
                    enabled: true,
                    value: 200.,
                    pixels: Raster::new(width, height),
                    settings: json!({}),
                },
                MaskStep {
                    id: id(),
                    kind: "paint".into(),
                    enabled: true,
                    value: 255.,
                    pixels: Raster::new(width, height),
                    settings: json!({}),
                },
            ],
        });
        let target = source.layers[0].id.clone();
        for (isolate, mask) in [(false, false), (true, false), (true, true)] {
            let mut cache = Cache::default();
            for (i, points) in [
                vec![[10., 20.]],
                vec![[10., 20.], [24., 27.]],
                vec![[10., 20.], [24., 27.], [51., 10.]],
                vec![[35., 18.], [49., 27.]],
            ]
            .into_iter()
            .enumerate()
            {
                let command = json!({"op":"paint","layer":target,"points":points,"radius":5,"mask":mask,"color":[180,90,50,255],"tip":"chalk","density":0.7,"taper_end":8});
                let doc = Engine::preview_edits(source.clone(), &[command]).unwrap();
                let x0 = points.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min) as i32 - 8;
                let x1 = points
                    .iter()
                    .map(|p| p[0])
                    .fold(f32::NEG_INFINITY, f32::max)
                    .ceil() as i32
                    + 8;
                let y0 = points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min) as i32 - 8;
                let y1 = points
                    .iter()
                    .map(|p| p[1])
                    .fold(f32::NEG_INFINITY, f32::max)
                    .ceil() as i32
                    + 8;
                let copied = cache
                    .render(
                        &doc,
                        "gesture",
                        Some([x0, y0, x1, y1]),
                        edge,
                        isolate.then_some(target.as_str()),
                        mask,
                    )
                    .unwrap();
                let expected = doc
                    .preview(None, edge, isolate.then_some(target.as_str()), mask)
                    .unwrap();
                assert_eq!(
                    (copied.width, copied.height, copied.bytes),
                    (expected.0, expected.1, expected.2),
                    "{width}x{height} edge {edge}, frame {i}, isolate {isolate}, mask {mask}"
                );
                assert_eq!(copied.dirty.is_none(), i == 0);
            }
        }
    }
}

#[test]
fn cache_resets_for_baselines_and_unbounded_effects() {
    let mut doc = Document::new(32, 24).unwrap();
    let mut cache = Cache::default();
    cache
        .render(&doc, "gesture", Some([2, 2, 8, 8]), 16, None, false)
        .unwrap();
    doc.layers[0].pixels.set(26, 18, [255; 4]);
    doc.revision += 1;
    let reset = cache
        .render(&doc, "gesture", Some([2, 2, 8, 8]), 16, None, false)
        .unwrap();
    assert!(reset.dirty.is_none());
    assert_eq!(reset.bytes, doc.preview(None, 16, None, false).unwrap().2);
    doc.layers[0].effects.push(Effect {
        id: id(),
        kind: "liquify".into(),
        enabled: true,
        settings: peerbrush::liquify::defaults(),
    });
    assert!(!preview::supports_dirty(&doc));
    assert!(cache
        .render(&doc, "gesture", Some([2, 2, 8, 8]), 16, None, false)
        .unwrap()
        .dirty
        .is_none());
    doc.layers[0].effects.clear();
    doc.layers[0].mask = Some(Mask {
        enabled: false,
        cache_key: id(),
        steps: vec![MaskStep {
            id: id(),
            kind: "blur".into(),
            enabled: true,
            value: 5.,
            pixels: Raster::new(32, 24),
            settings: json!({}),
        }],
    });
    assert!(preview::supports_dirty(&doc));
}

#[test]
fn incremental_engine_previews_match_full_replay_and_reset_settings_baselines_and_masks() {
    let mut base = Document::new(64, 48).unwrap();
    base.layers[0].x = -2;
    base.layers[0].y = 3;
    let target = base.layers[0].id.clone();
    base.layers[0].mask = Some(Mask {
        enabled: true,
        cache_key: id(),
        steps: vec![
            MaskStep {
                id: id(),
                kind: "fill".into(),
                enabled: true,
                value: 220.,
                pixels: Raster::new(64, 48),
                settings: json!({}),
            },
            MaskStep {
                id: id(),
                kind: "paint".into(),
                enabled: true,
                value: 255.,
                pixels: Raster::new(64, 48),
                settings: json!({}),
            },
        ],
    });
    base.selection = Some([8, 8, 52, 40]);
    base.selection_polygon = Some(vec![[8., 24.], [30., 8.], [52., 24.], [30., 40.]]);
    for mask in [false, true] {
        let mut cache = Cache::default();
        for count in [1, 2, 4, 8, 4] {
            let points = (0..count)
                .map(|i| {
                    [
                        10. + i as f32 * 5.,
                        22. + (i as f32 * 0.2).sin() * 6.,
                        0.2 + i as f32 * 0.08,
                    ]
                })
                .collect::<Vec<_>>();
            let commands = [
                json!({"op":"paint","layer":target,"mask":mask,"points":points,"radius":5,"tip":"dry","density":0.65,"taper_end":8,"color":[180,90,40,255]}),
            ];
            let cached = cache.edit(base.clone(), &commands, "stroke").unwrap();
            let expected = Engine::preview_edits(base.clone(), &commands).unwrap();
            assert_eq!(
                cached.preview(None, 64, None, false).unwrap().2,
                expected.preview(None, 64, None, false).unwrap().2,
                "mask {mask} count {count}"
            );
            assert_eq!(
                cached.preview(None, 64, Some(&target), mask).unwrap().2,
                expected.preview(None, 64, Some(&target), mask).unwrap().2
            );
        }
        let commands = [
            json!({"op":"paint","layer":target,"mask":mask,"points":[[14,24],[40,24]],"radius":8,"color":[40,210,80,255]}),
        ];
        let cached = cache.edit(base.clone(), &commands, "stroke").unwrap();
        let expected = Engine::preview_edits(base.clone(), &commands).unwrap();
        assert_eq!(
            cached.preview(None, 64, None, false).unwrap().2,
            expected.preview(None, 64, None, false).unwrap().2
        );
        let mut locked = base.clone();
        locked.layers[0].locked = true;
        locked.revision += 1;
        assert!(cache.edit(locked, &commands, "stroke").is_err());
    }
}

#[test]
fn native_dirty_previews_match_full_projection_with_local_masks_and_fractional_scale() {
    let mut source = Document::new(67, 43).unwrap();
    source.bit_depth = 16;
    source.layers[0].pixels.promote16();
    for y in 0..43 {
        for x in 0..67 {
            source.layers[0].pixels.set16(
                x,
                y,
                [
                    10001 + x as u16,
                    30003 + y as u16,
                    50007,
                    if x > 20 && y < 15 { 37 } else { 44009 },
                ],
            );
        }
    }
    source.layers[0].mask = Some(Mask {
        enabled: true,
        cache_key: id(),
        steps: vec![
            MaskStep {
                id: id(),
                kind: "fill".into(),
                enabled: true,
                value: 200.,
                pixels: Raster::new_depth(67, 43, 16),
                settings: json!({}),
            },
            MaskStep {
                id: id(),
                kind: "paint".into(),
                enabled: true,
                value: 255.,
                pixels: Raster::new_depth(67, 43, 16),
                settings: json!({}),
            },
        ],
    });
    let target = source.layers[0].id.clone();
    let before = source.layers[0].pixels.samples16.clone();
    let points = [
        [10., 20., 0.4],
        [24., 27., 0.8],
        [51., 10., 0.7],
        [55., 33., 0.5],
    ];
    for (isolate, mask) in [(false, false), (true, false), (true, true)] {
        let mut cache = Cache::default();
        cache
            .render(
                &source,
                "native",
                None,
                29,
                isolate.then_some(target.as_str()),
                mask,
            )
            .unwrap();
        for count in [1, 2, 4, 2] {
            let commands = [
                json!({"op":"paint","layer":target,"mask":mask,"points":&points[..count],"radius":5,"flow":0.3,"opacity":0.4,"tip":"chalk","taper_end":7,"color":[180,90,40,255]}),
            ];
            let doc = cache.edit(source.clone(), &commands, "native").unwrap();
            let copied = cache
                .render(
                    &doc,
                    "native",
                    Some([2, 2, 64, 42]),
                    29,
                    isolate.then_some(target.as_str()),
                    mask,
                )
                .unwrap();
            let full = doc
                .preview(None, 29, isolate.then_some(target.as_str()), mask)
                .unwrap();
            assert!(copied.dirty.is_some());
            assert_eq!(
                copied.bytes, full.2,
                "isolate {isolate}, mask {mask}, prefix {count}"
            );
        }
    }
    assert_eq!(source.layers[0].pixels.samples16, before);
}

fn filtered_fixture(depth: u16) -> Document {
    let mut doc = Document::new_depth(199, 131, depth).unwrap();
    let mut folder = Layer::new("Filtered folder", "group", 199, 131);
    folder.effects.push(Effect {
        id: id(),
        kind: "blur".into(),
        enabled: true,
        settings: json!({"radius":2}),
    });
    let mut paint = Layer::new("Offset painting", "paint", 149, 103);
    paint.x = 11;
    paint.y = 7;
    paint.parent = Some(folder.id.clone());
    paint.effects = vec![
        Effect {
            id: id(),
            kind: "levels".into(),
            enabled: true,
            settings: json!({"gamma":0.8}),
        },
        Effect {
            id: id(),
            kind: "bloom".into(),
            enabled: true,
            settings: json!({"threshold":0.1,"spread":3,"strength":0.8}),
        },
    ];
    paint.mask = Some(Mask {
        enabled: true,
        cache_key: id(),
        steps: vec![
            MaskStep {
                id: id(),
                kind: "fill".into(),
                enabled: true,
                value: 170.,
                pixels: Raster::new(149, 103),
                settings: json!({}),
            },
            MaskStep {
                id: id(),
                kind: "paint".into(),
                enabled: true,
                value: 255.,
                pixels: Raster::new(149, 103),
                settings: json!({}),
            },
            MaskStep {
                id: id(),
                kind: "gaussian".into(),
                enabled: true,
                value: 0.,
                pixels: Raster::new(149, 103),
                settings: json!({"radius":2}),
            },
            MaskStep {
                id: id(),
                kind: "curves".into(),
                enabled: true,
                value: 0.,
                pixels: Raster::new(149, 103),
                settings: json!({"points":[[0,0],[0.4,0.7],[1,1]],"interpolation":"smooth"}),
            },
        ],
    });
    let mut adjustment = Layer::new("Clipped tone", "adjustment", 199, 131);
    adjustment.parent = Some(folder.id.clone());
    adjustment.clip_to = Some(paint.id.clone());
    adjustment.effects.push(Effect {
        id: id(),
        kind: "hsl".into(),
        enabled: true,
        settings: json!({"hue":32,"saturation":0.2}),
    });
    let mut backdrop = Layer::new("Backdrop", "fill", 199, 131);
    backdrop.color = [20, 40, 60, 255];
    doc.layers = vec![folder, adjustment, paint, backdrop];
    for layer in &mut doc.layers {
        if depth == 16 {
            layer.pixels.promote16();
            if let Some(mask) = &mut layer.mask {
                for step in &mut mask.steps {
                    step.pixels.promote16();
                }
            }
        }
    }
    for y in 0..103 {
        for x in 0..149 {
            if depth == 16 {
                doc.layers[2].pixels.set16(
                    x,
                    y,
                    [12347 + x as u16 * 173, 33459 + y as u16 * 211, 51237, 42199],
                );
            } else {
                doc.layers[2]
                    .pixels
                    .set(x, y, [45 + x as u8, 95 + y as u8, 180, 164]);
            }
        }
    }
    doc
}

#[test]
fn padded_previews_match_full_nested_effects_clipping_adjustments_and_soft_masks() {
    for depth in [8, 16] {
        for edge in [113, 256] {
            for (isolate, mask) in [(false, false), (true, false), (true, true)] {
                let base = filtered_fixture(depth);
                let target = base.layers[2].id.clone();
                assert!(preview::supports_dirty(&base));
                let original = serde_json::to_value(&base).unwrap();
                let baseline = base
                    .preview(None, edge, isolate.then_some(target.as_str()), mask)
                    .unwrap()
                    .2;
                let mut cache = Cache::default();
                cache
                    .render(
                        &base,
                        "filtered",
                        None,
                        edge,
                        isolate.then_some(target.as_str()),
                        mask,
                    )
                    .unwrap();
                for points in [
                    vec![[29., 32.]],
                    vec![[29., 32.], [57., 42.]],
                    vec![[29., 32.]],
                ] {
                    let command = json!({"op":"paint","layer":target,"mask":mask,"points":points,"radius":3,"color":[255,245,215,210]});
                    let doc = cache.edit(base.clone(), &[command], "filtered").unwrap();
                    let copied = cache
                        .render(
                            &doc,
                            "filtered",
                            Some([24, 27, 62, 47]),
                            edge,
                            isolate.then_some(target.as_str()),
                            mask,
                        )
                        .unwrap();
                    let area = copied.dirty.unwrap();
                    assert!(
                        (area[2] - area[0]) * (area[3] - area[1]) < copied.width * copied.height,
                        "update must remain regional"
                    );
                    assert_eq!(
                        copied.bytes,
                        doc.preview(None, edge, isolate.then_some(target.as_str()), mask)
                            .unwrap()
                            .2,
                        "depth {depth}, edge {edge}, isolate {isolate}, mask {mask}"
                    );
                }
                assert_eq!(
                    serde_json::to_value(&base).unwrap(),
                    original,
                    "derived preview cannot change source"
                );
                assert_eq!(
                    base.preview(None, edge, isolate.then_some(target.as_str()), mask)
                        .unwrap()
                        .2,
                    baseline,
                    "regional buffers cannot overwrite globally cached baseline images"
                );
            }
        }
    }
}

#[test]
fn raw_disabled_small_mask_updates_clamped_edge_strips_at_both_depths() {
    for depth in [8, 16] {
        let mut base = Document::new_depth(137, 93, depth).unwrap();
        let layer = &mut base.layers[0];
        layer.x = 37;
        layer.y = 23;
        layer.pixels = Raster::new_depth(49, 33, depth);
        layer.mask = Some(Mask {
            enabled: false,
            cache_key: id(),
            steps: vec![
                MaskStep {
                    id: id(),
                    kind: "fill".into(),
                    enabled: true,
                    value: 0.,
                    pixels: Raster::new_depth(49, 33, depth),
                    settings: json!({}),
                },
                MaskStep {
                    id: id(),
                    kind: "paint".into(),
                    enabled: true,
                    value: 255.,
                    pixels: Raster::new_depth(49, 33, depth),
                    settings: json!({}),
                },
                MaskStep {
                    id: id(),
                    kind: "blur".into(),
                    enabled: true,
                    value: 4.,
                    pixels: Raster::new_depth(49, 33, depth),
                    settings: json!({}),
                },
            ],
        });
        let target = layer.id.clone();
        let mut cache = Cache::default();
        cache
            .render(&base, "edge", None, 137, Some(&target), true)
            .unwrap();
        for (point, dirty) in [
            ([37., 23.], [33, 19, 41, 27]),
            ([85., 55.], [81, 51, 89, 59]),
            ([61., 39.], [57, 35, 65, 43]),
        ] {
            let doc=cache.edit(base.clone(),&[json!({"op":"paint","layer":target,"mask":true,"points":[point],"radius":3,"color":[255,255,255,255]})],"edge").unwrap();
            let actual = cache
                .render(&doc, "edge", Some(dirty), 137, Some(&target), true)
                .unwrap();
            assert_eq!(
                actual.bytes,
                doc.preview(None, 137, Some(&target), true).unwrap().2,
                "depth {depth}, point {point:?}"
            );
        }
    }
}

#[test]
fn retained_filters_reset_for_changed_layout_settings_and_failed_frames() {
    for depth in [8, 16] {
        let mut doc = filtered_fixture(depth);
        let mut cache = Cache::default();
        cache
            .render(&doc, "metadata", None, 113, None, false)
            .unwrap();
        let command = json!({"op":"effect.update","layer":doc.layers[2].id,"effect":doc.layers[2].effects[1].id,"settings":{"threshold":0.1,"spread":8,"strength":0.8}});
        doc = Engine::preview_edits(doc, &[command]).unwrap();
        let reset = cache
            .render(&doc, "metadata", Some([10, 10, 20, 20]), 113, None, false)
            .unwrap();
        assert!(reset.dirty.is_none());
        assert_eq!(reset.bytes, doc.preview(None, 113, None, false).unwrap().2);
        // Model an invalid draft without changing the baseline revision.
        doc.layers[2].effects[1].settings["spread"] = json!(900);
        for layer in &mut doc.layers {
            layer.effect_key = id();
        }
        assert!(cache
            .render(&doc, "metadata", Some([10, 10, 20, 20]), 113, None, false)
            .is_err());
        doc.layers[2].effects[1].settings["spread"] = json!(8);
        let restored = cache
            .render(&doc, "metadata", Some([10, 10, 20, 20]), 113, None, false)
            .unwrap();
        assert!(restored.dirty.is_none());
        assert_eq!(
            restored.bytes,
            doc.preview(None, 113, None, false).unwrap().2
        );
        doc.layers
            .push(Layer::new("Extra layer", "paint", 199, 131));
        let structural = cache
            .render(&doc, "metadata", Some([10, 10, 20, 20]), 113, None, false)
            .unwrap();
        assert!(structural.dirty.is_none());
        assert_eq!(
            structural.bytes,
            doc.preview(None, 113, None, false).unwrap().2
        );
    }
}
