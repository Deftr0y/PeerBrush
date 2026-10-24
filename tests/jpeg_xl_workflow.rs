use base64::Engine as _;
use peerbrush::{
    color_profile,
    engine::{self, Document, Engine, Layer, Scope},
    image_import::{self, Encoded},
    jpeg_xl::{self, Options, Pixels, PixelsRef},
    production_export::{self as export, Control, Crop, Format, Resampling, Settings, Variant},
    raster::Raster,
    server, workspace,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pb-jxl-workflow-{}", engine::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn source(depth: u16) -> (Vec<u8>, Vec<u16>) {
    let words: Vec<u16> = (0..6)
        .flat_map(|p| {
            [
                12347 + p * 317,
                23459 + p * 797,
                34571 + p * 191,
                if p == 0 {
                    0
                } else if p == 5 {
                    65535
                } else {
                    12001 + p * 113
                },
            ]
        })
        .collect();
    if depth == 16 {
        (
            jpeg_xl::encode(
                3,
                2,
                PixelsRef::Sixteen(&words),
                None,
                Options::default(),
                &|| Ok(()),
            )
            .unwrap(),
            words,
        )
    } else {
        let bytes: Vec<_> = words
            .into_iter()
            .map(peerbrush::raster::project16)
            .collect();
        (
            jpeg_xl::encode(
                3,
                2,
                PixelsRef::Eight(&bytes),
                None,
                Options::default(),
                &|| Ok(()),
            )
            .unwrap(),
            bytes.into_iter().map(|v| u16::from(v) * 257).collect(),
        )
    }
}
fn words(pixels: Pixels) -> Vec<u16> {
    match pixels {
        Pixels::Eight(bytes) => bytes.into_iter().map(|v| u16::from(v) * 257).collect(),
        Pixels::Sixteen(words) => words,
    }
}
fn state(shared: &server::Shared) -> Value {
    let e = shared.lock().unwrap();
    let history = |entries: &[peerbrush::engine::History]| {
        entries.iter().map(|h|json!({"before":h.before,"after":h.after,
        "label":h.label,"actor":h.actor,"task":h.task,"scopes":h.scopes,"gesture":h.gesture,"reverted":h.reverted})).collect::<Vec<_>>()
    };
    json!({"document":e.doc,"png":e.doc.export_png().unwrap(),"undo":history(&e.undo),"redo":history(&e.redo),
        "saved_revision":e.saved_revision,"path":e.path})
}
fn fixture(depth: u16) -> Document {
    let mut doc = Document::new_depth(16, 12, depth).unwrap();
    doc.name = "JXL.psd".into();
    doc.srgb_tagged = true;
    for y in 4..8 {
        for x in 3..10 {
            doc.layers[0].pixels.set16(
                x,
                y,
                [
                    12347 + x as u16 * 17,
                    23459,
                    34571,
                    if x == 3 { 32781 } else { 65535 },
                ],
            );
        }
    }
    doc
}
fn preview_request(doc: &Document, settings: &Settings) -> Value {
    json!({"action":"preview","actor":"human","document_id":doc.id,
        "expected_revision":doc.revision,"settings":settings})
}
fn wait(shared: &server::Shared, run: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let state =
            server::dispatch(shared, "exports", &json!({"action":"status","run":run})).unwrap();
        if state["status"] != "running" {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "JPEG XL export job did not finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn open_creates_native_unsaved_editable_project_without_discarding_human_work_or_source_file() {
    for depth in [8, 16] {
        let temp = Temp::new();
        let (bytes, expected) = source(depth);
        let path = temp.file("source.JXL", &bytes);
        let mut engine = Engine::new();
        engine.doc = fixture(16);
        let layer = engine.doc.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"name":"Unsaved human work"})],
                None,
                None,
                "Human work",
            )
            .unwrap();
        let shared = Arc::new(Mutex::new(engine));
        let before = state(&shared);
        assert!(server::dispatch(
            &shared,
            "document",
            &json!({"action":"open","actor":"human","path":path})
        )
        .unwrap_err()
        .contains("Unsaved"));
        let opened = server::dispatch(
            &shared,
            "projects",
            &json!({"action":"open","actor":"human","path":path}),
        )
        .unwrap();
        let project = workspace::get(&shared, opened["project_id"].as_str().unwrap()).unwrap();
        assert_eq!(state(&shared), before);
        let e = project.lock().unwrap();
        assert_eq!((e.doc.width, e.doc.height, e.doc.bit_depth), (3, 2, depth));
        assert!(e.path.is_none() && e.file_version.is_none() && e.undo.is_empty());
        assert_eq!(e.saved_revision, u64::MAX);
        assert!(!e.doc.read_only && e.doc.srgb_tagged);
        assert_eq!(e.doc.layers[0].pixels.rgba16(), expected);
        for psb in [false, true] {
            let saved = if psb {
                peerbrush::psd::encode_psb(&e.doc)
            } else {
                peerbrush::psd::encode(&e.doc)
            }
            .unwrap();
            let reopened = peerbrush::psd::decode(&saved).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, depth);
            assert_eq!(reopened.layers[0].pixels.rgba16(), expected);
            assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
        }
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn import_and_place_share_native_precision_coordinates_reservations_and_one_undo() {
    for source_depth in [8, 16] {
        for target_depth in [8, 16] {
            let temp = Temp::new();
            let (bytes, expected) = source(source_depth);
            let path = temp.file("source.jxl", &bytes);
            for place in [false, true] {
                let mut engine = Engine::new();
                engine.doc = Document::new_depth(8, 6, target_depth).unwrap();
                let target = engine.doc.layers[0].id.clone();
                engine.doc.layers[0]
                    .pixels
                    .set16(7, 5, [53001, 44003, 35007, 65535]);
                let mut folder = Layer::new("Import folder", "group", 8, 6);
                folder.pixels = Raster::new_depth(8, 6, target_depth);
                let parent = folder.id.clone();
                engine.doc.layers.push(folder);
                let before = serde_json::to_value(&engine.doc).unwrap();
                let original = engine.doc.layers[0].pixels.rgba16();
                let command = if place {
                    json!({"op":"image.place","path":path,"layer":target,"new_layer":false,"mode":"replace","rect":[2,3,5,5]})
                } else {
                    json!({"op":"image.import","path":path,"parent":parent,"x":2,"y":3})
                };
                engine
                    .reserve(
                        "other",
                        "Reserved destination",
                        vec![Scope::layer(if place { &target } else { &parent })],
                    )
                    .unwrap();
                assert!(engine
                    .edit("human", &[command.clone()], Some(0), None, "Import")
                    .is_err());
                assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
                assert!(engine.undo.is_empty());
                engine.leases.clear();
                let draft = Engine::preview_edits(engine.doc.clone(), &[command.clone()]).unwrap();
                assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
                engine
                    .edit("human", &[command], Some(0), None, "Import")
                    .unwrap();
                assert_eq!(engine.doc.bit_depth, source_depth.max(target_depth));
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    draft.export_png().unwrap()
                );
                assert_eq!(engine.undo.len(), 1);
                let layer = engine
                    .doc
                    .layers
                    .iter()
                    .find(|l| {
                        if place {
                            l.id == target
                        } else {
                            l.parent.as_deref() == Some(parent.as_str())
                        }
                    })
                    .unwrap();
                assert_eq!(
                    (layer.x, layer.y, layer.pixels.width, layer.pixels.height),
                    if place { (0, 0, 8, 6) } else { (2, 3, 3, 2) }
                );
                for y in 0..2 {
                    for x in 0..3 {
                        let at = (y * 3 + x) as usize * 4;
                        let expected_pixel: [u16; 4] = expected[at..at + 4].try_into().unwrap();
                        assert_eq!(
                            layer.pixels.get16(
                                x + if place { 2 } else { 0 },
                                y + if place { 3 } else { 0 }
                            ),
                            expected_pixel
                        );
                    }
                }
                let old_pixel: [u16; 4] = original[(5 * 8 + 7) * 4..(5 * 8 + 7) * 4 + 4]
                    .try_into()
                    .unwrap();
                assert_eq!(
                    engine
                        .doc
                        .layers
                        .iter()
                        .find(|l| l.id == target)
                        .unwrap()
                        .pixels
                        .get16(7, 5),
                    old_pixel
                );
                let applied = engine.doc.export_png().unwrap();
                engine.undo("human").unwrap();
                assert_eq!(engine.doc.bit_depth, target_depth);
                assert_eq!(engine.doc.layers[0].pixels.rgba16(), original);
                engine.redo("human").unwrap();
                assert_eq!(engine.doc.export_png().unwrap(), applied);
            }
            let mut engine = Engine::new();
            engine.doc = Document::new_depth(8, 6, target_depth).unwrap();
            let target = engine.doc.layers[0].id.clone();
            let shared = Arc::new(Mutex::new(engine));
            let response = server::mcp(
                &shared,
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"peerbrush_place_image","arguments":{"path":path,"layer":target,"new_layer":true,"rect":[2,3,5,5],"actor":"human","expected_revision":0,"max_edge":64}}}),
            );
            assert_eq!(response["result"]["isError"], false);
            let content = response["result"]["content"].as_array().unwrap();
            let manifest: Value = serde_json::from_str(
                content.iter().find(|v| v["type"] == "text").unwrap()["text"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(manifest["placement"]["rect"], json!([2, 3, 5, 5]));
            assert_eq!(manifest["views"][0]["document_rect"], json!([2, 3, 5, 5]));
            let image = content.iter().find(|v| v["type"] == "image").unwrap();
            assert_eq!(image["mimeType"], "image/png");
            let png_bytes = base64::engine::general_purpose::STANDARD
                .decode(image["data"].as_str().unwrap())
                .unwrap();
            let feedback = image::load_from_memory(&png_bytes).unwrap().into_rgba8();
            assert_eq!(feedback.dimensions(), (3, 2));
            let opaque: [u16; 4] = expected[20..24].try_into().unwrap();
            assert_eq!(
                feedback.get_pixel(2, 1).0,
                opaque.map(peerbrush::raster::project16)
            );
            assert_eq!(shared.lock().unwrap().undo.len(), 1);
            assert_eq!(
                shared.lock().unwrap().doc.bit_depth,
                source_depth.max(target_depth)
            );
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }
}

