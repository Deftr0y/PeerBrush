use peerbrush::{
    engine::{Document, Engine, Scope},
    layer_clipboard,
    source::{Content, Source, TextLayout},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn source() -> Source {
    Source {
        width: 160,
        height: 72,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: Content::Text {
            text: "office a\u{0301}".into(),
            layout: TextLayout::Shaped,
            font: "regular".into(),
            size: 28.,
            line_height: 1.2,
            letter_spacing: 0.,
            align: "left".into(),
            color: [12501, 23003, 34507, 60001],
        },
    }
}
fn snapshot(engine: &Engine) -> Value {
    json!({"doc":engine.doc,"png":engine.doc.export_png().unwrap(),"undo":engine.undo.len(),"redo":engine.redo.len(),"saved":engine.saved_revision})
}
#[test]
fn shaped_source_protocol_preview_native_clipboard_and_guards_share_one_history_path() {
    use base64::Engine as _;
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(220, 120, depth).unwrap();
        let original_png = engine.doc.export_png().unwrap();
        let original = snapshot(&engine);
        let command = json!({"op":"source.add","source":source(),"x":9,"y":13});
        let expected = Engine::preview_edits(engine.doc.clone(), &[command.clone()]).unwrap();
        assert_eq!(snapshot(&engine), original);
        let shared = Arc::new(Mutex::new(engine));
        let reply = peerbrush::server::dispatch(&shared,"edit",&json!({"actor":"human","commands":[command],"expected_revision":0,"feedback":"always","max_edge":220})).unwrap();
        let observed =
            peerbrush::server::dispatch(&shared, "observe", &json!({"image": false})).unwrap();
        assert_eq!(
            observed["state"]["layers"][0]["source"]["content"]["layout"],
            "shaped"
        );
        assert_eq!(reply["images"][0]["document_rect"], json!([0, 0, 220, 120]));
        let actual_png = base64::engine::general_purpose::STANDARD
            .decode(reply["images"][0]["data"].as_str().unwrap())
            .unwrap();
        let (width, height, rgba, rect) = expected.preview(None, 220, None, false).unwrap();
        let feedback =
            image::load_from_memory_with_format(&actual_png, image::ImageFormat::Png).unwrap();
        assert_eq!(feedback.color(), image::ColorType::Rgba8);
        assert_eq!((feedback.width(), feedback.height()), (width, height));
        assert_eq!(feedback.as_bytes(), rgba.as_slice());
        assert_eq!(reply["images"][0]["mime_type"], "image/png");
        assert_eq!(reply["images"][0]["width"], json!(width));
        assert_eq!(reply["images"][0]["height"], json!(height));
        assert_eq!(reply["images"][0]["document_rect"], json!(rect));
        assert_eq!(reply["images"][0]["revision"], json!(1));
        assert_eq!(reply["revision"], json!(1));
        let mut engine = shared.lock().unwrap();
        assert_eq!(
            engine.doc.export_png().unwrap(),
            expected.export_png().unwrap()
        );
        assert_eq!(engine.undo.len(), 1);
        assert_eq!(engine.doc.layers[0].pixels.depth, depth);
        let id = engine.doc.layers[0].id.clone();
        let clip = layer_clipboard::copy(&engine.doc, &[id.clone()]).unwrap();
        let mut destination = Document::new_depth(220, 120, depth).unwrap();
        let mut original_destination = destination.layers[0].clone();
        let pasted = layer_clipboard::paste(&mut destination, &clip, "").unwrap();
        assert_eq!(pasted.len(), 1);
        let copied = destination
            .layers
            .iter()
            .find(|layer| layer.id == pasted[0])
            .unwrap();
        assert_eq!(copied.source, Some(source().into()));
        assert_eq!(copied.pixels.depth, depth);
        assert_eq!(
            (copied.x, copied.y),
            (engine.doc.layers[0].x, engine.doc.layers[0].y)
        );
        assert_eq!(copied.pixels.rgba16(), engine.doc.layers[0].pixels.rgba16());
        // Paste wraps the original top-level paint destination; returned IDs
        // identify copied roots, independently of their position in the stack.
        let parent = copied.parent.as_ref().expect("Paste destination wrapper");
        let wrapper = destination
            .layers
            .iter()
            .find(|layer| &layer.id == parent)
            .unwrap();
        assert_eq!(wrapper.kind, "group");
        assert!(wrapper.parent.is_none());
        let retained = destination
            .layers
            .iter()
            .find(|layer| layer.id == original_destination.id)
            .unwrap();
        original_destination.parent = Some(parent.clone());
        assert_eq!(json!(retained), json!(original_destination));
        let before = snapshot(&engine);
        let mut changed = source();
        if let Content::Text { text, .. } = &mut changed.content {
            *text = "a\u{0301} office".into();
        }
        let update = json!({"op":"source.update","layer":id,"source":changed});
        engine
            .reserve("other", "Protect text", vec![Scope::layer(&id)])
            .unwrap();
        assert!(engine
            .edit("human", &[update.clone()], Some(1), None, "Reserved")
            .is_err());
        assert_eq!(snapshot(&engine), before);
        engine.leases.clear();
        // Snapshot-derived properties use exact source guards in addition to
        // expected_revision's optimistic, disjoint-edit conflict handling.
        let mut stale = update.clone();
        stale["document_id"] = json!(engine.doc.id);
        stale["source_revision"] = json!(0);
        assert!(engine
            .edit("human", &[stale], Some(0), None, "Stale")
            .unwrap_err()
            .contains("The source project changed."));
        assert_eq!(snapshot(&engine), before);
        engine.doc.layers[0].locked = true;
        let locked = snapshot(&engine);
        assert!(engine
            .edit("human", &[update.clone()], Some(1), None, "Locked")
            .is_err());
        assert_eq!(snapshot(&engine), locked);
        engine.doc.layers[0].locked = false;
        for invalid_text in [
            "\u{10ffff}",
            "A\u{fe0f}",
            "A\u{e0100}",
            "\u{fe0f}",
            "A\u{fe0f}\u{fe0f}",
        ] {
            let mut invalid = changed.clone();
            if let Content::Text { text, .. } = &mut invalid.content {
                *text = invalid_text.into();
            }
            assert!(engine
                .edit(
                    "human",
                    &[
                        update.clone(),
                        json!({"op":"source.update","layer":id,"source":invalid})
                    ],
                    Some(1),
                    None,
                    "Invalid atomic batch"
                )
                .is_err());
            assert_eq!(snapshot(&engine), before);
        }
        engine
            .edit("human", &[update], Some(1), None, "Shaped update")
            .unwrap();
        assert_eq!(engine.undo.len(), 2);
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers[0].source, Some(source().into()));
        assert_eq!(
            engine.doc.export_png().unwrap(),
            expected.export_png().unwrap()
        );
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.export_png().unwrap(), original_png);
    }
}

