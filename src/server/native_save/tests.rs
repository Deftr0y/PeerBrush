use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    native::{worker::tests::Fixture, NativeDocument},
    scene::{CapturedScene, LegacyTileCache},
    Document,
};

fn with_document(depth: u16, run: impl FnOnce(&Shared, &Path, [u16; 4])) {
    with_document_profile(depth, false, run);
}

fn with_document_profile(depth: u16, srgb_tagged: bool, run: impl FnOnce(&Shared, &Path, [u16; 4])) {
    let fixture = Fixture::new();
    let shared = Arc::new(Mutex::new(Engine::new()));
    let mut connection = super::super::start(shared.clone(), fixture.path().join("state")).unwrap();
    connection
        .instance_lock
        .as_mut()
        .unwrap()
        .stop_recovery_for_test();
    connection.configure_native_sources(None, true).unwrap();
    let runtime = crate::workspace::native_dispatch::runtime_for(&shared).unwrap();
    let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
    let mut document = Document::new_depth(4, 2, depth).unwrap();
    document.srgb_tagged = srgb_tagged;
    for y in 0..2 {
        for x in 0..4 {
            document.layers[0]
                .pixels
                .set16(x, y, [12347, 23459, 34571, 65535]);
        }
    }
    let expected = document.layers[0].pixels.get16(1, 1);
    let mut staged = CapturedScene::from_parts(document, "save-fixture".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    {
        let mut engine = shared.lock().unwrap();
        engine.authority = DocumentAuthority::from_native(native, 8).unwrap();
        engine.saved_revision = u64::MAX;
    }
    drop(session);
    run(&shared, fixture.path(), expected);
    drop(shared);
    drop(cache);
    drop(runtime);
    drop(connection);
}

#[test]
fn flattened_file_export_uses_native_authority_and_keeps_original_samples() {
    for depth in [8, 16] {
        with_document(depth, |shared, directory, expected| {
            let (document, revision, key) = {
                let engine = shared.lock().unwrap();
                let native = engine.native_document().unwrap();
                (
                    native.metadata().id().to_owned(),
                    native.metadata().revision(),
                    native.key(),
                )
            };
            let png = directory.join("Native.png");
            super::super::export_confirmed_source(shared, &png, &document, revision).unwrap();
            let raster = crate::image_import::decode_file(&png, &json!({}), 1024 * 1024).unwrap();
            assert_eq!((raster.width, raster.height, raster.depth), (4, 2, depth));
            assert_eq!(raster.get16(1, 1), expected);

            let webp = directory.join("Native.webp");
            let result = super::super::dispatch_owned(
                shared,
                "document",
                &json!({
                    "action":"export", "path":webp, "document_id":document,
                    "expected_revision":revision, "bit_depth":8,
                }),
            );
            assert!(result.is_ok());
            let raster = crate::image_import::decode_file(&webp, &json!({}), 1024 * 1024).unwrap();
            assert_eq!((raster.width, raster.height, raster.depth), (4, 2, 8));
            assert_eq!(raster.get(1, 1), expected.map(crate::raster::project16));

            let bytes = fs::read(&png).unwrap();
            assert!(super::super::export(shared, &png).is_err());
            assert!(
                super::super::export_confirmed_source(shared, &png, &document, revision + 1)
                    .is_err()
            );
            assert_eq!(fs::read(&png).unwrap(), bytes);
            let engine = shared.lock().unwrap();
            assert_eq!(engine.native_document().unwrap().key(), key);
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert!(engine.path.is_none());
        });
    }
}

#[test]
fn native_production_export_plans_previews_and_publishes_the_same_outputs() {
    use crate::production_export::{Crop, Settings, Variant};
    use base64::Engine as _;
    for depth in [8, 16] {
        with_document(depth, |shared, directory, expected| {
            let (project, document, revision, layer, key) = {
                let engine = shared.lock().unwrap();
                let native = engine.native_document().unwrap();
                let metadata = native.metadata();
                (
                    engine.project_id.clone(),
                    metadata.id().to_owned(),
                    metadata.revision(),
                    metadata.layer(0).unwrap().id().to_owned(),
                    native.key(),
                )
            };
            let settings = Settings {
                crop: Crop::Alpha,
                padding: 1,
                variants: vec![
                    Variant {
                        name: "base".into(),
                        scale: 1.0,
                    },
                    Variant {
                        name: "large".into(),
                        scale: 2.0,
                    },
                ],
                ..Settings::default()
            };
            let mut request = json!({
                "action":"plan", "project_id":project, "document_id":document,
                "expected_revision":revision, "targets":[layer], "settings":settings,
            });
            let plan = crate::production_export::request(shared, &request).unwrap()["plan"].clone();
            assert_eq!(plan["isolated"], true);
            assert_eq!(plan["outputs"][0]["document_rect"], json!([-1, -1, 5, 3]));
            assert_eq!(plan["outputs"][1]["width"], 12);
            request["action"] = json!("preview");
            let preview = crate::production_export::request(shared, &request).unwrap();
            assert_eq!(preview["plan"], plan);
            let preview = base64::engine::general_purpose::STANDARD
                .decode(preview["images"][0]["data"].as_str().unwrap())
                .unwrap();
            let preview = image::load_from_memory(&preview).unwrap().into_rgba8();
            assert_eq!(preview.dimensions(), (6, 4));
            assert_eq!(preview.get_pixel(0, 0).0, [0; 4]);
            assert_eq!(
                preview.get_pixel(2, 2).0,
                crate::depth16::display_pixel(expected)
            );

            request["action"] = json!("start");
            request["actor"] = json!("human");
            request["directory"] = json!(directory);
            let started = crate::production_export::request(shared, &request).unwrap();
            let status = json!({"action":"status", "run":started["run"]});
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            let finished = loop {
                let state = crate::production_export::request(shared, &status).unwrap();
                if state["status"] != "running" {
                    break state;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "Batch export did not finish: {state}"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            assert_eq!(finished["status"], "completed", "{finished}");
            for (index, receipt) in finished["outputs"].as_array().unwrap().iter().enumerate() {
                assert_eq!(receipt["status"], "written");
                assert_eq!(receipt["output"], plan["outputs"][index]);
                let raster = crate::image_import::decode_file(
                    Path::new(receipt["path"].as_str().unwrap()),
                    &json!({}),
                    1024 * 1024,
                )
                .unwrap();
                assert_eq!(
                    (raster.width, raster.height, raster.depth),
                    (6 * (index as u32 + 1), 4 * (index as u32 + 1), depth)
                );
                assert_eq!(raster.get16(0, 0), [0; 4]);
                let center = 2 * (index as i32 + 1);
                assert_eq!(raster.get16(center, center), expected);
            }
            let engine = shared.lock().unwrap();
            assert_eq!(engine.native_document().unwrap().key(), key);
            assert_eq!(engine.history_lengths(), [0, 0]);
        });
    }
}

#[test]
fn recovery_protocol_lists_and_restores_native_checkpoints_with_owned_replies() {
    for depth in [8, 16] {
        with_document(depth, |shared, directory, expected| {
            let workspace = crate::workspace::attach(shared);
            workspace.lock().unwrap().recovery_dir = Some(directory.to_owned());
            let scene = shared
                .lock()
                .unwrap()
                .capture_scene(None)
                .unwrap()
                .into_native()
                .unwrap();
            let session = crate::workspace::native_dispatch::begin_scene(shared, &scene).unwrap();
            let limits = crate::recovery::backed::Limits {
                metadata: crate::psd::canonical36::Limits {
                    bytes: 65536,
                    nodes: 8192,
                    depth: 64,
                },
                refs: 1024,
                stores: 16,
            };
            let id = uuid::Uuid::new_v4();
            crate::recovery::backed::Capture::from_native(&scene, &session, limits, &|| Ok(()))
                .unwrap()
                .publish(*id.as_bytes(), &session, &|| Ok(()))
                .unwrap();
            let listed = super::super::dispatch_owned(
                shared,
                "recovery",
                &serde_json::json!({"action":"list"}),
            )
            .unwrap();
            assert!(listed.admitted());
            let record = &listed.value()["native_versions"][0];
            assert_eq!(record["snapshot"], format!("native:{id}"));
            assert_eq!(record["bit_depth"], depth);
            assert!(record["error"].is_null());
            assert!(listed.value()["native_catalog_error"].is_null());
            assert_eq!(
                listed.value()["native_discard_available"],
                crate::recovery::NATIVE_DISCARD_AVAILABLE
            );
            let restored = super::super::dispatch_owned(
                shared,
                "recovery",
                &serde_json::json!({"action":"restore","snapshot":record["snapshot"]}),
            )
            .unwrap();
            assert_eq!(restored.value()["recovery_retained"], true);
            let project = restored.value()["project_id"].as_str().unwrap();
            let tab = crate::workspace::get(shared, project).unwrap();
            let recovered = tab
                .lock()
                .unwrap()
                .capture_scene(None)
                .unwrap()
                .into_native()
                .unwrap();
            let source = recovered.rasters().iter().next().unwrap().1;
            let region = crate::raster::backed::Region::new([0, 0, 4, 2]).unwrap();
            assert_eq!(
                source
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(1, 1)
                    .unwrap(),
                expected
            );
            assert_eq!(
                shared.lock().unwrap().native_document().unwrap().key(),
                scene.key()
            );
            assert!(session.open_recovery_marker(*id.as_bytes()).is_ok());
            let disk = session.disk().used().unwrap();
            let bytes = session.open_recovery_marker(*id.as_bytes()).unwrap().len();
            drop(session);
            if !crate::recovery::NATIVE_DISCARD_AVAILABLE {
                assert!(super::super::dispatch_owned(
                    shared,
                    "recovery",
                    &json!({
                        "action":"discard", "snapshot":format!("native:{id}"), "actor":"human"
                    })
                )
                .is_err());
                return;
            }
            for actor in ["agent", ""] {
                let error = super::super::dispatch_owned(
                    shared,
                    "recovery",
                    &json!({
                        "action":"discard", "snapshot":format!("native:{id}"), "actor":actor
                    }),
                )
                .err()
                .unwrap();
                assert!(error.contains("Only human input"));
            }
            let discarded = super::super::dispatch_owned(
                shared,
                "recovery",
                &json!({
                    "action":"discard", "snapshot":format!("native:{id}"), "actor":"human"
                }),
            )
            .unwrap();
            assert_eq!(discarded.value()["discarded"], format!("native:{id}"));
            let runtime = crate::workspace::native_dispatch::runtime_for(shared).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk - bytes);
            let listed =
                super::super::dispatch_owned(shared, "recovery", &json!({"action":"list"}))
                    .unwrap();
            assert_eq!(listed.value()["native_versions"], json!([]));
            assert_eq!(
                source
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(1, 1)
                    .unwrap(),
                expected
            );
        });
    }
}

#[test]
fn native_save_and_reopen_use_the_same_full_layer_effects_as_export() {
    for depth in [8, 16] {
        with_document(depth, |shared, directory, expected| {
            let (document, layer) = {
                let engine = shared.lock().unwrap();
                let doc = engine.native_document().unwrap().metadata();
                (doc.id().to_owned(), doc.layer(0).unwrap().id().to_owned())
            };
            super::super::dispatch_owned(shared, "edit", &json!({
                "actor":"human", "document_id":document, "expected_revision":0, "max_edge":32,
                "commands":[{"op":"canvas.resize", "width":12, "height":5, "anchor":"top_left"}],
            })).unwrap();
            let mut settings = crate::effects::defaults("drop_shadow");
            settings["distance"] = json!(4.);
            settings["angle"] = json!(0.);
            settings["size"] = json!(0.);
            super::super::dispatch_owned(shared, "edit", &json!({
                "actor":"human", "document_id":document, "expected_revision":1, "max_edge":32,
                "commands":[{"op":"effect.add", "layer":layer, "kind":"drop_shadow", "settings":settings}],
            })).unwrap();
            let revision = shared
                .lock()
                .unwrap()
                .native_document()
                .unwrap()
                .metadata()
                .revision();
            let before = directory.join("Styled before.png");
            super::super::export_confirmed_source(shared, &before, &document, revision).unwrap();
            let before =
                crate::image_import::decode_file(&before, &json!({}), 1024 * 1024).unwrap();
            assert!(
                before.get16(6, 1)[3] > 0,
                "The exported shadow extends outside the original layer"
            );

            let saved = directory.join("Styled.psb");
            super::super::save_guarded(shared, &saved, &json!({
                "document_id":document, "expected_revision":revision, "accept_editability_loss":true,
            })).unwrap();
            let copy = directory.join("Styled reopened.psb");
            fs::copy(saved, &copy).unwrap();
            let reply = super::super::dispatch_owned(
                shared,
                "projects",
                &json!({"action":"open", "actor":"human", "path":copy}),
            )
            .unwrap();
            let opened =
                crate::workspace::get(shared, reply.value()["project_id"].as_str().unwrap())
                    .unwrap();
            {
                let engine = opened.lock().unwrap();
                let native = engine.native_document().unwrap();
                let layer = native.metadata().layer(0).unwrap();
                assert_eq!(layer.effects().next().unwrap().kind(), "drop_shadow");
                let region = crate::raster::backed::Region::new([1, 1, 2, 2]).unwrap();
                let view = native
                    .source(layer.pixels().slot())
                    .unwrap()
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap();
                assert_eq!(view.pixel16(1, 1).unwrap(), expected);
            }
            let after = directory.join("Styled after.png");
            super::super::export(&opened, &after).unwrap();
            let after = crate::image_import::decode_file(&after, &json!({}), 1024 * 1024).unwrap();
            assert_eq!((after.width, after.height, after.depth), (12, 5, depth));
            assert_eq!(after.rgba16(), before.rgba16());
        });
    }
}

#[test]
fn native_large_canvas_can_be_created_edited_and_reopened_above_the_resident_edge() {
    for depth in [8, 16] {
        with_document(depth, |root, directory, expected| {
            let created = super::super::dispatch_owned(root, "projects", &json!({
                "action":"new", "actor":"human", "width":16000, "height":16000, "bit_depth":depth,
            })).unwrap();
            let project = created.value()["project_id"].as_str().unwrap();
            let large = crate::workspace::get(root, project).unwrap();
            let (document, layer) = {
                let engine = large.lock().unwrap();
                let native = engine.native_document().unwrap();
                let doc = native.metadata();
                assert_eq!(
                    (doc.width(), doc.height(), doc.depth()),
                    (16000, 16000, depth)
                );
                let layer = doc.layer(0).unwrap();
                assert!(native
                    .source(layer.pixels().slot())
                    .unwrap()
                    .is_known_zero());
                (doc.id().to_owned(), layer.id().to_owned())
            };
            super::super::dispatch_owned(root, "edit", &json!({
                "actor":"human", "project_id":project, "document_id":document,
                "expected_revision":0, "max_edge":32,
                "commands":[
                    {"op":"fill", "layer":layer, "rect":[15999,15999,16000,16000], "color":[21, 89, 141, 255]},
                    {"op":"fill", "layer":layer, "rect":[0,0,1,1], "color":[53, 67, 179, 255]}
                ],
            })).unwrap();
            {
                let engine = large.lock().unwrap();
                let native = engine.native_document().unwrap();
                let source = native
                    .source(native.metadata().layer(0).unwrap().pixels().slot())
                    .unwrap();
                let corner =
                    crate::raster::backed::Region::new([15999, 15999, 16000, 16000]).unwrap();
                let view = source.prepare_region(corner, &mut || Ok(())).unwrap();
                assert_eq!(view.pixel(15999, 15999).unwrap(), [21, 89, 141, 255]);
                assert_eq!(engine.history_lengths(), [1, 0]);
            }
            super::super::dispatch_owned(root, "edit", &json!({
                "actor":"human", "project_id":project, "document_id":document,
                "expected_revision":1, "max_edge":32,
                "commands":[{"op":"transform", "layer":layer, "mirror_x":true, "pivot":[8000,8000]}],
            })).unwrap();
            let untouched = {
                let engine = large.lock().unwrap();
                let native = engine.native_document().unwrap();
                let source = native
                    .source(native.metadata().layer(0).unwrap().pixels().slot())
                    .unwrap();
                assert_eq!((source.width(), source.height()), (16000, 16000));
                assert_eq!(source.physical_tiles().count(), 2);
                let corner = crate::raster::backed::Region::new([0, 15999, 1, 16000]).unwrap();
                let view = source.prepare_region(corner, &mut || Ok(())).unwrap();
                assert_eq!(view.pixel(0, 15999).unwrap(), [21, 89, 141, 255]);
                assert_eq!(engine.history_lengths(), [2, 0]);
                let tile = source
                    .physical_tiles()
                    .find(|tile| tile.coordinate() == (62, 0))
                    .unwrap();
                (tile.owner().identity(), tile.reference())
            };
            super::super::dispatch_owned(
                root,
                "edit",
                &json!({
                    "actor":"human", "project_id":project, "document_id":document,
                    "expected_revision":2, "max_edge":32,
                    "commands":[
                        {"op":"selection", "rect":[0,15999,1,16000]},
                        {"op":"move", "layer":layer, "dx":1024, "dy":0}
                    ],
                }),
            )
            .unwrap();
            {
                let engine = large.lock().unwrap();
                let native = engine.native_document().unwrap();
                let source = native
                    .source(native.metadata().layer(0).unwrap().pixels().slot())
                    .unwrap();
                let tile = source
                    .physical_tiles()
                    .find(|tile| tile.coordinate() == (62, 0))
                    .unwrap();
                assert_eq!((tile.owner().identity(), tile.reference()), untouched);
                for (x, expected) in [(0, [0; 4]), (1024, [21, 89, 141, 255])] {
                    let region =
                        crate::raster::backed::Region::new([x, 15999, x + 1, 16000]).unwrap();
                    let view = source.prepare_region(region, &mut || Ok(())).unwrap();
                    assert_eq!(view.pixel(x, 15999).unwrap(), expected);
                }
                assert_eq!(
                    native.metadata().selection(),
                    Some([1024, 15999, 1025, 16000])
                );
                assert_eq!(engine.history_lengths(), [3, 0]);
            }

            // Keep the source layer small while crossing the old canvas edge
            // so the regular regression does not write a multi-gigabyte file.
            let (original_project, original_document) = {
                let engine = root.lock().unwrap();
                (
                    engine.project_id.clone(),
                    engine.native_document().unwrap().metadata().id().to_owned(),
                )
            };
            super::super::dispatch_owned(root, "edit", &json!({
                "actor":"human", "project_id":original_project, "document_id":original_document,
                "expected_revision":0, "max_edge":32,
                "commands":[{"op":"canvas.resize", "width":8193, "height":17, "anchor":"bottom_right"}],
            })).unwrap();
            let saved = directory.join("Wide.psb");
            super::super::save(root, &saved).unwrap();
            let reopened = directory.join("WideReopened.psb");
            fs::copy(saved, &reopened).unwrap();
            let reply = super::super::dispatch_owned(
                root,
                "projects",
                &json!({
                    "action":"open", "actor":"human", "path":reopened,
                }),
            )
            .unwrap();
            let opened =
                crate::workspace::get(root, reply.value()["project_id"].as_str().unwrap()).unwrap();
            let engine = opened.lock().unwrap();
            let native = engine.native_document().unwrap();
            let doc = native.metadata();
            assert_eq!((doc.width(), doc.height(), doc.depth()), (8193, 17, depth));
            let layer = doc.layer(0).unwrap();
            assert_eq!(layer.origin(), [8189, 15]);
            let region = crate::raster::backed::Region::new([1, 1, 2, 2]).unwrap();
            let view = native
                .source(layer.pixels().slot())
                .unwrap()
                .prepare_region(region, &mut || Ok(()))
                .unwrap();
            assert_eq!(view.pixel16(1, 1).unwrap(), expected);
        });
    }
}

#[test]
fn selected_move_expands_a_large_native_layer_and_retains_untouched_tiles() {
    for depth in [8, 16] {
        with_document(depth, |root, _, _| {
            let reply = super::super::dispatch_owned(root, "projects", &json!({
                "action":"new", "actor":"human", "width":15999, "height":9000, "bit_depth":depth,
            })).unwrap();
            let project = reply.value()["project_id"].as_str().unwrap();
            let tab = crate::workspace::get(root, project).unwrap();
            let (document, layer) = {
                let engine = tab.lock().unwrap();
                let doc = engine.native_document().unwrap().metadata();
                (doc.id().to_owned(), doc.layer(0).unwrap().id().to_owned())
            };
            super::super::dispatch_owned(root, "edit", &json!({
                "actor":"human", "project_id":project, "document_id":document, "expected_revision":0, "max_edge":32,
                "commands":[
                    {"op":"fill", "layer":layer, "rect":[12,3,13,4], "color":[53,67,179,255]},
                    {"op":"fill", "layer":layer, "rect":[15998,8999,15999,9000], "color":[21,89,141,255]}
                ],
            })).unwrap();
            let untouched = {
                let engine = tab.lock().unwrap();
                let native = engine.native_document().unwrap();
                let pixels = native.metadata().layer(0).unwrap().pixels();
                assert_eq!((pixels.width(), pixels.height()), (15999, 9000));
                let tile = native
                    .source(pixels.slot())
                    .unwrap()
                    .physical_tile((0, 0))
                    .unwrap();
                (tile.owner().identity(), tile.reference())
            };
            super::super::dispatch_owned(root, "edit", &json!({
                "actor":"human", "project_id":project, "document_id":document, "expected_revision":1, "max_edge":32,
                "commands":[
                    {"op":"canvas.resize", "width":16000, "height":9000, "anchor":"top_left"},
                    {"op":"selection", "rect":[15998,8999,15999,9000]},
                    {"op":"move", "layer":layer, "dx":1, "dy":0}
                ],
            })).unwrap();
            let engine = tab.lock().unwrap();
            let native = engine.native_document().unwrap();
            let layer = native.metadata().layer(0).unwrap();
            assert_eq!(layer.origin(), [0, 0]);
            let pixels = layer.pixels();
            assert_eq!(
                (pixels.width(), pixels.height(), pixels.depth()),
                (16000, 9000, depth)
            );
            let source = native.source(pixels.slot()).unwrap();
            let tile = source.physical_tile((0, 0)).unwrap();
            assert_eq!((tile.owner().identity(), tile.reference()), untouched);
            let region = crate::raster::backed::Region::new([15998, 8999, 16000, 9000]).unwrap();
            let view = source.prepare_region(region, &mut || Ok(())).unwrap();
            assert_eq!(view.pixel16(15998, 8999).unwrap(), [0; 4]);
            assert_eq!(
                view.pixel16(15999, 8999).unwrap(),
                [21u16, 89, 141, 255].map(|v| v * 257)
            );
            assert_eq!(
                native.metadata().selection(),
                Some([15999, 8999, 16000, 9000])
            );
            assert_eq!(engine.history_lengths(), [2, 0]);
        });
    }
}

#[test]
fn native_document_open_replaces_the_canvas_only_after_source36_qualification() {
    for (depth, extension) in [(8, "psd"), (16, "psb")] {
        with_document(depth, |shared, directory, expected| {
            let saved = directory.join(format!("Saved.{extension}"));
            super::super::save(shared, &saved).unwrap();
            let reopened = directory.join(format!("Reopened.{extension}"));
            fs::copy(&saved, &reopened).unwrap();
            let new = super::super::dispatch_owned(
                shared,
                "document",
                &json!({"action":"new","actor":"human","width":7,"height":5}),
            )
            .unwrap();
            let key = shared.lock().unwrap().native_document().unwrap().key();
            let mut request = json!({
                "action":"open", "actor":"human", "project_id":new.value()["project_id"],
                "document_id":new.value()["document"]["id"], "expected_revision":0, "path":reopened,
            });
            assert!(super::super::dispatch(shared, "document", &request).is_err());
            request["action"] = json!("open_async");
            assert!(super::super::dispatch(shared, "document", &request).is_err());
            assert!(shared.lock().unwrap().loading.is_none());
            request["action"] = json!("open");
            let corrupt = directory.join(format!("Corrupt.{extension}"));
            let mut bytes = fs::read(&saved).unwrap();
            *bytes.last_mut().unwrap() ^= 0xff;
            fs::write(&corrupt, bytes).unwrap();
            request["path"] = json!(corrupt);
            assert!(super::super::dispatch_owned(shared, "document", &request).is_err());
            assert_eq!(shared.lock().unwrap().native_document().unwrap().key(), key);
            request["path"] = json!(reopened);
            let reply = super::super::dispatch_owned(shared, "document", &request).unwrap();
            assert_eq!(reply.value()["project_id"], new.value()["project_id"]);
            assert_eq!(
                reply.value()["document"]["name"],
                format!("Reopened.{extension}")
            );
            assert_eq!(reply.value()["document"]["bit_depth"], depth);
            assert_eq!(reply.value()["dirty"], false);
            assert!(reply.value()["loading"].is_null());
            let engine = shared.lock().unwrap();
            let document = engine.native_document().unwrap();
            assert_ne!(document.key(), key);
            assert_eq!(document.metadata().id(), reply.value()["document"]["id"]);
            assert_eq!(engine.history_lengths(), [0, 0]);
            let layer = document.metadata().layer(0).unwrap();
            let source = document.source(layer.pixels().slot()).unwrap();
            let region = crate::raster::backed::Region::new([0, 0, 4, 2]).unwrap();
            assert_eq!(
                source
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(1, 1)
                    .unwrap(),
                expected,
            );
            let path = crate::file_safety::canonical_destination(&reopened).unwrap();
            assert_eq!(engine.path.as_deref(), Some(path.as_path()));
            assert_eq!(
                engine.file_version,
                Some(crate::file_safety::version(&path).unwrap())
            );
        });
    }
}

#[test]
fn source36_open_protects_a_changed_standard_working_profile_and_retains_saved_samples() {
    let srgb = crate::color_profile::srgb_profile();
    let p3 = moxcms::ColorProfile::new_display_p3().encode().unwrap();
    let resource = |profile: &[u8]| {
        let mut bytes = b"8BIM\x04\x0f\0\0".to_vec();
        bytes.extend_from_slice(&(profile.len() as u32).to_be_bytes());
        bytes.extend_from_slice(profile);
        if profile.len() % 2 != 0 {
            bytes.push(0);
        }
        bytes
    };
    for (depth, extension, tagged) in [(8, "psd", false), (16, "psb", true)] {
        with_document_profile(depth, tagged, |shared, directory, expected| {
            let saved = directory.join(format!("Saved.{extension}"));
            super::super::save(shared, &saved).unwrap();
            let original = fs::read(&saved).unwrap();
            let color_bytes = u32::from_be_bytes(original[26..30].try_into().unwrap()) as usize;
            let start = 30 + color_bytes;
            let resource_bytes = u32::from_be_bytes(original[start..start + 4].try_into().unwrap()) as usize;
            let end = start + 4 + resource_bytes;
            let mut resources = original[start + 4..end].to_vec();
            let working = resource(srgb);
            if tagged {
                let at = resources.windows(working.len()).position(|bytes| bytes == working).unwrap();
                resources.drain(at..at + working.len());
            }
            for (suffix, changed_profile) in [
                ("changed", if tagged { None } else { Some(srgb) }),
                ("p3", Some(p3.as_slice())),
            ] {
                let mut updated = resources.clone();
                if let Some(profile) = changed_profile {
                    updated.extend(resource(profile));
                }
                let mut changed = original[..start].to_vec();
                changed.extend_from_slice(&(updated.len() as u32).to_be_bytes());
                changed.extend(updated);
                changed.extend_from_slice(&original[end..]);
                let path = directory.join(format!("{suffix}.{extension}"));
                fs::write(&path, &changed).unwrap();
                super::super::dispatch_owned(
                    shared, "document", &json!({"action":"open","actor":"human","discard":true,"path":path}),
                ).unwrap();
                let engine = shared.lock().unwrap();
                let view = crate::engine::view::current(&engine);
                assert!(view.read_only());
                assert_eq!(view.depth(), depth);
                assert_eq!(view.profile(), changed_profile);
                let actual = if let Some(native) = view.native() {
                    let slot = native.metadata().layer(0).unwrap().pixels().slot();
                    let region = crate::raster::backed::Region::new([1, 1, 2, 2]).unwrap();
                    native.source(slot).unwrap().prepare_region(region, &mut || Ok(())).unwrap()
                        .pixel16(1, 1).unwrap()
                } else {
                    view.resident().unwrap().layers[0].pixels.get16(1, 1)
                };
                assert_eq!(actual, expected);
                assert!(engine.history_lengths() == [0, 0]);
                assert_eq!(fs::read(&path).unwrap(), changed);
                assert_eq!(fs::read(&saved).unwrap(), original);
            }
        });
    }
}

#[test]
fn conventional_psd_open_in_a_native_tab_keeps_decoder_protection_and_precision() {
    for protected in [false, true] {
        with_document(16, |root, directory, _| {
            let bytes = if protected {
                psd::protected_fixture(psd::Format::Psb, 16, false)
            } else {
                let mut document = Document::new_depth(4, 2, 16).unwrap();
                document.layers[0]
                    .pixels
                    .set16(1, 1, [12347, 23459, 34571, 65535]);
                psd::encode_psb(&document).unwrap()
            };
            let expected = psd::decode(&bytes).unwrap();
            assert_eq!(expected.read_only, protected);
            let path = directory.join("Conventional.psb");
            fs::write(&path, &bytes).unwrap();
            let (project, document, revision, key) = {
                let engine = root.lock().unwrap();
                let native = engine.native_document().unwrap();
                (
                    engine.project_id.clone(),
                    native.metadata().id().to_owned(),
                    native.metadata().revision(),
                    native.key(),
                )
            };
            let mut request = json!({
                "action":"open","actor":"human","project_id":project,"document_id":document,
                "expected_revision":revision,"path":path,
            });
            assert!(super::super::dispatch_owned(root, "document", &request).is_err());
            request["discard"] = json!(true);
            assert!(super::super::dispatch(root, "document", &request).is_err());
            let malformed = directory.join("Truncated.psb");
            fs::write(&malformed, &bytes[..bytes.len() - 1]).unwrap();
            request["path"] = json!(malformed);
            assert!(super::super::dispatch_owned(root, "document", &request).is_err());
            assert_eq!(root.lock().unwrap().native_document().unwrap().key(), key);
            request["path"] = json!(path);
            let result = super::super::dispatch_owned(root, "document", &request).unwrap();
            assert_eq!(result.value()["project_id"], project);
            assert_eq!(result.value()["document"]["read_only"], protected);
            assert_eq!(result.value()["document"]["bit_depth"], 16);
            assert_eq!(result.value()["dirty"], false);
            assert!(result.value()["loading"].is_null());
            {
                let engine = root.lock().unwrap();
                let opened = &engine.resident_state().unwrap().document;
                assert_ne!(opened.id, document);
                assert_eq!(opened.read_only, protected);
                assert_eq!(
                    opened.layers[0].pixels.rgba16(),
                    expected.layers[0].pixels.rgba16()
                );
                assert_eq!(engine.history_lengths(), [0, 0]);
            }
            if protected {
                assert!(super::super::save(root, &directory.join("Refused.psb")).is_err());
            }
            assert_eq!(crate::workspace::handles(root).len(), 1);
        });
    }
}

#[test]
fn conventional_backing_requires_an_exact_native_saved_composite() {
    fn standard(document: &Document, psb: bool) -> (Vec<u8>, usize) {
        let bytes = if psb { psd::encode_psb(document) } else { psd::encode(document) }.unwrap();
        let resources = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
        let layers = resources
            + 4
            + u32::from_be_bytes(bytes[resources..resources + 4].try_into().unwrap()) as usize;
        let (length_width, layer_bytes) = if psb {
            (8, u64::from_be_bytes(bytes[layers..layers + 8].try_into().unwrap()) as usize)
        } else {
            (4, u32::from_be_bytes(bytes[layers..layers + 4].try_into().unwrap()) as usize)
        };
        let mut standard = bytes[..resources].to_vec();
        standard.extend([0; 4]);
        let composite = standard.len() + length_width + layer_bytes;
        standard.extend_from_slice(&bytes[layers..]);
        (standard, composite)
    }
    for (depth, exact, independent, off_frame) in [
        (8, true, false, None),
        (16, true, false, None),
        (16, false, false, None),
        (16, true, true, None),
        (16, false, true, None),
        (8, true, false, Some(true)),
        (16, true, true, Some(false)),
        (16, false, true, Some(true)),
    ] {
        with_document(depth, |root, directory, _| {
            let mut document = Document::new_depth(4, 2, depth).unwrap();
            for y in 0..2 {
                for x in 0..4 {
                    document.layers[0]
                        .pixels
                        .set16(x, y, [12347, 23459, 34571, 65535]);
                }
            }
            let expected = document.layers[0].pixels.get16(1, 1);
            document.layers[0]
                .pixels
                .set16(0, 0, [12347, 23459, 34571, 0]);
            let hidden = document.layers[0].pixels.get16(0, 0);
            let mut mask = crate::raster::Raster::new_depth(4, 2, depth);
            mask.set16(1, 1, [12345, 12345, 12345, 65535]);
            let expected_mask = mask.get16(1, 1);
            let expected_original = off_frame.map(|_| {
                let mut full = crate::raster::Raster::new_depth(6, 4, depth);
                full.set16(0, 0, [34567, 34567, 34567, 65535]);
                full.set16(2, 2, expected_mask);
                let expected = full.get16(0, 0);
                let mut original = crate::retained::Original::from(&full);
                original.matrix[4] = -1.;
                original.matrix[5] = -1.;
                mask.retained = Some(Box::new(original));
                expected
            });
            document.layers[0].mask = Some(crate::engine::Mask {
                enabled: off_frame.is_some(),
                linked: off_frame.unwrap_or(true),
                cache_key: crate::engine::id(),
                steps: vec![crate::engine::MaskStep {
                    id: crate::engine::id(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 1.,
                    value: 0.,
                    pixels: mask,
                    settings: Value::Null,
                }],
            });
            let psb = depth == 16 && off_frame.is_some();
            let (mut bytes, composite) = standard(&document, psb);
            if !exact {
                document.layers[0]
                    .pixels
                    .set16(1, 1, [43211, 12553, 51227, 65535]);
                let (other, other_composite) = standard(&document, psb);
                bytes.truncate(composite);
                bytes.extend_from_slice(&other[other_composite..]);
            }
            let path = directory.join(if psb { "Standard.psb" } else { "Standard.psd" });
            fs::write(&path, bytes).unwrap();
            let sample = |surface: &crate::raster::Surface, x, y| {
                let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
                surface
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(x, y)
                    .unwrap()
            };
            if exact {
                let runtime = crate::workspace::native_dispatch::runtime_for(root).unwrap();
                let session = runtime.begin().unwrap();
                let fragment = session.fresh_fragment().unwrap();
                let mut file = crate::file_safety::open_native_read(&path).unwrap();
                let version = crate::file_safety::native_source_version(
                    &path,
                    &mut file,
                    &mut [0; 65536],
                    &|| session.check(),
                )
                .unwrap();
                let decoded = psd::decode_backed(
                    &mut file,
                    &crate::loading::Control::default(),
                    version,
                    fragment.store().clone(),
                    &|| session.check(),
                )
                .unwrap();
                let layer = &decoded.document.layers[0];
                assert_eq!(layer.pixels.bytes(), 0);
                assert_eq!(layer.mask.as_ref().unwrap().steps[1].pixels.bytes(), 0);
                assert_eq!(sample(&decoded.color[&layer.id], 0, 0), hidden);
                assert_eq!(sample(&decoded.color[&layer.id], 1, 1), expected);
                assert_eq!(sample(&decoded.masks[&layer.id], 1, 1), expected_mask);
                if let Some(expected) = expected_original {
                    let paint = &layer.mask.as_ref().unwrap().steps[1].pixels;
                    assert_eq!(paint.retained.as_ref().unwrap().pixels.bytes(), 0);
                    assert_eq!(sample(&decoded.mask_originals[&layer.id], 0, 0), expected);
                }
                drop(decoded);
                drop(file);
                session
                    .abort_fragment(fragment, &mut || session.cleanup_check())
                    .unwrap();
            }
            let result = super::super::dispatch_owned(
                root,
                if independent { "projects" } else { "document" },
                &json!({"action":"open","actor":"human","discard":true,"path":path}),
            )
            .unwrap();
            assert_eq!(result.value()["document"]["bit_depth"], depth);
            assert_eq!(result.value()["dirty"], false);
            let opened = if independent {
                crate::workspace::get(root, result.value()["project_id"].as_str().unwrap()).unwrap()
            } else {
                root.clone()
            };
            let engine = opened.lock().unwrap();
            assert_eq!(engine.native_document().is_some(), exact);
            let actual = if let Some(document) = engine.native_document() {
                let layer = document.metadata().layer(0).unwrap();
                if let Some(expected) = expected_original {
                    let mask = layer.mask().unwrap();
                    assert_eq!(mask.linked(), off_frame.unwrap());
                    let original = mask.steps().nth(1).unwrap().pixels().retained().unwrap();
                    assert_eq!(original.matrix(), [1., 0., 0., 1., -1., -1.]);
                    assert_eq!(
                        sample(document.source(original.pixels().slot()).unwrap(), 0, 0),
                        expected,
                    );
                }
                sample(document.source(layer.pixels().slot()).unwrap(), 1, 1)
            } else {
                let document = &engine.resident_state().unwrap().document;
                if off_frame.is_some() {
                    assert!(document.read_only);
                    return;
                }
                document.layers[0].pixels.get16(1, 1)
            };
            assert_eq!(actual, expected);
        });
    }
}

#[test]
fn conventional_pattern_fill_opens_natively_beyond_the_resident_file_limit() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    with_document(16, |root, directory, _| {
        let mut document = Document::new_depth(3, 2, 16).unwrap();
        let tile = crate::raster::png16(2, 1, &[12347, 23459, 34571, 65535, 19277, 30671, 40991, 65535]).unwrap();
        document.layers[0].kind = "fill".into();
        document.layers[0].color = [255; 4];
        document.layers[0].effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "pattern_overlay".into(),
            enabled: true,
            weight: 1.,
            settings: json!({"pattern":"bitmap","bitmap_png":STANDARD.encode(tile),
                "sampling":"nearest","opacity":1.,"use_pattern_alpha":true,"phase_x":1.}).into(),
        });
        let bytes = psd::encode_psb(&document).unwrap();
        let resources = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
        let layers = resources + 4 + u32::from_be_bytes(bytes[resources..resources + 4].try_into().unwrap()) as usize;
        let path = directory.join("Pattern.psb");
        let skipped = crate::file_safety::RESIDENT_FILE_BYTES as u32;
        {
            use std::io::{Seek, SeekFrom, Write};
            // Replace private resources with an ignored resource so file size
            // exceeds the old limit without a large heap allocation.
            let mut file = fs::File::create(&path).unwrap();
            file.write_all(&bytes[..resources]).unwrap();
            file.write_all(&(12 + skipped).to_be_bytes()).unwrap();
            file.write_all(b"8BIM\x10\x04\0\0").unwrap();
            file.write_all(&skipped.to_be_bytes()).unwrap();
            file.seek(SeekFrom::Current(i64::from(skipped))).unwrap();
            file.write_all(&bytes[layers..]).unwrap();
        }
        super::super::dispatch_owned(
            root, "document", &json!({"action":"open","actor":"human","discard":true,"path":path}),
        ).unwrap();
        let engine = root.lock().unwrap();
        let native = engine.native_document().unwrap();
        let metadata = native.metadata();
        assert_eq!((metadata.width(), metadata.height(), metadata.depth()), (3, 2, 16));
        assert!(!metadata.read_only());
        let layer = crate::engine::view::LayerView::Native(metadata.layer(0).unwrap());
        assert_eq!(layer.kind(), "fill");
        let pattern = layer.effect(0).unwrap();
        assert_eq!(pattern.kind(), "pattern_overlay");
        assert_eq!(pattern.settings()["phase_x"].as_f64(), Some(1.));
        assert_eq!(engine.history_lengths(), [0, 0]);
    });
}

