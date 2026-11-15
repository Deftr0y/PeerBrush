use super::*;
use base64::{engine::general_purpose::STANDARD, Engine as _};

fn png(depth: u16) -> Vec<u8> {
    let words: Vec<u16> = (0..17 * 13)
        .flat_map(|i| {
            [
                (i * 3271 + 1) as u16,
                (i * 773 + 127) as u16,
                (i * 1493 + 32769) as u16,
                if i % 7 == 0 {
                    0
                } else {
                    (i * 2917 + 7171) as u16
                },
            ]
        })
        .collect();
    if depth == 16 {
        crate::raster::png16(17, 13, &words).unwrap()
    } else {
        crate::raster::png(
            17,
            13,
            &words
                .into_iter()
                .map(crate::raster::project16)
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }
}

#[test]
fn malformed_selection_imports_retire_prior_batch_output_and_decoded_sources() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let native =
        NativeDocument::new_blank(7, 5, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let bytes = png(16);
    let mut damaged = bytes.clone();
    *damaged.last_mut().unwrap() ^= 1;
    let mut no_alpha = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        2,
        2,
        image::Rgb([231, 137, 51]),
    ))
    .write_to(&mut no_alpha, image::ImageFormat::Png)
    .unwrap();
    let valid = json!({"op":"selection.import","png":STANDARD.encode(&bytes),"rect":[1,1,6,4]});
    let baseline = runtime.disk().used().unwrap();
    for patch in [
        json!({"rect":null}),
        json!({"rect":[-1,0,6,4]}),
        json!({"mode":"invalid"}),
        json!({"png":"not base64"}),
        json!({"png":STANDARD.encode(&damaged)}),
        json!({"png":STANDARD.encode(no_alpha.into_inner()),"channel":"alpha"}),
        json!({"path":fixture.path().join("not-opened.png")}),
    ] {
        let mut invalid = valid.clone();
        for (key, value) in patch.as_object().unwrap() {
            invalid[key] = value.clone();
        }
        let failed = worker::prepare(
            capture(&state, &[valid.clone(), invalid]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failed.cleanup_error.is_none(), "{}", failed.reason);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
    }
    drop((state, native, session, runtime, connection));
}

