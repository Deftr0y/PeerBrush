use image::{codecs::png::PngDecoder, ImageDecoder};
use peerbrush::{
    effects::Effect,
    engine::{Document, Engine, Layer, Mask, MaskStep},
    production_export::{
        self as export, Control, Crop, DuplicateNames, Format, Resampling, Settings, Variant,
    },
    raster::Raster,
};
use serde_json::json;
use std::io::Cursor;

fn decoded(bytes: &[u8], depth: u16) -> (u32, u32, Vec<u16>) {
    let decoder = PngDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(
        decoder.color_type(),
        if depth == 16 {
            image::ColorType::Rgba16
        } else {
            image::ColorType::Rgba8
        }
    );
    let image = image::load_from_memory(bytes).unwrap().into_rgba16();
    (image.width(), image.height(), image.into_raw())
}
fn effect() -> Effect {
    Effect {
        id: peerbrush::engine::id(),
        kind: "invert".into(),
        enabled: true,
        weight: 0.7,
        settings: json!({}),
    }
}
fn fixture(depth: u16) -> Document {
    let mut doc = Document::new_depth(16, 12, depth).unwrap();
    doc.name = "Production.psd".into();
    doc.srgb_tagged = true;
    for y in 4..8 {
        for x in 3..10 {
            doc.layers[0]
                .pixels
                .set16(x, y, [12347, 23459, 34571, 65535]);
        }
    }
    doc
}
#[test]
fn mcp_exports_returns_pixels_and_coordinates_and_rejects_stale_or_mistyped_inputs() {
    use peerbrush::server;
    let mut engine = Engine::new();
    engine.doc = fixture(16);
    let shared = std::sync::Arc::new(std::sync::Mutex::new(engine));
    let doc = shared.lock().unwrap().doc.clone();
    let before = serde_json::to_value(&doc).unwrap();
    let request = json!({"action":"preview","document_id":doc.id,"expected_revision":doc.revision,"targets":[doc.layers[0].id],"settings":Settings {crop:Crop::Alpha,padding:2,..Default::default()}});
    let result = server::mcp(
        &shared,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_exports","arguments":request}}),
    );
    assert_eq!(result["result"]["isError"], false, "{result}");
    let content = result["result"]["content"].as_array().unwrap();
    assert!(content.iter().any(|v| v["type"] == "image"
        && v["mimeType"] == "image/png"
        && v["data"].as_str().unwrap().len() > 32));
    let text = content.iter().find(|v| v["type"] == "text").unwrap()["text"]
        .as_str()
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        metadata["plan"]["outputs"][0]["document_rect"],
        json!([1, 2, 12, 10])
    );
    assert_eq!(metadata["plan"]["outputs"][0]["bit_depth"], 16);
    for (key, bad) in [
        ("preset", json!(null)),
        ("actor", json!(false)),
        ("output_index", json!(-1)),
        ("max_edge", json!("large")),
        ("expected_revision", json!(1)),
    ] {
        let mut bad_request = request.clone();
        bad_request[key] = bad;
        assert!(
            server::dispatch(&shared, "exports", &bad_request).is_err(),
            "{key}"
        );
    }
    assert_eq!(
        serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
        before
    );
    assert!(shared.lock().unwrap().undo.is_empty());
}
#[test]
fn native_composite_presets_match_current_png_filters_depth_and_icc_without_editing() {
    for depth in [8, 16] {
        for tagged in [false, true] {
            let mut doc = fixture(depth);
            doc.srgb_tagged = false;
            if tagged {
                doc.icc_profile = Some(std::sync::Arc::new(
                    peerbrush::color_profile::srgb_profile().to_vec(),
                ));
            }
            doc.filters = vec![effect()];
            let before = serde_json::to_value(&doc).unwrap();
            let plan = export::plan(&doc, &[], &Settings::default(), &Control::default()).unwrap();
            assert!(!plan.isolated);
            assert_eq!(plan.outputs.len(), 1);
            assert_eq!(plan.outputs[0].filename, "Production_composite_native.png");
            let bytes = export::render(
                &doc,
                &plan.outputs[0],
                &Settings::default(),
                &Control::default(),
            )
            .unwrap();
            assert_eq!(bytes, doc.export_png().unwrap());
            let mut decoder = PngDecoder::new(Cursor::new(&bytes)).unwrap();
            assert_eq!(
                decoder.icc_profile().unwrap().as_deref(),
                doc.icc_profile.as_deref().map(Vec::as_slice)
            );
            assert_eq!(decoded(&bytes, depth).0, 16);
            assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        }
    }
}
#[test]
fn selected_folder_batches_render_masks_effects_and_clipping_from_the_shared_engine() {
    for depth in [8, 16] {
        let mut doc = fixture(depth);
        let mut group = Layer::new("Bundle", "group", 16, 12);
        group.pixels = Raster::new_depth(16, 12, depth);
        group.opacity = 0.63;
        group.mask = Some(Mask {
            linked: true,
            enabled: true,
            cache_key: peerbrush::engine::id(),
            steps: vec![MaskStep {
                id: peerbrush::engine::id(),
                kind: "fill".into(),
                enabled: true,
                weight: 1.,
                value: 222.,
                pixels: Raster::new_depth(16, 12, depth),
                settings: json!(null),
            }],
        });
        doc.layers[0].parent = Some(group.id.clone());
        doc.layers[0].effects = vec![effect()];
        let mut clipped = Layer::new("Clip", "paint", 16, 12);
        clipped.pixels = Raster::new_depth(16, 12, depth);
        clipped.parent = Some(group.id.clone());
        clipped.clip_to = Some(doc.layers[0].id.clone());
        clipped.pixels.set16(4, 5, [53001, 44003, 35007, 32223]);
        let mut background = Layer::new("Background", "paint", 16, 12);
        background.pixels = Raster::new_depth(16, 12, depth);
        let background_id = background.id.clone();
        for y in 0..12 {
            for x in 0..16 {
                background.pixels.set16(x, y, [41001, 32003, 23007, 65535]);
            }
        }
        let group_id = group.id.clone();
        let child = doc.layers[0].id.clone();
        doc.layers.insert(0, clipped);
        doc.layers.insert(0, group);
        doc.layers.push(background);
        doc.filters = vec![effect()];
        let filter = doc.filters[0].id.clone();
        let before = serde_json::to_value(&doc).unwrap();
        let plan = export::plan(
            &doc,
            &[group_id, child],
            &Settings::default(),
            &Control::default(),
        )
        .unwrap();
        assert!(plan.isolated);
        assert_eq!(plan.outputs.len(), 1);
        assert_eq!(plan.outputs[0].target_name, "Bundle");
        let bytes = export::render(
            &doc,
            &plan.outputs[0],
            &Settings::default(),
            &Control::default(),
        )
        .unwrap();
        let mut reference = Engine::new();
        reference.doc = doc.clone();
        reference
            .edit(
                "human",
                &[
                    json!({"op":"layer.update","layer":background_id,"visible":false}),
                    json!({"op":"filter.delete","filter":filter}),
                ],
                None,
                None,
                "Isolate reference artwork",
            )
            .unwrap();
        assert_eq!(bytes, reference.doc.export_png().unwrap());
        assert_ne!(bytes, doc.export_png().unwrap());
        let (_, _, words) = decoded(&bytes, depth);
        assert_eq!(&words[..4], &[0; 4]);
        assert!(words[(5 * 16 + 4) * 4 + 3] > 0);
        assert!(words[(5 * 16 + 4) * 4 + 3] < 65535);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }
}
#[test]
fn alpha_padding_and_size_variants_retain_coordinates_and_native_low_samples() {
    for depth in [8, 16] {
        let mut doc = fixture(depth);
        doc.layers[0].pixels.set16(
            3,
            4,
            [12347, 23459, 34571, if depth == 16 { 1 } else { 257 }],
        );
        let settings = Settings {
            crop: Crop::Alpha,
            padding: 2,
            resampling: Resampling::Nearest,
            variants: vec![
                Variant {
                    name: "native".into(),
                    scale: 1.,
                },
                Variant {
                    name: "double".into(),
                    scale: 2.,
                },
            ],
            ..Default::default()
        };
        let plan = export::plan(&doc, &[], &settings, &Control::default()).unwrap();
        let original = decoded(&doc.export_png().unwrap(), depth).2;
        for (index, output) in plan.outputs.iter().enumerate() {
            assert_eq!(output.document_rect, [1, 2, 12, 10]);
            assert_eq!(
                (output.width, output.height),
                (11 * (index as u32 + 1), 8 * (index as u32 + 1))
            );
            let (w, h, words) = decoded(
                &export::render(&doc, output, &settings, &Control::default()).unwrap(),
                depth,
            );
            let multiplier = index + 1;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let (sx, sy) = (x / multiplier + 1, y / multiplier + 2);
                    assert_eq!(
                        &words[(y * w as usize + x) * 4..(y * w as usize + x) * 4 + 4],
                        &original[(sy * 16 + sx) * 4..(sy * 16 + sx) * 4 + 4]
                    );
                }
            }
            assert_eq!(output.scale_xy, [multiplier as f64; 2]);
            if depth == 16 {
                assert_eq!(
                    &words[(2 * multiplier * w as usize + 2 * multiplier) * 4
                        ..(2 * multiplier * w as usize + 2 * multiplier) * 4 + 4],
                    &[12347, 23459, 34571, 1]
                );
            }
        }
    }
}
#[test]
fn bilinear_variants_interpolate_premultiplied_alpha_without_transparent_color_halos() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(4, 4, depth).unwrap();
        doc.layers[0].pixels.set16(1, 1, [65535, 0, 0, 65535]);
        doc.layers[0].pixels.set16(2, 1, [0, 0, 65535, 0]);
        let settings = Settings {
            variants: vec![Variant {
                name: "double".into(),
                scale: 2.,
            }],
            ..Default::default()
        };
        let plan = export::plan(&doc, &[], &settings, &Control::default()).unwrap();
        let (_, _, pixels) = decoded(
            &export::render(&doc, &plan.outputs[0], &settings, &Control::default()).unwrap(),
            depth,
        );
        let p = &pixels[(3 * 8 + 3) * 4..(3 * 8 + 3) * 4 + 4];
        assert_eq!(&p[..3], &[65535, 0, 0]);
        assert_eq!(p[3], if depth == 16 { 36863 } else { 143 * 257 });
    }
}
#[test]
fn portable_names_and_numbered_collisions_are_explicit_stable_and_source_bound() {
    let mut doc = fixture(16);
    doc.layers[0].name = "CON".into();
    let mut second = doc.layers[0].clone();
    second.id = peerbrush::engine::id();
    doc.layers.push(second);
    let targets = doc.layers.iter().map(|l| l.id.clone()).collect::<Vec<_>>();
    let mut settings = Settings {
        template: "{layer}".into(),
        ..Default::default()
    };
    assert!(export::plan(&doc, &targets, &settings, &Control::default())
        .unwrap_err()
        .contains("collide"));
    settings.duplicate_names = DuplicateNames::Number;
    let first = export::plan(&doc, &targets, &settings, &Control::default()).unwrap();
    let second = export::plan(&doc, &targets, &settings, &Control::default()).unwrap();
    assert_eq!(
        first
            .outputs
            .iter()
            .map(|o| o.filename.as_str())
            .collect::<Vec<_>>(),
        vec!["_CON.png", "_CON_2.png"]
    );
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(second).unwrap()
    );
    assert_eq!(export::safe_name("../A:B\\C? ."), "_A_B_C_");
    assert_eq!(export::safe_name("nul.txt"), "_nul.txt");
}
#[test]
fn invalid_exports_and_cancellation_keep_original_precision_pixels_and_definitions() {
    let mut doc = fixture(16);
    let before = serde_json::to_value(&doc).unwrap();
    let mut settings = Settings::default();
    let control = Control::default();
    let plan = export::plan(&doc, &[], &settings, &control).unwrap();
    control.cancel();
    assert!(export::plan(&doc, &[], &settings, &control).is_err());
    assert!(export::render(&doc, &plan.outputs[0], &settings, &control).is_err());
    for template in ["../{layer}", "{unknown}", "X/Y"] {
        settings.template = template.into();
        assert!(settings.validate().is_err());
    }
    settings = Settings::default();
    settings.variants[0].scale = f64::INFINITY;
    assert!(settings.validate().is_err());
    settings.variants[0].scale = 8.;
    settings.padding = 4096;
    assert!(export::plan(&doc, &[], &settings, &Control::default()).is_err());
    assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    doc.read_only = true;
    doc.warnings = vec!["Unsupported Photoshop style".into()];
    assert!(export::plan(
        &doc,
        &[doc.layers[0].id.clone()],
        &Settings::default(),
        &Control::default()
    )
    .is_err());
    assert!(export::plan(&doc, &[], &Settings::default(), &Control::default()).is_ok());
    doc.read_only = false;
    doc.layers[0].clip_to = Some("external-base".into());
    assert!(export::plan(
        &doc,
        &[doc.layers[0].id.clone()],
        &Settings::default(),
        &Control::default()
    )
    .unwrap_err()
    .contains("clipped"));
}