#[test]
fn wide_native_open_uses_the_alternate_gradient_grid_only_for_an_exact_match() {
    for (depth, exact, compression) in [
        (8, true, 0), (16, true, 0), (16, false, 0),
        (8, true, 1), (16, true, 2), (8, true, 3), (16, true, 3), (16, false, 3),
    ] {
        with_document(depth, |root, directory, _| {
            let bytes = psd::native_gradient_grid_fixture(depth, exact, compression);
            let path = directory.join(if depth == 16 { "Gradient.psb" } else { "Gradient.psd" });
            fs::write(&path, bytes).unwrap();
            super::super::dispatch_owned(
                root, "document", &json!({"action":"open","actor":"human","discard":true,"path":path}),
            ).unwrap();
            let engine = root.lock().unwrap();
            let native = engine.native_document().unwrap();
            let metadata = native.metadata();
            assert_eq!((metadata.width(), metadata.height(), metadata.depth()), (8198, 1, depth));
            assert_eq!(metadata.read_only(), !exact);
            let layer = metadata.layer(0).unwrap();
            if exact {
                assert!(layer.mask().is_some());
                let effect = layer.effects().next().unwrap();
                assert_eq!(effect.kind(), "gradient_overlay");
                assert_eq!(effect.settings()["pixel_grid"], true);
            } else {
                assert_eq!(layer.name(), "Saved PSD composite");
                assert!(layer.effects().next().is_none());
            }
            assert_eq!(engine.history_lengths(), [0, 0]);
        });
    }
}

