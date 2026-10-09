use peerbrush::{
    effects::Effect,
    engine::{id, Document, Engine, Layer, Mask, MaskStep, Scope},
    layer_clipboard, merge, preview, psd,
    raster::{blend, blend16, Raster},
};
use serde_json::json;

fn layer(name: &str, kind: &str, depth: u16, pixels: [[u16; 4]; 2]) -> Layer {
    let mut l = Layer::new(name, kind, 2, 1);
    l.pixels = Raster::new_depth(2, 1, depth);
    for (x, p) in pixels.into_iter().enumerate() {
        l.pixels.set16(x as i32, 0, p);
    }
    l
}
fn op(a: [u16; 4], b: [u16; 4], t: f64, mode: &str, depth: u16) -> [u16; 4] {
    if depth == 16 {
        blend16(a, b, t, mode)
    } else {
        blend(
            a.map(|v| (v / 257) as u8),
            b.map(|v| (v / 257) as u8),
            t as f32,
            mode,
        )
        .map(|v| u16::from(v) * 257)
    }
}
// Independent premultiplied interpolation: group opacity/mask attenuate the
// change to the entire backdrop, rather than every child's opacity separately.
fn mix(a: [u16; 4], b: [u16; 4], t: f64, depth: u16) -> [u16; 4] {
    if depth == 8 {
        let a = a.map(|v| f32::from(v / 257));
        let b = b.map(|v| f32::from(v / 257));
        let t = t as f32;
        let alpha = a[3] * (1. - t) + b[3] * t;
        return std::array::from_fn(|c| {
            let v = if c == 3 {
                alpha
            } else if alpha == 0. {
                0.
            } else {
                (a[c] * a[3] * (1. - t) + b[c] * b[3] * t) / alpha
            };
            v.round() as u16 * 257
        });
    }
    let alpha = a[3] as f64 * (1. - t) + b[3] as f64 * t;
    std::array::from_fn(|c| {
        let v = if c == 3 {
            alpha
        } else if alpha == 0. {
            0.
        } else {
            (a[c] as f64 * a[3] as f64 * (1. - t) + b[c] as f64 * b[3] as f64 * t) / alpha
        };
        v.round() as u16
    })
}
fn mask(l: &mut Layer, depth: u16, values: [u16; 2]) {
    let mut pixels = Raster::new_depth(2, 1, depth);
    for (x, v) in values.into_iter().enumerate() {
        pixels.set16(x as i32, 0, [v, v, v, 65535]);
    }
    l.mask = Some(Mask {
        enabled: true,
        cache_key: id(),
        steps: vec![MaskStep {
            id: id(),
            kind: "paint".into(),
            enabled: true,
            value: 255.,
            pixels,
            settings: json!({}),
        }],
    });
}
fn invert(l: &mut Layer) {
    l.effects.push(Effect {
        id: id(),
        kind: "invert".into(),
        enabled: true,
        settings: json!({}),
    });
}
fn inverse(mut p: [u16; 4]) -> [u16; 4] {
    for c in &mut p[..3] {
        *c = 65535 - *c;
    }
    p
}
fn coverage(l: &Layer, x: i32, depth: u16) -> f64 {
    if depth == 16 {
        f64::from(l.mask.as_ref().unwrap().steps[0].pixels.get16(x, 0)[0]) / 65535.
    } else {
        f64::from(l.mask_value_raw(x, 0))
    }
}
fn render(doc: &Document) -> Vec<u16> {
    if doc.bit_depth == 16 {
        peerbrush::depth16::render(doc).unwrap().words
    } else {
        doc.preview(None, 2, None, false)
            .unwrap()
            .2
            .into_iter()
            .map(|v| u16::from(v) * 257)
            .collect()
    }
}
fn nested(depth: u16) -> Document {
    let mut doc = Document::new(2, 1).unwrap();
    doc.bit_depth = depth;
    let mut outer = layer("Outer", "group", depth, [[0; 4]; 2]);
    outer.blend = "pass_through".into();
    outer.opacity = 0.6;
    mask(&mut outer, depth, [23001, 65535]);
    let mut inner = layer("Inner", "group", depth, [[0; 4]; 2]);
    inner.parent = Some(outer.id.clone());
    inner.blend = "pass_through".into();
    inner.opacity = 0.4;
    let mut ink = layer("Ink", "paint", depth, [[50001, 27003, 17009, 45003]; 2]);
    ink.parent = Some(inner.id.clone());
    ink.blend = "multiply".into();
    let bg = layer(
        "Backdrop",
        "paint",
        depth,
        [[11001, 22003, 37009, 32769], [31003, 42007, 13001, 65535]],
    );
    doc.layers = vec![outer, inner, ink, bg];
    doc
}