#[test]
fn malformed_mismatched_canceled_budget_and_stale_imports_preserve_existing_work() {
    let temp = Temp::new();
    let (bytes, _) = source(16);
    let valid = temp.file("valid.jxl", &bytes);
    let mut engine = Engine::new();
    engine.doc = fixture(16);
    let shared = Arc::new(Mutex::new(engine));
    let before = state(&shared);
    for (name, bad) in [
        (
            "wrong.jxl",
            peerbrush::raster::png(1, 1, &[1, 2, 3, 4]).unwrap(),
        ),
        ("wrong.png", bytes.clone()),
        ("truncated.jxl", bytes[..bytes.len() / 2].to_vec()),
    ] {
        let path = temp.file(name, &bad);
        assert!(image_import::decode_file(&path, &json!({}), image_import::BUDGET).is_err());
        assert!(server::open(&shared, &path).is_err());
        assert_eq!(state(&shared), before);
        assert_eq!(fs::read(path).unwrap(), bad);
    }
    let encoded = Encoded::file(&valid).unwrap();
    assert!(encoded
        .decode(&json!({}), 1)
        .unwrap_err()
        .contains("budget"));
    let canceled = peerbrush::loading::Control::default();
    canceled.cancel();
    assert!(server::open_options(&shared, &valid, &canceled, &json!({}))
        .unwrap_err()
        .contains("canceled"));
    assert!(shared.lock().unwrap().loading.is_none());
    let doc = shared.lock().unwrap().doc.clone();
    for commands in [
        vec![
            json!({"op":"image.import","path":valid}),
            json!({"op":"image.import","path":valid,"frame":0}),
        ],
        vec![json!({"op":"image.import","path":valid,"source_revision":doc.revision+1})],
        vec![json!({"op":"image.import","path":valid,"document_id":"different source"})],
    ] {
        assert!(shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &commands,
                Some(doc.revision),
                None,
                "Invalid import"
            )
            .is_err());
        assert_eq!(state(&shared), before);
    }
    assert_eq!(fs::read(valid).unwrap(), bytes);
}