#[test]
fn wide_native_open_requires_binary_stroke_alpha_and_an_exact_composite() {
    for depth in [8, 16] {
        for (exact, partial) in [(true, false), (false, false), (true, true)] {
            with_document(depth, |root, directory, _| {
                let bytes = psd::native_stroke_fixture(depth, exact, partial);
                let path = directory.join(if depth == 16 { "Stroke.psb" } else { "Stroke.psd" });
                fs::write(&path, &bytes).unwrap();
                super::super::dispatch_owned(root, "document", &json!({
                    "action":"open", "actor":"human", "discard":true, "path":path
                })).unwrap();
                let engine = root.lock().unwrap();
                let native = engine.native_document().unwrap();
                let metadata = native.metadata();
                assert_eq!((metadata.width(), metadata.height(), metadata.depth()), (8324, 1, depth));
                assert_eq!(metadata.read_only(), !exact || partial, "{:?}", metadata.warnings());
                let layer = metadata.layer(0).unwrap();
                if exact && !partial {
                    let effect = layer.effects().next().unwrap();
                    assert_eq!(effect.kind(), "stroke");
                    assert_eq!(effect.settings()["size"], 128.);
                    let pixels = native.source(layer.pixels().slot()).unwrap();
                    let view = pixels.prepare_region(crate::raster::backed::Region::new([0, 0, 3, 1]).unwrap(), &mut || Ok(())).unwrap();
                    let source = [12347, 23459, 34571, 65535].map(|word| {
                        if depth == 16 { word } else { u16::from(crate::raster::project16(word)) * 257 }
                    });
                    assert_eq!(view.pixel16(0, 0).unwrap(), [0; 4]);
                    assert_eq!(view.pixel16(1, 0).unwrap(), source);
                } else {
                    assert_eq!(layer.name(), "Saved PSD composite");
                    assert!(layer.effects().next().is_none());
                }
                assert_eq!(engine.history_lengths(), [0, 0]);
                drop(engine);
                if exact && !partial {
                    let saved = directory.join(if depth == 16 { "Stroke Copy.psb" } else { "Stroke Copy.psd" });
                    super::super::save(root, &saved).unwrap();
                    super::super::dispatch_owned(root, "document", &json!({
                        "action":"open", "actor":"human", "discard":true, "path":saved
                    })).unwrap();
                    let engine = root.lock().unwrap();
                    let metadata = engine.native_document().unwrap().metadata();
                    assert!(!metadata.read_only(), "{:?}", metadata.warnings());
                    assert_eq!(metadata.depth(), depth);
                    let layer = metadata.layer(0).unwrap();
                    let effect = layer.effects().next().unwrap();
                    assert_eq!(effect.kind(), "stroke");
                    assert_eq!(effect.settings()["size"], 128.);
                    assert_eq!(engine.history_lengths(), [0, 0]);
                }
                assert_eq!(fs::read(&path).unwrap(), bytes);
            });
        }
    }
}

