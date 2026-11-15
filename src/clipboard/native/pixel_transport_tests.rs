use super::*;
use crate::clipboard::{ClipboardImage, ClipboardPng, Image, ImageIdentity, Provider};

#[derive(Default)]
struct Sink {
    writes: usize,
    fail: bool,
    display_readback: bool,
    png_readback: Option<ClipboardPng>,
    image: Option<Image>,
}
impl Provider for Sink {
    fn write_image(
        &mut self,
        image: &Image,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<ImageIdentity>, String> {
        check()?;
        self.writes += 1;
        if self.fail {
            return Err("OS image write refused".into());
        }
        self.image = Some(image.clone());
        Ok(Some(ImageIdentity::Generation(self.writes as u64)))
    }
    fn read_image(&mut self) -> Result<(ClipboardImage, Option<ImageIdentity>), String> {
        if let Some(png) = self.png_readback.take() {
            return Ok((ClipboardImage::Png(png), None));
        }
        assert!(self.display_readback, "Copy must not read the clipboard");
        let mut image = self.image.take().unwrap();
        image.samples16 = None;
        image.origin = None;
        Ok((ClipboardImage::Bitmap(image), Some(ImageIdentity::Generation(self.writes as u64))))
    }
    fn write_text(&mut self, _: &str) -> Result<(), String> {
        panic!("Pixel copies must not write markers")
    }
    fn read_text(&mut self) -> Result<String, String> {
        panic!("Pixel copies must not read markers")
    }
    fn sequence(&mut self) -> Option<u64> {
        Some(self.writes as u64)
    }
}

#[test]
fn external_png_paste_streams_beyond_bitmap_budget_and_refuses_invalid_sources() {
    use crate::engine::{
        native::worker::tests::Fixture as RuntimeFixture,
        scene::{CapturedScene, LegacyTileCache},
        Engine,
    };
    use std::io::Write;

    let (width, height) = (4096, 4097);
    assert!(crate::clipboard::native::transport_layout(width, height, 16).is_err());
    let pixel = [12347u16, 23459, 34571, 0];
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Sixteen);
        let mut writer = encoder.write_header().unwrap();
        let mut stream = writer.stream_writer().unwrap();
        let mut row = vec![0; width as usize * 8];
        for (out, word) in row.chunks_exact_mut(2).zip(pixel) {
            out.copy_from_slice(&word.to_be_bytes());
        }
        stream.write_all(&row).unwrap();
        row.fill(0);
        for _ in 1..height {
            stream.write_all(&row).unwrap();
        }
        stream.finish().unwrap();
        writer.finish().unwrap();
    }

    let document = Document::new(8, 8).unwrap();
    let target = document.layers[0].id.clone();
    let fixture = RuntimeFixture::new();
    let root = Arc::new(std::sync::Mutex::new(Engine::from_document(document.clone())));
    let connection = crate::server::start(root.clone(), fixture.path().join("state")).unwrap();
    connection.configure_native_sources(None, true).unwrap();
    let workspace = crate::workspace::attach(&root);
    let runtime = workspace.lock().unwrap().source_runtime.clone().unwrap();
    let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
    let staged = CapturedScene::from_parts(document, "clipboard-png".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(())).unwrap();
    let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let control = crate::loading::Control::default();
    let mut tab = crate::workspace::native_registration::Prepared::from_qualified_engine(
        &workspace, &runtime, Engine::from_native(native, 4).unwrap(), &control,
    ).unwrap();
    tab.publish_with(&workspace, &control, None, || ()).unwrap();

    let snapshot = tab.shared.lock().unwrap().doc_snapshot();
    let paste = || crate::clipboard::EditRequest::Paste {
        shared: tab.shared.clone(), doc: snapshot.clone(), target: target.clone(),
    };
    let mut clipboard = crate::clipboard::Session::default();
    let mut sink = Sink::default();
    let disk = runtime.disk().used().unwrap();
    let mut corrupt = encoded.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    let animated = crate::image_import::prepared_png::apng::tests::single();
    for (bytes, reason) in [(corrupt, "checksum"), (animated, "explicit frame")] {
        sink.png_readback = Some(bytes.into());
        assert!(clipboard.process_edit(&mut sink, paste()).result.unwrap_err().contains(reason));
        assert_eq!(tab.shared.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    #[cfg(windows)]
    let incoming = {
        encoded.extend_from_slice(&[0; 13]);
        crate::clipboard::windows::png_offer(encoded).unwrap()
    };
    #[cfg(not(windows))]
    let incoming = encoded.into();
    sink.png_readback = Some(incoming);
    let reply = clipboard.process_edit(&mut sink, paste());
    reply.result.unwrap();
    let selected = reply.selected.unwrap();
    let engine = tab.shared.lock().unwrap();
    assert_eq!(engine.history_lengths(), [1, 0]);
    let view = crate::engine::view::current(&engine);
    let document = view.native().unwrap();
    assert_eq!(document.metadata().depth(), 16);
    let layer = document.metadata().layers().find(|layer| layer.id() == selected).unwrap();
    assert_eq!(layer.origin(), [(8 - width as i32) / 2, (8 - height as i32) / 2]);
    let surface = document.source(layer.pixels().slot()).unwrap();
    assert_eq!((surface.width(), surface.height()), (width, height));
    let first = surface.prepare_region(Region::new([0, 0, 2, 1]).unwrap(), &mut || Ok(())).unwrap();
    assert_eq!(first.pixel16(0, 0).unwrap(), pixel);
    assert_eq!(first.pixel16(1, 0).unwrap(), [0; 4]);
}

fn copied(fixture: &Fixture, session: &TestSession) -> StagedCopy {
    stage_copy(
        &fixture.document,
        &fixture.scene,
        Request {
            target: &fixture.legacy.layers[0].id,
            mask: false,
            merged: false,
        },
        session,
        &mut Work::new(1_000_000).unwrap(),
        &|| Ok(()),
    )
    .unwrap()
}

#[test]
fn wide_native_copy_survives_bitmap_transport_and_png_roundtrip_at_original_depth() {
    for depth in [8, 16] {
        let mut document = Document::new_depth(1, 1, depth).unwrap();
        document.width = 16000;
        document.height = 3;
        document.layers[0].pixels = Raster::new_depth(16000, 3, depth);
        for (x, y) in [(0, 0), (255, 1), (256, 1), (15999, 2)] {
            document.layers[0].pixels.set16(x, y, [12347, 33559, 51237, 65535]);
        }
        let mut expected = Image::from_raster(document.layers[0].pixels.clone());
        expected.origin = Some([0, 0]);
        let fixture = Fixture::new(document);
        let session = fixture.session();
        let disk = fixture.disk.used().unwrap();
        let hash = fixture.source_hash();
        let image = copied(&fixture, &session)
            .retire_into_transport(&session, &fixture.pool, &|| Ok(())).unwrap();
        assert_eq!((image.width, image.height), (16000, 3));
        assert_eq!(image.bytes, expected.bytes);
        assert_eq!(image.samples16, expected.samples16);
        assert_eq!(image.origin, expected.origin);
        assert_eq!(fixture.disk.used().unwrap(), disk);
        let mut sink = Sink::default();
        image.write(&mut sink, &|| Ok(())).unwrap();
        let received = Image::png_bytes(sink.image.as_ref().unwrap().png().unwrap()).unwrap();
        assert_eq!((received.width, received.height), (16000, 3));
        assert_eq!(received.bytes, expected.bytes);
        assert_eq!(received.samples16, expected.samples16);
        assert_eq!(fixture.source_hash(), hash);
        drop((image, session));
        fixture.finish();
    }
}

#[test]
fn selected_mask_cut_copies_the_same_raw_step_that_it_erases() {
    use crate::engine::{
        native::worker::tests::Fixture as RuntimeFixture,
        scene::{CapturedScene, LegacyTileCache},
        view::DocumentView,
        Engine, Mask, MaskStep,
    };
    for depth in [8, 16] {
        let mut document = Document::new_depth(8, 8, depth).unwrap();
        let layer = &mut document.layers[0];
        (layer.x, layer.y) = (2, 1);
        let id = layer.id.clone();
        let native = |p: [u16; 4]| {
            if depth == 16 { p } else { p.map(|v| u16::from(crate::raster::project16(v)) * 257) }
        };
        let raw = [
            [12347, 23459, 34571, 49153], [40001, 21023, 32345, 50000],
            [45671, 23453, 52341, 62001], [321, 2345, 65431, 0],
        ].map(native);
        let sibling = native([56555, 56555, 56555, 65535]);
        let requested = (depth == 8).then_some("early");
        let cut_index = usize::from(requested.is_none());
        let steps = ["early", "last"].into_iter().enumerate().map(|(index, name)| {
            let mut pixels = Raster::new_depth(8, 8, depth);
            for (x, pixel) in raw.into_iter().enumerate() {
                pixels.set16(x as i32, 1, if index == cut_index { pixel } else { sibling });
            }
            MaskStep {
                id: name.into(), kind: "paint".into(), enabled: index != cut_index,
                weight: 1., value: 0., pixels, settings: serde_json::Value::Null,
            }
        }).collect();
        layer.mask = Some(Mask { enabled: true, linked: true, steps, cache_key: crate::engine::id() });
        let bounds = [2, 2, 6, 3];
        let mut stencil = Raster::new(4, 1);
        for (x, coverage) in [255, 85, 0, 255].into_iter().enumerate() {
            stencil.set(x as i32, 0, [coverage; 4]);
        }
        document.selection = Some(bounds);
        document.selection_coverage = Some(crate::selection::Coverage {
            mask: stencil, origin: [2, 2], bounds, contours: vec![],
        });
        let mut expected = raw;
        expected[2] = [0; 4];
        let scale = if depth == 16 { 1 } else { 257 };
        expected[1][3] = ((raw[1][3] / scale + 1) / 3) * scale;

        let fixture = RuntimeFixture::new();
        let root = Arc::new(std::sync::Mutex::new(Engine::from_document(document.clone())));
        let connection = crate::server::start(root.clone(), fixture.path().join("state")).unwrap();
        connection.configure_native_sources(None, true).unwrap();
        let workspace = crate::workspace::attach(&root);
        let runtime = workspace.lock().unwrap().source_runtime.clone().unwrap();
        let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
        let staged = CapturedScene::from_parts(document, "mask-cut".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let control = crate::loading::Control::default();
        let mut tab = crate::workspace::native_registration::Prepared::from_qualified_engine(
            &workspace, &runtime, Engine::from_native(native, 4).unwrap(), &control,
        ).unwrap();
        tab.publish_with(&workspace, &control, None, || ()).unwrap();

        for shared in [&root, &tab.shared] {
            let snapshot = shared.lock().unwrap().doc_snapshot();
            let request = || crate::clipboard::EditRequest::Pixels {
                shared: shared.clone(), doc: snapshot.clone(), target: id.clone(), mask: true,
                merged: false, cut: true, step: requested.map(str::to_owned),
            };
            let mut clipboard = crate::clipboard::Session::default();
            let mut sink = Sink { fail: true, ..Sink::default() };
            let disk = runtime.disk().used().unwrap();
            assert!(clipboard.process_edit(&mut sink, request()).result.unwrap_err().contains("OS image write refused"));
            assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
            assert_eq!(runtime.disk().used().unwrap(), disk);
            sink.fail = false;
            clipboard.process_edit(&mut sink, request()).result.unwrap();
            let image = sink.image.as_ref().unwrap();
            assert_eq!((image.width, image.height, image.origin), (4, 1, Some([2, 2])));
            assert_eq!(image.bytes, expected.into_iter().flatten().map(crate::raster::project16).collect::<Vec<_>>());
            assert_eq!(image.samples16, (depth == 16).then(|| expected.concat()));
            let decoded = Image::png_bytes(image.png().unwrap()).unwrap();
            assert_eq!(decoded.bytes, image.bytes);
            assert_eq!(decoded.samples16, image.samples16);

            let engine = shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [1, 0]);
            let view = crate::engine::view::current(&engine);
            assert_eq!(view.selection(), Some(bounds));
            for index in 0..2 {
                for x in 0..4 {
                    let actual = match view {
                        DocumentView::Resident(doc) => doc.layers[0].mask.as_ref().unwrap().steps[index].pixels.get16(x, 1),
                        DocumentView::Native(doc) => {
                            let slot = doc.metadata().layer(0).unwrap().mask().unwrap().steps().nth(index).unwrap().pixels().slot();
                            doc.source(slot).unwrap().prepare_region(Region::new([x, 1, x + 1, 2]).unwrap(), &mut || Ok(())).unwrap().pixel16(x, 1).unwrap()
                        }
                    };
                    let expected = if index != cut_index {
                        sibling
                    } else if x == 0 || x == 3 {
                        [0; 4]
                    } else if x == 1 {
                        let mut pixel = raw[1];
                        pixel[3] -= expected[1][3];
                        pixel
                    } else {
                        raw[2]
                    };
                    assert_eq!(actual, expected, "depth {depth}, step {index}, pixel {x}");
                }
            }
            if view.native().is_some() {
                let snapshot = engine.doc_snapshot();
                drop(engine);
                sink.display_readback = true;
                let pasted = clipboard.process_edit(&mut sink, crate::clipboard::EditRequest::Paste {
                    shared: shared.clone(), doc: snapshot, target: id.clone(),
                });
                assert!(pasted.result.unwrap().starts_with("Pasted as a new layer"));
                let selected = pasted.selected.unwrap();
                let engine = shared.lock().unwrap();
                assert_eq!(engine.history_lengths(), [2, 0]);
                let view = crate::engine::view::current(&engine);
                assert_eq!(view.selection(), None);
                let document = view.native().unwrap();
                let layer = document.metadata().layers().find(|layer| layer.id() == selected).unwrap();
                assert_eq!(layer.origin(), [2, 2]);
                assert_eq!(layer.pixels().depth(), depth);
                let pixels = document.source(layer.pixels().slot()).unwrap()
                    .prepare_region(Region::new([0, 0, 4, 1]).unwrap(), &mut || Ok(())).unwrap();
                for (x, pixel) in expected.into_iter().enumerate() {
                    assert_eq!(pixels.pixel16(x as i32, 0).unwrap(), pixel);
                }
            }
        }
    }
}

#[test]
fn oversized_native_os_copy_and_cut_preserve_existing_clipboard_before_scene_capture() {
    for (depth, width, height, reason) in [
        (16, 16000, 2048, "transport budget"),
        (16, 8192, 3072, "transport budget"),
        (16, 16000, 1100, "import budget"),
        (8, 16000, 2200, "import budget"),
    ] {
        for cut in [false, true] {
            let mut original = doc(depth, 3);
            original.width = width;
            original.height = height;
            original.selection = cut.then_some([0, 0, width as i32, height as i32]);
            original.selection_polygon = None;
            original.selection_coverage = None;
            let fixture = Fixture::new(original);
            let disk = fixture.disk.used().unwrap();
            let hash = fixture.source_hash();
            let mut clipboard = crate::clipboard::Session::default();
            let previous = Image {
                width: 1, height: 1, bytes: vec![13, 57, 123, 255],
                samples16: Some(vec![3341, 14649, 31611, 65535]), origin: Some([7, 9]),
            };
            clipboard.image = Some(previous.clone().into());
            clipboard.image_identity = Some(ImageIdentity::Generation(42));
            let mut sink = Sink { image: Some(previous.clone()), ..Sink::default() };
            let shared = Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
            let error = clipboard.copy_native_pixels(&mut sink, shared.clone(), &fixture.document,
                &fixture.legacy.layers[0].id, false, false, cut, None).unwrap_err();
            assert!(error.contains(reason), "{error}");
            assert_eq!(sink.writes, 0);
            assert_eq!(sink.image.as_ref().unwrap().bytes, previous.bytes);
            assert_eq!(sink.image.as_ref().unwrap().samples16, previous.samples16);
            assert_eq!(clipboard.image.as_ref().unwrap().samples16, previous.samples16);
            assert_eq!(clipboard.image.as_ref().unwrap().origin, previous.origin);
            assert_eq!(clipboard.image_identity, Some(ImageIdentity::Generation(42)));
            assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
            assert_eq!(fixture.disk.used().unwrap(), disk);
            assert_eq!(fixture.source_hash(), hash);
            fixture.finish();
        }
    }
}

#[test]
fn native_transport_refuses_cancelled_foreign_and_oversized_buffers_then_retires_sources() {
    for failure in ["cancel", "foreign", "foreign-zero", "size"] {
        let fixture = Fixture::new(doc(16, 3));
        let hash = fixture.source_hash();
        let disk = fixture.disk.used().unwrap();
        let session = fixture.session();
        let copy = if failure == "size" || failure == "foreign-zero" {
            // Stay inside resident geometry while exceeding retained native16 bytes.
            let [width, height] = if failure == "size" { [8192, 3072] } else { [8, 8] };
            let output = crate::engine::native::OutputDraft::new(
                Surface::zero(width, height, 16).unwrap(),
                vec![],
            )
            .seal(&fixture.document, &session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
            StagedCopy {
                source: fixture.document.key(),
                bounds: [0, 0, width as i32, height as i32],
                output,
                budget: fixture.pool.clone(),
            }
        } else {
            copied(&fixture, &session)
        };
        let other = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let calls = Cell::new(0);
        let check = || {
            calls.set(calls.get() + 1);
            if failure == "cancel" && calls.get() >= 9 {
                Err("transport interrupted".into())
            } else {
                Ok(())
            }
        };
        let error = copy
            .retire_into_transport(
                &session,
                if failure.starts_with("foreign") {
                    &other
                } else {
                    &fixture.pool
                },
                &check,
            )
            .err()
            .unwrap();
        assert!(
            error.contains(match failure {
                "cancel" => "transport interrupted",
                "foreign" | "foreign-zero" => "another source admission",
                _ => "256 MiB",
            }),
            "{error}"
        );
        assert_eq!(fixture.disk.used().unwrap(), disk);
        assert_eq!(fixture.source_hash(), hash);
        assert_eq!(other.used(), 0);
        drop(session);
        fixture.finish();
    }
}

#[test]
fn native_os_transport_requires_admission_and_retains_words_for_retry_after_write_failure() {
    let fixture = Fixture::new(doc(16, 3));
    let session = fixture.session();
    let image = copied(&fixture, &session)
        .retire_into_transport(&session, &fixture.pool, &|| Ok(()))
        .unwrap();
    let expected =
        Image::copy(&fixture.legacy, &fixture.legacy.layers[0].id, false, false).unwrap();
    let mut sink = Sink::default();
    let held = MetadataCharge::reserve(
        &fixture.pool,
        fixture.pool.limit() - fixture.pool.used() - 1024 * 1024,
        &|| Ok(()),
    )
    .unwrap();
    assert!(image.write(&mut sink, &|| Ok(())).is_err());
    assert_eq!(sink.writes, 0);
    drop(held);
    assert!(image
        .write(&mut sink, &|| Err("before OS write".into()))
        .is_err());
    assert_eq!(sink.writes, 0);
    sink.fail = true;
    let baseline = fixture.pool.used();
    assert_eq!(
        image.write(&mut sink, &|| Ok(())).unwrap_err(),
        "OS image write refused"
    );
    assert_eq!(fixture.pool.used(), baseline);
    assert_eq!(image.samples16, expected.samples16);
    sink.fail = false;
    image.write(&mut sink, &|| Ok(())).unwrap();
    assert_eq!(sink.image.as_ref().unwrap().samples16, expected.samples16);
    assert_eq!(sink.image.as_ref().unwrap().bytes, expected.bytes);
    assert_eq!(sink.image.as_ref().unwrap().origin, expected.origin);
    assert_eq!(fixture.pool.used(), baseline);
    drop((image, session));
    fixture.finish();
}

#[test]
fn retained_native_paste_holds_transport_admission_and_preserves_words_on_refusal_and_retry() {
    let fixture = Fixture::new(doc(16, 3));
    let session = fixture.session();
    let image = copied(&fixture, &session)
        .retire_into_transport(&session, &fixture.pool, &|| Ok(()))
        .unwrap();
    let words = image.samples16.as_ref().unwrap().clone();
    let pointer = image.samples16.as_ref().unwrap().as_ptr();
    let origin = image.origin.unwrap();
    let os = Image {
        width: image.width,
        height: image.height,
        bytes: image.bytes.clone(),
        samples16: None,
        origin: None,
    };
    let mut engine = crate::engine::Engine::new();
    engine.resident_state_mut().expect("resident fixture").document = Document::new(17, 19).unwrap();
    let destination = engine.resident_state().expect("resident fixture").document.clone();
    let shared = Arc::new(std::sync::Mutex::new(engine));
    let mut clipboard = crate::clipboard::Session::default();
    clipboard.image = Some(image);
    clipboard.image_identity = Some(ImageIdentity::Png { digest: [1; 32], generation: Some(17) });
    let mut provider = crate::clipboard::tests::Fake::with_images(vec![Ok(os.clone()), Ok(os)]);
    provider.image_identity = Some(ImageIdentity::Generation(17));
    let request = || crate::clipboard::EditRequest::Paste {
        shared: shared.clone(),
        doc: destination.clone().into(),
        target: destination.layers[0].id.clone(),
    };
    let baseline = fixture.pool.used();
    let held = MetadataCharge::reserve(
        &fixture.pool,
        fixture.pool.limit() - baseline - 1024 * 1024,
        &|| Ok(()),
    )
    .unwrap();
    assert!(clipboard
        .process_edit(&mut provider, request())
        .result
        .is_err());
    assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
    assert_eq!(shared.lock().unwrap().resident_state().expect("resident fixture").document.bit_depth, 8);
    assert_eq!(
        clipboard
            .image
            .as_ref()
            .unwrap()
            .samples16
            .as_ref()
            .unwrap()
            .as_ptr(),
        pointer
    );
    drop(held);
    assert_eq!(fixture.pool.used(), baseline);
    let reply = clipboard.process_edit(&mut provider, request());
    reply.result.unwrap();
    assert_eq!(fixture.pool.used(), baseline);
    let engine = shared.lock().unwrap();
    let layer = engine
        .resident_state().expect("resident fixture").document
        .layers
        .iter()
        .find(|l| Some(&l.id) == reply.selected.as_ref())
        .unwrap();
    assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, 16);
    assert_eq!([layer.x, layer.y], origin);
    assert_eq!(layer.pixels.rgba16(), words);
    assert_eq!(engine.history_lengths(), [1, 0]);
    assert_eq!(
        clipboard
            .image
            .as_ref()
            .unwrap()
            .samples16
            .as_ref()
            .unwrap()
            .as_ptr(),
        pointer
    );
    drop((engine, clipboard, session));
    fixture.finish();
}

#[test]
fn snapshot_paste_refuses_replaced_authority_before_reading_the_os_clipboard() {
    let fixture = Fixture::new(doc(16, 3));
    let mut engine = crate::engine::Engine::new();
    engine.resident_state_mut().expect("resident fixture").document = fixture.legacy.clone();
    let before = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
    let shared = Arc::new(std::sync::Mutex::new(engine));
    let mut clipboard = crate::clipboard::Session::default();
    let mut sink = Sink::default();
    let reply = clipboard.process_edit(
        &mut sink,
        crate::clipboard::EditRequest::Paste {
            shared: shared.clone(),
            doc: fixture.document.clone().into(),
            target: fixture.legacy.layers[0].id.clone(),
        },
    );
    assert!(reply
        .result
        .unwrap_err()
        .contains("Observed paste destination changed"));
    assert_eq!(
        serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
        before
    );
    assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
    fixture.finish();
}

#[test]
fn native_pixel_worker_never_falls_back_to_a_resident_document_with_matching_identity() {
    let fixture = Fixture::new(doc(16, 3));
    let mut engine = crate::engine::Engine::new();
    engine.resident_state_mut().expect("resident fixture").document = fixture.legacy.clone();
    let shared = Arc::new(std::sync::Mutex::new(engine));
    let before = serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
    let worker = crate::clipboard::Worker::memory(eframe::egui::Context::default());
    for cut in [false, true] {
        worker
            .copy_pixels(
                shared.clone(),
                fixture.document.clone().into(),
                fixture.legacy.layers[0].id.clone(),
                false,
                false,
                cut,
                None,
            )
            .unwrap();
        let reply = worker
            .replies
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(reply.document, fixture.legacy.id);
        assert!(reply
            .result
            .unwrap_err()
            .contains("Observed clipboard source changed"));
        assert_eq!(
            serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
            before
        );
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
    }
    drop(worker);
    fixture.finish();
}