#[test]
fn png_selection_import_preserves_artwork_placement_combination_and_previous_history() {
    for depth in [8, 16] {
        for prior in 0..3 {
            let fixture = Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
            let mut original = Document::new_depth(269, 19, depth).unwrap();
            original.layers[0]
                .pixels
                .set16(3, 4, [12347, 23459, 34571, 49153]);
            let mut remembered = original.clone();
            remembered.selection = Some([2, 3, 11, 14]);
            original.selection_previous = crate::selection::current(&remembered);
            if prior == 1 {
                let polygon = vec![[1.5, 0.5], [267.5, 1.5], [19.5, 17.5]];
                original.selection = Some(crate::selection::bounds(&polygon).unwrap());
                original.selection_polygon = Some(polygon);
            } else if prior == 2 {
                let mut mask = crate::raster::Raster::new(17, 13);
                mask.set(4, 3, [137, 0, 0, 0]);
                mask.set(12, 11, [231, 17, 19, 255]);
                let coverage = crate::selection::Coverage {
                    mask,
                    bounds: [252, 1, 265, 15],
                    origin: [250, -2],
                    contours: vec![],
                };
                original.selection = Some(coverage.bounds);
                original.selection_coverage = Some(coverage);
            }
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "selection-import".into(),
                vec![],
                None,
                None,
            )
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
            let native =
                NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
                    .unwrap();
            let session = staged.session.take().unwrap();
            let color_key = native
                .source(native.metadata().layer(0).unwrap().pixels().slot())
                .unwrap()
                .key();
            let path = fixture.path().join("selection.png");
            for (channel, png_depth) in [("alpha", 8), ("luma", 16)] {
                let bytes = png(png_depth);
                std::fs::write(&path, &bytes).unwrap();
                for from_file in [false, true] {
                    for mode in ["replace", "add", "subtract", "intersect"] {
                        let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                        let mut command = json!({"op":"selection.import","rect":[3,2,267,18],"channel":channel,"mode":mode,"feather":3,"edge":47});
                        if from_file {
                            command["path"] = json!(path);
                        } else {
                            command["png"] = json!(STANDARD.encode(&bytes));
                        }
                        let expected = crate::engine::Engine::preview_edits(
                            original.clone(),
                            &[command.clone()],
                        )
                        .unwrap();
                        let baseline = runtime.disk().used().unwrap();
                        let preview =
                            worker::prepare(capture(&state, &[command.clone()]), &session, &|| {
                                Ok(())
                            })
                            .unwrap();
                        compare(preview.preview(), &expected);
                        let result = preview.preview();
                        assert_eq!(
                            result
                                .source(result.metadata().layer(0).unwrap().pixels().slot())
                                .unwrap()
                                .key(),
                            color_key
                        );
                        assert_eq!(
                            json!(result.metadata())["layers"],
                            json!(native.metadata())["layers"]
                        );
                        preview
                            .abort(&session, &|| session.cleanup_check())
                            .unwrap();
                        assert_eq!(runtime.disk().used().unwrap(), baseline);
                        if mode == "replace" {
                            let completed = crate::selection::CompletedTiles::observe();
                            let failed = worker::prepare(
                                capture(&state, &[command.clone()]),
                                &session,
                                &|| {
                                    if completed.count() > 0 {
                                        Err("PNG selection canceled after coverage tile".into())
                                    } else {
                                        Ok(())
                                    }
                                },
                            )
                            .err()
                            .unwrap();
                            assert!(failed
                                .reason
                                .contains("PNG selection canceled after coverage tile"));
                            assert!(failed.cleanup_error.is_none());
                            assert_eq!(completed.count(), 1);
                            drop(completed);
                            assert_eq!(runtime.disk().used().unwrap(), baseline);
                            if from_file {
                                let completed = crate::selection::CompletedTiles::observe();
                                let changed = std::cell::Cell::new(false);
                                let mut corrupted = bytes.clone();
                                *corrupted.last_mut().unwrap() ^= 1;
                                let failed = worker::prepare(
                                    capture(&state, &[command.clone()]),
                                    &session,
                                    &|| {
                                        if completed.count() > 0 && !changed.replace(true) {
                                            std::fs::write(&path, &corrupted)
                                                .map_err(|e| e.to_string())?;
                                        }
                                        Ok(())
                                    },
                                )
                                .err()
                                .unwrap();
                                assert!(changed.get());
                                assert!(
                                    failed.reason.to_lowercase().contains("changed"),
                                    "{}",
                                    failed.reason
                                );
                                assert!(failed.cleanup_error.is_none());
                                drop(completed);
                                assert_eq!(runtime.disk().used().unwrap(), baseline);
                                std::fs::write(&path, &bytes).unwrap();
                            }
                            let mut invalid = command.clone();
                            invalid["channel"] = json!("invalid");
                            let failed = worker::prepare(
                                capture(&state, &[command.clone(), invalid]),
                                &session,
                                &|| Ok(()),
                            )
                            .err()
                            .unwrap();
                            assert!(failed.reason.contains("luma or alpha"));
                            assert!(failed.cleanup_error.is_none());
                            assert_eq!(runtime.disk().used().unwrap(), baseline);
                            let prepared = worker::prepare(
                                capture(&state, &[command.clone()]),
                                &session,
                                &|| Ok(()),
                            )
                            .unwrap();
                            let (base, history, delta) = prepared.into_parts();
                            let committed = history
                                .prepare_commit(
                                    &base,
                                    delta,
                                    HistoryRequest {
                                        actor: "human",
                                        label: "Import selection",
                                        task: None,
                                        gesture: None,
                                    },
                                    &|| Ok(()),
                                )
                                .unwrap();
                            let leases = [crate::engine::Lease {
                                id: "late".into(),
                                owner: "other".into(),
                                description: "Selection".into(),
                                scopes: vec![crate::engine::Scope::layer("@selection")],
                                expires: 100,
                            }];
                            let refused = state
                                .commit_native(publication(&leases), committed)
                                .unwrap_err();
                            assert!(refused.reason().contains("Reserved"));
                            refused
                                .abort(&session, &|| session.cleanup_check())
                                .unwrap();
                            assert_eq!(runtime.disk().used().unwrap(), baseline);
                            assert_eq!(state.native().unwrap().key(), native.key());
                            assert_eq!(state.history_lengths(), [0, 0]);
                            let prepared =
                                worker::prepare(capture(&state, &[command]), &session, &|| Ok(()))
                                    .unwrap();
                            commit(&mut state, prepared, &session);
                            for (direction, expected) in
                                [(Direction::Undo, &original), (Direction::Redo, &expected)]
                            {
                                let moved = state
                                    .native_history()
                                    .unwrap()
                                    .prepare_move(
                                        state.native().unwrap(),
                                        direction,
                                        "human",
                                        &|| Ok(()),
                                    )
                                    .unwrap();
                                drop(state.move_native(publication(&[]), moved).unwrap());
                                compare(state.native().unwrap(), expected);
                            }
                        }
                    }
                }
            }
            drop((native, staged.seals, session, cache, runtime, connection));
        }
    }
}