#[test]
fn omitted_or_explicit_legacy_serialization_pixels_and_script_rejection_stay_identical() {
    let mut historical = source();
    if let Content::Text { text, layout, .. } = &mut historical.content {
        *text = "AV office".into();
        *layout = TextLayout::Legacy;
    }
    let serialized = serde_json::to_value(&historical).unwrap();
    assert!(serialized["content"].get("layout").is_none());
    let mut explicit = serialized.clone();
    explicit["content"]["layout"] = json!("legacy");
    let a: Source = serde_json::from_value(serialized).unwrap();
    let b: Source = serde_json::from_value(explicit).unwrap();
    assert_eq!(a, b);
    for depth in [8, 16] {
        assert_eq!(
            a.render(160, 72, depth).unwrap().rgba16(),
            b.render(160, 72, depth).unwrap().rgba16()
        );
    }
    if let Content::Text { text, .. } = &mut historical.content {
        *text = "a\u{0301}".into();
    }
    assert!(
        historical.validate().is_err(),
        "Old layout must never silently reflow new complex text"
    );
}

#[test]
fn shaped_updates_affine_placement_masks_effects_and_explicit_rasterize_keep_native_sources() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(240, 160, depth).unwrap();
        engine
            .edit(
                "human",
                &[json!({"op":"source.add","source":source(),"x":21,"y":17})],
                None,
                None,
                "Text",
            )
            .unwrap();
        let id = engine.doc.layers[0].id.clone();
        engine.edit("human",&[json!({"op":"mask.add","layer":id}),json!({"op":"effect.add","layer":id,"kind":"hsl","settings":{"hue":12.,"saturation":0.,"lightness":0.}})],None,None,"Editable mask/effect").unwrap();
        let mut mask = peerbrush::raster::Raster::new_depth(160, 72, depth);
        mask.set16(7, 9, [45003, 45003, 45003, 65535]);
        engine.doc.layers[0]
            .mask
            .as_mut()
            .unwrap()
            .steps
            .push(peerbrush::engine::MaskStep {
                id: "native-mask-fixture".into(),
                kind: "paint".into(),
                enabled: true,
                weight: 1.,
                value: 0.,
                pixels: mask,
                settings: Value::Null,
            });
        let original = engine.doc.clone();
        let mut changed = source();
        if let Content::Text { text, .. } = &mut changed.content {
            *text = "a\u{0301} office".into();
        }
        let command = json!({"op":"source.update","layer":id,"source":changed});
        let preview = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
        let count = engine.undo.len();
        engine
            .edit("human", &[command], None, None, "Shaped content")
            .unwrap();
        assert_eq!(engine.undo.len(), count + 1);
        assert_eq!(
            engine.doc.export_png().unwrap(),
            preview.export_png().unwrap()
        );
        assert_eq!(
            json!(engine.doc.layers[0].mask),
            json!(original.layers[0].mask)
        );
        assert_eq!(
            json!(engine.doc.layers[0].effects),
            json!(original.layers[0].effects)
        );
        engine.undo("human").unwrap();
        assert_eq!(json!(engine.doc.layers), json!(original.layers));
        engine.redo("human").unwrap();
        let contents = engine.doc.layers[0]
            .source
            .as_ref()
            .unwrap()
            .content
            .clone();
        engine
            .edit(
                "human",
                &[json!({"op":"transform","layer":id,"angle":15.,"selection_only":false})],
                None,
                None,
                "Affine shaped text",
            )
            .unwrap();
        assert_eq!(
            engine.doc.layers[0].source.as_ref().unwrap().content,
            contents
        );
        assert_eq!(engine.doc.layers[0].pixels.depth, depth);
        let before = engine.doc.clone();
        for encoded in [
            peerbrush::psd::encode(&before).unwrap(),
            peerbrush::psd::encode_psb(&before).unwrap(),
        ] {
            let reopened = peerbrush::psd::decode(&encoded).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.layers[0].source, before.layers[0].source);
            assert_eq!(
                reopened.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(json!(reopened.layers[0].mask), json!(before.layers[0].mask));
            assert_eq!(reopened.export_png().unwrap(), before.export_png().unwrap());
        }
        engine
            .edit(
                "human",
                &[json!({"op":"source.rasterize","layer":id})],
                None,
                None,
                "Explicit rasterize",
            )
            .unwrap();
        assert!(engine.doc.layers[0].source.is_none());
        assert_eq!(
            engine.doc.layers[0].pixels.rgba16(),
            before.layers[0].pixels.rgba16()
        );
        assert_eq!(
            engine.doc.export_png().unwrap(),
            before.export_png().unwrap()
        );
        engine.undo("human").unwrap();
        assert_eq!(json!(engine.doc.layers), json!(before.layers));
    }
}