#[test]
fn lossless_exports_match_shared_native_png_with_crop_padding_resampling_and_current_composite() {
    for depth in [8, 16] {
        let mut doc = fixture(depth);
        doc.filters.push(peerbrush::effects::Effect {
            id: engine::id(),
            kind: "invert".into(),
            enabled: true,
            weight: 0.7,
            settings: json!({}).into(),
        });
        let before = serde_json::to_value(&doc).unwrap();
        let current = doc.export_png().unwrap();
        for crop in [false, true] {
            for resampling in [Resampling::Nearest, Resampling::Bilinear] {
                let png = Settings {
                    crop: if crop { Crop::Alpha } else { Crop::Canvas },
                    padding: if crop { 2 } else { 0 },
                    resampling,
                    variants: vec![Variant {
                        name: "sample".into(),
                        scale: if crop { 1.7 } else { 1. },
                    }],
                    ..Default::default()
                };
                for targets in [vec![], vec![doc.layers[0].id.clone()]] {
                    let reference =
                        export::plan(&doc, &targets, &png, &Control::default()).unwrap();
                    let expected =
                        export::render(&doc, &reference.outputs[0], &png, &Control::default())
                            .unwrap();
                    let settings = Settings {
                        format: Format::Jxl,
                        ..png.clone()
                    };
                    let plan =
                        export::plan(&doc, &targets, &settings, &Control::default()).unwrap();
                    let output = &plan.outputs[0];
                    assert_eq!(output.document_rect, reference.outputs[0].document_rect);
                    assert_eq!(
                        (output.bit_depth, output.native_source_depth),
                        (depth, depth)
                    );
                    assert!(output.filename.ends_with(".jxl"));
                    let bytes =
                        export::render(&doc, output, &settings, &Control::default()).unwrap();
                    let decoded = jpeg_xl::decode(&bytes, &|| Ok(())).unwrap();
                    assert_eq!(
                        (
                            decoded.info.width,
                            decoded.info.height,
                            decoded.info.bit_depth
                        ),
                        (output.width, output.height, depth)
                    );
                    assert_eq!(decoded.icc_profile, color_profile::srgb_profile());
                    assert_eq!(
                        words(decoded.pixels),
                        image::load_from_memory(&expected)
                            .unwrap()
                            .into_rgba16()
                            .into_raw()
                    );
                    let control = Control::default();
                    control.cancel();
                    assert!(export::render(&doc, output, &settings, &control).is_err());
                    let mut mismatch = output.clone();
                    mismatch.format = Format::Png;
                    assert!(
                        export::render(&doc, &mismatch, &settings, &Control::default()).is_err()
                    );
                }
            }
        }
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert_eq!(doc.export_png().unwrap(), current);
    }
}

