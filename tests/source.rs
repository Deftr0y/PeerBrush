use peerbrush::{
    engine::{Document, Engine, Scope},
    layer_clipboard, psd,
    source::{Content, Source},
};
use serde_json::{json, Value};
fn shape() -> Source {
    Source {
        width: 80,
        height: 64,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: Content::Shape {
            shape: "rectangle".into(),
            bounds: [8., 8., 72., 56.],
            points: vec![[8., 56.], [40., 8.], [72., 56.]],
            closed: true,
            fill: [12345, 23457, 34569, 65535],
            stroke: [60001, 50003, 40007, 65535],
            stroke_width: 2.,
        },
    }
}
fn text() -> Source {
    Source {
        width: 180,
        height: 100,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: Content::Text {
            text: "PeerBrush\nEditable".into(),
            layout: peerbrush::source::TextLayout::Legacy,
            font: "regular".into(),
            size: 24.,
            line_height: 1.2,
            letter_spacing: 0.,
            align: "left".into(),
            color: [12501, 23003, 34507, 65535],
        },
    }
}
fn installed_fixture_font() -> peerbrush::fonts::TextFont {
    peerbrush::fonts::TextFont::Installed(peerbrush::fonts::InstalledFont {
        family: "Synthetic absent font".into(),
        postscript: "PeerBrushSynthetic-Absent".into(),
        style: "normal".into(),
        weight: 400,
        stretch: 5,
        index: 0,
        sha256: "0".repeat(64),
    })
}
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(240, 180, depth).unwrap();
    e
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], Some(e.doc.revision), None, "Source")
        .unwrap();
}
#[test]
fn native_text_shape_projection_preview_psd_and_single_undo_preserve_source() {
    for depth in [8, 16] {
        for source in [text(), shape()] {
            let mut e = fixture(depth);
            let before = e.doc.export_png().unwrap();
            let command = json!({"op":"source.add","source":source,"x":21,"y":19});
            let preview = Engine::preview_edits(e.doc.clone(), &[command.clone()]).unwrap();
            edit(&mut e, command);
            assert_eq!(e.undo.len(), 1);
            assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
            assert_eq!(e.doc.layers[0].source, Some(source.clone().into()));
            assert!(e.doc.layers[0].pixels.bytes() > 0);
            assert_eq!(e.state()["layers"][0]["source"], json!(source));
            if let Content::Shape { .. } = source.content {
                assert_eq!(
                    e.doc.layers[0].pixels.get16(30, 30),
                    if depth == 16 {
                        [12345, 23457, 34569, 65535]
                    } else {
                        [48 * 257, 91 * 257, 135 * 257, 65535]
                    }
                );
            }
            let bytes = psd::encode(&e.doc).unwrap();
            let decoded = psd::decode(&bytes).unwrap();
            assert!(!decoded.read_only);
            assert_eq!(decoded.layers[0].source, e.doc.layers[0].source);
            assert_eq!(decoded.export_png().unwrap(), e.doc.export_png().unwrap());
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), before);
            e.redo("human").unwrap();
            assert_eq!(e.doc.layers[0].source, Some(source.into()));
        }
    }
}
#[test]
fn editable_source_masks_effects_native_clipboard_and_updates_stay_coherent() {
    let mut e = fixture(16);
    edit(&mut e, json!({"op":"source.add","source":shape()}));
    let id = e.doc.layers[0].id.clone();
    edit(&mut e, json!({"op":"mask.add","layer":id,"value":255}));
    edit(
        &mut e,
        json!({"op":"paint","layer":id,"mask":true,"points":[[30,30]],"radius":8,"color":[0,0,0,255]}),
    );
    edit(
        &mut e,
        json!({"op":"effect.add","layer":id,"kind":"hsl","settings":{"hue":20.,"saturation":0.,"lightness":0.}}),
    );
    let original = e.doc.export_png().unwrap();
    let original_source = e.doc.layers[0].source.clone();
    let mut updated = original_source.as_deref().cloned().unwrap();
    if let Content::Shape { fill, .. } = &mut updated.content {
        *fill = [51001, 16003, 8007, 65535];
    }
    let command = json!({"op":"source.update","layer":id,"source":updated});
    let preview = Engine::preview_edits(e.doc.clone(), &[command.clone()]).unwrap();
    edit(&mut e, command);
    assert_eq!(preview.export_png().unwrap(), e.doc.export_png().unwrap());
    assert_ne!(original, e.doc.export_png().unwrap());
    assert!(e.doc.layers[0].mask.is_some());
    assert_eq!(e.doc.layers[0].effects.len(), 1);
    e.undo("human").unwrap();
    assert_eq!(original, e.doc.export_png().unwrap());
    assert_eq!(original_source, e.doc.layers[0].source);
    let snapshot = layer_clipboard::copy(&e.doc, &[id]).unwrap();
    let mut target = Document::new_depth(240, 180, 16).unwrap();
    layer_clipboard::paste(&mut target, &snapshot, "").unwrap();
    assert!(target.layers.iter().any(|l| l.source == original_source));
    let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert_eq!(decoded.export_png().unwrap(), original);
    assert_eq!(decoded.layers[0].source, original_source);
}
#[test]
fn transforms_and_geometry_keep_editable_sources_and_native_precision() {
    let mut e = fixture(16);
    edit(
        &mut e,
        json!({"op":"source.add","source":shape(),"x":21,"y":19}),
    );
    let id = e.doc.layers[0].id.clone();
    let before = e.doc.export_png().unwrap();
    let original = e.doc.layers[0].source.clone();
    edit(
        &mut e,
        json!({"op":"transform","layer":id,"angle":90.,"selection_only":false}),
    );
    let rotated = e.doc.layers[0].clone();
    assert_eq!(
        rotated.source.as_ref().unwrap().content,
        original.as_ref().unwrap().content
    );
    assert_eq!(rotated.pixels.get16(20, 20), [12345, 23457, 34569, 65535]);
    let mut source = rotated.source.as_deref().cloned().unwrap();
    if let Content::Shape { fill, .. } = &mut source.content {
        *fill = [11111, 22223, 33337, 65535];
    }
    edit(
        &mut e,
        json!({"op":"source.update","layer":id,"source":source}),
    );
    assert_eq!(
        e.doc.layers[0].pixels.get16(20, 20),
        [11111, 22223, 33337, 65535]
    );
    e.undo("human").unwrap();
    e.undo("human").unwrap();
    assert_eq!(before, e.doc.export_png().unwrap());
    edit(&mut e, json!({"op":"crop","rect":[10,10,230,170]}));
    assert_eq!(e.doc.layers[0].source, original);
    assert_eq!((e.doc.layers[0].x, e.doc.layers[0].y), (11, 9));
    edit(
        &mut e,
        json!({"op":"image.resize","width":440,"height":320}),
    );
    assert_eq!(e.doc.layers[0].source.as_ref().unwrap().width, 80);
    assert_eq!(
        e.doc.layers[0].source.as_ref().unwrap().matrix,
        [2., 0., 0., 2., 0., 0.]
    );
    assert_eq!(
        e.doc.layers[0].pixels.get16(40, 40),
        [12345, 23457, 34569, 65535]
    );
    let doc = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert_eq!(doc.layers[0].source, e.doc.layers[0].source);
    assert_eq!(doc.export_png().unwrap(), e.doc.export_png().unwrap());
}
#[test]
fn invalid_sources_locked_reserved_stale_and_pixel_edits_are_atomic() {
    let mut e = fixture(16);
    edit(&mut e, json!({"op":"source.add","source":shape()}));
    let id = e.doc.layers[0].id.clone();
    let original = serde_json::to_value(&e.doc).unwrap();
    let revision = e.doc.revision;
    let mut invalid = shape();
    invalid.matrix = [0.; 6];
    for command in [
        json!({"op":"source.update","layer":id,"source":invalid}),
        json!({"op":"source.add","source":{"width":80,"height":64,"content":{"kind":"unknown"}}}),
        json!({"op":"paint","layer":id,"points":[[20,20]],"radius":6}),
        json!({"op":"clone","layer":id,"source":[1,1],"points":[[20,20]]}),
        json!({"op":"source.update","layer":id,"source":shape(),"source_revision":revision-1}),
        json!({"op":"image.place","layer":id,"new_layer":false,"rect":[0,0,10,10],"path":"missing.png"}),
    ] {
        assert!(e
            .edit("human", &[command], Some(revision), None, "Invalid")
            .is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), original);
    }
    let mut unsupported = text();
    if let Content::Text { text, .. } = &mut unsupported.content {
        *text = "שלום".into();
    }
    assert!(unsupported.validate().unwrap_err().contains("shaping"));
    let mut unsupported = text();
    if let Content::Text { font, .. } = &mut unsupported.content {
        *font = "unknown".into();
    }
    assert!(unsupported.validate().is_err());
    e.reserve("agent", "Source", vec![Scope::layer(&id)])
        .unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"source.update","layer":id,"source":shape()})],
            Some(revision),
            None,
            "Reserved"
        )
        .is_err());
    e.leases.clear();
    e.doc.layers[0].locked = true;
    assert!(e
        .edit(
            "human",
            &[json!({"op":"source.rasterize","layer":id})],
            Some(revision),
            None,
            "Locked"
        )
        .is_err());
    e.doc.layers[0].locked = false;
    let png = e.doc.export_png().unwrap();
    edit(&mut e, json!({"op":"source.rasterize","layer":id}));
    assert!(e.doc.layers[0].source.is_none());
    assert_eq!(png, e.doc.export_png().unwrap());
    e.undo("human").unwrap();
    assert!(e.doc.layers[0].source.is_some());
}
#[test]
fn ellipse_paths_fill_stroke_and_text_font_alignment_are_editable() {
    for kind in ["ellipse", "path"] {
        let mut source = shape();
        if let Content::Shape { shape, .. } = &mut source.content {
            *shape = kind.into();
        }
        let p = source.render(80, 64, 16).unwrap();
        assert!(p.bytes() > 0);
        assert_eq!(p.get16(0, 0), [0; 4]);
        assert!(p.get16(40, 30)[3] > 0);
    }
    let mut source = text();
    let original = source.render(180, 100, 16).unwrap();
    if let Content::Text { font, align, .. } = &mut source.content {
        *font = "semibold".into();
        *align = "right".into();
    }
    let changed = source.render(180, 100, 16).unwrap();
    assert_ne!(original, changed);
    assert!(changed
        .rgba16()
        .chunks_exact(4)
        .any(|p| p[3] > 0 && p[3] < 65535));
}