#[test]
fn nested_folders_blend_against_external_backdrops_at_native_depth_with_group_masks_and_opacity() {
    for depth in [8, 16] {
        let mut doc = nested(depth);
        let expected: Vec<u16> = (0..2)
            .flat_map(|x| {
                let bg = doc.layers[3].pixels.get16(x, 0);
                let ink = doc.layers[2].pixels.get16(x, 0);
                let inner = mix(
                    bg,
                    op(bg, ink, 1., "multiply", depth),
                    f64::from(doc.layers[1].opacity),
                    depth,
                );
                mix(
                    bg,
                    inner,
                    f64::from(doc.layers[0].opacity) * coverage(&doc.layers[0], x, depth),
                    depth,
                )
            })
            .collect();
        assert_eq!(render(&doc), expected);
        if depth == 8 {
            assert_eq!(
                doc.preview(None, 2, None, false).unwrap().2,
                preview::reference(&doc, 2).unwrap().2
            );
        }
        let copied = layer_clipboard::copy(&doc, &[doc.layers[0].id.clone()]).unwrap();
        assert_eq!(copied.layers[0].blend, "pass_through");
        assert_eq!(copied.layers[1].blend, "pass_through");
        assert_eq!(
            copied.layers[2].pixels.rgba16(),
            doc.layers[2].pixels.rgba16()
        );
        let restored = psd::decode(&psd::encode(&doc).unwrap()).unwrap();
        assert_eq!(render(&restored), expected);
        // A surrounding isolated folder is a boundary even for pass-through descendants.
        doc.layers[0].blend = "normal".into();
        let isolated: Vec<u16> = (0..2)
            .flat_map(|x| {
                let ink = doc.layers[2].pixels.get16(x, 0);
                let inner = mix([0; 4], ink, f64::from(doc.layers[1].opacity), depth);
                op(
                    doc.layers[3].pixels.get16(x, 0),
                    inner,
                    f64::from(doc.layers[0].opacity) * coverage(&doc.layers[0], x, depth),
                    "normal",
                    depth,
                )
            })
            .collect();
        assert_eq!(render(&doc), isolated);
        assert_ne!(isolated, expected);
    }
}

#[test]
fn nested_adjustments_read_prepared_external_layers_and_refresh_after_edits_undo_and_blend_preview()
{
    for depth in [8, 16] {
        let mut e = Engine::new();
        e.doc = nested(depth);
        let mut adjustment = layer("Inner invert", "adjustment", depth, [[0; 4]; 2]);
        adjustment.parent = Some(e.doc.layers[1].id.clone());
        invert(&mut adjustment);
        let mut lower = layer("Outside invert", "adjustment", depth, [[0; 4]; 2]);
        lower.opacity = 0.5;
        invert(&mut lower);
        invert(&mut e.doc.layers[3]);
        e.doc.layers.insert(2, adjustment);
        e.doc.layers.insert(4, lower);
        let expected = |doc: &Document| -> Vec<u16> {
            (0..2)
                .flat_map(|x| {
                    let raw = doc.layers[5].pixels.get16(x, 0);
                    let bg = mix(inverse(raw), raw, 0.5, depth);
                    let changed = inverse(op(
                        bg,
                        doc.layers[3].pixels.get16(x, 0),
                        1.,
                        "multiply",
                        depth,
                    ));
                    let inner = mix(bg, changed, f64::from(doc.layers[1].opacity), depth);
                    mix(
                        bg,
                        inner,
                        f64::from(doc.layers[0].opacity) * coverage(&doc.layers[0], x, depth),
                        depth,
                    )
                })
                .collect()
        };
        let baseline = render(&e.doc);
        assert_eq!(baseline, expected(&e.doc));
        let mut scattered = e.doc.clone();
        scattered.layers = [2, 3, 1, 0, 4, 5].map(|i| e.doc.layers[i].clone()).to_vec();
        assert_eq!(render(&scattered), baseline);
        let top = e.doc.layers[0].id.clone();
        let preview = Engine::preview_edits(
            e.doc.clone(),
            &[json!({"op":"layer.update","layer":top,"blend":"normal"})],
        )
        .unwrap();
        assert_ne!(render(&preview), baseline);
        assert_eq!(render(&e.doc), baseline);
        assert_eq!(e.doc.revision, 0);
        assert!(e.undo.is_empty());
        let bg = e.doc.layers[5].id.clone();
        e.edit(
            "human",
            &[json!({"op":"paint.fill","layer":bg,"color":[23,67,89,255]})],
            None,
            None,
            "Outside backdrop",
        )
        .unwrap();
        assert_eq!(render(&e.doc), expected(&e.doc));
        assert_ne!(render(&e.doc), baseline);
        e.undo("human").unwrap();
        assert_eq!(render(&e.doc), baseline);
        e.redo("human").unwrap();
        assert_eq!(render(&e.doc), expected(&e.doc));
        assert_eq!(
            render(&psd::decode(&psd::encode(&e.doc).unwrap()).unwrap()),
            render(&e.doc)
        );
    }
}