// Independent matrix/linear-TRC RGB fixture from the existing profile suite.
fn linear_profile() -> Vec<u8> {
    let fixed = |v: f64| ((v * 65536.).round() as i32).to_be_bytes();
    let mut entries = Vec::new();
    for (key, xyz) in [
        (*b"rXYZ", [0.4360747, 0.2225045, 0.0139322]),
        (*b"gXYZ", [0.3850649, 0.7168786, 0.0971045]),
        (*b"bXYZ", [0.1430804, 0.0606169, 0.7141733]),
        (*b"wtpt", [0.9642, 1., 0.8249]),
    ] {
        let mut data = b"XYZ \0\0\0\0".to_vec();
        for v in xyz {
            data.extend(fixed(v));
        }
        entries.push((key, data));
    }
    for key in [*b"rTRC", *b"gTRC", *b"bTRC"] {
        entries.push((key, b"curv\0\0\0\0\0\0\0\0".to_vec()));
    }
    let mut profile = vec![0; 132 + entries.len() * 12];
    profile[8..12].copy_from_slice(&0x04200000u32.to_be_bytes());
    profile[12..24].copy_from_slice(b"mntrRGB XYZ ");
    profile[24..36].copy_from_slice(&[0x07, 0xcc, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]);
    profile[36..40].copy_from_slice(b"acsp");
    for (i, v) in [0.9642, 1., 0.8249].iter().enumerate() {
        profile[68 + i * 4..72 + i * 4].copy_from_slice(&fixed(*v));
    }
    profile[128..132].copy_from_slice(&(entries.len() as u32).to_be_bytes());
    for (i, (key, data)) in entries.iter().enumerate() {
        let at = 132 + i * 12;
        let offset = profile.len() as u32;
        profile[at..at + 4].copy_from_slice(key);
        profile[at + 4..at + 8].copy_from_slice(&offset.to_be_bytes());
        profile[at + 8..at + 12].copy_from_slice(&(data.len() as u32).to_be_bytes());
        profile.extend(data);
    }
    let size = profile.len() as u32;
    profile[..4].copy_from_slice(&size.to_be_bytes());
    profile
}