#[test]
fn conventional_wide_photoshop_open_requires_exact_native_composite_and_keeps_mask_words() {
    let width = 16000u32;
    for (depth, version, exact, off_frame) in [
        (8u16, 1u16, true, false),
        (16, 2, true, false),
        (16, 2, false, false),
        (8, 1, true, true),
        (16, 2, true, true),
    ] {
        with_document(depth, |root, directory, _| {
            let length = |out: &mut Vec<u8>, bytes: usize| {
                if version == 2 {
                    out.extend_from_slice(&(bytes as u64).to_be_bytes());
                } else {
                    out.extend_from_slice(&(bytes as u32).to_be_bytes());
                }
            };
            let append_word = |out: &mut Vec<u8>, value: u16| {
                if depth == 16 {
                    out.extend_from_slice(&value.to_be_bytes());
                } else {
                    out.push(crate::raster::project16(value));
                }
            };
            let native_word = |value: u16| {
                if depth == 16 { value } else { u16::from(crate::raster::project16(value)) * 257 }
            };
            let hidden = [12347u16, 23459, 34571, 0].map(native_word);
            let mask_word = native_word(19017);
            let off_frame_word = native_word(32127);
            let row_bytes = width as usize * usize::from(depth / 8);
            let mut info = (-1i16).to_be_bytes().to_vec();
            for value in [0, 0, 1, width] {
                info.extend_from_slice(&value.to_be_bytes());
            }
            info.extend_from_slice(&5u16.to_be_bytes());
            for id in [0i16, 1, 2, -1, -2] {
                info.extend_from_slice(&id.to_be_bytes());
                length(&mut info, row_bytes * (1 + usize::from(id == -2 && off_frame)) + 2);
            }
            info.extend_from_slice(b"8BIMnorm\xff\0\0\0");
            let mut extra = 20u32.to_be_bytes().to_vec();
            for value in [if off_frame { -1 } else { 0 }, 0, 1, width as i32] {
                extra.extend_from_slice(&value.to_be_bytes());
            }
            extra.extend_from_slice(&[255, u8::from(off_frame), 0, 0]);
            extra.extend_from_slice(&[0; 8]); // Default blending ranges and empty Pascal name.
            info.extend_from_slice(&(extra.len() as u32).to_be_bytes());
            info.extend_from_slice(&extra);
            for (index, (first, middle, last)) in [
                (hidden[0], 0, 65535), (hidden[1], 0, 65535), (hidden[2], 0, 65535),
                (0, 0, 65535), (65535, 65535, mask_word),
            ].into_iter().enumerate() {
                info.extend_from_slice(&[0, 0]); // Raw channel.
                if index == 4 && off_frame {
                    append_word(&mut info, off_frame_word);
                    info.resize(info.len() + row_bytes - usize::from(depth / 8), 255);
                }
                append_word(&mut info, first);
                for _ in 1..width - 1 {
                    append_word(&mut info, middle);
                }
                append_word(&mut info, last);
            }
            let mut section = Vec::new();
            length(&mut section, info.len());
            section.extend_from_slice(&info);
            section.extend_from_slice(&[0; 4]); // No global mask.
            let mut bytes = b"8BPS".to_vec();
            bytes.extend_from_slice(&version.to_be_bytes());
            bytes.extend_from_slice(&[0; 6]);
            bytes.extend_from_slice(&4u16.to_be_bytes());
            bytes.extend_from_slice(&1u32.to_be_bytes());
            bytes.extend_from_slice(&width.to_be_bytes());
            bytes.extend_from_slice(&depth.to_be_bytes());
            bytes.extend_from_slice(&3u16.to_be_bytes());
            let resolution = [1, 44, 128, 0, 0, 1, 0, 1, 1, 145, 64, 0, 0, 2, 0, 2];
            bytes.extend_from_slice(&0u32.to_be_bytes());
            bytes.extend_from_slice(&180u32.to_be_bytes());
            bytes.extend_from_slice(b"8BIM\x03\xed\0\0\0\0\0\x10");
            bytes.extend_from_slice(&resolution);
            bytes.extend_from_slice(b"8BIM\x04\x26\0\0\0\0\0\x0e");
            bytes.extend_from_slice(&psd::TEST_PRINT_SCALE);
            bytes.extend_from_slice(b"8BIM\x04\x28\0\0\0\0\0\x0c");
            bytes.extend_from_slice(&psd::TEST_PIXEL_ASPECT);
            bytes.extend_from_slice(b"8BIM\x04\x08\0\0\0\0\0\x10");
            bytes.extend_from_slice(&psd::TEST_GRID);
            bytes.extend_from_slice(b"8BIM\x04\x14\0\0\0\0\0\x04");
            bytes.extend_from_slice(&psd::TEST_LAYER_ID_SEED);
            bytes.extend_from_slice(b"8BIM\x03\xf3\0\0\0\0\0\x09");
            bytes.extend_from_slice(&psd::TEST_PRINT_FLAGS);
            bytes.push(0);
            bytes.extend_from_slice(b"8BIM\x27\x10\0\0\0\0\0\x0a");
            bytes.extend_from_slice(&psd::TEST_PRINT_FLAGS_INFO);
            bytes.extend_from_slice(b"8BIM\x04\x11\0\0\0\0\0\x01\x01\0");
            length(&mut bytes, section.len());
            bytes.extend_from_slice(&section);
            bytes.extend_from_slice(&[0, 0]); // Raw merged channels, matte RGB and alpha.
            bytes.resize(bytes.len() + row_bytes * 3, 255);
            bytes.resize(bytes.len() + row_bytes - usize::from(depth / 8), 0);
            append_word(&mut bytes, if exact { mask_word } else { mask_word + 1 });

            let path = directory.join(if version == 2 { "Wide.psb" } else { "Wide.psd" });
            fs::write(&path, bytes).unwrap();
            let opened = super::super::dispatch_owned(
                root, "document", &json!({"action":"open","actor":"human","discard":true,"path":path}),
            );
            let engine = root.lock().unwrap();
            let native = engine.native_document().unwrap();
            let meta = native.metadata();
            assert!(opened.is_ok());
            assert_eq!((meta.width(), meta.height(), meta.depth()), (width, 1, depth));
            assert_eq!(meta.psd_resources(), psd::DocumentResources {
                resolution: Some(resolution), scale: Some(psd::TEST_PRINT_SCALE),
                pixel_aspect: Some(psd::TEST_PIXEL_ASPECT),
                grid: Some(psd::TEST_GRID),
                layer_id_seed: Some(psd::TEST_LAYER_ID_SEED),
                print_flags: Some(psd::TEST_PRINT_FLAGS),
                print_flags_info: Some(psd::TEST_PRINT_FLAGS_INFO),
                copyright: None,
                global_angle: None,
                global_altitude: None,
                intentionally_untagged: true,
            });
            let layer = meta.layer(0).unwrap();
            let sample = |slot, x| {
                let region = crate::raster::backed::Region::new([x, 0, x + 1, 1]).unwrap();
                native.source(slot).unwrap().prepare_region(region, &mut || Ok(()))
                    .unwrap().pixel16(x, 0).unwrap()
            };
            if !exact {
                assert!(meta.read_only());
                let protected_id = meta.id().to_owned();
                let protected_surface = native.source(layer.pixels().slot()).unwrap().key();
                assert_eq!(layer.name(), "Saved PSD composite");
                assert_eq!(
                    sample(layer.pixels().slot(), 15999),
                    [65535, 65535, 65535, mask_word + 1],
                );
                assert!(layer.mask().is_none());
                assert_eq!(engine.history_lengths(), [0, 0]);
                let scene = Arc::new(engine.capture_scene(None).unwrap().into_native().unwrap());
                drop(engine);
                {
                    use crate::render::native_preview::Request;
                    let read = crate::workspace::native_dispatch::begin_read_scene(root, &scene).unwrap();
                    let rect = Some([15999, 0, 16000, 1]);
                    let frame = Request::capture(scene.clone(), read.clone(), rect, 32, None, false)
                        .unwrap().render(&mut || Ok(())).unwrap();
                    assert_eq!(
                        &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                        &[255, 255, 255, crate::raster::project16(mask_word + 1)],
                    );
                    let wide = Request::capture(scene.clone(), read.clone(), None, 16000, None, false)
                        .unwrap().render(&mut || Ok(())).unwrap();
                    assert_eq!((wide.key().width, wide.key().height), (16000, 1));
                    assert!(wide.tag().matches_edge(16000));
                    assert!(!wide.tag().matches_edge(8192));
                    let bytes = wide.copy_rgba8(&mut || Ok(())).unwrap();
                    assert_eq!(&bytes[15999 * 4..], &[255, 255, 255, crate::raster::project16(mask_word + 1)]);
                    assert!(Request::capture(
                        scene.clone(), read.clone(), rect, 32, Some("original-layer"), false,
                    ).is_err());
                    assert!(Request::capture(scene, read, rect, 32, None, true).is_err());
                }
                let destination = directory.join("Protected copy.psb");
                assert!(super::super::save(root, &destination).is_err());
                assert!(!destination.exists());
                let original = fs::read(&path).unwrap();
                assert!(super::super::compatible_copy_guarded(
                    root, "human", Some(0), false, Some("stale document"),
                ).is_err());
                super::super::compatible_copy_guarded(
                    root, "human", Some(0), false, Some(&protected_id),
                ).unwrap();
                let engine = root.lock().unwrap();
                let native = engine.native_document().unwrap();
                let meta = native.metadata();
                assert!(!meta.read_only());
                assert_ne!(meta.id(), protected_id);
                assert_eq!((meta.width(), meta.height(), meta.depth()), (width, 1, depth));
                assert_eq!(meta.psd_resources(), psd::DocumentResources {
                    resolution: Some(resolution), scale: Some(psd::TEST_PRINT_SCALE),
                    pixel_aspect: Some(psd::TEST_PIXEL_ASPECT),
                    grid: Some(psd::TEST_GRID),
                    layer_id_seed: Some(psd::TEST_LAYER_ID_SEED),
                    print_flags: Some(psd::TEST_PRINT_FLAGS),
                    print_flags_info: Some(psd::TEST_PRINT_FLAGS_INFO),
                    copyright: None,
                    global_angle: None,
                    global_altitude: None,
                    intentionally_untagged: true,
                });
                let copy = native.source(meta.layer(0).unwrap().pixels().slot()).unwrap();
                assert_eq!(copy.key(), protected_surface);
                assert!(engine.path.is_none() && engine.file_ownership.is_none());
                assert_ne!(engine.saved_revision, meta.revision());
                assert_eq!(engine.history_lengths(), [0, 0]);
                drop(engine);
                assert_eq!(fs::read(&path).unwrap(), original);
                return;
            }
            assert!(!meta.read_only());
            assert_eq!(sample(layer.pixels().slot(), 0), hidden);
            let mask = layer.mask().unwrap();
            let paint = mask.steps().nth(1).unwrap();
            assert_eq!(sample(paint.pixels().slot(), 15999), [mask_word, mask_word, mask_word, 65535]);
            assert_eq!(engine.history_lengths(), [0, 0]);
            if off_frame {
                let original = paint.pixels().retained().unwrap();
                assert_eq!((original.pixels().width(), original.pixels().height()), (width, 2));
                assert_eq!(original.matrix(), [1., 0., 0., 1., 0., -1.]);
                let expected = [off_frame_word, off_frame_word, off_frame_word, 65535];
                assert_eq!(sample(original.pixels().slot(), 0), expected);
                let id = meta.id().to_owned();
                let revision = meta.revision();
                let layer_id = layer.id().to_owned();
                assert!(!mask.linked());
                drop(engine);
                super::super::edit_shared(
                    root, "human", &[json!({"op":"paint.fill","layer":layer_id,"mask":true,"rect":[0,0,1,1],"color":[81,81,81,255]})],
                    Some(revision), None, "Paint mask", None, Some(&id),
                ).unwrap();
                let engine = root.lock().unwrap();
                let revision = engine.native_document().unwrap().metadata().revision();
                assert_eq!(engine.history_lengths(), [1, 0]);
                let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
                let plan = psd::scene_save36::ScenePlan::for_report(&scene, &|| Ok(())).unwrap();
                assert_eq!(plan.standard().records()[0].mask.unwrap().bounds, [0, -1, width as i32, 1]);
                drop(plan);
                drop(scene);
                drop(engine);
                let saved = directory.join(if version == 2 { "Retained.psb" } else { "Retained.psd" });
                super::super::save_accepted_source(root, &saved, &id, revision, false).unwrap();
                super::super::dispatch_owned(
                    root, "document", &json!({"action":"open","actor":"human","discard":true,"path":saved}),
                ).unwrap();
                let engine = root.lock().unwrap();
                let native = engine.native_document().unwrap();
                assert!(!native.metadata().read_only());
                let paint = native.metadata().layer(0).unwrap().mask().unwrap().steps().nth(1).unwrap();
                let original = paint.pixels().retained().unwrap();
                let region = crate::raster::backed::Region::new([0, 0, 1, 1]).unwrap();
                let retained = native.source(original.pixels().slot()).unwrap();
                assert_eq!(retained.prepare_region(region, &mut || Ok(())).unwrap().pixel16(0, 0).unwrap(), expected);
                let region = crate::raster::backed::Region::new([0, 1, 1, 2]).unwrap();
                assert_eq!(retained.prepare_region(region, &mut || Ok(())).unwrap().pixel16(0, 1).unwrap(), [81 * 257, 81 * 257, 81 * 257, 65535]);
                let id = native.metadata().id().to_owned();
                let revision = native.metadata().revision();
                drop(engine);
                super::super::edit_shared(
                    root, "human", &[json!({"op":"move","layer":layer_id,"dy":-1,"selection_only":false})],
                    Some(revision), None, "Reveal retained mask", None, Some(&id),
                ).unwrap();
                let engine = root.lock().unwrap();
                let native = engine.native_document().unwrap();
                let moved = native.metadata().layer(0).unwrap();
                assert_eq!(moved.origin(), [0, -1]);
                let paint = moved.mask().unwrap().steps().nth(1).unwrap().pixels();
                let region = crate::raster::backed::Region::new([0, 0, 1, 1]).unwrap();
                let pixels = native.source(paint.slot()).unwrap();
                assert_eq!(pixels.prepare_region(region, &mut || Ok(())).unwrap().pixel16(0, 0).unwrap(), expected);
            }
        });
    }
}