#[test]
fn standard_psd_rasters_match_current_source_without_private_definitions() {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    for depth in [8, 16] {
        for source in [text(), shape()] {
            let mut e = fixture(depth);
            edit(
                &mut e,
                json!({"op":"source.add","source":source,"x":21,"y":19}),
            );
            let bytes = psd::encode(&e.doc).unwrap();
            let start = bytes.windows(4).position(|w| w == b"PBR1").unwrap() + 4;
            let mut json = Vec::new();
            ZlibDecoder::new(&bytes[start..])
                .read_to_end(&mut json)
                .unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&json).unwrap()["format"], 8);
            let color_len = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
            let offset = 30 + color_len;
            let resource_len =
                u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let mut standard = bytes[..offset].to_vec();
            standard.extend_from_slice(&0u32.to_be_bytes());
            standard.extend_from_slice(&bytes[offset + 4 + resource_len..]);
            let decoded = psd::decode(&standard).unwrap();
            assert!(decoded.layers.iter().all(|l| l.source.is_none()));
            assert_eq!(
                decoded.layers[0].pixels.rgba16(),
                e.doc.layers[0].pixels.rgba16()
            );
            assert_eq!(decoded.export_png().unwrap(), e.doc.export_png().unwrap());
        }
    }
}

#[test]
fn text_tracking_preserves_legacy_zero_kerning_alignment_and_explicit_lines() {
    use ab_glyph::{Font, PxScale, ScaleFont};
    use peerbrush::raster::Raster;
    let bounds = |raster: &Raster| {
        let mut left = raster.width;
        let mut right = 0;
        for y in 0..28 {
            for x in 0..raster.width {
                if raster.get16(x as i32, y)[3] > 0 {
                    left = left.min(x);
                    right = right.max(x);
                }
            }
        }
        assert!(left < right);
        [left, right]
    };
    for depth in [8, 16] {
        let mut source = text();
        if let Content::Text { text, .. } = &mut source.content {
            *text = "AV\nV".into();
        }
        let legacy = json!(source);
        assert!(legacy["content"].get("letter_spacing").is_none());
        let mut explicit = legacy.clone();
        explicit["content"]["letter_spacing"] = json!(0.);
        assert_eq!(serde_json::from_value::<Source>(explicit).unwrap(), source);
        assert_eq!(
            serde_json::from_value::<Source>(legacy.clone()).unwrap(),
            source
        );
        for align in ["left", "center", "right"] {
            if let Content::Text {
                align: value,
                letter_spacing,
                ..
            } = &mut source.content
            {
                *value = align.into();
                *letter_spacing = 0.;
            }
            let original = source.render(180, 100, depth).unwrap();
            let old_bounds = bounds(&original);
            for spacing in [-4., 64.] {
                if let Content::Text { letter_spacing, .. } = &mut source.content {
                    *letter_spacing = spacing;
                }
                let tracked = source.render(180, 100, depth).unwrap();
                let new_bounds = bounds(&tracked);
                assert_eq!(
                    new_bounds[1] as i32 - new_bounds[0] as i32,
                    old_bounds[1] as i32 - old_bounds[0] as i32 + spacing as i32
                );
                match align {
                    "left" => assert_eq!(new_bounds[0], old_bounds[0]),
                    "right" => assert_eq!(new_bounds[1], old_bounds[1]),
                    _ => assert_eq!(
                        new_bounds[0] as i32 * 2,
                        old_bounds[0] as i32 * 2 - spacing as i32
                    ),
                }
                // A one-character second line has no trailing tracking and no cross-line spacing.
                for y in 32..100 {
                    for x in 0..180 {
                        assert_eq!(tracked.get16(x, y), original.get16(x, y));
                    }
                }
            }
        }
        let font = peerbrush::source::font("regular").unwrap();
        let scaled = font.as_scaled(PxScale::from(24.));
        let (a, v) = (scaled.glyph_id('A'), scaled.glyph_id('V'));
        // Place the second glyph at an integer position after accounting for the bundled kerning.
        let mut pair = text();
        if let Content::Text {
            text,
            letter_spacing,
            ..
        } = &mut pair.content
        {
            *text = "AV".into();
            *letter_spacing = 64. - (scaled.h_advance(a) + scaled.kern(a, v));
        }
        let mut single = text();
        if let Content::Text { text, .. } = &mut single.content {
            *text = "V".into();
        }
        single.matrix[4] = 64.;
        let actual = pair.render(180, 100, depth).unwrap();
        let expected = single.render(180, 100, depth).unwrap();
        for y in 0..100 {
            for x in 60..180 {
                // Outline translation has f32 roundoff; two native words bound its edge coverage.
                for (actual, expected) in actual.get16(x, y).into_iter().zip(expected.get16(x, y)) {
                    assert!(actual.abs_diff(expected) <= if depth == 16 { 2 } else { 257 });
                }
            }
        }
        for spacing in [-1024., 1024.] {
            if let Content::Text { letter_spacing, .. } = &mut pair.content {
                *letter_spacing = spacing;
            }
            pair.validate().unwrap();
        }
    }
}

