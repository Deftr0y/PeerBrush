use super::large::sample;
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::io::Write;

fn large_png(depth: u16) -> Vec<u8> {
    let mut encoded = vec![];
    {
        let mut encoder = png::Encoder::new(&mut encoded, 16000, 1001);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(if depth == 16 {
            png::BitDepth::Sixteen
        } else {
            png::BitDepth::Eight
        });
        let mut writer = encoder.write_header().unwrap();
        let mut stream = writer.stream_writer().unwrap();
        let stride = usize::from(depth / 8) * 4;
        let mut row = vec![0u8; 16000 * stride];
        for _ in 0..1000 {
            stream.write_all(&row).unwrap();
        }
        for (x, value) in [(0, 12347u16), (15999, 54321u16)] {
            let at = x * stride + 3 * usize::from(depth / 8);
            if depth == 16 {
                row[at..at + 2].copy_from_slice(&value.to_be_bytes());
            } else {
                row[at] = crate::raster::project16(value);
            }
        }
        stream.write_all(&row).unwrap();
        stream.finish().unwrap();
        writer.finish().unwrap();
    }
    encoded
}

#[test]
fn large_sampled_and_imported_selections_preserve_artwork_and_cleanup() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
        let mut original = Document::new_depth(1, 1, depth).unwrap();
        original.width = 16000;
        original.height = 1001;
        original.layers[0].pixels = crate::raster::Raster::new_depth(16000, 1001, depth);
        original.layers[0]
            .pixels
            .set16(15996, 997, [65535, 0, 0, 65535]);
        let target = original.layers[0].id.clone();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original, "large-pixel-selection".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let baseline = runtime.disk().used().unwrap();
        for (kind, contiguous) in [
            ("wand", true),
            ("wand", false),
            ("quick", true),
            ("object", false),
        ] {
            let command = json!({"op":"selection","kind":kind,"contiguous":contiguous,"layer":target,
                "point":[15996,997],"rect":[15994,995,16000,1001],"tolerance":0,"radius":2});
            let prepared =
                worker::prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            assert_eq!(
                prepared.preview().metadata().selection(),
                Some([15996, 997, 15997, 998])
            );
            assert_eq!(sample(prepared.preview(), 15996, 997), 255);
            assert_eq!(sample(prepared.preview(), 15995, 997), 0);
            assert_eq!(sample(prepared.preview(), 0, 0), 0);
            assert_eq!(
                prepared
                    .preview()
                    .source(
                        prepared
                            .preview()
                            .metadata()
                            .layer(0)
                            .unwrap()
                            .pixels()
                            .slot()
                    )
                    .unwrap()
                    .key(),
                native
                    .source(native.metadata().layer(0).unwrap().pixels().slot())
                    .unwrap()
                    .key()
            );
            prepared
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), baseline);
        }
        let bytes = large_png(depth);
        let path = fixture.path().join("large-selection.png");
        std::fs::write(&path, &bytes).unwrap();
        for transport in [json!({"png":STANDARD.encode(&bytes)}), json!({"path":path})] {
            let mut command =
                json!({"op":"selection.import","rect":[0,0,16000,1001],"channel":"alpha"});
            command
                .as_object_mut()
                .unwrap()
                .extend(transport.as_object().unwrap().clone());
            let prepared =
                worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            assert_eq!(
                prepared.preview().metadata().selection(),
                Some([0, 1000, 16000, 1001])
            );
            assert_eq!(
                sample(prepared.preview(), 0, 1000),
                crate::raster::project16(12347)
            );
            assert_eq!(
                sample(prepared.preview(), 15999, 1000),
                crate::raster::project16(54321)
            );
            assert_eq!(sample(prepared.preview(), 15999, 999), 0);
            prepared
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            let completed = crate::selection::CompletedTiles::observe();
            let failed = worker::prepare(capture(&state, &[command]), &session, &|| {
                if completed.count() > 0 {
                    Err("large import canceled after coverage tile".into())
                } else {
                    Ok(())
                }
            })
            .err()
            .unwrap();
            assert!(failed
                .reason
                .contains("large import canceled after coverage tile"));
            assert!(failed.cleanup_error.is_none());
            assert_eq!(runtime.disk().used().unwrap(), baseline);
        }
        assert_eq!(state.history_lengths(), [0, 0]);
        assert_eq!(state.native().unwrap().key(), native.key());
        drop((
            state,
            native,
            staged.seals,
            session,
            cache,
            runtime,
            connection,
        ));
    }
}