#[test]
fn native_protected_copy_requires_profile_consent_and_converts_native_words() {
    with_document(16, |root, _, sample| {
        use crate::{color_profile::prepared::DisplayStatus, render::native_preview::Request};
        let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        let mut expected = sample;
        crate::color_profile::convert16(&profile, &mut expected).unwrap();
        let (protected, source) = {
            let engine = root.lock().unwrap();
            let base = engine.native_document().unwrap();
            let slot = base.metadata().layer(0).unwrap().pixels().slot();
            let source = base.source(slot).unwrap().clone();
            let protected = NativeDocument::new_protected_preview(
                source.clone(), "Profiled.psb", "Protected Photoshop fixture",
                Some(profile), false, Default::default(), None, base.budget(), base.disk_budget(), &|| Ok(()),
            ).unwrap();
            (protected, source)
        };
        let id = protected.metadata().id().to_owned();
        root.lock().unwrap().authority = DocumentAuthority::from_native(protected, 8).unwrap();
        root.lock().unwrap().saved_revision = 0;
        {
            let scene = Arc::new(root.lock().unwrap().capture_scene(None).unwrap().into_native().unwrap());
            let read = crate::workspace::native_dispatch::begin_read_scene(root, &scene).unwrap();
            let frame = Request::capture(scene, read, Some([1, 1, 2, 2]), 32, None, false)
                .unwrap().render(&mut || Ok(())).unwrap();
            assert_eq!(frame.display_status(), DisplayStatus::Converted);
            assert_eq!(
                &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                &expected.map(crate::raster::project16),
            );
        }
        assert!(super::super::compatible_copy_guarded(
            root, "human", Some(0), false, Some(&id),
        ).unwrap_err().contains("convert_to_srgb=true"));
        assert_eq!(root.lock().unwrap().native_document().unwrap().metadata().id(), id);
        super::super::compatible_copy_guarded(root, "human", Some(0), true, Some(&id)).unwrap();
        let engine = root.lock().unwrap();
        let copy = engine.native_document().unwrap();
        let meta = copy.metadata();
        assert!(!meta.read_only() && meta.srgb_tagged() && meta.profile().is_none());
        assert_eq!(meta.depth(), 16);
        let slot = meta.layer(0).unwrap().pixels().slot();
        let output = copy.source(slot).unwrap();
        let region = crate::raster::backed::Region::new([1, 1, 2, 2]).unwrap();
        assert_eq!(output.prepare_region(region, &mut || Ok(())).unwrap().pixel16(1, 1).unwrap(), expected);
        assert_eq!(source.prepare_region(region, &mut || Ok(())).unwrap().pixel16(1, 1).unwrap(), sample);
        assert_ne!(output.key(), source.key());
    });
}

#[test]
fn native_project_open_uses_owned_tabs_from_resident_and_file_dialog_entry_points() {
    with_document(16, |root, directory, expected| {
        let saved = directory.join("Saved.psb");
        super::super::save(root, &saved).unwrap();
        let input = directory.join("Independent.psb");
        fs::copy(&saved, &input).unwrap();
        let workspace = crate::workspace::attach(root);
        let initial = crate::workspace::active_id_in(&workspace);
        let resident =
            crate::workspace::register_in(&workspace, Engine::new(), Some(&initial)).unwrap();
        let initiating = resident.lock().unwrap().project_id.clone();
        let request = json!({"action":"open","actor":"human","path":input});
        let before = crate::workspace::entries_in(&workspace).len();
        assert!(super::super::dispatch(root, "projects", &request).is_err());
        assert_eq!(crate::workspace::entries_in(&workspace).len(), before);
        assert_eq!(crate::workspace::active_id_in(&workspace), initiating);
        let reply = super::super::dispatch_owned(root, "projects", &request).unwrap();
        let opened = reply.value()["project_id"].as_str().unwrap();
        assert_eq!(crate::workspace::active_id_in(&workspace), opened);
        assert_eq!(reply.value()["document"]["name"], "Independent.psb");
        assert_eq!(reply.value()["document"]["bit_depth"], 16);
        let opened = crate::workspace::get_in(&workspace, opened).unwrap();
        {
            let engine = opened.lock().unwrap();
            let document = engine.native_document().unwrap();
            let slot = document.metadata().layer(0).unwrap().pixels().slot();
            let region = crate::raster::backed::Region::new([0, 0, 4, 2]).unwrap();
            assert_eq!(
                document
                    .source(slot)
                    .unwrap()
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(1, 1)
                    .unwrap(),
                expected,
            );
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert_eq!(engine.saved_revision, document.metadata().revision());
        }
        let duplicate = super::super::dispatch_owned(root, "projects", &request)
            .err()
            .unwrap();
        assert!(duplicate.contains("already open"));
        assert_eq!(crate::workspace::entries_in(&workspace).len(), before + 1);
        assert!(resident.lock().unwrap().native_document().is_none());

        let blank = crate::workspace::new_project(&opened, 5, 3, 16, None).unwrap();
        let transparent = directory.join("Transparent.psd");
        super::super::save(&blank, &transparent).unwrap();
        let blank_copy = directory.join("Transparent copy.psd");
        fs::copy(&transparent, &blank_copy).unwrap();
        let active = crate::workspace::active_id_in(&workspace);
        let opened_blank = super::super::open_project_in(
            &workspace,
            &blank_copy,
            &crate::loading::Control::default(),
            Some(&initiating),
        )
        .unwrap();
        assert_eq!(crate::workspace::active_id_in(&workspace), active);
        let engine = opened_blank.lock().unwrap();
        let native = engine.native_document().unwrap();
        assert_eq!(native.metadata().depth(), 16);
        let slot = native.metadata().layer(0).unwrap().pixels().slot();
        let region = crate::raster::backed::Region::new([0, 0, 5, 3]).unwrap();
        assert_eq!(
            native
                .source(slot)
                .unwrap()
                .prepare_region(region, &mut || Ok(()))
                .unwrap()
                .pixel16(2, 2)
                .unwrap(),
            [0; 4],
        );
    });
}

#[test]
fn native_png_open_retains_hidden_rgb_and_has_no_working_save_destination() {
    with_document(16, |root, directory, _| {
        let words = [12347, 23459, 34571, 0, 5789, 43211, 65433, 65535];
        let path = directory.join("Source16.png");
        fs::write(&path, raster::png16(2, 1, &words).unwrap()).unwrap();
        let workspace = crate::workspace::attach(root);
        let initial = crate::workspace::active_id_in(&workspace);
        let control = crate::loading::Control::default();
        assert!(matches!(
            super::super::begin_image_project_in(&workspace, &path, &control, &initial).unwrap(),
            super::super::ImageOpen::Opened
        ));
        let opened = crate::workspace::active(root).unwrap();
        let (project, document) = {
            let engine = opened.lock().unwrap();
            let doc = engine.native_document().unwrap();
            assert_eq!(doc.metadata().depth(), 16);
            assert_eq!(doc.metadata().name(), "Source16.png");
            let layer = doc.metadata().layer(0).unwrap();
            assert_eq!(layer.name(), "Source16");
            let region = crate::raster::backed::Region::new([0, 0, 2, 1]).unwrap();
            let view = doc
                .source(layer.pixels().slot())
                .unwrap()
                .prepare_region(region, &mut || Ok(()))
                .unwrap();
            assert_eq!(view.pixel16(0, 0).unwrap(), words[..4]);
            assert_eq!(view.pixel16(1, 0).unwrap(), words[4..]);
            assert_eq!(engine.saved_revision, u64::MAX);
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert!(engine.path.is_none() && engine.file_version.is_none());
            assert!(engine.file_ownership.is_some());
            (engine.project_id.clone(), doc.metadata().id().to_owned())
        };
        let second = directory.join("Replacement.png");
        fs::copy(&path, &second).unwrap();
        let mut request = json!({
            "action":"open","actor":"human","project_id":project,"document_id":document,
            "expected_revision":0,"path":second,
        });
        assert!(super::super::dispatch_owned(root, "document", &request).is_err());
        request["discard"] = json!(true);
        assert!(super::super::dispatch(root, "document", &request).is_err());
        let invalid = directory.join("Invalid.png");
        let mut bytes = fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 0xff;
        fs::write(&invalid, bytes).unwrap();
        request["path"] = json!(invalid);
        assert!(super::super::dispatch_owned(root, "document", &request).is_err());
        assert_eq!(
            opened
                .lock()
                .unwrap()
                .native_document()
                .unwrap()
                .metadata()
                .id(),
            document
        );
        request["path"] = json!(second);
        let result = super::super::dispatch_owned(root, "document", &request).unwrap();
        assert_eq!(result.value()["project_id"], project);
        assert_eq!(result.value()["document"]["bit_depth"], 16);
        assert_eq!(result.value()["dirty"], true);
        assert_eq!(crate::workspace::entries_in(&workspace).len(), 2);
        assert!(crate::file_ownership::local_project(&path).is_none());
    });
}