fn artwork(doc: &Document) -> Value {
    let mut value = json!(doc);
    value.as_object_mut().unwrap().remove("revision");
    value
}

#[test]
fn text_field_resize_preserves_affine_placement_native_masks_effect_controls_and_history() {
    for depth in [8, 16] {
        for mirrored in [false, true] {
            let mut e = fixture(depth);
            edit(
                &mut e,
                json!({"op":"source.add","source":text(),"x":30,"y":40}),
            );
            let id = e.doc.layers[0].id.clone();
            edit(
                &mut e,
                json!({"op":"transform","layer":id,"angle":90.,"mirror_x":mirrored,"selection_only":false}),
            );
            edit(&mut e, json!({"op":"mask.add","layer":id,"value":255}));
            edit(
                &mut e,
                json!({"op":"mask.link","layer":id,"linked":!mirrored}),
            );
            let layer = &mut e.doc.layers[0];
            let mut pixels = peerbrush::raster::Raster::new_depth(
                layer.pixels.width,
                layer.pixels.height,
                depth,
            );
            pixels.set16(5, 7, [23457, 23457, 23457, 60001]);
            pixels.set16(
                layer.pixels.width as i32 - 2,
                layer.pixels.height as i32 - 2,
                [45003, 45003, 45003, 50007],
            );
            layer
                .mask
                .as_mut()
                .unwrap()
                .steps
                .push(peerbrush::engine::MaskStep {
                    id: "native-paint-mask".into(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 1.,
                    value: 0.,
                    pixels,
                    settings: Value::Null,
                });
            let mut coverage = peerbrush::raster::Raster::new(16, 12);
            for y in 0..12 {
                for x in 0..16 {
                    coverage.set(x, y, [173, 173, 173, 255]);
                }
            }
            let coverage = peerbrush::selection::from_mask(coverage)
                .unwrap()
                .local(-8, -7);
            edit(
                &mut e,
                json!({"op":"effect.add","layer":id,"kind":"liquify","settings":{"radius":4.,"strength":0.5,"strokes":[{"mode":"push","points":[[10.,10.],[14.,10.]],"selection":[8,7,24,19],"polygon":[[8,7],[24,7],[24,19],[8,19]],"coverage":coverage}]}}),
            );
            edit(
                &mut e,
                json!({"op":"effect.add","layer":id,"kind":"hsl","settings":{"hue":12.,"saturation":0.,"lightness":0.}}),
            );
            let before = e.doc.clone();
            let original = before.layers[0].clone();
            let mut source = original.source.as_deref().cloned().unwrap();
            source.width = 220;
            source.height = 150;
            if let Content::Text { letter_spacing, .. } = &mut source.content {
                *letter_spacing = 3.5;
            }
            let command = json!({"op":"source.update","layer":id,"source":source,"document_id":before.id,"source_revision":before.revision});
            let history = e.undo.len();
            let preview = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
            assert_eq!(artwork(&e.doc), artwork(&before));
            assert_eq!(e.undo.len(), history);
            edit(&mut e, command);
            assert_eq!(e.undo.len(), history + 1);
            assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
            assert_eq!(
                e.doc.layers[0].pixels.rgba16(),
                preview.layers[0].pixels.rgba16()
            );
            let resized = &e.doc.layers[0];
            assert!(
                resized.x < original.x,
                "rotation grows into negative projection X"
            );
            if mirrored {
                assert!(resized.y < original.y);
            }
            let retained = resized.source.as_ref().unwrap();
            assert_eq!(retained.content, source.content);
            assert_eq!(
                &retained.matrix[..4],
                &original.source.as_ref().unwrap().matrix[..4]
            );
            for point in [[0., 0.], [60., 20.], [180., 100.]] {
                assert_eq!(
                    peerbrush::path::document_point(&original, point),
                    peerbrush::path::document_point(resized, point)
                );
            }
            let (old_mask, new_mask) = (
                original.mask.as_ref().unwrap(),
                resized.mask.as_ref().unwrap(),
            );
            assert_eq!(old_mask.linked, new_mask.linked);
            assert_eq!(old_mask.enabled, new_mask.enabled);
            for (old, new) in old_mask.steps.iter().zip(&new_mask.steps) {
                let mut old_metadata = json!(old);
                let mut new_metadata = json!(new);
                old_metadata.as_object_mut().unwrap().remove("pixels");
                new_metadata.as_object_mut().unwrap().remove("pixels");
                assert_eq!(old_metadata, new_metadata);
                for y in 0..original.pixels.height as i32 {
                    for x in 0..original.pixels.width as i32 {
                        assert_eq!(
                            old.pixels.get16(x, y),
                            new.pixels
                                .get16(x + original.x - resized.x, y + original.y - resized.y)
                        );
                    }
                }
                assert_eq!(
                    peerbrush::retained::Original::from(&old.pixels)
                        .pixels
                        .rgba16(),
                    peerbrush::retained::Original::from(&new.pixels)
                        .pixels
                        .rgba16()
                );
            }
            assert_eq!(json!(resized.effects[1]), json!(original.effects[1]));
            let (old_effect, new_effect) = (&original.effects[0], &resized.effects[0]);
            assert_eq!(
                (
                    old_effect.id.as_str(),
                    old_effect.enabled,
                    old_effect.weight
                ),
                (
                    new_effect.id.as_str(),
                    new_effect.enabled,
                    new_effect.weight
                )
            );
            let (old_stroke, new_stroke) = (
                &old_effect.settings["strokes"][0],
                &new_effect.settings["strokes"][0],
            );
            for key in ["points", "polygon"] {
                for (old, new) in old_stroke[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .zip(new_stroke[key].as_array().unwrap())
                {
                    assert_eq!(
                        old[0].as_f64().unwrap() + f64::from(original.x),
                        new[0].as_f64().unwrap() + f64::from(resized.x)
                    );
                    assert_eq!(
                        old[1].as_f64().unwrap() + f64::from(original.y),
                        new[1].as_f64().unwrap() + f64::from(resized.y)
                    );
                }
            }
            for axis in 0..4 {
                let (old_origin, new_origin) = if axis % 2 == 0 {
                    (original.x, resized.x)
                } else {
                    (original.y, resized.y)
                };
                assert_eq!(
                    old_stroke["selection"][axis].as_i64().unwrap() + i64::from(old_origin),
                    new_stroke["selection"][axis].as_i64().unwrap() + i64::from(new_origin)
                );
            }
            let shifted: peerbrush::selection::Coverage =
                serde_json::from_value(new_stroke["coverage"].clone()).unwrap();
            assert_eq!(
                coverage.value(12, 10),
                shifted.value(12 + original.x - resized.x, 10 + original.y - resized.y)
            );
            assert_eq!(coverage.mask.rgba(), shifted.mask.rgba());
            let grown = e.doc.clone();
            e.undo("human").unwrap();
            assert_eq!(artwork(&e.doc), artwork(&before));
            e.redo("human").unwrap();
            assert_eq!(artwork(&e.doc), artwork(&grown));
            let mut smaller = e.doc.layers[0].source.as_deref().cloned().unwrap();
            smaller.width = 96;
            smaller.height = 70;
            edit(
                &mut e,
                json!({"op":"source.update","layer":id,"source":smaller}),
            );
            assert_eq!(
                (
                    e.doc.layers[0].x,
                    e.doc.layers[0].y,
                    e.doc.layers[0].pixels.width,
                    e.doc.layers[0].pixels.height
                ),
                (
                    grown.layers[0].x,
                    grown.layers[0].y,
                    grown.layers[0].pixels.width,
                    grown.layers[0].pixels.height
                )
            );
            assert_eq!(json!(e.doc.layers[0].mask), json!(grown.layers[0].mask));
            let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
            assert!(!decoded.read_only);
            assert_eq!(artwork(&decoded), artwork(&e.doc));
            assert_eq!(decoded.export_png().unwrap(), e.doc.export_png().unwrap());
        }
    }
}

#[test]
fn text_font_records_preserve_legacy_strings_and_reject_private_or_malformed_references() {
    for name in ["regular", "medium", "semibold"] {
        let mut source = text();
        if let Content::Text { font, .. } = &mut source.content {
            *font = name.into();
        }
        let value = json!(source);
        assert_eq!(value["content"]["font"], name);
        assert_eq!(serde_json::from_value::<Source>(value).unwrap(), source);
        source.validate_structure().unwrap();
    }
    let mut source = text();
    if let Content::Text { font, .. } = &mut source.content {
        *font = installed_fixture_font();
    }
    source.validate_structure().unwrap();
    let value = json!(source);
    assert!(value["content"]["font"]["sha256"].is_string());
    assert!(value["content"]["font"].get("path").is_none());
    let mut private = value.clone();
    private["content"]["font"]["path"] = json!("/synthetic/private/font.ttf");
    assert!(serde_json::from_value::<Source>(private).is_err());
    for (key, invalid) in [
        ("family", json!("/synthetic/private/font.ttf")),
        ("postscript", json!("")),
        ("style", json!("simulated")),
        ("weight", json!(0)),
        ("stretch", json!(10)),
        ("index", json!(256)),
        ("sha256", json!("A".repeat(64))),
    ] {
        let mut bad = value.clone();
        bad["content"]["font"][key] = invalid;
        assert!(serde_json::from_value::<Source>(bad)
            .unwrap()
            .validate_structure()
            .is_err());
    }
    for unsupported in [
        "שלום",
        "مرحبا",
        "हिन्दी",
        "A\u{0301}",
        "\u{1780}",
        "\u{1000}",
    ] {
        let mut saved = value.clone();
        saved["content"]["text"] = json!(unsupported);
        let saved: Source = serde_json::from_value(saved).unwrap();
        saved.validate_structure().unwrap();
        assert!(saved.validate().unwrap_err().contains("shaping"));
    }
    for unsupported in ["A\tB", "A\u{200d}B", "A\u{202e}B"] {
        let mut bad = value.clone();
        bad["content"]["text"] = json!(unsupported);
        assert!(serde_json::from_value::<Source>(bad)
            .unwrap()
            .validate_structure()
            .is_err());
    }
}

#[test]
fn unavailable_installed_definitions_survive_group_clipboard_and_task_compensation() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        edit(&mut engine, json!({"op":"source.add","source":text()}));
        let id = engine.doc.layers[0].id.clone();
        let other = engine.doc.layers[1].id.clone();
        // A saved fixture's font identity is absent here; its existing projection
        // comes from the real bundled font rather than an invented raster.
        if let Content::Text { font, .. } =
            &mut std::sync::Arc::make_mut(engine.doc.layers[0].source.as_mut().unwrap()).content
        {
            *font = installed_fixture_font();
        }
        let definition = engine.doc.layers[0].source.clone();
        let native = engine.doc.layers[0].pixels.rgba16();
        let task = engine
            .reserve("agent", "Text layer effect", vec![])
            .unwrap()
            .id;
        engine
            .edit(
                "agent",
                &[json!({"op":"effect.add","layer":id,"kind":"invert","settings":{}})],
                Some(engine.doc.revision),
                Some(&task),
                "Agent effect",
            )
            .unwrap();
        edit(
            &mut engine,
            json!({"op":"fill","layer":other,"rect":[0,0,5,5],"color":[53,97,173,255]}),
        );
        edit(
            &mut engine,
            json!({"op":"layer.update","layer":id,"name":"Human retained title"}),
        );
        let human = engine.doc.layers[1].pixels.rgba16();
        engine.undo_task("human", "agent", &task).unwrap();
        assert!(engine.doc.layers[0].effects.is_empty());
        assert_eq!(engine.doc.layers[0].name, "Human retained title");
        assert_eq!(engine.doc.layers[0].source, definition);
        assert_eq!(engine.doc.layers[0].pixels.rgba16(), native);
        assert_eq!(engine.doc.layers[1].pixels.rgba16(), human);
        edit(
            &mut engine,
            json!({"op":"group.create_selected","layers":[id]}),
        );
        let folder = engine.doc.layers[0].id.clone();
        let layer = engine
            .doc
            .layers
            .iter()
            .find(|layer| layer.id == id)
            .unwrap();
        assert_eq!(layer.parent.as_deref(), Some(folder.as_str()));
        assert_eq!(layer.source, definition);
        let clipboard = layer_clipboard::copy(&engine.doc, &[folder]).unwrap();
        let mut target = Document::new_depth(240, 180, depth).unwrap();
        layer_clipboard::paste(&mut target, &clipboard, "").unwrap();
        let pasted = target
            .layers
            .iter()
            .find(|layer| layer.source.is_some())
            .unwrap();
        assert_eq!(pasted.source, definition);
        assert_eq!(pasted.pixels.rgba16(), native);
        for bytes in [
            psd::encode(&engine.doc).unwrap(),
            psd::encode_psb(&engine.doc).unwrap(),
        ] {
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, depth);
            let layer = reopened.layers.iter().find(|layer| layer.id == id).unwrap();
            assert_eq!(layer.source, definition);
            assert_eq!(layer.pixels.rgba16(), native);
            assert_eq!(
                reopened.export_png().unwrap(),
                engine.doc.export_png().unwrap()
            );
        }
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers[0].source, definition);
        assert_eq!(engine.doc.layers[0].pixels.rgba16(), native);
        assert_eq!(engine.doc.layers[1].pixels.rgba16(), human);
    }
}