#[test]
fn lossy_native16_preview_uses_actual_decoded_profile_and_protocol_reports_real_pixels_and_coordinates(
) {
    let mut engine = Engine::new();
    engine.doc = fixture(16);
    engine.doc.icc_profile = Some(Arc::new(linear_profile()));
    let doc = engine.doc.clone();
    let shared = Arc::new(Mutex::new(engine));
    let before = state(&shared);
    let settings = Settings {
        format: Format::Jxl,
        jxl: Options {
            lossless: false,
            distance: 0.1,
            effort: 3,
        },
        crop: Crop::Alpha,
        padding: 2,
        ..Default::default()
    };
    let plan = export::plan(&doc, &[], &settings, &Control::default()).unwrap();
    let output = &plan.outputs[0];
    let encoded = export::render(&doc, output, &settings, &Control::default()).unwrap();
    let decoded = jpeg_xl::decode(&encoded, &|| Ok(())).unwrap();
    assert_eq!(decoded.info.bit_depth, 16);
    let Pixels::Sixteen(mut decoded_words) = decoded.pixels else {
        panic!("Native16 output projected")
    };
    let png_settings = Settings {
        format: Format::Png,
        ..settings.clone()
    };
    let reference = export::plan(&doc, &[], &png_settings, &Control::default()).unwrap();
    let png = export::render(
        &doc,
        &reference.outputs[0],
        &png_settings,
        &Control::default(),
    )
    .unwrap();
    let original = image::load_from_memory(&png)
        .unwrap()
        .into_rgba16()
        .into_raw();
    assert!(decoded_words
        .iter()
        .skip(3)
        .step_by(4)
        .eq(original.iter().skip(3).step_by(4)));
    assert!(decoded_words
        .chunks_exact(4)
        .any(|p| p[..3].iter().any(|v| v % 257 != 0)));
    color_profile::convert16(&decoded.icc_profile, &mut decoded_words).unwrap();
    let expected: Vec<_> = decoded_words
        .chunks_exact(4)
        .flat_map(|p| peerbrush::depth16::display_pixel(p.try_into().unwrap()))
        .collect();
    let request = preview_request(&doc, &settings);
    let preview = server::dispatch(&shared, "exports", &request).unwrap();
    let image = &preview["images"][0];
    assert_eq!(image["document_rect"], json!(output.document_rect));
    assert_eq!(image["output_format"], "jxl");
    assert_eq!(image["output_bit_depth"], 16);
    assert_eq!(image["display_managed"], true);
    let png = base64::engine::general_purpose::STANDARD
        .decode(image["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        image::load_from_memory(&png)
            .unwrap()
            .into_rgba8()
            .into_raw(),
        expected
    );
    let mcp = server::mcp(
        &shared,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_exports","arguments":request}}),
    );
    assert_eq!(mcp["result"]["isError"], false);
    assert!(mcp["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["type"] == "image" && v["mimeType"] == "image/png"));
    let image = mcp["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == "image")
        .unwrap();
    let png = base64::engine::general_purpose::STANDARD
        .decode(image["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        image::load_from_memory(&png)
            .unwrap()
            .into_rgba8()
            .into_raw(),
        expected
    );
    let metadata: Value = serde_json::from_str(
        mcp["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "text")
            .unwrap()["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        metadata["plan"]["outputs"][0]["document_rect"],
        json!(output.document_rect)
    );
    assert_eq!(metadata["plan"]["outputs"][0]["bit_depth"], 16);
    let tools = server::tools();
    let schema = &tools
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "peerbrush_exports")
        .unwrap()["inputSchema"]["properties"]["settings"]["properties"];
    assert_eq!(schema["format"]["enum"], json!(["png", "webp", "jxl"]));
    assert_eq!(schema["jxl"]["additionalProperties"], false);
    assert_eq!(schema["jxl"]["properties"]["lossless"]["default"], true);
    for bad in [
        json!({"distance":0}),
        json!({"effort":10}),
        json!({"lossless":"false"}),
        json!({"unknown":true}),
    ] {
        let mut invalid = preview_request(&doc, &settings);
        invalid["settings"]["jxl"] = bad;
        assert!(server::dispatch(&shared, "exports", &invalid).is_err());
    }
    let temp = Temp::new();
    let path = temp.file("source.jxl", &encoded);
    let info = server::dispatch(&shared, "image_info", &json!({"path":path})).unwrap();
    assert_eq!(
        (info["format"].as_str(), info["bit_depth"].as_u64()),
        (Some("JPEG XL"), Some(16))
    );
    let imported = image_import::decode_file(&path, &json!({}), image_import::BUDGET).unwrap();
    assert_eq!(imported.depth, 16);
    assert_eq!(imported.rgba16(), decoded_words);
    assert_eq!(state(&shared), before);
}

#[test]
fn presets_batches_and_direct_export_use_current_native_sources_and_preserve_loaded_originals() {
    for depth in [8, 16] {
        let temp = Temp::new();
        let mut engine = Engine::new();
        engine.doc = fixture(depth);
        engine.export_library = Arc::new(Mutex::new(export::library::Library::load(
            temp.0.join("presets.json"),
        )));
        let shared = Arc::new(Mutex::new(engine));
        let doc = shared.lock().unwrap().doc.clone();
        let before = state(&shared);
        let settings = Settings {
            format: Format::Jxl,
            ..Default::default()
        };
        let saved=server::dispatch(&shared,"exports",&json!({"action":"save_preset","name":"Native JXL","actor":"human","expected_library_revision":0,"settings":settings})).unwrap();
        let preset = saved["preset"]["id"].as_str().unwrap();
        let loaded = export::library::Library::load(temp.0.join("presets.json"));
        assert_eq!(loaded.get(preset).unwrap().settings, settings);
        let start = json!({"action":"start","actor":"human","document_id":doc.id,"expected_revision":doc.revision,"directory":temp.0,"preset":preset,"expected_library_revision":1});
        let run = server::dispatch(&shared, "exports", &start).unwrap();
        let completed = wait(&shared, run["run"].as_str().unwrap());
        assert_eq!(completed["status"], "completed", "{completed}");
        let receipt = &completed["outputs"][0];
        assert_eq!(receipt["status"], "written");
        assert_eq!(receipt["output"]["bit_depth"], depth);
        assert_eq!(receipt["output"]["native_source_depth"], depth);
        let path = PathBuf::from(receipt["path"].as_str().unwrap());
        let bytes = fs::read(&path).unwrap();
        let decoded = jpeg_xl::decode(&bytes, &|| Ok(())).unwrap();
        let expected = image::load_from_memory(&doc.export_png().unwrap())
            .unwrap()
            .into_rgba16()
            .into_raw();
        assert_eq!(words(decoded.pixels), expected);
        let reopened = Arc::new(Mutex::new(Engine::new()));
        server::open(&reopened, &path).unwrap();
        assert_eq!(reopened.lock().unwrap().doc.bit_depth, depth);
        assert!(server::dispatch(
            &reopened,
            "document",
            &json!({"action":"export","actor":"human","path":path,"overwrite":true})
        )
        .unwrap_err()
        .contains("working source"));
        assert!(server::export_confirmed_source(&shared, &path, &doc.id, doc.revision).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let direct = temp.file("direct.JXL", b"Exact previous destination");
        assert!(server::export(&shared, &direct).is_err());
        assert_eq!(fs::read(&direct).unwrap(), b"Exact previous destination");
        server::export_confirmed_source(&shared, &direct, &doc.id, doc.revision).unwrap();
        assert_eq!(
            words(
                jpeg_xl::decode(&fs::read(&direct).unwrap(), &|| Ok(()))
                    .unwrap()
                    .pixels
            ),
            expected
        );
        assert!(fs::read_dir(temp.0.join(".peerbrush-versions"))
            .unwrap()
            .flatten()
            .any(|e| fs::read(e.path()).unwrap() == b"Exact previous destination"));
        for (extension, options) in [
            ("png", json!({"lossless":true})),
            ("jxl", json!({"distance":0})),
            ("jxl", json!({"unknown":true})),
        ] {
            let invalid = temp.0.join(format!("invalid.{}", extension));
            assert!(server::dispatch(
                &shared,
                "document",
                &json!({"action":"export","path":invalid,"jxl":options})
            )
            .is_err());
            assert!(!invalid.exists());
        }
        let wrong = temp.0.join("wrong-depth.jxl");
        assert!(server::dispatch(
            &shared,
            "document",
            &json!({"action":"export","path":wrong,"bit_depth":if depth==8{16}else{8}})
        )
        .is_err());
        assert!(!wrong.exists());
        assert_eq!(state(&shared), before);
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
