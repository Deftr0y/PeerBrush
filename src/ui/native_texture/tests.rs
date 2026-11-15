use super::*;
use std::time::{Duration, Instant};

#[test]
fn resident_frame_uploads_full_samples_and_patches_both_sides_of_a_filtering_seam() {
    let ctx = egui::Context::default();
    let input = || egui::RawInput { max_texture_side: Some(1024), ..Default::default() };
    let size = [1026, 3];
    let mut bytes = vec![0; (size[0] * size[1] * 4) as usize];
    let last = bytes.len() - 4;
    bytes[last..].copy_from_slice(&[17, 41, 127, 255]);
    let mut texture = None;
    let initial = ctx.run(input(), |ctx| {
        texture = Some(RgbaTexture::new(ctx, "resident frame", size, &bytes).unwrap());
    });
    let mut texture = texture.unwrap();
    assert_eq!(texture.size(), size);
    assert_eq!(texture.tiles.len(), 2);
    assert!(texture.tiles.iter().all(|tile| tile.texture.size()[0] <= 1024));
    let (_, edge) = initial.textures_delta.set.iter()
        .find(|(id, _)| *id == texture.tiles[1].texture.id()).unwrap();
    let egui::ImageData::Color(edge) = &edge.image else { panic!("RGBA upload required"); };
    assert_eq!(edge.pixels.last(), Some(&egui::Color32::from_rgb(17, 41, 127)));
    let seam = texture.tiles[1].area.inner[0];
    assert!(texture.tiles[0].area.outer[2] > seam);
    let at = ((size[0] + seam) * 4) as usize;
    bytes[at..at + 4].copy_from_slice(&[19, 63, 191, 255]);
    let output = ctx.run(input(), |_| texture.patch(&bytes, [seam, 1, seam + 1, 2]));
    for tile in &texture.tiles {
        let (_, delta) = output.textures_delta.set.iter()
            .find(|(id, _)| *id == tile.texture.id()).unwrap();
        assert_eq!(delta.pos, Some([(seam - tile.area.outer[0]) as usize, 1]));
        let egui::ImageData::Color(image) = &delta.image else { panic!("RGBA patch required"); };
        assert_eq!(image.size, [1, 1]);
        assert_eq!(image.pixels, [egui::Color32::from_rgb(19, 63, 191)]);
    }
}