#[test]
fn native_static_png_open_and_import_adopt_sparse_16000_wide_images_at_both_depths() {
    use std::io::Write;

    for depth in [8, 16] {
        with_document(8, |root, directory, base_sample| {
            let path = directory.join("Wide source.png");
            let mut encoder = png::Encoder::new(fs::File::create(&path).unwrap(), 16000, 17);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(if depth == 16 { png::BitDepth::Sixteen } else { png::BitDepth::Eight });
            let mut writer = encoder.write_header().unwrap();
            let mut stream = writer.stream_writer().unwrap();
            let mut row = vec![0; 16000 * usize::from(depth / 8) * 4];
            for _ in 0..16 {
                stream.write_all(&row).unwrap();
            }
            let expected = if depth == 16 {
                let pixel = [12347u16, 23459, 34571, 0];
                let last = row.len() - 8;
                for (sample, bytes) in pixel.iter().zip(row[last..].chunks_exact_mut(2)) {
                    bytes.copy_from_slice(&sample.to_be_bytes());
                }
                pixel
            } else {
                let pixel = [39u8, 83, 177, 0];
                let last = row.len() - 4;
                row[last..].copy_from_slice(&pixel);
                pixel.map(|sample| u16::from(sample) * 257)
            };
            stream.write_all(&row).unwrap();
            stream.finish().unwrap();
            writer.finish().unwrap();

            let workspace = crate::workspace::attach(root);
            let initial = crate::workspace::active_id_in(&workspace);
            let runtime = crate::workspace::native_dispatch::runtime_for(root).unwrap();
            let session = runtime.begin().unwrap();
            let inspection = crate::image_import::prepared_png::apng::inspect_layer(
                &session, &path, &|| Ok(()),
            ).unwrap();
            assert!(inspection.frames.is_none());
            drop(session);
            let document_id = root.lock().unwrap().native_document().unwrap().metadata().id().to_owned();
            let mut wrong = inspection.digest;
            wrong[0] ^= 1;
            let mut edit = json!({
                "actor":"human", "project_id":initial, "document_id":document_id,
                "expected_revision":0, "max_edge":32,
                "commands":[{"op":"image.import", "path":path, "x":-2, "source_sha256":wrong}]
            });
            assert!(super::super::dispatch_owned(root, "edit", &edit).is_err());
            assert_eq!(root.lock().unwrap().history_lengths(), [0, 0]);
            edit["commands"][0]["source_sha256"] = json!(inspection.digest);
            super::super::dispatch_owned(root, "edit", &edit).unwrap();
            {
                let engine = root.lock().unwrap();
                let document = engine.native_document().unwrap();
                let meta = document.metadata();
                assert_eq!((meta.width(), meta.height(), meta.depth()), (4, 2, depth));
                assert_eq!(meta.layers().len(), 2);
                let imported = meta.layer(0).unwrap();
                assert_eq!(imported.origin(), [-2, 0]);
                let source = document.source(imported.pixels().slot()).unwrap();
                assert_eq!((source.width(), source.height()), (16000, 17));
                let corner = crate::raster::backed::Region::new([15999, 16, 16000, 17]).unwrap();
                assert_eq!(source.prepare_region(corner, &mut || Ok(())).unwrap().pixel16(15999, 16).unwrap(), expected);
                let base = document.source(meta.layer(1).unwrap().pixels().slot()).unwrap();
                let spot = crate::raster::backed::Region::new([1, 1, 2, 2]).unwrap();
                assert_eq!(base.prepare_region(spot, &mut || Ok(())).unwrap().pixel16(1, 1).unwrap(), base_sample);
                assert_eq!(engine.history_lengths(), [1, 0]);
            }
            assert!(matches!(
                super::super::begin_image_project_in(&workspace, &path, &Default::default(), &initial)
                    .unwrap(),
                super::super::ImageOpen::Opened
            ));
            let opened = crate::workspace::active(root).unwrap();
            let engine = opened.lock().unwrap();
            let document = engine.native_document().unwrap();
            let metadata = document.metadata();
            assert_eq!((metadata.width(), metadata.height(), metadata.depth()), (16000, 17, depth));
            let layer = metadata.layer(0).unwrap();
            let source = document.source(layer.pixels().slot()).unwrap();
            assert_eq!(source.physical_tiles().count(), 1);
            let corner = crate::raster::backed::Region::new([15999, 16, 16000, 17]).unwrap();
            let view = source.prepare_region(corner, &mut || Ok(())).unwrap();
            assert_eq!(view.pixel16(15999, 16).unwrap(), expected);
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert!(engine.path.is_none());
        });
    }
}

#[test]
fn native_row_files_open_and_pinned_import_keep_wide_native_samples() {
    use crate::image_import::native::OpenFormat;
    use image::ImageFormat;

    for (format, codec, name, incoming, depth) in [
        (OpenFormat::Bmp, ImageFormat::Bmp, "Wide source.BMP", 8, 8),
        (OpenFormat::Bmp, ImageFormat::Bmp, "Wide source.BMP", 8, 16),
        (OpenFormat::Gif, ImageFormat::Gif, "Wide source.GIF", 8, 16),
        (OpenFormat::Jpeg, ImageFormat::Jpeg, "Wide source.JPEG", 8, 16),
        (OpenFormat::Qoi, ImageFormat::Qoi, "Wide source.QOI", 8, 8),
        (OpenFormat::Qoi, ImageFormat::Qoi, "Wide source.QOI", 8, 16),
        (OpenFormat::Pnm, ImageFormat::Pnm, "Wide source.PAM", 16, 8),
        (OpenFormat::Pnm, ImageFormat::Pnm, "Wide source.PAM", 16, 16),
        (OpenFormat::Tga, ImageFormat::Tga, "Wide source.TGA", 8, 8),
        (OpenFormat::Tga, ImageFormat::Tga, "Wide source.TGA", 8, 16),
        (OpenFormat::Tiff, ImageFormat::Tiff, "Wide source.TIFF", 8, 16),
        (OpenFormat::Tiff, ImageFormat::Tiff, "Wide source.TIFF", 16, 8),
        (OpenFormat::Webp, ImageFormat::WebP, "Wide source.WEBP", 8, 16),
    ] {
        with_document(depth, |root, directory, base_sample| {
            let path = directory.join(name);
            let bytes = match format {
                OpenFormat::Bmp => crate::image_import::bmp::tests::fixture(16000, 3, 56, 32, 3, false, true),
                OpenFormat::Gif => crate::engine::native::worker::gif_import_tests::static_geometry(16000, 3, false),
                OpenFormat::Jpeg => crate::engine::native::worker::jpeg_tests::fixture_geometry(16000, 3, false),
                OpenFormat::Webp => crate::image_import::webp::tests::lossless(16000, 3, 71, true),
                OpenFormat::Pnm => crate::image_import::pnm::tests::fixture(7, 16000, 3, 4, 65535),
                OpenFormat::Qoi => crate::image_import::qoi::tests::fixture(16000, 3, 4),
                OpenFormat::Tga => crate::image_import::tga::tests::fixture(16000, 3, 10, 32, 0, 0x10),
                OpenFormat::Tiff => crate::engine::native::worker::tiff_tests::fixture_geometry(
                    incoming, false, false, true, true, (16000, 3), 1,
                ),
                _ => unreachable!(),
            };
            fs::write(&path, &bytes).unwrap();
            let image = image::load_from_memory_with_format(&bytes, codec).unwrap().to_rgba16();
            let expected = image.get_pixel(15999, 2).0;
            let info = super::super::dispatch_owned(root, "image_info", &json!({"path":path})).unwrap();
            let info = info.value();
            assert_eq!((info["width"].as_u64(), info["height"].as_u64(), info["bit_depth"].as_u64()),
                       (Some(16000), Some(3), Some(u64::from(incoming))));
            assert!(crate::image_import::Encoded::file(&path).is_err());

            let workspace = crate::workspace::attach(root);
            let initial = crate::workspace::active_id_in(&workspace);
            let runtime = crate::workspace::native_dispatch::runtime_for(root).unwrap();
            let session = runtime.begin().unwrap();
            let separate_limit = !matches!(format, OpenFormat::Gif | OpenFormat::Qoi);
            let mut remaining = if separate_limit { 0 } else { 128 * 1024 * 1024 };
            let (choice, digest, _) = format.inspect_layer(&session, &path, &mut remaining, &|| Ok(())).unwrap();
            assert!(choice.is_none());
            if separate_limit {
                assert_eq!(remaining, 0);
            }
            drop(session);
            let document_id = root.lock().unwrap().native_document().unwrap().metadata().id().to_owned();
            let mut wrong = digest;
            wrong[0] ^= 1;
            let mut edit = json!({
                "actor":"human", "project_id":initial, "document_id":document_id,
                "expected_revision":0, "max_edge":32,
                "commands":[{"op":"image.import", "path":path, "source_sha256":wrong}]
            });
            assert!(super::super::dispatch_owned(root, "edit", &edit).is_err());
            assert_eq!(root.lock().unwrap().history_lengths(), [0, 0]);
            edit["commands"][0]["source_sha256"] = json!(digest);
            super::super::dispatch_owned(root, "edit", &edit).unwrap();

            let inspect = |engine: &Engine, width, height, depth| {
                let document = engine.native_document().unwrap();
                let meta = document.metadata();
                assert_eq!((meta.width(), meta.height(), meta.depth()), (width, height, depth));
                let source = document.source(meta.layer(0).unwrap().pixels().slot()).unwrap();
                assert_eq!((source.width(), source.height(), source.depth()), (16000, 3, depth));
                let corner = crate::raster::backed::Region::new([15999, 2, 16000, 3]).unwrap();
                assert_eq!(source.prepare_region(corner, &mut || Ok(())).unwrap().pixel16(15999, 2).unwrap(), expected);
            };
            {
                let engine = root.lock().unwrap();
                inspect(&engine, 4, 2, depth.max(incoming));
                let document = engine.native_document().unwrap();
                let base = document.source(document.metadata().layer(1).unwrap().pixels().slot()).unwrap();
                let spot = crate::raster::backed::Region::new([1, 1, 2, 2]).unwrap();
                assert_eq!(base.prepare_region(spot, &mut || Ok(())).unwrap().pixel16(1, 1).unwrap(), base_sample);
                assert_eq!(engine.history_lengths(), [1, 0]);
            }
            assert!(matches!(super::super::begin_image_project_in(&workspace, &path, &Default::default(), &initial)
                .unwrap(), super::super::ImageOpen::Opened));
            let opened = crate::workspace::active(root).unwrap();
            let engine = opened.lock().unwrap();
            inspect(&engine, 16000, 3, incoming);
            assert_eq!(engine.history_lengths(), [0, 0]);
        });
    }
}

