use base64::{engine::general_purpose::STANDARD, Engine as _};
use peerbrush::{
    engine::{self, Document, Engine, Layer, Scope},
    image_import, psd, raster, server, workspace,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct File(PathBuf);
impl File {
    fn new(bytes: &[u8]) -> Self {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("pb-row-workflow-{}.png", engine::id()));
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        Self(path)
    }
}
impl Drop for File {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn fixture(depth: u16) -> (File, Vec<u8>, Vec<u16>) {
    let words = (0..259 * 3u32)
        .flat_map(|index| {
            [
                12347 + index as u16 * 17,
                23459,
                34571,
                if index % 5 == 0 { 0 } else { 45679 },
            ]
            .map(|value| {
                if depth == 16 {
                    value
                } else {
                    u16::from(raster::project16(value)) * 257
                }
            })
        })
        .collect::<Vec<_>>();
    let bytes = if depth == 16 {
        raster::png16_with_profile(
            259,
            3,
            &words,
            Some(peerbrush::color_profile::srgb_profile()),
        )
        .unwrap()
    } else {
        raster::png_with_profile(
            259,
            3,
            &words
                .iter()
                .map(|value| (value / 257) as u8)
                .collect::<Vec<_>>(),
            Some(peerbrush::color_profile::srgb_profile()),
        )
        .unwrap()
    };
    (File::new(&bytes), bytes, words)
}
fn snapshot(engine: &Engine) -> Value {
    json!({"document":engine.doc,"undo":engine.undo.len(),"redo":engine.redo.len(),"saved":engine.saved_revision})
}

#[test]
fn row_open_keeps_other_work_and_owned_png_and_saves_exact_native_psd_psb() {
    for depth in [8, 16] {
        let (file, bytes, words) = fixture(depth);
        let root = Arc::new(Mutex::new(Engine::new()));
        {
            let mut engine = root.lock().unwrap();
            let layer = engine.doc.layers[0].id.clone();
            engine
                .edit(
                    "human",
                    &[json!({"op":"layer.update","layer":layer,"name":"Existing human work"})],
                    Some(0),
                    None,
                    "Keep work",
                )
                .unwrap();
        }
        let before = snapshot(&root.lock().unwrap());
        let reply = server::dispatch(
            &root,
            "projects",
            &json!({"action":"open","actor":"human","path":file.0}),
        )
        .unwrap();
        let opened = workspace::get(&root, reply["project_id"].as_str().unwrap()).unwrap();
        assert!(snapshot(&root.lock().unwrap()) == before);
        {
            let engine = opened.lock().unwrap();
            assert_eq!(
                (engine.doc.width, engine.doc.height, engine.doc.bit_depth),
                (259, 3, depth)
            );
            assert!(engine.path.is_none() && engine.file_version.is_none());
            assert_eq!(engine.saved_revision, u64::MAX);
            assert!(
                engine.undo.is_empty()
                    && engine.redo.is_empty()
                    && !engine.doc.read_only
                    && engine.doc.srgb_tagged
            );
            assert_eq!(engine.doc.layers.len(), 1);
            assert!(engine.doc.layers[0].source.is_none());
            assert!(engine.doc.layers[0].pixels.rgba16() == words);
            for encoded in [
                psd::encode(&engine.doc).unwrap(),
                psd::encode_psb(&engine.doc).unwrap(),
            ] {
                let decoded = psd::decode(&encoded).unwrap();
                assert!(!decoded.read_only && decoded.srgb_tagged);
                assert_eq!(
                    (decoded.width, decoded.height, decoded.bit_depth),
                    (259, 3, depth)
                );
                assert_eq!(decoded.layers.len(), 1);
                assert!(decoded.layers[0].pixels.rgba16() == words);
                assert_eq!(
                    decoded.export_png().unwrap(),
                    engine.doc.export_png().unwrap()
                );
            }
        }
        assert!(server::dispatch(
            &opened,
            "document",
            &json!({"action":"export","actor":"human","path":file.0,"overwrite":true})
        )
        .is_err());
        assert_eq!(fs::read(&file.0).unwrap(), bytes);
        assert!(snapshot(&root.lock().unwrap()) == before);
    }
}

#[test]
fn row_import_path_base64_placement_and_pixel_paste_keep_native_samples_and_one_history_step() {
    for source_depth in [8, 16] {
        let (file, bytes, words) = fixture(source_depth);
        for target_depth in [8, 16] {
            for operation in ["import", "path", "base64", "paste"] {
                let mut engine = Engine::new();
                engine.doc = Document::new_depth(270, 12, target_depth).unwrap();
                engine.doc.layers[0]
                    .pixels
                    .set16(266, 9, [12347, 23459, 34571, 45679]);
                let target = engine.doc.layers[0].id.clone();
                let before = serde_json::to_value(&engine.doc).unwrap();
                let command = match operation {
                    "import" => json!({"op":"image.import","path":file.0,"x":-3,"y":5}),
                    "path" => {
                        json!({"op":"image.place","path":file.0,"layer":target,"new_layer":true,"rect":[-3,5,256,8]})
                    }
                    "base64" => {
                        json!({"op":"image.place","png":STANDARD.encode(&bytes),"layer":target,"new_layer":true,"rect":[-3,5,256,8]})
                    }
                    _ => {
                        json!({"op":"image.paste","png":STANDARD.encode(&bytes),"layer":target,"origin":[-3,5]})
                    }
                };
                let preview =
                    Engine::preview_edits(engine.doc.clone(), &[command.clone()]).unwrap();
                assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
                engine
                    .edit("human", &[command], Some(0), None, "Native PNG")
                    .unwrap();
                assert_eq!(engine.doc.bit_depth, source_depth.max(target_depth));
                let layer = engine
                    .doc
                    .layers
                    .iter()
                    .find(|layer| layer.id != target && layer.kind == "paint")
                    .unwrap();
                assert_eq!(
                    (layer.x, layer.y, layer.pixels.width, layer.pixels.height),
                    (-3, 5, 259, 3)
                );
                assert!(
                    layer.pixels.rgba16() == words,
                    "Natural-size callers retain every alpha and hidden RGB word"
                );
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    preview.export_png().unwrap()
                );
                assert_eq!(engine.undo.len(), 1);
                let applied = serde_json::to_value(&engine.doc).unwrap();
                engine.undo("human").unwrap();
                let mut restored = serde_json::to_value(&engine.doc).unwrap();
                restored["revision"] = before["revision"].clone();
                assert_eq!(restored, before);
                engine.redo("human").unwrap();
                let mut restored = serde_json::to_value(&engine.doc).unwrap();
                restored["revision"] = applied["revision"].clone();
                assert_eq!(restored, applied);
            }
        }
        assert_eq!(fs::read(&file.0).unwrap(), bytes);
    }
}