fn wait_empty(pool: &ResidentBudget) {
    let end = Instant::now() + Duration::from_secs(5);
    while pool.used() != 0 {
        assert!(
            Instant::now() < end,
            "Image owner or retirement admission did not release"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn image(retirement: &Retirement, size: [usize; 2], value: u8) -> tiles::OwnedImage {
    tiles::OwnedImage::convert(size, retirement, |i| Ok([value, i as u8, 71, 199])).unwrap()
}

#[test]
fn tile_row_conversion_preserves_halo_samples_and_refunds_cancelled_uploads() {
    struct RasterRows([[u8; 20]; 3]);
    impl tiles::Rows for RasterRows {
        fn row(&self, y: u32) -> Result<&[u8]> { Ok(&self.0[y as usize]) }
    }
    let rows = RasterRows(std::array::from_fn(|y| {
        std::array::from_fn(|i| (13 * i + y * 79) as u8)
    }));
    let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
    let retirement = Retirement::new(&pool).unwrap();
    let baseline = pool.used();
    let area = tiles::Area { inner: [2, 1, 4, 2], outer: [1, 0, 5, 3] };
    let upload = tiles::Upload::prepare(area, 5, &rows, &retirement, &mut || Ok(())).unwrap();
    let expected = rows.0.iter().flat_map(|row| (1..5).map(move |x| {
        egui::Color32::from_rgba_unmultiplied(row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3])
    })).collect::<Vec<_>>();
    assert_eq!(upload.pixels.image.size, [4, 3]);
    assert_eq!(upload.pixels.image.pixels, expected);
    drop(upload);
    let mut checks = 0;
    let refused = tiles::Upload::prepare(area, 5, &rows, &retirement, &mut || {
        checks += 1;
        if checks == 2 { Err("Interrupted on the next row".into()) } else { Ok(()) }
    });
    assert_eq!(refused.err().as_deref(), Some("Interrupted on the next row"));
    assert_eq!(pool.used(), baseline);
}

#[test]
fn full_canvas_requests_stream_above_the_cpu_frame_limit_without_materializing_it() {
    use crate::{engine::{native::{worker, NativeDocument}, scene::{SceneIdentity, SceneSnapshot}}, render::native_preview::Request};
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let pool = runtime.resident();
        let document = NativeDocument::new_blank(16000, 16000, depth, &pool, &runtime.disk(), &|| Ok(())).unwrap();
        let metadata = serde_json::to_value(document.metadata()).unwrap();
        let scene = Arc::new(SceneSnapshot::from_native(
            SceneIdentity { project_id: "full-canvas".into(), document_id: document.metadata().id().into(), revision: document.metadata().revision() },
            document.clone(), vec![], None, None,
        ).unwrap());
        let session = Arc::new(runtime.begin().unwrap());
        let request = Request::capture(scene, session, None, 16000, None, false).unwrap();
        assert_eq!((request.key().width, request.key().height), (8192, 8192));
        let request = request.for_canvas(16000).unwrap();
        assert_eq!((request.key().width, request.key().height), (16000, 16000));
        assert!(request.tag().matches_edge(16000));
        assert!(!request.tag().matches_edge(8192));
        assert!(request.clone().for_canvas(8000).is_err(), "Shared sampling tags must remain immutable");
        let baseline = pool.used();
        assert!(request.render(&mut || Ok(())).is_err());
        assert_eq!(pool.used(), baseline);
        let mut next = 0;
        let mut count = 0;
        let result = request.render_into(&mut || Ok(()), &mut |rows, bytes| {
            assert_eq!(rows[0], next);
            assert_eq!(bytes.len(), (rows[1] - rows[0]) as usize * 16000 * 4);
            assert!(bytes.iter().all(|value| *value == 0));
            assert!(pool.used() - baseline < 8 * 1024 * 1024);
            next = rows[1];
            count += 1;
            if count == 3 { Err("Stopped full-detail stream".into()) } else { Ok(()) }
        });
        assert!(result.is_err());
        assert_eq!(count, 3);
        assert!(next < 16000);
        assert_eq!(pool.used(), baseline);
        assert_eq!(serde_json::to_value(document.metadata()).unwrap(), metadata);
    }
}

#[test]
fn actual_streamed_requests_match_frame_uploads_and_release_source_owners() {
    use crate::{
        engine::{native::{worker, NativeDocument}, scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot}, Document},
        render::native_preview::Request,
    };
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let pool = runtime.resident();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let mut doc = Document::new_depth(257, 1025, depth).unwrap();
        doc.layers[0].kind = "fill".into();
        doc.layers[0].color = [43, 71, 139, 199];
        doc.selection = Some([2, 3, 254, 1021]);
        let staged = CapturedScene::from_parts(doc, "stream-test".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let document = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let metadata = serde_json::to_value(document.metadata()).unwrap();
        let retirement = retirement::small(&pool, 256).unwrap();
        for mode in [None, Some("mask"), Some("overlay"), Some("cutout")] {
            let scene = Arc::new(SceneSnapshot::from_native(
                SceneIdentity { project_id: "stream-test".into(), document_id: document.metadata().id().into(), revision: document.metadata().revision() },
                document.clone(), vec![], None, None,
            ).unwrap());
            let weak_scene = Arc::downgrade(&scene);
            let session = Arc::new(runtime.begin().unwrap());
            let weak_session = Arc::downgrade(&session);
            let request = Request::capture_selection(scene, session, None, 8192, None, false, mode).unwrap();
            let reference = Prepared::from_frame(request.render(&mut || Ok(())).unwrap(), &retirement, 64, &mut || Ok(())).unwrap();
            let mut calls = 0;
            let candidate = Prepared::from_request(&request, &retirement, 64, None, None, &mut || { calls += 1; Ok(()) }).unwrap();
            assert!(candidate.tag().same_allocation(&reference.tag()));
            assert_eq!(candidate.status, reference.status);
            assert_eq!(candidate.tiles.pixels().uploads.len(), reference.tiles.pixels().uploads.len());
            for (actual, expected) in candidate.tiles.pixels().uploads.iter().zip(&reference.tiles.pixels().uploads) {
                assert_eq!(actual.area.inner, expected.area.inner);
                assert_eq!(actual.area.outer, expected.area.outer);
                assert_eq!(actual.pixels.image.pixels, expected.pixels.image.pixels);
            }
            if mode.is_none() {
                let ctx = egui::Context::default();
                let stop = std::sync::atomic::AtomicBool::new(false);
                let deadline = Instant::now() + Duration::from_secs(10);
                std::thread::scope(|scope| {
                    let pump = scope.spawn(|| {
                        let mut delivered = std::collections::HashMap::new();
                        while !stop.load(std::sync::atomic::Ordering::Acquire) && Instant::now() < deadline {
                            let delta = ctx.tex_manager().write().take_delta();
                            for (id, image) in &delta.set {
                                if let egui::ImageData::Color(image) = &image.image {
                                    delivered.insert(*id, image.pixels.clone());
                                }
                            }
                            drop(delta);
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        delivered
                    });
                    let mut check = || {
                        if Instant::now() < deadline { Ok(()) } else { Err("Test renderer stalled".into()) }
                    };
                    let uploaded = if depth == 8 {
                        Prepared::from_request(&request, &retirement, 64, Some(&ctx), None, &mut check)
                    } else {
                        Prepared::from_one_shot_request(&request, &retirement, 64, &ctx, &mut check)
                    }.unwrap();
                    stop.store(true, std::sync::atomic::Ordering::Release);
                    let delivered = pump.join().unwrap();
                    let upload::Candidate::Uploaded { textures, context } = uploaded.tiles else { panic!("Missing uploaded candidate"); };
                    for (tile, expected) in textures.tiles.iter().zip(&reference.tiles.pixels().uploads) {
                        assert_eq!(tile.digest.is_some(), depth == 8);
                        assert_eq!(delivered.get(&tile.texture.id()).unwrap(), &expected.pixels.image.pixels);
                    }
                    let foreign = egui::Context::default();
                    assert!(upload::Candidate::Uploaded { textures, context }.publish(&foreign, &mut || Ok(())).is_err());
                });
            }
            drop(reference);
            let baseline = pool.used();
            for stop in [calls / 2, calls - 1] {
                let mut count = 0;
                let result = Prepared::from_request(&request, &retirement, 64, None, None, &mut || {
                    count += 1;
                    if count == stop { Err("Streamed request cancelled".into()) } else { Ok(()) }
                });
                assert!(result.is_err());
                drop(result);
                assert_eq!(pool.used(), baseline);
            }
            drop(request);
            assert!(weak_scene.upgrade().is_none());
            assert!(weak_session.upgrade().is_none());
            assert_eq!(candidate.tag().key().source, document.key());
            assert_eq!(serde_json::to_value(document.metadata()).unwrap(), metadata);
        }
    }
}

#[test]
fn actual_native_frames_release_consumed_stripes_under_upload_pressure() {
    use crate::compositor::native::tests::{capture_kind_with_budget, cleanup};
    for depth in [8, 16] {
        let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
        let mut doc = crate::engine::Document::new_depth(257, 1025, depth).unwrap();
        for y in 0..1025 {
            for x in 0..257 {
                doc.layers[0].pixels.set16(x, y, [
                    (x * 173 + 12347) as u16, (y * 31 + 23459) as u16, 34571, 51713,
                ]);
            }
        }
        let (scene, path) = capture_kind_with_budget(&doc, true, pool.clone());
        let frame = crate::render::native_preview::test_frame(&scene, None, 1025, None, false, &mut || Ok(())).unwrap();
        assert!(frame.stripes().len() > 1);
        let expected = frame.copy_rgba8(&mut || Ok(())).unwrap().to_vec();
        let retirement = retirement::small(&pool, 128).unwrap();
        let retained = pool.used();
        let cancelled = crate::render::native_preview::test_frame(&scene, None, 1025, None, false, &mut || Ok(())).unwrap();
        let mut previous = pool.used();
        let mut released = false;
        let result = Prepared::from_frame(cancelled, &retirement, 64, &mut || {
            let current = pool.used();
            if current < previous {
                released = true;
                return Err("Cancelled after stripe retirement".into());
            }
            previous = current;
            Ok(())
        });
        assert!(released && result.is_err());
        drop(result);
        assert_eq!(pool.used(), retained);
        // Less than one duplicate frame can fit. Retiring whole source stripes
        // between tile bands leaves room for every admitted texture image.
        assert!(expected.len() > 800 * 1024);
        let pressure = pool.reserve_bytes(pool.limit() - pool.used() - 800 * 1024, pool.limit()).unwrap();
        let prepared = Prepared::from_frame(frame, &retirement, 64, &mut || Ok(())).unwrap();
        for upload in &prepared.tiles.pixels().uploads {
            let area = upload.area;
            for y in area.outer[1]..area.outer[3] {
                for x in area.outer[0]..area.outer[2] {
                    let at = (y as usize * 257 + x as usize) * 4;
                    let p = &expected[at..at + 4];
                    let offset = (y - area.outer[1]) as usize * upload.pixels.image.size[0]
                        + (x - area.outer[0]) as usize;
                    assert_eq!(upload.pixels.image.pixels[offset], egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]));
                }
            }
        }
        drop(prepared);
        drop(pressure);
        drop(retirement);
        drop(scene);
        wait_empty(&pool);
        cleanup(&path);
    }
}