#[test]
fn native_mixed_import_keeps_frame_choice_and_rolls_back_if_a_later_file_changes() {
    with_document(8, |root, directory, base_sample| {
        let png = directory.join("Still.png");
        let gif = directory.join("Animation.gif");
        let still = [12347, 23459, 34571, 0];
        fs::write(&png, crate::raster::png16(1, 1, &still).unwrap()).unwrap();
        let animation = |second| {
            let mut bytes = Vec::new();
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder
                .encode_frames([[255, 0, 0, 255], second].map(|color| {
                    image::Frame::new(image::RgbaImage::from_pixel(2, 1, image::Rgba(color)))
                }))
                .unwrap();
            drop(encoder);
            bytes
        };
        let original = animation([0, 255, 0, 255]);
        fs::write(&gif, &original).unwrap();

        let workspace = crate::workspace::attach(root);
        let project = crate::workspace::active_id_in(&workspace);
        let runtime = crate::workspace::native_dispatch::runtime_for(root).unwrap();
        let session = runtime.begin().unwrap();
        let check = || session.check();
        let png_digest = crate::image_import::prepared_png::apng::inspect_layer(&session, &png, &check)
            .unwrap()
            .digest;
        let mut encoded_remaining = 128 * 1024 * 1024;
        let (choice, gif_digest, _) = crate::image_import::native::OpenFormat::Gif
            .inspect_layer(&session, &gif, &mut encoded_remaining, &check)
            .unwrap();
        assert_eq!(choice, Some(("frame", 2)));
        drop(session);

        let document_id = root.lock().unwrap().native_document().unwrap().metadata().id().to_owned();
        let edit = json!({
            "actor":"human", "project_id":project, "document_id":document_id,
            "expected_revision":0, "max_edge":32,
            "commands":[
                {"op":"image.import", "path":png, "source_sha256":png_digest},
                {"op":"image.import", "path":gif, "source_sha256":gif_digest, "frame":1}
            ]
        });
        fs::write(&gif, animation([0, 0, 255, 255])).unwrap();
        assert!(super::super::dispatch_owned(root, "edit", &edit).is_err());
        {
            let engine = root.lock().unwrap();
            let metadata = engine.native_document().unwrap().metadata();
            assert_eq!((metadata.revision(), metadata.depth(), metadata.layers().len()), (0, 8, 1));
            assert_eq!(engine.history_lengths(), [0, 0]);
        }

        fs::write(&gif, original).unwrap();
        super::super::dispatch_owned(root, "edit", &edit).unwrap();
        let engine = root.lock().unwrap();
        let doc = engine.native_document().unwrap();
        let meta = doc.metadata();
        assert_eq!((meta.width(), meta.height(), meta.depth(), meta.layers().len()), (4, 2, 16, 3));
        let pixel = |layer, x, y| {
            let source = doc.source(meta.layer(layer).unwrap().pixels().slot()).unwrap();
            let bounds = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
            source.prepare_region(bounds, &mut || Ok(())).unwrap().pixel16(x, y).unwrap()
        };
        assert_eq!(meta.layer(0).unwrap().name(), "Animation");
        assert_eq!(meta.layer(1).unwrap().name(), "Still");
        assert_eq!(pixel(0, 0, 0), [0, 65535, 0, 65535]);
        assert_eq!(pixel(1, 0, 0), still);
        assert_eq!(pixel(2, 1, 1), base_sample);
        assert_eq!(engine.history_lengths(), [1, 0]);
    });
}

#[test]
fn native_png_chooser_holds_the_source_and_opens_only_the_selected_frame() {
    use crate::image_import::prepared_png::apng;
    with_document(8, |root, directory, _| {
        let bytes = apng::tests::fixture(true, true, 8, 6);
        let path = directory.join("Animated.png");
        fs::write(&path, &bytes).unwrap();
        let workspace = crate::workspace::attach(root);
        let initiating = crate::workspace::active_id_in(&workspace);
        let begin = |control: &crate::loading::Control| {
            let super::super::ImageOpen::Choose(choice) =
                super::super::begin_image_project_in(&workspace, &path, control, &initiating)
                    .unwrap()
            else {
                panic!("APNG did not request a frame");
            };
            assert_eq!((choice.key, choice.count), ("frame", 5));
            assert_eq!(crate::workspace::entries_in(&workspace).len(), 1);
            choice
        };
        let control = crate::loading::Control::default();
        let choice = begin(&control);
        assert!(crate::file_ownership::local_project(&path).is_some());
        let mut changed = bytes.clone();
        *changed.last_mut().unwrap() ^= 0xff;
        fs::write(&path, changed).unwrap();
        assert!(choice.finish(&workspace, &control, &initiating, 3).is_err());
        assert!(crate::file_ownership::local_project(&path).is_none());
        assert_eq!(crate::workspace::entries_in(&workspace).len(), 1);

        fs::write(&path, &bytes).unwrap();
        let control = crate::loading::Control::default();
        let choice = begin(&control);
        control.cancel();
        assert!(choice.finish(&workspace, &control, &initiating, 3).is_err());
        assert!(crate::file_ownership::local_project(&path).is_none());
        assert_eq!(crate::workspace::entries_in(&workspace).len(), 1);

        let control = crate::loading::Control::default();
        begin(&control)
            .finish(&workspace, &control, &initiating, 3)
            .unwrap();
        assert_eq!(crate::workspace::entries_in(&workspace).len(), 2);
        let opened = crate::workspace::active(root).unwrap();
        let engine = opened.lock().unwrap();
        let native = engine.native_document().unwrap();
        assert_eq!(native.metadata().depth(), 8);
        assert!(engine.path.is_none());
        let slot = native.metadata().layer(0).unwrap().pixels().slot();
        let region = crate::raster::backed::Region::new([0, 0, 270, 9]).unwrap();
        let actual = native
            .source(slot)
            .unwrap()
            .prepare_region(region, &mut || Ok(()))
            .unwrap();
        let expected = apng::resident(&bytes, 3, &crate::loading::Control::default()).unwrap();
        for (x, y, pixel) in expected.as_rgba8().unwrap().enumerate_pixels() {
            assert_eq!(
                actual.pixel16(x as i32, y as i32).unwrap(),
                pixel.0.map(|v| u16::from(v) * 257)
            );
        }
    });
}

#[test]
fn native_tiff_open_requires_a_page_and_preserves_selected_16_bit_samples() {
    with_document(8, |root, directory, _| {
        let first = [1103u16, 2207, 3301, 65535];
        let second = [12347u16, 23459, 34571, 0];
        let mut bytes = std::io::Cursor::new(Vec::new());
        {
            let mut tiff = tiff::encoder::TiffEncoder::new(&mut bytes).unwrap();
            tiff.write_image::<tiff::encoder::colortype::RGBA16>(1, 1, &first)
                .unwrap();
            tiff.write_image::<tiff::encoder::colortype::RGBA16>(1, 1, &second)
                .unwrap();
        }
        let path = directory.join("Pages.tiff");
        fs::write(&path, bytes.into_inner()).unwrap();
        let workspace = crate::workspace::attach(root);
        let initiating = crate::workspace::active_id_in(&workspace);
        for options in [
            json!({"action":"open","path":path}),
            json!({"action":"open","path":path,"frame":1}),
        ] {
            assert!(super::super::dispatch_owned(root, "projects", &options).is_err());
            assert_eq!(crate::workspace::entries_in(&workspace).len(), 1);
        }
        let control = crate::loading::Control::default();
        let super::super::ImageOpen::Choose(choice) =
            super::super::begin_image_project_in(&workspace, &path, &control, &initiating).unwrap()
        else {
            panic!("TIFF did not request a page");
        };
        assert_eq!((choice.key, choice.count), ("page", 2));
        assert_eq!(crate::workspace::entries_in(&workspace).len(), 1);
        choice.finish(&workspace, &control, &initiating, 1).unwrap();
        let opened = crate::workspace::active(root).unwrap();
        let engine = opened.lock().unwrap();
        let native = engine.native_document().unwrap();
        assert_eq!(native.metadata().depth(), 16);
        assert!(engine.path.is_none());
        let slot = native.metadata().layer(0).unwrap().pixels().slot();
        let region = crate::raster::backed::Region::new([0, 0, 1, 1]).unwrap();
        let pixel = native
            .source(slot)
            .unwrap()
            .prepare_region(region, &mut || Ok(()))
            .unwrap()
            .pixel16(0, 0)
            .unwrap();
        assert_eq!(pixel, second);
    });
}

#[test]
fn native_svg_open_retains_the_editable_definition_and_standard_working_pixels() {
    use crate::engine::scene::Content36;
    const MARKUP: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="3"><rect width="4" height="3" fill="#17429c"/></svg>"##;
    with_document(8, |root, directory, _| {
        let workspace = crate::workspace::attach(root);
        let initiating = crate::workspace::active_id_in(&workspace);
        let check_source = |project: &Shared| {
            let engine = project.lock().unwrap();
            let native = engine.native_document().unwrap();
            let layer = native.metadata().layer(0).unwrap();
            let definition = layer.source().unwrap();
            assert!(matches!(definition.content.value(), Content36::Svg { svg } if svg == MARKUP));
            assert!(definition.content.uses_budget(native.budget()));
            assert_eq!(
                (native.metadata().width(), native.metadata().height()),
                (4, 3)
            );
            let region = crate::raster::backed::Region::new([0, 0, 4, 3]).unwrap();
            let pixel = native
                .source(layer.pixels().slot())
                .unwrap()
                .prepare_region(region, &mut || Ok(()))
                .unwrap()
                .pixel16(1, 1)
                .unwrap();
            assert_eq!(pixel, [23u16, 66, 156, 255].map(|value| value * 257));
            (
                native.metadata().id().to_owned(),
                native.metadata().revision(),
            )
        };
        let path = directory.join("Vector.svg");
        fs::write(&path, MARKUP).unwrap();
        let control = crate::loading::Control::default();
        assert!(matches!(
            super::super::begin_image_project_in(&workspace, &path, &control, &initiating).unwrap(),
            super::super::ImageOpen::Opened
        ));
        let tab = crate::workspace::active(root).unwrap();
        let (document, revision) = check_source(&tab);
        assert!(tab.lock().unwrap().path.is_none());

        let path = directory.join("Compressed.svgz");
        let mut compressed =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        compressed.write_all(MARKUP.as_bytes()).unwrap();
        fs::write(&path, compressed.finish().unwrap()).unwrap();
        let receipt =
            super::super::dispatch_owned(root, "projects", &json!({"action":"open","path":path}))
                .unwrap();
        let compressed_tab =
            crate::workspace::get_in(&workspace, receipt.value()["project_id"].as_str().unwrap())
                .unwrap();
        check_source(&compressed_tab);

        let saved = directory.join("Vector working.psd");
        super::super::save_guarded(&tab, &saved, &json!({
            "document_id":document, "expected_revision":revision, "accept_editability_loss":true,
        })).unwrap();
        let standard = psd::decode(&fs::read(&saved).unwrap()).unwrap();
        assert_eq!(
            standard.layers[0].pixels.get16(1, 1),
            [23u16, 66, 156, 255].map(|value| value * 257)
        );
        let copy = directory.join("Vector reopened.psd");
        fs::copy(saved, &copy).unwrap();
        let reopened = super::super::open_project_in(
            &workspace,
            &copy,
            &crate::loading::Control::default(),
            None,
        )
        .unwrap();
        check_source(&reopened);
    });
}

#[test]
fn static_image_open_reuses_native_samples_and_legacy_color_semantics() {
    with_document(16, |root, directory, _| {
        use image::{DynamicImage, ImageFormat, RgbImage};
        let mut sources = Vec::new();
        let rgb = DynamicImage::ImageRgb8(RgbImage::from_fn(4, 3, |x, y| {
            image::Rgb([(x * 37) as u8, (y * 81) as u8, ((x + y) * 31) as u8])
        }));
        for (extension, format) in [
            ("jpg", ImageFormat::Jpeg),
            ("bmp", ImageFormat::Bmp),
            ("qoi", ImageFormat::Qoi),
            ("tga", ImageFormat::Tga),
        ] {
            let mut encoded = std::io::Cursor::new(Vec::new());
            rgb.write_to(&mut encoded, format).unwrap();
            sources.push((extension, encoded.into_inner(), 8));
        }
        let mut ppm = b"P6\n2 1\n65535\n".to_vec();
        for word in [12347u16, 23459, 34571, 5789, 43211, 65433] {
            ppm.extend_from_slice(&word.to_be_bytes());
        }
        sources.push(("ppm", ppm, 16));
        let workspace = crate::workspace::attach(root);
        let initial = crate::workspace::active_id_in(&workspace);
        for (extension, bytes, depth) in sources {
            let path = directory.join(format!("Image.{extension}"));
            fs::write(&path, bytes).unwrap();
            let expected =
                crate::image_import::decode_file(&path, &json!({}), crate::image_import::BUDGET)
                    .unwrap();
            let reply = super::super::dispatch_owned(
                root,
                "projects",
                &json!({"action":"open","path":path}),
            )
            .unwrap();
            let tab =
                crate::workspace::get_in(&workspace, reply.value()["project_id"].as_str().unwrap())
                    .unwrap();
            let engine = tab.lock().unwrap();
            let document = engine.native_document().unwrap();
            assert_eq!(document.metadata().depth(), depth);
            assert!(engine.path.is_none());
            assert_eq!(engine.saved_revision, u64::MAX);
            let slot = document.metadata().layer(0).unwrap().pixels().slot();
            let region = crate::raster::backed::Region::new([
                0,
                0,
                expected.width as i32,
                expected.height as i32,
            ])
            .unwrap();
            let actual = document
                .source(slot)
                .unwrap()
                .prepare_region(region, &mut || Ok(()))
                .unwrap();
            for y in 0..expected.height as i32 {
                for x in 0..expected.width as i32 {
                    assert_eq!(
                        actual.pixel16(x, y).unwrap(),
                        expected.get16(x, y),
                        "{extension} at {x}, {y}"
                    );
                }
            }
        }
        assert_eq!(crate::workspace::active_id_in(&workspace), initial);
    });
}

