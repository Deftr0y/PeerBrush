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
            font: "regular".into(),
            size: 24.,
            line_height: 1.2,
            align: "left".into(),
            color: [12501, 23003, 34507, 65535],
        },
    }
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
            assert_eq!(e.doc.layers[0].source, Some(source.clone()));
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
            assert_eq!(e.doc.layers[0].source, Some(source));
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
    let mut updated = original_source.clone().unwrap();
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
    let mut source = rotated.source.clone().unwrap();
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