#[test]
fn actual_delta_clones_hold_pixel_charge_after_handle_presenter_and_context_teardown() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 8).unwrap();
    let baseline = pool.used();
    let ctx = egui::Context::default();
    let texture = image(&retirement, [3, 2], 43).upload(&ctx);
    let delta = ctx.tex_manager().write().take_delta();
    assert!(!delta.set.is_empty());
    let retained_renderer = delta.clone();
    drop(texture);
    drop(ctx);
    drop(delta);
    retirement.collect();
    assert!(
        pool.used() > baseline,
        "Renderer-held ColorImage must retain its exact charge"
    );
    // A presenter may disappear before deferred viewport/renderer deltas.
    drop(retirement);
    assert!(pool.used() > 0);
    drop(retained_renderer);
    wait_empty(&pool);
}
#[test]
fn small_budget_upload_waits_for_a_paused_renderer_before_exhausting_admission() {
    use std::sync::{atomic::{AtomicBool, Ordering}, mpsc};
    let pool = ResidentBudget::new(2 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 32).unwrap();
    let ctx = egui::Context::default();
    let layout = tiles::Layout::new(1024, 512, 512).unwrap();
    // Retain the actual index allowance while the batch owns its handles.
    let index = tiles::Tiles::allocate(&layout, &retirement).unwrap();
    assert!(layout.retained_upload_bytes().unwrap() > pool.limit());
    let probe = AtomicBool::new(false);
    let (waiting, observed) = mpsc::channel();
    let deadline = Instant::now() + Duration::from_secs(5);
    std::thread::scope(|scope| {
        let producer = scope.spawn(|| -> Result<Vec<tiles::TextureTile>> {
            let mut batch = upload::Batch::new(&ctx, layout.count, &pool)?;
            let mut notified = false;
            let mut check = || {
                if probe.load(Ordering::Acquire) && !notified {
                    waiting.send(()).unwrap();
                    notified = true;
                }
                if Instant::now() < deadline { Ok(()) } else { Err("Renderer wait timed out".into()) }
            };
            for area in layout.areas() {
                let size = [(area.outer[2] - area.outer[0]) as usize, (area.outer[3] - area.outer[1]) as usize];
                let pixels = tiles::OwnedImage::convert(size, &retirement, |_| Ok([31, 73, 149, 199]))?;
                batch.push(tiles::Upload { area, pixels }, None, false, &mut check)?;
            }
            batch.finish(&mut check)
        });
        let held = loop {
            let delta = ctx.tex_manager().write().take_delta();
            if !delta.set.is_empty() { break delta; }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(held.set.len(), 1);
        let retained = pool.used();
        probe.store(true, Ordering::Release);
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!producer.is_finished(), "Renderer-held pixels must hold back the producer");
        assert_eq!(pool.used(), retained);
        drop(held);
        while !producer.is_finished() {
            drop(ctx.tex_manager().write().take_delta());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let handles = producer.join().unwrap().unwrap();
        assert_eq!(handles.len(), layout.count);
        drop(handles);
    });
    drop((index, ctx, retirement));
    wait_empty(&pool);
}

#[test]
fn wide_native_streams_upload_each_tile_before_the_band_exhausts_admission() {
    use crate::{
        engine::{native::{worker, NativeDocument}, scene::{SceneIdentity, SceneSnapshot}},
        render::native_preview::Request,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    for depth in [8, 16] {
        for interrupt in [false, true] {
            let fixture = worker::tests::Fixture::new();
            let (runtime, _connection) = fixture.runtime_with_budgets(8 * 1024 * 1024, 8 * 1024 * 1024);
            let pool = runtime.resident();
            let document = NativeDocument::new_blank(8192, 257, depth, &pool, &runtime.disk(), &|| Ok(())).unwrap();
            let metadata = serde_json::to_value(document.metadata()).unwrap();
            let scene = Arc::new(SceneSnapshot::from_native(SceneIdentity {
                project_id: "wide-stream".into(), document_id: document.metadata().id().into(),
                revision: document.metadata().revision(),
            }, document.clone(), vec![], None, None).unwrap());
            let read = runtime.begin_read(&scene).unwrap();
            let request = Request::capture(scene, read, None, 8192, None, false).unwrap().for_canvas(8192).unwrap();
            let retirement = retirement::small(&pool, 256).unwrap();
            let layout = tiles::Layout::streamed(8192, 257, 128).unwrap();
            let band_images = layout.areas().take_while(|area| area.inner[1] == 0).map(|area| {
                retirement::image_bytes((area.outer[2] - area.outer[0]) as usize
                    * (area.outer[3] - area.outer[1]) as usize).unwrap()
            }).sum::<usize>();
            // Eight engine stripes are needed to cover the first tile band.
            // Holding that band and all its converted images cannot fit.
            assert!(128 * 8192 * 4 + band_images > pool.limit());
            let baseline = pool.used();
            let ctx = egui::Context::default();
            let cancelled = AtomicBool::new(false);
            let deadline = Instant::now() + Duration::from_secs(30);
            std::thread::scope(|scope| {
                let producer = scope.spawn(|| Prepared::from_request(&request, &retirement, 128, Some(&ctx), None, &mut || {
                    if cancelled.load(Ordering::Acquire) { Err("Wide stream cancelled".into()) }
                    else if Instant::now() >= deadline { Err("Wide stream renderer timed out".into()) }
                    else { Ok(()) }
                }));
                let mut received = 0;
                let mut held = None;
                while !producer.is_finished() {
                    let delta = ctx.tex_manager().write().take_delta();
                    for (_, image) in &delta.set {
                        let egui::ImageData::Color(image) = &image.image else { panic!("Unexpected font image"); };
                        assert!(image.pixels.iter().all(|pixel| *pixel == egui::Color32::TRANSPARENT));
                        received += 1;
                    }
                    if interrupt && held.is_none() && !delta.set.is_empty() {
                        held = Some(delta);
                        cancelled.store(true, Ordering::Release);
                    }
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                let result = producer.join().unwrap();
                if interrupt {
                    assert!(received > 0, "Cancellation must follow a real upload");
                    assert!(result.err().unwrap().contains("Wide stream cancelled"));
                    let retained = held.as_ref().unwrap().set.iter().map(|(_, image)| {
                        let egui::ImageData::Color(image) = &image.image else { unreachable!() };
                        retirement::image_bytes(image.pixels.len()).unwrap()
                    }).sum::<usize>();
                    assert!(pool.used() >= baseline + retained, "Renderer-held images remain charged after cancellation");
                } else {
                    let prepared = result.unwrap();
                    assert!(prepared.tag().same_allocation(&request.tag()));
                    let upload::Candidate::Uploaded { textures, .. } = prepared.tiles else { panic!("Missing uploaded candidate"); };
                    assert_eq!(textures.tiles.len(), layout.count);
                    let mut blank_textures = std::collections::BTreeMap::new();
                    for tile in &textures.tiles {
                        assert!(tile.transparent);
                        let size = tile.area.texture_size();
                        let id = tile.texture.id();
                        assert_eq!(*blank_textures.entry(size).or_insert(id), id);
                    }
                    assert_eq!(received, blank_textures.len());
                    assert!(received <= 9 && received < layout.count);
                    drop(textures);
                }
                drop(held);
            });
            drop(ctx.tex_manager().write().take_delta());
            retirement.collect();
            assert_eq!(pool.used(), baseline);
            assert_eq!(serde_json::to_value(document.metadata()).unwrap(), metadata);
        }
    }
}

#[test]
fn streamed_canvas_reuses_only_tiles_with_unchanged_pixels_and_filtering_borders() {
    use crate::{
        engine::{
            native::{worker, NativeDocument},
            scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
            Document,
        },
        render::native_preview::Request,
    };

    let fixture = worker::tests::Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let pool = runtime.resident();
    let cache = LegacyTileCache::new(&pool, 8).unwrap();
    let retirement = retirement::small(&pool, 8).unwrap();
    let make_scene = |doc: Document| {
        let staged = CapturedScene::from_parts(doc, "texture-reuse".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
        let document =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        Arc::new(SceneSnapshot::from_native(
            SceneIdentity {
                project_id: "texture-reuse".into(),
                document_id: document.metadata().id().into(),
                revision: document.metadata().revision(),
            },
            document,
            vec![],
            None,
            None,
        ).unwrap())
    };
    let make_request = |scene: &Arc<SceneSnapshot>, rect| {
        let edge = scene.metadata().width().max(scene.metadata().height());
        let read = runtime.begin_read(scene).unwrap();
        Request::capture(scene.clone(), read, rect, edge, None, false).unwrap()
    };
    let upload = |request: &Request, ctx: &egui::Context, reuse: Option<&Reuse>, materialized| {
        let retirement = &retirement;
        std::thread::scope(|scope| {
            let deadline = Instant::now() + Duration::from_secs(5);
            let worker = scope.spawn(move || {
                let mut check = || {
                    if Instant::now() < deadline { Ok(()) } else { Err("Test upload stalled".into()) }
                };
                if materialized {
                    let frame = request.render(&mut check)?;
                    Prepared::from_frame_with_display(frame, retirement, 64, Some((ctx, reuse)), &mut check)
                } else {
                    Prepared::from_canvas_request_reusing_tiles(request, retirement, 64, Some(ctx), reuse, &mut check)
                }
            });
            let mut count = 0;
            while !worker.is_finished() {
                count += ctx.tex_manager().write().take_delta().set.len();
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            (worker.join().unwrap().unwrap(), count)
        })
    };
    let ctx = egui::Context::default();
    let mut doc = Document::new_depth(130, 8, 16).unwrap();
    let (baseline, count) = upload(&make_request(&make_scene(doc.clone()), None), &ctx, None, false);
    assert_eq!(count, 3);
    let mut presenter = Presenter::default();
    let tag = baseline.tag().clone();
    presenter.present(baseline, &tag, &ctx, &mut || Ok(())).unwrap();
    let prior = presenter.current().unwrap();
    let ids: Vec<_> = prior.texture_ids().collect();
    let reuse = prior.reuse();

    doc.layers[0].pixels.set16(62, 3, [12347, 23459, 34571, 65535]);
    let changed = make_request(&make_scene(doc), None);
    assert!(changed.texture_reuse(&reuse.tag).unwrap().is_none());
    let (candidate, count) = upload(&changed, &ctx, Some(&reuse), false);
    assert_eq!(count, 2);
    assert_eq!(presenter.current().unwrap().texture_ids().collect::<Vec<_>>(), ids);
    let tag = candidate.tag().clone();
    presenter.present(candidate, &tag, &ctx, &mut || Ok(())).unwrap();
    let current: Vec<_> = presenter.current().unwrap().texture_ids().collect();
    // Pixel 62 is inside the second tile and is the first tile's right halo.
    assert_ne!(current[0], ids[0]);
    assert_ne!(current[1], ids[1]);
    assert_eq!(current[2], ids[2]);
    let foreign = egui::Context::default();
    let (_, count) = upload(&changed, &foreign, Some(&presenter.current().unwrap().reuse()), false);
    assert_eq!(count, 3);

    for depth in [8, 16] {
        let mut doc = Document::new_depth(254, 8, depth).unwrap();
        for y in 0..8 {
            for x in 0..254 {
                doc.layers[0].pixels.set16(x, y, [x as u16 * 257, y as u16 * 257, 23459, 65535]);
            }
        }
        let scene = make_scene(doc);
        let (baseline, count) = upload(&make_request(&scene, None), &ctx, None, true);
        assert_eq!(count, 5);
        let tag = baseline.tag().clone();
        presenter.present(baseline, &tag, &ctx, &mut || Ok(())).unwrap();
        let previous = presenter.current().unwrap();
        let ids: Vec<_> = previous.texture_ids().collect();

        // The crop shifts the grid by one cell. The first tile loses its left
        // halo and needs a new upload. The three remaining tiles are identical.
        let cropped = make_request(&scene, Some([62, 0, 254, 8]));
        let pan = cropped.texture_reuse(previous.tag()).unwrap().unwrap();
        let stream = stream::Stream::for_upload(192, 8, 64, &retirement, Some(&previous.reuse.textures), true).unwrap();
        let positions = stream.position_reuse(previous.key().rect, cropped.key().rect, pan.damage()).unwrap();
        let reference = cropped.render(&mut || Ok(())).unwrap().copy_rgba8(&mut || Ok(())).unwrap();
        let complete = pan.render_into(&positions, &mut || Ok(()), &mut |rows, bytes| {
            let bytes = bytes.expect("Every stripe intersects the new crop edge");
            for y in rows[0]..rows[1] {
                assert_eq!(bytes.columns(y - rows[0], 192, 0..63)?,
                    &reference[y as usize * 192 * 4..(y as usize * 192 + 63) * 4]);
                assert!(bytes.columns(y - rows[0], 192, 63..64).is_err());
            }
            Ok(())
        }).unwrap();
        assert!(complete.tag.same_allocation(&cropped.tag()));
        drop((positions, stream));
        let (candidate, count) = upload(&cropped, &ctx, Some(&previous.reuse()), false);
        assert_eq!(count, 1);
        let tag = candidate.tag().clone();
        presenter.present(candidate, &tag, &ctx, &mut || Ok(())).unwrap();
        let canvas = presenter.current().unwrap();
        let current: Vec<_> = canvas.texture_ids().collect();
        assert_eq!(canvas.key().rect, [62, 0, 254, 8]);
        assert!(!ids.contains(&current[0]));
        assert_eq!(current[1..], ids[2..]);

        let document = scene.native_document().unwrap();
        let mut authority = crate::engine::authority::DocumentAuthority::from_native(document.clone(), 4).unwrap();
        let layer = document.metadata().layer(0).unwrap().id();
        let disk = runtime.disk().used().unwrap();
        let mut prior_ids = current;
        for (x, damage) in [(124, [124, 3, 125, 4]), (126, [124, 3, 127, 4])] {
            let prior = presenter.current().unwrap().reuse();
            let rect = [x, 3, x + 1, 4];
            let command = serde_json::json!({"op":"paint.fill","layer":layer,"rect":rect,"color":[211,77,9,255]});
            let session = Arc::new(runtime.begin().unwrap());
            let prepared = worker::prepare(worker::tests::capture(&authority, &[command]), &session, &|| Ok(())).unwrap();
            assert_eq!(prepared.raster_damage().map(|damage| damage.bounds), Some(rect));
            let frame = crate::render::native_preview::render_prepared_with(
                scene.clone(), prepared, session, Some([62, 0, 254, 8]), 254, None, false, None,
                &|| Ok(()), true, |request, check| {
                    let proof = request.texture_reuse(&prior.tag)?.unwrap();
                    assert_eq!(proof.damage(), Some(damage));
                    let stream = stream::Stream::for_upload(192, 8, 64, &retirement, Some(&prior.textures), true)?;
                    let positions = stream.position_reuse(prior.tag.key().rect, request.key().rect, proof.damage()).unwrap();
                    let reference = request.render(&mut || check())?.copy_rgba8(&mut || check())?;
                    proof.render_into(&positions, &mut || check(), &mut |rows, bytes| {
                        let bytes = bytes.expect("The damaged texture spans the full height");
                        for y in rows[0]..rows[1] {
                            let end = 125 * 4;
                            let start = y as usize * 192 * 4;
                            assert_eq!(bytes.columns(y - rows[0], 192, 0..125)?, &reference[start..start + end]);
                            assert!(bytes.columns(y - rows[0], 192, 125..126).is_err());
                        }
                        Ok(())
                    })?;
                    let (candidate, count) = upload(request, &ctx, Some(&prior), false);
                    assert_eq!(count, 2);
                    Ok(candidate)
                },
            ).unwrap();
            let (_, candidate, _) = frame.into_parts();
            let tag = candidate.tag().clone();
            presenter.present(candidate, &tag, &ctx, &mut || Ok(())).unwrap();
            let next: Vec<_> = presenter.current().unwrap().texture_ids().collect();
            assert_ne!(next[0], prior_ids[0], "the preceding stroke may have changed the filtering border");
            assert_ne!(next[1], prior_ids[1]);
            assert_eq!(next[2..], prior_ids[2..]);
            prior_ids = next;
        }
        let mixed = [
            serde_json::json!({"op":"paint.fill","layer":layer,"rect":[124,3,125,4],"color":[211,77,9,255]}),
            serde_json::json!({"op":"layer.update","layer":layer,"opacity":0.5}),
        ];
        let session = runtime.begin().unwrap();
        let prepared = worker::prepare(worker::tests::capture(&authority, &mixed), &session, &|| Ok(())).unwrap();
        assert!(prepared.raster_damage().is_none());
        prepared.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(authority.history_lengths(), [0, 0]);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        drop(session);

        let command = serde_json::json!({"op":"paint.fill","layer":layer,"rect":[126,3,127,4],"color":[211,77,9,255]});
        let session = runtime.begin().unwrap();
        let prepared = worker::prepare(worker::tests::capture(&authority, &[command]), &session, &|| Ok(())).unwrap();
        let pending = prepared.prepare_commit(crate::engine::native::HistoryRequest {
            actor: "human", label: "Paint", task: None, gesture: None,
        }, &|| Ok(())).ok().unwrap();
        let gate = || crate::engine::native::Publication {
            actor: "human", leases: &[], now: crate::engine::now(), closed: false, write_available: true,
        };
        assert!(authority.native_raster_damage().is_none());
        let retired = session.with_live_fence(|| authority.commit_native(gate(), pending)).unwrap().unwrap();
        retired.retire(&session, &|| session.cleanup_check()).unwrap();
        drop(session);
        let damage = authority.native_raster_damage().unwrap();
        assert_eq!(damage.base, document.key());
        assert_eq!(damage.candidate, authority.native().unwrap().key());
        assert_eq!(damage.bounds, [126, 3, 127, 4]);
        assert_eq!(authority.history_lengths(), [1, 0]);
        let committed = Arc::new(authority.capture_scene(None, "texture-reuse".into(), vec![], None, None)
            .unwrap().into_native().unwrap());
        let request = make_request(&committed, Some([62, 0, 254, 8])).with_raster_damage(Some(damage)).unwrap();
        let prior = presenter.current().unwrap().reuse();
        assert_eq!(request.texture_reuse(&prior.tag).unwrap().unwrap().damage(), Some(damage.bounds));
        let (candidate, count) = upload(&request, &ctx, Some(&prior), false);
        assert_eq!(count, 0, "the committed result matches all displayed tiles");
        let tag = candidate.tag().clone();
        presenter.present(candidate, &tag, &ctx, &mut || Ok(())).unwrap();
        assert_eq!(presenter.current().unwrap().texture_ids().collect::<Vec<_>>(), prior_ids);
        drop((request, committed));
        let undo = authority.native_history().unwrap().prepare_move(
            authority.native().unwrap(), crate::engine::native::Direction::Undo, "human", &|| Ok(()),
        ).unwrap();
        drop(authority.move_native(gate(), undo).unwrap());
        assert!(authority.native_raster_damage().is_none());
        assert_eq!(authority.history_lengths(), [0, 1]);

        let mut tall = Document::new_depth(64, 2300, depth).unwrap();
        tall.layers[0].pixels.set16(15, 66, [65535, 13107, 257, 65535]);
        tall.layers[0].pixels.set16(15, 2231, [257, 13107, 65535, 65535]);
        let tall = make_scene(tall);
        let (candidate, _) = upload(&make_request(&tall, None), &ctx, None, false);
        let tag = candidate.tag().clone();
        presenter.present(candidate, &tag, &ctx, &mut || Ok(())).unwrap();
        let prior = presenter.current().unwrap().reuse();
        let request = make_request(&tall, Some([0, 62, 64, 2300]));
        let proof = request.texture_reuse(&prior.tag).unwrap().unwrap();
        let mut stream = stream::Stream::for_upload(64, 2238, 64, &retirement, Some(&prior.textures), true).unwrap();
        let positions = stream.position_reuse(prior.tag.key().rect, request.key().rect, proof.damage()).unwrap();
        let mut rendered = Vec::new();
        proof.render_into(&positions, &mut || Ok(()), &mut |rows, pixels| {
            if let Some(pixels) = pixels {
                rendered.push(rows);
                assert_eq!(pixels.columns(4, 64, 15..16)?, &[255, 51, 1, 255]);
            }
            Ok(())
        }).unwrap();
        assert_eq!(rendered, [[0, 63]], "The first tile needs its bottom filtering row");
        let mut batch = upload::Batch::new(&ctx, stream.tile_count(), &pool).unwrap();
        assert!(stream.push([0, 64], None, Some(&mut batch), Some(&positions), &mut || Ok(())).is_err());
        drop((batch, positions, stream));

        let bottom = make_request(&tall, Some([0, 0, 64, 2238]));
        let proof = bottom.texture_reuse(&prior.tag).unwrap().unwrap();
        let stream = stream::Stream::for_upload(64, 2238, 64, &retirement, Some(&prior.textures), true).unwrap();
        let positions = stream.position_reuse(prior.tag.key().rect, bottom.key().rect, proof.damage()).unwrap();
        rendered.clear();
        proof.render_into(&positions, &mut || Ok(()), &mut |rows, pixels| {
            if let Some(pixels) = pixels {
                rendered.push(rows);
                assert_eq!(pixels.columns(0, 64, 15..16)?, &[1, 51, 255, 255]);
            }
            Ok(())
        }).unwrap();
        assert_eq!(rendered, [[2231, 2238]], "The last tile needs its top filtering row");
        drop((positions, stream, bottom));

        let (candidate, count) = upload(&request, &ctx, Some(&prior), false);
        assert_eq!(count, 1);
        let tag = candidate.tag().clone();
        presenter.present(candidate, &tag, &ctx, &mut || Ok(())).unwrap();
        let updated = presenter.current().unwrap();
        assert!(updated.texture_ids().skip(1).eq(prior.textures.ids().skip(2)));
    }
}

#[test]
fn upload_fence_waits_for_actual_image_retirement_and_can_be_cancelled() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 8).unwrap();
    let baseline = pool.used();
    let ctx = egui::Context::default();
    let (handle, fence) = image(&retirement, [23, 17], 71).upload_tracked(&ctx);
    let delta = ctx.tex_manager().write().take_delta();
    let renderer_copy = delta.clone();
    drop(delta);
    let retained = pool.used();
    assert!(retained > baseline);
    assert!(fence.wait(&mut || Err("Cancelled upload wait".into())).is_err());
    assert_eq!(pool.used(), retained);
    let mut observed = false;
    assert!(fence.wait(&mut || {
        observed = true;
        Err("Renderer still retains its delta".into())
    }).is_err());
    assert!(observed);
    drop(renderer_copy);
    fence.wait(&mut || Ok(())).unwrap();
    assert_eq!(pool.used(), baseline);
    drop((fence, handle, ctx, retirement));
    wait_empty(&pool);
}
#[test]
fn retained_owner_waits_for_weak_upgrade_and_seals_future_resurrection_before_refund() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 8).unwrap();
    let baseline = pool.used();
    let ctx = egui::Context::default();
    let candidate = image(&retirement, [3, 2], 43);
    let weak = Arc::downgrade(&candidate.image);
    let texture = candidate.upload(&ctx);
    let upgraded = weak.upgrade().unwrap();
    drop(texture);
    drop(ctx);
    retirement.collect();
    assert!(pool.used() > baseline);
    drop(upgraded);
    retirement.collect();
    assert_eq!(pool.used(), baseline);
    assert!(weak.upgrade().is_none());
    drop(weak);
    drop(retirement);
    wait_empty(&pool);
}
#[test]
fn new_admission_retires_released_images_before_reserving_replacement_pixels() {
    let pool = ResidentBudget::new(1024 * 1024).unwrap();
    let (retirement, release) = retirement::paused(&pool).unwrap();
    let ctx = egui::Context::default();
    let texture = image(&retirement, [3, 2], 43).upload(&ctx);
    let delta = ctx.tex_manager().write().take_delta();
    let pressure = pool
        .reserve_bytes(pool.limit() - pool.used() - 100, pool.limit())
        .unwrap();
    drop(texture);
    drop(ctx);
    drop(delta);
    // The background reaper is gated, and132 bytes cannot fit until the old
    // actual image is synchronously destroyed by the admission path itself.
    let replacement = image(&retirement, [1, 1], 97);
    drop(replacement);
    drop(pressure);
    release.send(()).unwrap();
    drop(retirement);
    wait_empty(&pool);
}
#[test]
fn whole_overwrite_and_partial_delta_owners_retire_independently() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 8).unwrap();
    let baseline = pool.used();
    let ctx = egui::Context::default();
    let mut texture = image(&retirement, [4, 3], 11).upload(&ctx);
    let first = ctx.tex_manager().write().take_delta();
    let old_charge = pool.used() - baseline;
    let tiles::OwnedImage {
        image: replacement,
        ticket,
    } = image(&retirement, [4, 3], 97);
    ticket.track(&replacement);
    texture.set(
        egui::ImageData::Color(replacement),
        egui::TextureOptions::LINEAR,
    );
    let tiles::OwnedImage {
        image: partial,
        ticket,
    } = image(&retirement, [1, 1], 149);
    ticket.track(&partial);
    texture.set_partial(
        [1, 1],
        egui::ImageData::Color(partial),
        egui::TextureOptions::LINEAR,
    );
    let second = ctx.tex_manager().write().take_delta();
    assert_eq!(second.set.len(), 2);
    let renderer_color = second.set[1].1.image.clone();
    drop(texture);
    drop(ctx);
    drop(first);
    retirement.collect();
    assert!(pool.used() > baseline);
    assert_eq!(pool.used() - baseline, old_charge + 4 + 128);
    drop(second);
    retirement.collect();
    assert_eq!(pool.used() - baseline, 4 + 128);
    drop(renderer_color);
    retirement.collect();
    assert_eq!(pool.used(), baseline);
    drop(retirement);
    wait_empty(&pool);
}
#[test]
fn retirement_start_slots_and_pixel_admission_refuse_before_unowned_allocations() {
    let pool = ResidentBudget::new(1024 * 1024).unwrap();
    assert!(retirement::refused_start(&pool).is_err());
    assert_eq!(pool.used(), 0);
    let retirement = retirement::small(&pool, 2).unwrap();
    let baseline = pool.used();
    let a = image(&retirement, [1, 1], 1);
    let b = image(&retirement, [1, 1], 2);
    let before = pool.used();
    let mut called = false;
    assert!(tiles::OwnedImage::convert([1, 1], &retirement, |_| {
        called = true;
        Ok([0; 4])
    })
    .is_err());
    assert!(!called);
    assert_eq!(pool.used(), before);
    drop(a);
    drop(b);
    assert_eq!(pool.used(), baseline);
    assert!(
        tiles::OwnedImage::convert([MAX_PIXELS + 1, 1], &retirement, |_| panic!(
            "Pixel conversion before admission"
        ))
        .is_err()
    );
    let pressure = pool
        .reserve_bytes(pool.limit() - pool.used() - 100, pool.limit())
        .unwrap();
    assert!(tiles::OwnedImage::convert([1, 1], &retirement, |_| panic!(
        "Pixel conversion before admission"
    ))
    .is_err());
    drop(pressure);
    drop(retirement);
    wait_empty(&pool);
}
struct Bytes {
    width: u32,
    pixels: Vec<u8>,
}
#[test]
fn full_size_stream_admits_only_a_texture_band_of_stripe_slots() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 64).unwrap();
    let before = pool.used();
    let layout = tiles::Layout::streamed(16_000, 16_000, 8192).unwrap();
    let tiles = tiles::Tiles::allocate(&layout, &retirement).unwrap();
    let tile_index = pool.used() - before;
    drop(tiles);
    let stream = stream::Stream::for_upload(16_000, 16_000, 8192, &retirement, None, false).unwrap();
    let rows = layout.areas().map(|area| area.outer[3] - area.outer[1]).max().unwrap();
    let stripe_slot_bytes = std::mem::size_of::<([u32; 2], StripePixels)>();
    assert_eq!(rows, 16);
    assert_eq!(pool.used() - before - tile_index, 128 + rows as usize * stripe_slot_bytes);
    drop(stream);
    assert_eq!(pool.used(), before);
}