fn wait_job(shared: &peerbrush::server::Shared, run: &str) -> serde_json::Value {
    for _ in 0..1000 {
        let state =
            peerbrush::server::dispatch(shared, "exports", &json!({"action":"status","run":run}))
                .unwrap();
        if state["status"] != "running" {
            return state;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("Export run did not finish")
}
#[test]
fn export_presets_persist_with_revision_and_disk_conflict_guards_outside_history() {
    use export::library::Library;
    let root = std::env::temp_dir().join(format!(
        "peerbrush-export-library-{}",
        peerbrush::engine::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("export-presets.json");
    let mut library = Library::load(path.clone());
    let preset = library
        .save(None, "Production", Settings::default(), 0, "human")
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(library
        .save(Some(&preset.id), "Other", Settings::default(), 0, "agent")
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let loaded = Library::load(path.clone());
    assert_eq!(loaded.snapshot().revision, 1);
    assert_eq!(
        loaded.get(&preset.id).unwrap().settings,
        Settings::default()
    );
    let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
    changed["presets"][0]["name"] = json!("Human revised preset");
    let changed = serde_json::to_vec(&changed).unwrap();
    std::fs::write(&path, &changed).unwrap();
    assert!(library
        .delete(&preset.id, 1, "agent")
        .unwrap_err()
        .contains("disk"));
    assert_eq!(std::fs::read(&path).unwrap(), changed);
    std::fs::write(root.join("bad.json"), b"private user data").unwrap();
    let mut corrupt = Library::load(root.join("bad.json"));
    assert!(corrupt.snapshot().error.is_some());
    assert!(corrupt
        .save(None, "Replacement", Settings::default(), 0, "human")
        .is_err());
    assert_eq!(
        std::fs::read(root.join("bad.json")).unwrap(),
        b"private user data"
    );
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn protocol_presets_preview_and_repeated_batches_keep_native_files_profiles_and_source() {
    use peerbrush::{server, workspace};
    use std::sync::{Arc, Mutex};
    for depth in [8, 16] {
        let root = std::env::temp_dir().join(format!(
            "peerbrush-export-batch-{}",
            peerbrush::engine::id()
        ));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let mut engine = Engine::new();
        engine.doc = fixture(depth);
        engine.doc.layers[0].name = "Art:Base".into();
        let shared = Arc::new(Mutex::new(engine));
        workspace::attach(&shared);
        let doc = shared.lock().unwrap().doc.clone();
        let before = serde_json::to_value(&doc).unwrap();
        let project = shared.lock().unwrap().project_id.clone();
        let settings = Settings {
            crop: Crop::Alpha,
            padding: 2,
            variants: vec![
                Variant {
                    name: "native".into(),
                    scale: 1.,
                },
                Variant {
                    name: "half".into(),
                    scale: 0.5,
                },
            ],
            ..Default::default()
        };
        let saved=server::dispatch(&shared,"exports",&json!({"action":"save_preset","name":"Production","settings":settings,"expected_library_revision":0,"actor":"human"})).unwrap();
        let preset = saved["preset"]["id"].as_str().unwrap();
        let request = json!({"action":"preview","project_id":project,"document_id":doc.id,"expected_revision":doc.revision,"preset":preset,"expected_library_revision":1,"targets":[doc.layers[0].id]});
        let preview = server::dispatch(&shared, "exports", &request).unwrap();
        assert_eq!(preview["preview_only"], true);
        assert_eq!(preview["images"][0]["document_rect"], json!([1, 2, 12, 10]));
        assert_eq!(preview["images"][0]["native_source_depth"], depth);
        for directory in [&first, &second] {
            let mut request = request.clone();
            request["action"] = json!("start");
            request["directory"] = json!(directory);
            request["actor"] = json!("human");
            let started = server::dispatch(&shared, "exports", &request).unwrap();
            let state = wait_job(&shared, started["run"].as_str().unwrap());
            assert_eq!(state["status"], "completed", "{state}");
            assert_eq!(state["outputs"].as_array().unwrap().len(), 2);
            for (index, receipt) in state["outputs"].as_array().unwrap().iter().enumerate() {
                assert_eq!(receipt["status"], "written");
                let output: &export::Output =
                    &serde_json::from_value(receipt["output"].clone()).unwrap();
                assert_eq!(
                    output.filename,
                    format!(
                        "Production_Art_Base_{}.png",
                        if index == 0 { "native" } else { "half" }
                    )
                );
                let bytes = std::fs::read(directory.join(&output.filename)).unwrap();
                assert_eq!(
                    bytes,
                    export::render(&doc, output, &settings, &Control::default()).unwrap()
                );
                let mut decoder = PngDecoder::new(Cursor::new(&bytes)).unwrap();
                assert_eq!(
                    decoder.icc_profile().unwrap().as_deref(),
                    Some(peerbrush::color_profile::srgb_profile())
                );
                assert_eq!(decoded(&bytes, depth).0, output.width);
            }
        }
        for filename in [
            "Production_Art_Base_native.png",
            "Production_Art_Base_half.png",
        ] {
            assert_eq!(
                std::fs::read(first.join(filename)).unwrap(),
                std::fs::read(second.join(filename)).unwrap()
            );
        }
        assert_eq!(
            serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
            before
        );
        assert!(shared.lock().unwrap().undo.is_empty());
        assert!(shared.lock().unwrap().path.is_none());
        drop(shared);
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn batch_existing_file_policies_report_written_skipped_failed_and_unattempted_outputs() {
    use peerbrush::server;
    use std::sync::{Arc, Mutex};
    let root = std::env::temp_dir().join(format!(
        "peerbrush-export-policy-{}",
        peerbrush::engine::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut engine = Engine::new();
    engine.doc = fixture(16);
    let shared = Arc::new(Mutex::new(engine));
    let doc = shared.lock().unwrap().doc.clone();
    let settings = Settings {
        variants: vec![
            Variant {
                name: "one".into(),
                scale: 1.,
            },
            Variant {
                name: "two".into(),
                scale: 1.,
            },
        ],
        ..Default::default()
    };
    let existing = root.join("Production_composite_one.png");
    std::fs::write(&existing, b"Original user PNG").unwrap();
    let request = json!({"action":"start","actor":"human","document_id":doc.id,"expected_revision":doc.revision,"directory":root,"settings":settings});
    let started = server::dispatch(&shared, "exports", &request).unwrap();
    let state = wait_job(&shared, started["run"].as_str().unwrap());
    assert_eq!(state["status"], "failed");
    assert_eq!(state["outputs"][0]["status"], "failed");
    assert_eq!(state["outputs"][1]["status"], "not_written");
    assert_eq!(std::fs::read(&existing).unwrap(), b"Original user PNG");
    let mut skipped = request.clone();
    skipped["existing"] = json!("skip");
    let started = server::dispatch(&shared, "exports", &skipped).unwrap();
    let state = wait_job(&shared, started["run"].as_str().unwrap());
    assert_eq!(state["status"], "completed");
    assert_eq!(state["outputs"][0]["status"], "skipped");
    assert_eq!(state["outputs"][1]["status"], "written");
    assert_eq!(std::fs::read(&existing).unwrap(), b"Original user PNG");
    let mut replaced = request;
    replaced["existing"] = json!("replace");
    let started = server::dispatch(&shared, "exports", &replaced).unwrap();
    let state = wait_job(&shared, started["run"].as_str().unwrap());
    assert_eq!(state["status"], "completed");
    assert_eq!(state["outputs"][0]["status"], "written");
    assert_eq!(state["outputs"][1]["status"], "written");
    assert_eq!(std::fs::read(&existing).unwrap(), doc.export_png().unwrap());
    assert!(std::fs::read_dir(root.join(".peerbrush-versions"))
        .unwrap()
        .flatten()
        .any(|v| {
            v.path().extension().is_some_and(|e| e == "png")
                && std::fs::read(v.path()).unwrap() == b"Original user PNG"
        }));
    drop(shared);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn lossless_webp_retains_rendered_alpha_icc_and_declares_derived_eight_bit_output() {
    for depth in [8, 16] {
        for tagged in [0, 1] {
            let mut doc = fixture(depth);
            doc.srgb_tagged = tagged == 1;
            doc.layers[0]
                .pixels
                .set16(3, 4, [12347, 23459, 34571, 32781]);
            doc.layers[0].pixels.set16(
                4,
                4,
                [45679, 23459, 12347, if depth == 16 { 129 } else { 257 }],
            );
            doc.filters = vec![effect()];
            let before = serde_json::to_value(&doc).unwrap();
            let png = doc.export_png().unwrap();
            let expected: Vec<_> = decoded(&png, depth)
                .2
                .into_iter()
                .map(peerbrush::raster::project16)
                .collect();
            let settings = Settings {
                format: Format::Webp,
                ..Default::default()
            };
            let plan = export::plan(&doc, &[], &settings, &Control::default()).unwrap();
            let output = &plan.outputs[0];
            assert_eq!(output.filename, "Production_composite_native.webp");
            assert_eq!(output.format, Format::Webp);
            assert_eq!(output.native_source_depth, depth);
            assert_eq!(output.bit_depth, 8);
            let bytes = export::render(&doc, output, &settings, &Control::default()).unwrap();
            assert_eq!(&bytes[..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WEBP");
            let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(&bytes)).unwrap();
            assert_eq!(decoder.dimensions(), (doc.width, doc.height));
            assert_eq!(decoder.color_type(), image::ColorType::Rgba8);
            assert_eq!(
                decoder.icc_profile().unwrap().as_deref(),
                if tagged == 0 {
                    None
                } else {
                    Some(peerbrush::color_profile::srgb_profile())
                }
            );
            let actual = image::load_from_memory(&bytes)
                .unwrap()
                .into_rgba8()
                .into_raw();
            assert_eq!(actual, expected);
            assert!(actual.chunks_exact(4).any(|pixel| pixel[3] == 0));
            assert!(actual
                .chunks_exact(4)
                .any(|pixel| pixel[3] > 0 && pixel[3] < 255));
            assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            assert_eq!(doc.export_png().unwrap(), png);
            let saved = peerbrush::psd::decode(&peerbrush::psd::encode_psb(&doc).unwrap()).unwrap();
            assert_eq!(saved.bit_depth, depth);
            assert_eq!(
                saved.layers[0].pixels.rgba16(),
                doc.layers[0].pixels.rgba16()
            );
            let mut mismatched = output.clone();
            mismatched.format = Format::Png;
            assert!(export::render(&doc, &mismatched, &settings, &Control::default()).is_err());
            mismatched = output.clone();
            mismatched.bit_depth = 16;
            assert!(export::render(&doc, &mismatched, &settings, &Control::default()).is_err());
        }
    }
}

#[test]
fn webp_crop_padding_and_resampling_match_native_png_before_final_projection() {
    for depth in [8, 16] {
        let doc = fixture(depth);
        for resampling in [Resampling::Nearest, Resampling::Bilinear] {
            let settings = Settings {
                crop: Crop::Alpha,
                padding: 2,
                resampling,
                variants: vec![Variant {
                    name: "scaled".into(),
                    scale: 1.7,
                }],
                ..Default::default()
            };
            let png_plan = export::plan(&doc, &[], &settings, &Control::default()).unwrap();
            let png =
                export::render(&doc, &png_plan.outputs[0], &settings, &Control::default()).unwrap();
            let webp_settings = Settings {
                format: Format::Webp,
                ..settings
            };
            let webp_plan = export::plan(&doc, &[], &webp_settings, &Control::default()).unwrap();
            assert_eq!(
                webp_plan.outputs[0].document_rect,
                png_plan.outputs[0].document_rect
            );
            let webp = export::render(
                &doc,
                &webp_plan.outputs[0],
                &webp_settings,
                &Control::default(),
            )
            .unwrap();
            let expected: Vec<_> = decoded(&png, depth)
                .2
                .into_iter()
                .map(peerbrush::raster::project16)
                .collect();
            assert_eq!(
                image::load_from_memory(&webp)
                    .unwrap()
                    .into_rgba8()
                    .into_raw(),
                expected
            );
            let control = Control::default();
            control.cancel();
            assert!(export::render(&doc, &webp_plan.outputs[0], &webp_settings, &control).is_err());
        }
    }
}

#[test]
fn old_presets_default_to_png_and_webp_presets_use_the_same_guarded_library() {
    use export::library::Library;
    let root = std::env::temp_dir().join(format!("pb-webp-presets-{}", peerbrush::engine::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("export-presets.json");
    let mut old_settings = serde_json::to_value(Settings::default()).unwrap();
    old_settings.as_object_mut().unwrap().remove("format");
    let old = serde_json::to_vec(&json!({"version":1,"revision":7,"presets":[{"id":"export-original","name":"Original PNG","settings":old_settings}]})).unwrap();
    std::fs::write(&path, &old).unwrap();
    let mut library = Library::load(path.clone());
    assert!(library.snapshot().error.is_none());
    assert_eq!(
        library.get("export-original").unwrap().settings.format,
        Format::Png
    );
    assert_eq!(std::fs::read(&path).unwrap(), old);
    let preset = library
        .save(
            None,
            "WebP",
            Settings {
                format: Format::Webp,
                ..Default::default()
            },
            7,
            "human",
        )
        .unwrap();
    let loaded = Library::load(path.clone());
    assert_eq!(
        loaded.get(&preset.id).unwrap().settings.format,
        Format::Webp
    );
    assert_eq!(
        loaded.get("export-original").unwrap().settings.format,
        Format::Png
    );
    let saved = std::fs::read(&path).unwrap();
    for invalid in [json!("jpeg"), json!(null), json!(8)] {
        let mut settings = old_settings.clone();
        settings["format"] = invalid;
        assert!(serde_json::from_value::<Settings>(settings).is_err());
    }
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn webp_shared_protocol_preview_batch_and_reopening_keep_the_original_project() {
    use peerbrush::server;
    use std::sync::{Arc, Mutex};
    for depth in [8, 16] {
        let root =
            std::env::temp_dir().join(format!("pb-webp-protocol-{}", peerbrush::engine::id()));
        std::fs::create_dir(&root).unwrap();
        let mut engine = Engine::new();
        engine.doc = fixture(depth);
        engine.doc.layers[0]
            .pixels
            .set16(3, 4, [12347, 23459, 34571, 32781]);
        let doc = engine.doc.clone();
        let shared = Arc::new(Mutex::new(engine));
        let before = serde_json::to_value(&doc).unwrap();
        let settings = Settings {
            format: Format::Webp,
            ..Default::default()
        };
        let request = json!({"action":"preview","actor":"human","document_id":doc.id,"expected_revision":doc.revision,"settings":settings});
        let preview = server::dispatch(&shared, "exports", &request).unwrap();
        assert_eq!(preview["images"][0]["mime_type"], "image/png");
        assert_eq!(preview["images"][0]["output_format"], "webp");
        assert_eq!(preview["images"][0]["output_bit_depth"], 8);
        assert_eq!(preview["images"][0]["native_source_depth"], depth);
        let mut start = request.clone();
        start["action"] = json!("start");
        start["directory"] = json!(root);
        let run = server::dispatch(&shared, "exports", &start).unwrap();
        let state = wait_job(&shared, run["run"].as_str().unwrap());
        assert_eq!(state["status"], "completed", "{state}");
        assert_eq!(state["outputs"][0]["output"]["bit_depth"], 8);
        assert_eq!(state["outputs"][0]["output"]["native_source_depth"], depth);
        let path = root.join("Production_composite_native.webp");
        let bytes = std::fs::read(&path).unwrap();
        let rerun = server::dispatch(&shared, "exports", &start).unwrap();
        let failed = wait_job(&shared, rerun["run"].as_str().unwrap());
        assert_eq!(failed["status"], "failed", "{failed}");
        assert_eq!(failed["outputs"][0]["status"], "failed");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        start["existing"] = json!("skip");
        let rerun = server::dispatch(&shared, "exports", &start).unwrap();
        let skipped = wait_job(&shared, rerun["run"].as_str().unwrap());
        assert_eq!(skipped["status"], "completed", "{skipped}");
        assert_eq!(skipped["outputs"][0]["status"], "skipped");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let expected = image::load_from_memory(&bytes).unwrap().into_rgba8();
        let reopened = Arc::new(Mutex::new(Engine::new()));
        server::open(&reopened, &path).unwrap();
        assert_eq!(reopened.lock().unwrap().doc.bit_depth, 8);
        assert_eq!(
            reopened.lock().unwrap().doc.layers[0].pixels.rgba(),
            expected.as_raw().clone()
        );
        assert_eq!(
            peerbrush::image_import::decode_file(
                &path,
                &json!({}),
                peerbrush::image_import::BUDGET
            )
            .unwrap()
            .rgba(),
            expected.into_raw()
        );
        let direct = root.join("direct.WEBP");
        std::fs::write(&direct, b"Previous exact WebP").unwrap();
        assert!(server::export(&shared, &direct).is_err());
        assert_eq!(std::fs::read(&direct).unwrap(), b"Previous exact WebP");
        server::export_confirmed_source(&shared, &direct, &doc.id, doc.revision).unwrap();
        assert_eq!(std::fs::read(&direct).unwrap(), bytes);
        assert!(std::fs::read_dir(root.join(".peerbrush-versions"))
            .unwrap()
            .flatten()
            .any(|entry| std::fs::read(entry.path()).unwrap() == b"Previous exact WebP"));
        assert!(server::export_confirmed_source(&shared, &path, &doc.id, doc.revision).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(server::dispatch(
            &shared,
            "document",
            &json!({"action":"export","path":root.join("invalid.webp"),"bit_depth":16})
        )
        .is_err());
        assert!(!root.join("invalid.webp").exists());
        let mut invalid = request.clone();
        invalid["settings"]["format"] = json!("jpeg");
        assert!(server::dispatch(&shared, "exports", &invalid).is_err());
        let tools = server::tools();
        let tool = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "peerbrush_exports")
            .unwrap();
        assert_eq!(
            tool["inputSchema"]["properties"]["settings"]["properties"]["format"]["enum"],
            json!(["png", "webp"])
        );
        let mcp = server::mcp(
            &shared,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_exports","arguments":request}}),
        );
        assert_eq!(mcp["result"]["isError"], false, "{mcp}");
        assert!(mcp["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "image" && item["mimeType"] == "image/png"));
        assert_eq!(
            serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
            before
        );
        assert!(shared.lock().unwrap().undo.is_empty());
        drop(reopened);
        drop(shared);
        std::fs::remove_dir_all(root).unwrap();
    }
}
