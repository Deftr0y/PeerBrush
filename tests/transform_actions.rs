use peerbrush::{
    engine::{Document, Engine, Layer, Scope},
    psd,
    raster::Raster,
    server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(16, 12, depth).unwrap();
    for y in 0..12 {
        for x in 0..16 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                [10001 + x as u16 * 97, 20003 + y as u16 * 137, 30007, 65535],
            );
        }
    }
    e
}
fn edit(e: &mut Engine, parameters: Value) {
    let mut c = parameters;
    c["op"] = json!("transform");
    c["layer"] = json!(e.doc.layers[0].id);
    e.edit(
        "human",
        &[c],
        Some(e.doc.revision),
        None,
        "Transform action",
    )
    .unwrap();
}
#[test]
fn reflections_and_quarter_turns_map_native_samples_and_reopen_with_exact_inverse() {
    for depth in [8, 16] {
        for (parameters, inverse) in [
            (json!({"mirror_x":true}), json!({"mirror_x":true})),
            (json!({"mirror_y":true}), json!({"mirror_y":true})),
            (json!({"angle":90.}), json!({"angle":-90.})),
            (json!({"angle":-90.}), json!({"angle":90.})),
        ] {
            let mut e = fixture(depth);
            let before = e.doc.clone();
            let mut c = parameters.clone();
            c["op"] = json!("transform");
            c["layer"] = json!(e.doc.layers[0].id);
            c["pivot"] = json!([8., 6.]);
            let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
            e.edit("human", &[c], Some(before.revision), None, "Action")
                .unwrap();
            assert_eq!(e.undo.len(), 1);
            assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
            let l = &e.doc.layers[0];
            for y in 0..12 {
                for x in 0..16 {
                    let (xx, yy) = if parameters["mirror_x"] == true {
                        (15 - x, y)
                    } else if parameters["mirror_y"] == true {
                        (x, 11 - y)
                    } else if parameters["angle"] == 90. {
                        (13 - y, x - 2)
                    } else {
                        (y + 2, 13 - x)
                    };
                    assert_eq!(
                        l.pixels.get16(xx - l.x, yy - l.y),
                        before.layers[0].pixels.get16(x, y)
                    );
                }
            }
            let reopened = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, depth);
            assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            e.redo("human").unwrap();
            e.doc = reopened;
            let mut inverse = inverse;
            inverse["pivot"] = json!([8., 6.]);
            edit(&mut e, inverse);
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            assert_eq!(
                e.doc.layers[0]
                    .pixels
                    .retained
                    .as_ref()
                    .unwrap()
                    .pixels
                    .rgba16(),
                before.layers[0].pixels.rgba16()
            );
        }
    }
}
#[test]
fn selected_mask_reflections_preserve_color_other_steps_and_folder_children() {
    for depth in [8, 16] {
        for folder in [false, true] {
            for selected in [false, true] {
                let mut e = fixture(depth);
                let id = e.doc.layers[0].id.clone();
                e.edit("human", &[json!({"op":"mask.add","layer":id,"value":255}),json!({"op":"paint","layer":id,"mask":true,"points":[[3,4]],"radius":2,"color":[0,0,0,255]})],None,None,"Mask").unwrap();
                let step = e.doc.layers[0]
                    .mask
                    .as_ref()
                    .unwrap()
                    .steps
                    .last()
                    .unwrap()
                    .id
                    .clone();
                if folder {
                    let mut child = Layer::new("Child", "paint", 16, 12);
                    child.pixels = e.doc.layers[0].pixels.clone();
                    child.parent = Some(id.clone());
                    e.doc.layers[0].kind = "group".into();
                    e.doc.layers[0].pixels = Raster::new_depth(16, 12, depth);
                    e.doc.layers.push(child);
                }
                if selected {
                    e.edit(
                        "human",
                        &[json!({"op":"selection","rect":[1,1,8,9]})],
                        None,
                        None,
                        "Selection",
                    )
                    .unwrap();
                }
                e.undo.clear();
                let before = e.doc.clone();
                edit(
                    &mut e,
                    json!({"mirror_x":true,"mask":true,"step":step,"pivot":[8.,6.]}),
                );
                assert_eq!(e.undo.len(), 1);
                assert_eq!(
                    e.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert_eq!(e.doc.layers[0].kind, before.layers[0].kind);
                assert_eq!(
                    e.doc.layers[0].mask.as_ref().unwrap().steps[0].pixels,
                    before.layers[0].mask.as_ref().unwrap().steps[0].pixels
                );
                if folder {
                    assert_eq!(e.doc.layers[1].pixels, before.layers[1].pixels);
                }
                assert_ne!(
                    e.doc.layers[0]
                        .mask
                        .as_ref()
                        .unwrap()
                        .steps
                        .last()
                        .unwrap()
                        .pixels
                        .rgba16(),
                    before.layers[0]
                        .mask
                        .as_ref()
                        .unwrap()
                        .steps
                        .last()
                        .unwrap()
                        .pixels
                        .rgba16()
                );
                let reopened = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
                assert_eq!(e.doc.export_png().unwrap(), reopened.export_png().unwrap());
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                if !selected {
                    e.doc = reopened;
                    edit(
                        &mut e,
                        json!({"mirror_x":true,"mask":true,"step":step,"pivot":[8.,6.]}),
                    );
                    assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                }
            }
        }
    }
}
#[test]
fn reflection_flags_invalid_batches_and_selection_reservations_are_atomic() {
    let mut e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    let original = serde_json::to_vec(&e.doc).unwrap();
    for bad in [
        json!({"mirror_x":1}),
        json!({"mirror_y":"yes"}),
        json!({"scale_x":-1.}),
        json!({"scale_y":null}),
    ] {
        let mut c = bad;
        c["op"] = json!("transform");
        c["layer"] = json!(layer);
        assert!(e
            .edit(
                "human",
                &[json!({"op":"move","layer":layer,"dx":1}), c],
                None,
                None,
                "Bad action"
            )
            .is_err());
        assert_eq!(serde_json::to_vec(&e.doc).unwrap(), original);
        assert!(e.undo.is_empty());
    }
    e.doc.selection = Some([1, 1, 5, 5]);
    e.reserve(
        "other",
        "Destination",
        vec![Scope {
            target: Some(layer.clone()),
            rect: Some([12, 1, 16, 5]),
        }],
    )
    .unwrap();
    let before = serde_json::to_vec(&e.doc).unwrap();
    let c = json!({"op":"transform","layer":layer,"mirror_x":true,"pivot":[8.,6.]});
    assert!(e
        .edit("human", &[c.clone()], None, None, "Reserved destination")
        .is_err());
    assert_eq!(serde_json::to_vec(&e.doc).unwrap(), before);
    e.leases.clear();
    e.reserve("other", "Selection", vec![Scope::layer("@selection")])
        .unwrap();
    assert!(e
        .edit("human", &[c.clone()], None, None, "Reserved selection")
        .is_err());
    e.leases.clear();
    e.doc.layers[0].locked = true;
    assert!(e.edit("human", &[c], None, None, "Locked").is_err());
}
#[test]
fn protocol_reflection_returns_real_pixels_document_coordinates_and_one_undo() {
    let e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    let id = e.doc.id.clone();
    let revision = e.doc.revision;
    let shared = Arc::new(Mutex::new(e));
    let result=server::dispatch(&shared,"edit",&json!({"actor":"reflection-agent","document_id":id,"expected_revision":revision,"commands":[{"op":"transform","layer":layer,"mirror_x":true}],"feedback":"always","max_edge":16})).unwrap();
    assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 16, 12]));
    assert!(result["images"][0]["data"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    let e = shared.lock().unwrap();
    assert_eq!(e.undo.len(), 1);
    assert_eq!(
        e.doc.layers[0].pixels.get16(15, 0),
        [10001, 20003, 30007, 65535]
    );
}

#[test]
fn folder_reflections_include_hidden_children_and_share_the_selection_once() {
    for depth in [8, 16] {
        for selected in [false, true] {
            let mut e = fixture(depth);
            let mut root = Layer::new("Folder", "group", 16, 12);
            root.pixels = Raster::new_depth(16, 12, depth);
            let root_id = root.id.clone();
            e.doc.layers[0].parent = Some(root_id.clone());
            let mut hidden = e.doc.layers[0].clone();
            hidden.id = peerbrush::engine::id();
            hidden.visible = false;
            hidden.name = "Hidden child".into();
            e.doc.layers.push(hidden);
            e.doc.layers.insert(0, root);
            if selected {
                e.edit(
                    "human",
                    &[json!({"op":"selection","rect":[1,2,8,10],"feather":1.})],
                    None,
                    None,
                    "Soft selection",
                )
                .unwrap();
            }
            e.undo.clear();
            let before = e.doc.clone();
            let command = json!({"op":"transform","layer":root_id,"mirror_x":true,"pivot":[8.,6.]});
            let children = before
                .layers
                .iter()
                .filter(|l| l.kind == "paint")
                .map(|l| json!({"op":"transform","layer":l.id,"mirror_x":true,"pivot":[8.,6.]}))
                .collect::<Vec<_>>();
            let reference = Engine::preview_edits(before.clone(), &children).unwrap();
            e.edit("human", &[command], None, None, "Folder mirror")
                .unwrap();
            assert_eq!(e.undo.len(), 1);
            assert_eq!(e.doc.export_png().unwrap(), reference.export_png().unwrap());
            assert_eq!(e.doc.selection, reference.selection);
            for child in e.doc.layers.iter().filter(|l| l.kind == "paint") {
                let expected = reference.layers.iter().find(|l| l.id == child.id).unwrap();
                assert_eq!(child.pixels.rgba16(), expected.pixels.rgba16());
            }
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        }
    }
}

#[test]
fn editable_shape_reflection_preserves_definition_and_native_color_through_reopen() {
    use peerbrush::source::{Content, Source};
    for depth in [8, 16] {
        let source = Source {
            width: 16,
            height: 12,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content::Shape {
                shape: "rectangle".into(),
                bounds: [1., 2., 6., 10.],
                points: vec![],
                closed: true,
                fill: [10001, 20003, 30007, 65535],
                stroke: [0; 4],
                stroke_width: 0.,
            },
        };
        let mut e = fixture(depth);
        e.doc.layers[0].source = Some(source.clone());
        e.doc.layers[0].pixels = source.render(16, 12, depth).unwrap();
        let before = e.doc.clone();
        edit(&mut e, json!({"mirror_x":true,"selection_only":false}));
        assert_eq!(
            e.doc.layers[0].source.as_ref().unwrap().content,
            source.content
        );
        assert_eq!(
            e.doc.layers[0].pixels.get16(12, 4),
            before.layers[0].pixels.get16(3, 4)
        );
        e.doc = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        edit(&mut e, json!({"mirror_x":true,"selection_only":false}));
        assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
    }
}

#[test]
fn expanding_selected_mask_keeps_color_original_for_later_inverse_after_reopen() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let original = e.doc.layers[0].pixels.rgba16();
        let id = e.doc.layers[0].id.clone();
        e.edit("human",&[json!({"op":"mask.add","layer":id,"value":255}),json!({"op":"paint","layer":id,"mask":true,"points":[[3,4]],"radius":2,"color":[0,0,0,255]})],None,None,"Mask").unwrap();
        edit(
            &mut e,
            json!({"scale_x":0.5,"scale_y":0.5,"pivot":[8.,6.],"selection_only":false}),
        );
        let original_source = e.doc.layers[0]
            .pixels
            .retained
            .as_ref()
            .unwrap()
            .pixels
            .clone();
        e.edit(
            "human",
            &[json!({"op":"selection","rect":[5,3,8,7]})],
            None,
            None,
            "Selection",
        )
        .unwrap();
        edit(
            &mut e,
            json!({"mirror_x":true,"mask":true,"pivot":[20.,6.]}),
        );
        assert!(e.doc.layers[0].pixels.width > 8);
        assert_eq!(
            e.doc.layers[0].pixels.retained.as_ref().unwrap().pixels,
            original_source
        );
        e.doc = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        edit(
            &mut e,
            json!({"scale_x":2.,"scale_y":2.,"pivot":[8.,6.],"selection_only":false}),
        );
        let l = &e.doc.layers[0];
        for y in 0..12 {
            for x in 0..16 {
                let at = ((y * 16 + x) * 4) as usize;
                assert_eq!(l.pixels.get16(x - l.x, y - l.y), original[at..at + 4]);
            }
        }
    }
}