#[test]
fn streamed_tiles_keep_halos_across_stripe_boundaries_and_reject_incomplete_candidates() {
    use crate::large_document::ScratchBytes;
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 128).unwrap();
    let mut input = Bytes::new(23, 17);
    let reference = tiles::Tiles::prepare(23, 17, 9, &mut input, &retirement, &mut || Ok(())).unwrap();
    let baseline = pool.used();
    let stripe = |rows: [u32; 2]| {
        let start = rows[0] as usize * 23 * 4;
        let end = rows[1] as usize * 23 * 4;
        let mut bytes = ScratchBytes::new(&pool, end - start, 4096).unwrap();
        bytes.copy_from_slice(&input.pixels[start..end]);
        StripePixels::Full(bytes.freeze())
    };
    for step in [1, 3, 11, 17] {
        let mut stream = stream::Stream::new(23, 17, 9, &retirement).unwrap();
        for top in (0..17).step_by(step) {
            let rows = [top, (top + step as u32).min(17)];
            stream.push(rows, Some(stripe(rows)), None, None, &mut || Ok(())).unwrap();
        }
        let candidate = stream.finish(&mut || Ok(())).unwrap();
        assert_eq!(candidate.uploads.len(), reference.uploads.len());
        for (actual, expected) in candidate.uploads.iter().zip(&reference.uploads) {
            assert_eq!(actual.area.inner, expected.area.inner);
            assert_eq!(actual.area.outer, expected.area.outer);
            assert_eq!(actual.pixels.image.size, expected.pixels.image.size);
            assert_eq!(actual.pixels.image.pixels, expected.pixels.image.pixels);
        }
        drop(candidate);
        assert_eq!(pool.used(), baseline);
    }
    for fail in 0..4 {
        let mut stream = stream::Stream::new(23, 17, 9, &retirement).unwrap();
        assert!(stream.push([0, 10], None, None, None, &mut || Ok(())).is_err());
        stream.push([0, 10], Some(stripe([0, 10])), None, None, &mut || Ok(())).unwrap();
        match fail {
            0 => assert!(stream.finish(&mut || Ok(())).is_err()),
            1 => assert!(stream.push([9, 17], Some(stripe([9, 17])), None, None, &mut || Ok(())).is_err()),
            2 => assert!(stream.push([10, 17], Some(stripe([11, 17])), None, None, &mut || Ok(())).is_err()),
            _ => assert!(stream.push([10, 17], Some(stripe([10, 17])), None, None, &mut || Err("Cancelled".into())).is_err()),
        }
        // The consumed case has already dropped; other failures retain only
        // private candidate owners until the loop iteration ends.
    }
    assert_eq!(pool.used(), baseline);
    let other = ResidentBudget::new(4096).unwrap();
    let mut stream = stream::Stream::new(23, 17, 9, &retirement).unwrap();
    let foreign = ScratchBytes::new(&other, 23 * 4, 4096).unwrap().freeze();
    assert!(stream.push([0, 1], Some(StripePixels::Full(foreign)), None, None, &mut || Ok(())).is_err());
    drop(stream);
    assert_eq!(pool.used(), baseline);
}
impl Bytes {
    fn new(w: u32, h: u32) -> Self {
        let mut pixels = vec![];
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&[
                    (x * 13 + y * 3) as u8,
                    (x * 7 + y * 17) as u8,
                    71,
                    if (x + y) % 5 == 0 { 0 } else { 199 },
                ]);
            }
        }
        Self { width: w, pixels }
    }
}
impl tiles::Rows for Bytes {
    fn row(&self, y: u32) -> Result<&[u8]> {
        let stride = self.width as usize * 4;
        self.pixels
            .get(y as usize * stride..(y as usize + 1) * stride)
            .ok_or_else(|| "Invalid test row".into())
    }
}
fn linear(pixels: &[egui::Color32], size: [usize; 2], x: f64, y: f64) -> [f64; 4] {
    let x = x - 0.5;
    let y = y - 0.5;
    let left = x.floor();
    let top = y.floor();
    let tx = x - left;
    let ty = y - top;
    let mut result = [0.; 4];
    for (dy, wy) in [(0., 1. - ty), (1., ty)] {
        for (dx, wx) in [(0., 1. - tx), (1., tx)] {
            let sx = (left + dx).clamp(0., size[0] as f64 - 1.) as usize;
            let sy = (top + dy).clamp(0., size[1] as f64 - 1.) as usize;
            let value = pixels[sy * size[0] + sx].to_array();
            for c in 0..4 {
                result[c] += value[c] as f64 * wx * wy;
            }
        }
    }
    result
}
#[test]
fn bounded_halo_tiles_cover_full_grid_and_match_global_linear_samples_at_every_seam() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 64).unwrap();
    let mut bytes = Bytes::new(23, 17);
    let full: Vec<_> = bytes
        .pixels
        .chunks_exact(4)
        .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();
    let layout = tiles::Layout::new(23, 17, 9).unwrap();
    let mut covered = vec![0; 23 * 17];
    let before = pool.used();
    let expected_bytes = layout.retained_upload_bytes().unwrap();
    let tiles = tiles::Tiles::prepare(23, 17, 9, &mut bytes, &retirement, &mut || Ok(())).unwrap();
    assert_eq!(pool.used() - before, expected_bytes);
    assert_eq!(tiles.uploads.len(), layout.count);
    for upload in &tiles.uploads {
        let a = upload.area;
        let size = upload.pixels.image.size;
        assert!(size[0] <= 9 && size[1] <= 9 && size[0] * size[1] <= MAX_PIXELS);
        for y in a.inner[1]..a.inner[3] {
            for x in a.inner[0]..a.inner[2] {
                covered[(y * 23 + x) as usize] += 1;
            }
        }
        for y in [
            a.inner[1] as f64,
            a.inner[1] as f64 + 0.125,
            (a.inner[3] as f64 - 0.125).max(a.inner[1] as f64),
            a.inner[3] as f64,
        ] {
            for x in [
                a.inner[0] as f64,
                a.inner[0] as f64 + 0.125,
                (a.inner[2] as f64 - 0.125).max(a.inner[0] as f64),
                a.inner[2] as f64,
            ] {
                let actual = linear(
                    &upload.pixels.image.pixels,
                    size,
                    x - a.outer[0] as f64,
                    y - a.outer[1] as f64,
                );
                let expected = linear(&full, [23, 17], x, y);
                for c in 0..4 {
                    assert!(
                        (actual[c] - expected[c]).abs() < 1e-9,
                        "Halo changes global sample at {x},{y}"
                    );
                }
            }
        }
    }
    assert!(covered.iter().all(|n| *n == 1));
    drop(tiles);
    drop(retirement);
    wait_empty(&pool);
    for side in [8192, 4096, 256] {
        let layout = tiles::Layout::new(8192, 8192, side).unwrap();
        assert!(layout.count <= 4096);
        for a in layout.areas() {
            assert!(
                (a.outer[2] - a.outer[0]) as usize * (a.outer[3] - a.outer[1]) as usize
                    <= MAX_PIXELS
            );
        }
    }
    assert!(tiles::Layout::new(8192, 8192, 64).is_err());
    for (width, height) in [(16000, 1), (1, 16000), (16000, 4194), (4194, 16000)] {
        let layout = tiles::Layout::new(width, height, 4096).unwrap();
        let mut covered = 0u64;
        for area in layout.areas() {
            let [left, top, right, bottom] = area.inner;
            covered += u64::from(right - left) * u64::from(bottom - top);
            assert!((area.outer[2] - area.outer[0]) as usize
                * (area.outer[3] - area.outer[1]) as usize <= MAX_PIXELS);
        }
        assert_eq!(covered, u64::from(width) * u64::from(height));
    }
    assert!(tiles::Layout::new(16000, 4195, 4096).is_err());
    assert!(tiles::Layout::new(16000, 16000, 4096).is_err());
    for side in [256, 2048, 4096, 8192] {
        let layout = tiles::Layout::streamed(16000, 16000, side).unwrap();
        let mut covered = 0u64;
        for area in layout.areas() {
            let [left, top, right, bottom] = area.inner;
            covered += u64::from(right - left) * u64::from(bottom - top);
            let width = (area.outer[2] - area.outer[0]) as usize;
            let height = (area.outer[3] - area.outer[1]) as usize;
            assert!(width <= side && height <= side && width * height <= MAX_PIXELS);
        }
        assert_eq!(covered, 256_000_000);
        assert!(layout.count <= 4096);
    }
    assert!(tiles::Layout::streamed(16000, 16000, 128).is_err());
}
#[test]
fn actual_painter_meshes_share_transformed_edges_and_crop_each_neighbor_halo() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 64).unwrap();
    let mut bytes = Bytes::new(13, 11);
    let ctx = egui::Context::default();
    let textures = tiles::Tiles::prepare(13, 11, 8, &mut bytes, &retirement, &mut || Ok(()))
        .unwrap()
        .publish(&ctx, &mut || Ok(()))
        .unwrap();
    let project = |uv: [f32; 2]| {
        egui::pos2(
            73. - uv[0] * 199. - uv[1] * 37.,
            41. + uv[0] * 51. - uv[1] * 131.,
        )
    };
    let screen = egui::Rect::from_min_max(egui::pos2(-256., -256.), egui::pos2(256., 256.));
    let meshes_in = |clip| {
        let output = ctx.run(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new("native-mesh-test"),
            )).with_clip_rect(clip);
            textures.paint_with(&painter, project, egui::Color32::WHITE);
        });
        output.shapes.into_iter().filter_map(|shape| {
            if let egui::Shape::Mesh(mesh) = shape.shape { Some(mesh) } else { None }
        })
        .collect::<Vec<_>>()
    };
    let meshes = meshes_in(screen);
    assert_eq!(meshes.len(), textures.tiles.len());
    let clip = egui::Rect::from_min_max(egui::pos2(-34., 24.), egui::pos2(-24., 34.));
    let expected: Vec<_> = meshes.iter().filter(|mesh| clip.intersects(mesh.calc_bounds())).cloned().collect();
    assert!(expected.len() >= 2 && expected.len() < meshes.len());
    assert_eq!(meshes_in(clip), expected);
    assert!(meshes_in(egui::Rect::from_min_max(egui::pos2(180., 180.), egui::pos2(200., 200.))).is_empty());
    assert!(meshes_in(egui::Rect::from_min_max(egui::pos2(10., 10.), egui::pos2(10., 30.))).is_empty());
    let mut shared = std::collections::HashMap::new();
    for (tile, mesh) in textures.tiles.iter().zip(meshes) {
        assert_eq!(mesh.texture_id, tile.texture.id());
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices, [0, 1, 2, 0, 2, 3]);
        let a = tile.area;
        let points = [
            [a.inner[0], a.inner[1]],
            [a.inner[2], a.inner[1]],
            [a.inner[2], a.inner[3]],
            [a.inner[0], a.inner[3]],
        ];
        for ([x, y], vertex) in points.into_iter().zip(&mesh.vertices) {
            assert_eq!(vertex.pos, project([x as f32 / 13., y as f32 / 11.]));
            assert_eq!(
                vertex.uv,
                egui::pos2(
                    (x - a.outer[0]) as f32 / (a.outer[2] - a.outer[0]) as f32,
                    (y - a.outer[1]) as f32 / (a.outer[3] - a.outer[1]) as f32
                )
            );
            let bits = [vertex.pos.x.to_bits(), vertex.pos.y.to_bits()];
            if let Some(previous) = shared.insert([x, y], bits) {
                assert_eq!(previous, bits);
            }
        }
    }
    drop(textures);
    drop(ctx);
    retirement.collect();
    drop(retirement);
    wait_empty(&pool);
}
#[test]
fn conversion_cancel_and_post_upload_cancel_keep_prior_complete_texture_set() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 64).unwrap();
    let baseline = pool.used();
    let mut bytes = Bytes::new(13, 11);
    let ctx = egui::Context::default();
    let old = tiles::Tiles::prepare(13, 11, 8, &mut bytes, &retirement, &mut || Ok(()))
        .unwrap()
        .publish(&ctx, &mut || Ok(()))
        .unwrap();
    let ids: Vec<_> = old.tiles.iter().map(|t| t.texture.id()).collect();
    let before = pool.used();
    let mut checks = 0;
    assert!(
        tiles::Tiles::prepare(13, 11, 8, &mut bytes, &retirement, &mut || {
            checks += 1;
            if checks > 8 {
                Err("Cancelled upload".into())
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    assert_eq!(pool.used(), before);
    assert_eq!(
        ids,
        old.tiles.iter().map(|t| t.texture.id()).collect::<Vec<_>>()
    );
    let candidate = tiles::Tiles::prepare(13, 11, 8, &mut bytes, &retirement, &mut || Ok(())).unwrap();
    let mut checks = 0;
    assert!(candidate
        .publish(&ctx, &mut || {
            checks += 1;
            if checks == 3 {
                Err("Superseded after enqueue".into())
            } else {
                Ok(())
            }
        })
        .is_err());
    assert_eq!(
        ids,
        old.tiles.iter().map(|t| t.texture.id()).collect::<Vec<_>>()
    );
    drop(old);
    let delta = ctx.tex_manager().write().take_delta();
    drop(ctx);
    assert!(pool.used() > baseline);
    drop(delta);
    retirement.collect();
    assert_eq!(pool.used(), baseline);
    drop(retirement);
    wait_empty(&pool);
}