#[test]
fn shared_commands_reject_unsupported_combinations_atomically_and_honor_folder_reservations() {
    let mut e = Engine::new();
    e.doc = nested(16);
    let folder = e.doc.layers[0].id.clone();
    let ink = e.doc.layers[2].id.clone();
    let top = layer(
        "Above folder",
        "paint",
        16,
        [[50001, 17001, 23003, 65535]; 2],
    );
    let top_id = top.id.clone();
    e.doc.layers.insert(0, top);
    let baseline = serde_json::to_value(&e.doc).unwrap();
    for command in [
        json!({"op":"layer.update","layer":ink,"blend":"pass_through"}),
        json!({"op":"effect.add","layer":folder,"kind":"blur"}),
        json!({"op":"layer.clip","layer":top_id,"base":folder}),
    ] {
        assert!(e
            .edit("human", &[command], None, None, "Unsupported combination")
            .is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), baseline);
        assert!(e.undo.is_empty());
    }
    assert!(merge::prepare(&e.doc, &[folder.clone()], None)
        .err()
        .unwrap()
        .contains("isolated"));
    e.reserve("artist", "Folder color", vec![Scope::layer(&folder)])
        .unwrap();
    assert!(e
        .edit(
            "agent",
            &[json!({"op":"layer.update","layer":folder,"blend":"normal"})],
            None,
            None,
            "Folder blend"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), baseline);
    e.leases.clear();
    e.edit(
        "human",
        &[json!({"op":"layer.update","layer":folder,"blend":"normal"})],
        None,
        None,
        "Isolate folder",
    )
    .unwrap();
    e.undo("human").unwrap();
    assert_eq!(
        e.doc.layers.iter().find(|l| l.id == folder).unwrap().blend,
        "pass_through"
    );
}

#[test]
fn dirty_stroke_previews_refresh_pass_through_adjustment_backdrops_at_both_depths() {
    for depth in [8, 16] {
        let mut doc = Document::new(67, 43).unwrap();
        doc.bit_depth = depth;
        let mut group = Layer::new("Pass", "group", 67, 43);
        group.pixels = Raster::new_depth(67, 43, depth);
        group.blend = "pass_through".into();
        let mut adjustment = Layer::new("Invert", "adjustment", 67, 43);
        adjustment.pixels = Raster::new_depth(67, 43, depth);
        adjustment.parent = Some(group.id.clone());
        invert(&mut adjustment);
        let mut bg = Layer::new("Backdrop", "paint", 67, 43);
        bg.pixels = Raster::new_depth(67, 43, depth);
        invert(&mut bg);
        let target = bg.id.clone();
        doc.layers = vec![group, adjustment, bg];
        let mut cache = preview::Cache::default();
        for end in [20., 30., 42.] {
            let command = json!({"op":"paint","layer":target,"points":[[12.,17.],[end,24.]],"radius":4.,"color":[23,77,129,255]});
            let draft = Engine::preview_edits(doc.clone(), &[command]).unwrap();
            let frame = cache
                .render(
                    &draft,
                    "pass-stroke",
                    Some([7, 12, end as i32 + 6, 30]),
                    67,
                    None,
                    false,
                )
                .unwrap();
            assert_eq!(
                frame.bytes,
                draft.preview(None, 67, None, false).unwrap().2,
                "depth {depth}, end {end}"
            );
        }
    }
}