#[test]
fn text_field_resize_and_tracking_failures_preserve_shared_state_and_nontext_frames() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        edit(&mut e, json!({"op":"source.add","source":text()}));
        let id = e.doc.layers[0].id.clone();
        let mut grown = text();
        grown.width = 220;
        if let Content::Text { letter_spacing, .. } = &mut grown.content {
            *letter_spacing = 8.;
        }
        let good = json!({"op":"source.update","layer":id,"source":grown});
        for guard in ["locked", "reserved", "stale", "document", "read_only"] {
            let mut command = good.clone();
            match guard {
                "locked" => e.doc.layers[0].locked = true,
                "reserved" => {
                    e.reserve("agent", "Reserved text", vec![Scope::layer(&id)])
                        .unwrap();
                }
                "stale" => command["source_revision"] = json!(e.doc.revision - 1),
                "document" => command["document_id"] = json!("another-document"),
                _ => e.doc.read_only = true,
            }
            let before = json!(e.doc);
            let history = (e.undo.len(), e.redo.len());
            assert!(
                e.edit(
                    "human",
                    &[command],
                    Some(e.doc.revision),
                    None,
                    "Refused text edit"
                )
                .is_err(),
                "{guard}"
            );
            assert_eq!(json!(e.doc), before);
            assert_eq!((e.undo.len(), e.redo.len()), history);
            e.doc.layers[0].locked = false;
            e.doc.read_only = false;
            e.leases.clear();
        }
        for spacing in [f32::NAN, f32::INFINITY, -1024.5, 1024.5] {
            let mut invalid = grown.clone();
            if let Content::Text { letter_spacing, .. } = &mut invalid.content {
                *letter_spacing = spacing;
            }
            assert!(invalid.validate().is_err());
        }
        for (width, height, matrix) in [
            (0, 100, [1., 0., 0., 1., 0., 0.]),
            (4001, 1000, [1., 0., 0., 1., 0., 0.]),
            (220, 100, [100., 0., 0., 1., 0., 0.]),
            (220, 100, [1., 0., 0., 1., 100001., 0.]),
        ] {
            let mut invalid = grown.clone();
            invalid.width = width;
            invalid.height = height;
            invalid.matrix = matrix;
            let before = json!(e.doc);
            assert!(e
                .edit(
                    "human",
                    &[json!({"op":"source.update","layer":id,"source":invalid})],
                    None,
                    None,
                    "Invalid frame"
                )
                .is_err());
            assert_eq!(json!(e.doc), before);
        }
        edit(&mut e, json!({"op":"mask.add","layer":id,"value":255}));
        let step = e.doc.layers[0].mask.as_ref().unwrap().steps[0].clone();
        e.doc.layers[0].mask.as_mut().unwrap().steps = (0..32)
            .map(|i| {
                let mut s = step.clone();
                s.id = format!("fill-{i}");
                s
            })
            .collect();
        let mut excessive = grown.clone();
        excessive.width = 2000;
        excessive.height = 2000;
        let before = json!(e.doc.layers[0]);
        assert!(
            peerbrush::source::update(&mut e.doc.layers[0], &json!({"source":excessive}))
                .unwrap_err()
                .contains("128-million")
        );
        assert_eq!(json!(e.doc.layers[0]), before);

        let mut layer = e.doc.layers[0].clone();
        layer.mask = None;
        layer.x = -99999;
        let mut mirrored = text();
        mirrored.matrix = [-1., 0., 0., 1., 180., 0.];
        layer.source = Some(mirrored.clone().into());
        mirrored.width = 182;
        let before = json!(layer);
        assert!(
            peerbrush::source::update(&mut layer, &json!({"source":mirrored}))
                .unwrap_err()
                .contains("coordinate")
        );
        assert_eq!(json!(layer), before);

        let mut layer = e.doc.layers[0].clone();
        layer.mask.as_mut().unwrap().steps.truncate(2);
        let mut mirrored = text();
        mirrored.matrix = [-1., 0., 0., 1., 180., 0.];
        layer.source = Some(mirrored.clone().into());
        layer.effects.push(peerbrush::effects::Effect {
            id: "far-control".into(),
            kind: "liquify".into(),
            enabled: true,
            weight: 1.,
            settings: json!({"strokes":[{"mode":"push","points":[[99990.,10.]]}]}).into(),
        });
        mirrored.width = 220;
        let before = json!(layer);
        // A valid control cannot be shifted beyond its limits; earlier mask reframing stays private.
        assert!(
            peerbrush::source::update(&mut layer, &json!({"source":mirrored}))
                .unwrap_err()
                .contains("liquify coordinate")
        );
        assert_eq!(json!(layer), before);

        for source in [
            shape(),
            Source {
                content: Content::Bezier {
                    nodes: vec![], closed: false, fill: [0;4], stroke: [65535;4], stroke_width: 1.,
                },
                ..shape()
            },
            Source {
                content: Content::Svg {
                    svg: "<svg xmlns='http://www.w3.org/2000/svg' width='80' height='64'><rect width='8' height='8'/></svg>".into(),
                },
                ..shape()
            },
        ] {
            let mut layer = peerbrush::engine::Layer::new("Vector", "paint", source.width, source.height);
            layer.pixels = source.render(source.width, source.height, depth).unwrap();
            layer.source = Some(source.clone().into());
            let before = json!(layer);
            let mut resized = source;
            resized.width += 1;
            assert!(peerbrush::source::update(&mut layer, &json!({"source":resized}))
                .unwrap_err().contains("Only editable text"));
            assert_eq!(json!(layer), before);
        }
    }
}