#[test]
fn shared_native_save_as_publishes_exact_native_standard_pixels_and_keeps_history_and_sources() {
    for depth in [8, 16] {
        with_document(depth, |shared, directory, expected| {
            let key = shared.lock().unwrap().native_document().unwrap().key();
            for extension in ["psd", "psb"] {
                let destination = directory.join(format!("Saved.{extension}"));
                super::super::save(shared, &destination).unwrap();
                let bytes = fs::read(&destination).unwrap();
                assert_eq!(&bytes[..4], b"8BPS");
                assert_eq!(u16::from_be_bytes(bytes[22..24].try_into().unwrap()), depth);
                // Source36 remains protected in the resident reader. Its standard
                // native-depth composite is independently readable without sources.
                let standard = psd::decode(&bytes).unwrap();
                assert_eq!(standard.bit_depth, depth);
                assert_eq!(standard.layers[0].pixels.get16(1, 1), expected);
                let normalized = crate::file_safety::canonical_destination(&destination).unwrap();
                let engine = shared.lock().unwrap();
                assert!(engine.resident_state().is_err());
                assert_ne!(engine.native_document().unwrap().key(), key);
                assert_eq!(
                    engine.native_document().unwrap().metadata().name(),
                    format!("Saved.{extension}")
                );
                assert_eq!(engine.native_document().unwrap().metadata().revision(), 0);
                assert_eq!(engine.history_lengths(), [0, 0]);
                assert_eq!(engine.saved_revision, 0);
                assert_eq!(engine.path.as_deref(), Some(normalized.as_path()));
                assert_eq!(engine.file_version.unwrap().bytes, bytes.len() as u64);
                assert!(engine.file_state_charge.is_some());
                assert!(engine.saving.as_ref().unwrap().status().finished);
                assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
            }
        });
    }
}

#[test]
fn native_save_as_updates_the_name_once_and_retains_redo_and_future_history() {
    use crate::engine::native::Direction;
    for depth in [8, 16] {
        with_document(depth, |shared, directory, _| {
            let (id, layer, source) = {
                let engine = shared.lock().unwrap();
                let document = engine.native_document().unwrap();
                let layer = document.metadata().layer(0).unwrap();
                (
                    document.metadata().id().to_owned(),
                    layer.id().to_owned(),
                    document.source(layer.pixels().slot()).unwrap().key(),
                )
            };
            crate::server::edit_shared(
                shared,
                "human",
                &[json!({"op":"layer.update","layer":layer,"name":"Changed layer"})],
                Some(0),
                None,
                "Rename layer",
                None,
                Some(&id),
            )
            .unwrap();
            crate::server::history_shared(shared, "human", Direction::Undo, Some(1), Some(&id))
                .unwrap();
            let (seed, frozen, history) = {
                let engine = shared.lock().unwrap();
                let seed = engine.native_seed().unwrap().unwrap();
                let history = engine.capture_native_history(&seed).unwrap();
                (seed, engine.native_document().unwrap().clone(), history)
            };
            let stale = history
                .prepare_move(&frozen, Direction::Redo, "human", &|| Ok(()))
                .unwrap();
            let destination = directory.join("History Ω.psb");
            super::super::save(shared, &destination).unwrap();
            {
                let mut engine = shared.lock().unwrap();
                let refused = engine.move_native_history(&seed, "human", stale);
                drop(engine);
                assert!(refused.is_err());
            }
            assert_eq!(frozen.metadata().name(), "Untitled.psd");
            for (direction, revision, lengths) in
                [(Direction::Redo, 3, [1, 0]), (Direction::Undo, 4, [0, 1])]
            {
                {
                    let engine = shared.lock().unwrap();
                    let document = engine.native_document().unwrap();
                    assert_eq!(document.metadata().name(), "History Ω.psb");
                    assert_eq!(document.metadata().revision(), revision - 1);
                    assert_eq!(engine.saved_revision, 2);
                }
                crate::server::history_shared(
                    shared,
                    "human",
                    direction,
                    Some(revision - 1),
                    Some(&id),
                )
                .unwrap();
                let engine = shared.lock().unwrap();
                let document = engine.native_document().unwrap();
                assert_eq!(document.metadata().name(), "History Ω.psb");
                assert_eq!(document.metadata().revision(), revision);
                assert_eq!(engine.history_lengths(), lengths);
                assert_eq!(
                    document
                        .source(document.metadata().layer(0).unwrap().pixels().slot())
                        .unwrap()
                        .key(),
                    source
                );
            }
            let other = directory.join("Other");
            fs::create_dir(&other).unwrap();
            let key = shared.lock().unwrap().native_document().unwrap().key();
            super::super::save(shared, &other.join("History Ω.psb")).unwrap();
            let engine = shared.lock().unwrap();
            assert_ne!(engine.native_document().unwrap().key(), key);
            assert_eq!(
                engine.native_document().unwrap().metadata().name(),
                "History Ω.psb"
            );
            assert_eq!(engine.saved_revision, 4);
            assert_eq!(engine.history_lengths(), [0, 1]);
        });
    }
}

#[test]
fn native_existing_save_as_requires_explicit_replacement_consent() {
    for depth in [8, 16] {
        with_document(depth, |shared, directory, _| {
            let destination = directory.join("Human.psd");
            let original = b"Existing human file is not a native replacement permit";
            fs::write(&destination, original).unwrap();
            let key = shared.lock().unwrap().native_document().unwrap().key();
            let result = super::super::save(shared, &destination);
            assert!(result.unwrap_err().contains("overwrite=true"));
            assert_eq!(fs::read(&destination).unwrap(), original);
            let engine = shared.lock().unwrap();
            assert_eq!(engine.native_document().unwrap().key(), key);
            assert_eq!(engine.saved_revision, u64::MAX);
            assert!(engine.path.is_none() && engine.file_version.is_none());
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
        });
    }
}
#[cfg(windows)]
#[test]
fn native_save_refuses_named_stream_loss_before_candidate_or_backup_creation() {
    with_document(16, |shared, directory, _| {
        let path = directory.join("Human.psb");
        let stream = path.with_file_name("Human.psb:notes");
        fs::write(&path, b"human main data").unwrap();
        fs::write(&stream, b"human metadata").unwrap();
        let (id, revision, key) = {
            let engine = shared.lock().unwrap();
            let doc = engine.native_document().unwrap();
            (
                doc.metadata().id().to_owned(),
                doc.metadata().revision(),
                doc.key(),
            )
        };
        let result = super::super::save_confirmed_source(shared, &path, &id, revision, true);
        assert!(result.unwrap_err().contains("named streams"));
        assert_eq!(fs::read(&path).unwrap(), b"human main data");
        assert_eq!(fs::read(&stream).unwrap(), b"human metadata");
        for entry in fs::read_dir(directory.join(".peerbrush-versions")).unwrap() {
            assert_eq!(entry.unwrap().metadata().unwrap().len(), 0);
        }
        let engine = shared.lock().unwrap();
        assert_eq!(engine.native_document().unwrap().key(), key);
        assert!(engine.path.is_none() && engine.file_version.is_none());
        assert_eq!(engine.saved_revision, u64::MAX);
        assert_eq!(engine.history_lengths(), [0, 0]);
    });
}

#[test]
fn shared_native_save_replaces_its_observed_source_and_preserves_the_old_physical_file() {
    for depth in [8, 16] {
        for extension in ["psd", "psb"] {
            with_document(depth, |shared, directory, expected| {
                let path = directory.join(format!("Working.{extension}"));
                super::super::save(shared, &path).unwrap();
                let old = fs::read(&path).unwrap();
                let (version, layer) = {
                    let engine = shared.lock().unwrap();
                    (
                        engine.file_version.unwrap(),
                        engine
                            .native_document()
                            .unwrap()
                            .metadata()
                            .layer(0)
                            .unwrap()
                            .id()
                            .to_owned(),
                    )
                };
                crate::server::edit_shared(
                    shared,
                    "human",
                    &[json!({"op":"layer.update","layer":layer,"name":"Human revision"})],
                    Some(0),
                    None,
                    "Rename layer",
                    None,
                    None,
                )
                .unwrap();
                super::super::save(shared, &path).unwrap();
                let saved = psd::decode(&fs::read(&path).unwrap()).unwrap();
                assert_eq!(saved.bit_depth, depth);
                assert_eq!(saved.layers[0].pixels.get16(1, 1), expected);
                let retained = fs::read_dir(directory.join(".peerbrush-versions"))
                    .unwrap()
                    .map(|item| item.unwrap().path())
                    .find(|p| {
                        crate::file_safety::open_native_identity(p).is_ok_and(|f| {
                            crate::file_safety::identity(&f).unwrap() == version.identity
                        })
                    })
                    .unwrap();
                assert_eq!(fs::read(&retained).unwrap(), old);
                let engine = shared.lock().unwrap();
                assert_ne!(engine.file_version.unwrap().identity, version.identity);
                assert_eq!(
                    engine.saved_revision,
                    engine.native_document().unwrap().metadata().revision()
                );
                assert_eq!(engine.saved_revision, 1);
                assert_eq!(engine.history_lengths(), [1, 0]);
                assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
                assert!(engine.saving.as_ref().unwrap().status().finished);
            });
        }
    }
}

#[test]
fn confirmed_native_save_as_replaces_another_file_and_preserves_its_exact_bytes() {
    for depth in [8, 16] {
        with_document(depth, |shared, directory, expected| {
            let path = directory.join("Confirmed.psb");
            let old = b"the human explicitly chose to replace this file";
            fs::write(&path, old).unwrap();
            let old_id = crate::file_safety::identity(
                &crate::file_safety::open_native_identity(&path).unwrap(),
            )
            .unwrap();
            let (id, revision) = {
                let engine = shared.lock().unwrap();
                let document = engine.native_document().unwrap();
                (
                    document.metadata().id().to_owned(),
                    document.metadata().revision(),
                )
            };
            super::super::save_confirmed_source(shared, &path, &id, revision, true).unwrap();
            let saved = psd::decode(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved.bit_depth, depth);
            assert_eq!(saved.layers[0].pixels.get16(1, 1), expected);
            let retained = fs::read_dir(directory.join(".peerbrush-versions"))
                .unwrap()
                .map(|item| item.unwrap().path())
                .find(|p| {
                    crate::file_safety::open_native_identity(p)
                        .is_ok_and(|f| crate::file_safety::identity(&f).unwrap() == old_id)
                })
                .unwrap();
            assert_eq!(fs::read(retained).unwrap(), old);
            let engine = shared.lock().unwrap();
            assert_eq!(engine.saved_revision, revision);
            assert_eq!(engine.history_lengths(), [0, 0]);
        });
    }
}

#[test]
fn confirmed_native_save_cannot_override_a_changed_loaded_source() {
    for depth in [8, 16] {
        with_document(depth, |shared, directory, _| {
            let path = directory.join("Changed.psb");
            super::super::save(shared, &path).unwrap();
            let (id, revision, version, key) = {
                let engine = shared.lock().unwrap();
                let document = engine.native_document().unwrap();
                (
                    document.metadata().id().to_owned(),
                    document.metadata().revision(),
                    engine.file_version,
                    document.key(),
                )
            };
            fs::write(&path, b"external human source survives").unwrap();
            let result = super::super::save_confirmed_source(shared, &path, &id, revision, true);
            assert!(result.unwrap_err().contains("changed outside"));
            assert_eq!(fs::read(&path).unwrap(), b"external human source survives");
            let engine = shared.lock().unwrap();
            assert_eq!(engine.file_version, version);
            assert_eq!(engine.native_document().unwrap().key(), key);
            assert_eq!(engine.saved_revision, revision);
            assert_eq!(engine.history_lengths(), [0, 0]);
        });
    }
}

#[test]
fn native_late_cancel_or_newer_edit_refuses_publication_and_keeps_live_work_dirty() {
    for depth in [8, 16] {
        for existing in [false, true] {
            for cancel in [false, true] {
                with_document(depth, |shared, directory, _| {
                    let destination = directory.join("Refused.psb");
                    let original = if existing {
                        super::super::save(shared, &destination).unwrap();
                        Some(fs::read(&destination).unwrap())
                    } else {
                        None
                    };
                    let previous = {
                        let engine = shared.lock().unwrap();
                        (
                            engine.path.clone(),
                            engine.file_version,
                            engine.saved_revision,
                        )
                    };
                    let control = Control::default();
                    let key = shared.lock().unwrap().native_document().unwrap().key();
                    let result = super::super::save_controlled(
                        shared,
                        &destination,
                        &json!({}),
                        &control,
                        || {
                            assert!(
                                shared.try_lock().is_ok(),
                                "Native save callback retained Engine"
                            );
                            if cancel {
                                assert!(control.cancel());
                            } else {
                                let layer = shared
                                    .lock()
                                    .unwrap()
                                    .native_document()
                                    .unwrap()
                                    .metadata()
                                    .layer(0)
                                    .unwrap()
                                    .id()
                                    .to_owned();
                                crate::server::edit_shared(shared, "human",
                            &[json!({"op":"layer.update","layer":layer,"name":"Newer human work"})],
                            Some(0), None, "Newer work", None, None).unwrap();
                            }
                        },
                    );
                    assert!(result.is_err());
                    if let Some(original) = original {
                        assert_eq!(fs::read(&destination).unwrap(), original);
                    } else {
                        assert!(!destination.exists());
                    }
                    assert!(control.status().finished);
                    let engine = shared.lock().unwrap();
                    assert_eq!(engine.saved_revision, previous.2);
                    assert_eq!(engine.path, previous.0);
                    assert_eq!(engine.file_version, previous.1);
                    assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
                    if cancel {
                        assert_eq!(engine.native_document().unwrap().key(), key);
                        assert_eq!(engine.history_lengths(), [0, 0]);
                    } else {
                        assert_ne!(engine.native_document().unwrap().key(), key);
                        assert_eq!(
                            engine
                                .native_document()
                                .unwrap()
                                .metadata()
                                .layer(0)
                                .unwrap()
                                .name(),
                            "Newer human work"
                        );
                        assert_eq!(engine.history_lengths(), [1, 0]);
                    }
                });
            }
        }
    }
}