#[test]
fn large_dense_wand_and_refinement_keep_coverage_and_history() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let native = NativeDocument::new_blank(
        16000,
        4200,
        16,
        &runtime.resident(),
        &runtime.disk(),
        &|| Ok(()),
    )
    .unwrap();
    let mut state = DocumentAuthority::from_native(native, 4).unwrap();
    let prepared = worker::prepare(
        capture(
            &state,
            &[json!({"op":"selection","kind":"wand","point":[15999,4199],"tolerance":0})],
        ),
        &session,
        &|| Ok(()),
    )
    .unwrap();
    assert_eq!(
        prepared.preview().metadata().selection(),
        Some([0, 0, 16000, 4200])
    );
    assert_eq!(sample(prepared.preview(), 0, 0), 255);
    assert_eq!(sample(prepared.preview(), 15999, 4199), 255);
    prepared
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(runtime.disk().used().unwrap(), 0);
    let commands = [
        json!({"op":"selection","rect":[15990,4190,15994,4194]}),
        json!({"op":"selection.refine","feather":1}),
    ];
    let prepared = worker::prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
    assert_eq!(
        prepared.preview().metadata().selection(),
        Some([15989, 4189, 15995, 4195])
    );
    for (x, y, value) in [
        (15989, 4189, 28),
        (15990, 4190, 113),
        (15991, 4191, 255),
        (15995, 4195, 0),
    ] {
        assert_eq!(sample(prepared.preview(), x, y), value);
    }
    commit(&mut state, prepared, &session);
    for direction in [Direction::Undo, Direction::Redo] {
        let moved = state
            .native_history()
            .unwrap()
            .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
            .unwrap();
        drop(state.move_native(publication(&[]), moved).unwrap());
        if matches!(direction, Direction::Undo) {
            assert!(state.native().unwrap().metadata().selection().is_none());
        } else {
            assert_eq!(sample(state.native().unwrap(), 15990, 4190), 113);
        }
    }
    drop((state, session, runtime, connection));
}

#[test]
fn selection_file_larger_than_inline_limit_uses_bounded_memory_and_retires_sources() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(48 * 1024 * 1024, 16 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let native = NativeDocument::new_blank(
            1,
            1,
            depth,
            &runtime.resident(),
            &runtime.disk(),
            &|| Ok(()),
        )
        .unwrap();
        let state = DocumentAuthority::from_native(native, 4).unwrap();
        let path = fixture.path().join("padded-selection.png");
        {
            let mut encoder = png::Encoder::new(std::fs::File::create(&path).unwrap(), 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(if depth == 16 {
                png::BitDepth::Sixteen
            } else {
                png::BitDepth::Eight
            });
            let mut writer = encoder.write_header().unwrap();
            // Valid ancillary bytes exceed both the inline limit and the entire
            // resident pool, without allocating the complete fixture in memory.
            let padding = vec![0u8; 1024 * 1024];
            for _ in 0..65 {
                writer
                    .write_chunk(png::chunk::ChunkType(*b"paDd"), &padding)
                    .unwrap();
            }
            if depth == 16 {
                writer
                    .write_image_data(&[0, 1, 0, 2, 0, 3, 0x30, 0x3b])
                    .unwrap();
            } else {
                writer
                    .write_image_data(&[1, 2, 3, crate::raster::project16(12347)])
                    .unwrap();
            }
            writer.finish().unwrap();
        }
        assert!(path.metadata().unwrap().len() > 64 * 1024 * 1024);
        let command =
            json!({"op":"selection.import", "path":path, "rect":[0,0,1,1], "channel":"alpha"});
        let baseline = runtime.disk().used().unwrap();
        let prepared =
            worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
        assert_eq!(
            sample(prepared.preview(), 0, 0),
            crate::raster::project16(12347)
        );
        prepared
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        // Corruption in ignored ancillary data must still be rejected.
        use std::io::{Seek, SeekFrom};
        let mut changed = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        changed.seek(SeekFrom::Start(41)).unwrap();
        changed.write_all(&[1]).unwrap();
        drop(changed);
        let failed = worker::prepare(capture(&state, &[command]), &session, &|| Ok(()))
            .err()
            .unwrap();
        assert!(failed.reason.contains("checksum"));
        assert!(failed.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.history_lengths(), [0, 0]);
        drop((state, session, runtime, connection));
    }
}

#[test]
fn selection_file_is_bound_through_coverage_sampling() {
    use std::{cell::Cell, fs::OpenOptions};
    #[cfg(unix)]
    use std::io::{Seek, SeekFrom};
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(48 * 1024 * 1024, 16 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let native =
        NativeDocument::new_blank(1, 1, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let key = native.key();
    let state = DocumentAuthority::from_native(native, 4).unwrap();
    let path = fixture.path().join("held-selection.png");
    image::RgbaImage::from_raw(1, 1, vec![1, 2, 3, 127])
        .unwrap()
        .save(&path)
        .unwrap();
    #[cfg(unix)]
    let modified = path.metadata().unwrap().modified().unwrap();
    let completed = crate::selection::CompletedTiles::observe();
    let attempted = Cell::new(false);
    let command =
        json!({"op":"selection.import", "path":path, "rect":[0,0,1,1], "channel":"alpha"});
    let result = worker::prepare(capture(&state, &[command]), &session, &|| {
        if completed.count() > 0 && !attempted.replace(true) {
            let writer = OpenOptions::new().write(true).open(&path);
            #[cfg(windows)]
            assert!(writer.is_err(), "held source must exclude writers");
            #[cfg(unix)]
            {
                let mut writer = writer.unwrap();
                writer.seek(SeekFrom::End(-1)).unwrap();
                writer.write_all(&[0]).unwrap();
                writer
                    .set_times(std::fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            }
        }
        Ok(())
    });
    assert!(attempted.get());
    #[cfg(windows)]
    {
        let prepared = result.unwrap();
        assert_eq!(sample(prepared.preview(), 0, 0), 127);
        prepared
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
    }
    #[cfg(unix)]
    {
        let failed = result.err().unwrap();
        assert!(failed.reason.contains("differ from the admitted source"));
        assert!(failed.cleanup_error.is_none());
    }
    assert_eq!(runtime.disk().used().unwrap(), 0);
    assert_eq!(state.native().unwrap().key(), key);
    assert_eq!(state.history_lengths(), [0, 0]);
    drop((state, session, runtime, connection));
}