#[test]
fn row_import_rejections_preserve_exact_source_history_native_depth_and_files() {
    for depth in [8, 16] {
        let (file, bytes, _) = fixture(depth);
        let mut broken = bytes.clone();
        *broken.last_mut().unwrap() ^= 1;
        let corrupt = File::new(&broken);
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(270, 12, depth).unwrap();
        let mut group = Layer::new("Reserved destination", "group", 270, 12);
        group.pixels = raster::Raster::new_depth(270, 12, depth);
        let parent = group.id.clone();
        engine.doc.layers.insert(0, group);
        let command = json!({"op":"image.import","path":file.0,"parent":parent,"x":-3,"y":5});
        engine
            .reserve(
                "other",
                "Current human work",
                vec![Scope {
                    target: Some(parent.clone()),
                    rect: None,
                }],
            )
            .unwrap();
        let before = snapshot(&engine);
        assert!(engine
            .edit(
                "human",
                &[command.clone()],
                Some(0),
                None,
                "Reserved import"
            )
            .is_err());
        assert!(snapshot(&engine) == before);
        engine.leases.clear();
        let before = snapshot(&engine);
        for commands in [
            vec![
                command.clone(),
                json!({"op":"image.import","path":corrupt.0}),
            ],
            vec![json!({"op":"image.import","path":file.0,"document_id":"closed"})],
            vec![json!({"op":"image.import","path":file.0,"source_revision":1})],
            vec![json!({"op":"image.import","path":file.0,"x":100001})],
        ] {
            assert!(Engine::preview_edits(engine.doc.clone(), &commands).is_err());
            assert!(engine
                .edit("human", &commands, Some(0), None, "Rejected PNG batch")
                .is_err());
            assert!(snapshot(&engine) == before);
        }
        assert!(image_import::decode_file(&file.0, &json!({}), 1).is_err());
        assert_eq!(fs::read(&file.0).unwrap(), bytes);
        assert_eq!(fs::read(&corrupt.0).unwrap(), broken);
    }
}

#[test]
fn mcp_row_placement_returns_actual_png_pixels_and_explicit_document_coordinates() {
    for depth in [8, 16] {
        let (file, bytes, words) = fixture(depth);
        let shared = Arc::new(Mutex::new(Engine::new()));
        {
            shared.lock().unwrap().doc = Document::new(270, 12).unwrap();
        }
        let target = shared.lock().unwrap().doc.layers[0].id.clone();
        let reply = server::mcp(
            &shared,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"peerbrush_place_image","arguments":{"actor":"artist","layer":target,"new_layer":true,
            "png":STANDARD.encode(&bytes),"rect":[1,2,260,5],"expected_revision":0,"max_edge":512}}}),
        );
        assert_eq!(reply["result"]["isError"], false);
        let content = reply["result"]["content"].as_array().unwrap();
        let manifest: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(manifest["placement"]["rect"], json!([1, 2, 260, 5]));
        assert_eq!(manifest["views"][0]["document_rect"], json!([1, 2, 260, 5]));
        assert_eq!(content[1]["mimeType"], "image/png");
        let png = STANDARD
            .decode(content[1]["data"].as_str().unwrap())
            .unwrap();
        let projected = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
        assert_eq!(projected.color(), image::ColorType::Rgba8);
        assert_eq!((projected.width(), projected.height()), (259, 3));
        let engine = shared.lock().unwrap();
        let layer = engine
            .doc
            .layers
            .iter()
            .find(|layer| layer.id == manifest["placement"]["layer"].as_str().unwrap())
            .unwrap();
        assert!(layer.pixels.rgba16() == words);
        assert_eq!(engine.doc.bit_depth, depth);
        assert_eq!(engine.undo.len(), 1);
        assert_eq!(
            projected.into_rgba8().into_raw(),
            engine
                .doc
                .preview(Some([1, 2, 260, 5]), 512, None, false)
                .unwrap()
                .2
        );
        assert_eq!(fs::read(&file.0).unwrap(), bytes);
    }
}
