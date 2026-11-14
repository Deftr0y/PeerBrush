use super::*;
use super::{
    grouping_tests::gate,
    import_tests::{file, pixels},
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Engine,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;

#[test]
fn natural_placement_matches_resident_across_partial_tiles_at_native_precision() {
    for (depth, incoming) in [(8, 8), (8, 16), (16, 8), (16, 16)] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut original = crate::engine::Document::new_depth(300, 270, depth).unwrap();
        let layer = &mut original.layers[0];
        layer.x = -3;
        layer.y = 2;
        for y in 0..270 {
            for x in 0..300 {
                layer.pixels.set16(x, y, [12347, 23459, (x * 131 + y * 37) as u16, 49153]);
            }
        }
        let words: Vec<u16> = (0..270 * 259)
            .flat_map(|i| [
                (i * 37 % 65536) as u16, 34571, 51713,
                match i % 3 { 0 => 0, 1 => 23459, _ => 65535 },
            ])
            .collect();
        let png = if incoming == 16 {
            crate::raster::png16(270, 259, &words).unwrap()
        } else {
            crate::raster::png(270, 259, &words.iter().copied()
                .map(crate::raster::project16).collect::<Vec<_>>()).unwrap()
        };
        let path = fixture.path().join("natural.png");
        std::fs::write(&path, png).unwrap();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(
            original.clone(), "natural-placement".into(), vec![], None, None,
        ).stage(&runtime, &cache, &|| Ok(())).unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let disk = runtime.disk().used().unwrap();
        for mode in ["replace", "over"] {
            let command = json!({"op":"image.place","path":path,"layer":original.layers[0].id,
                "new_layer":false,"mode":mode,"rect":[-1,4,269,263]});
            let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            let preview = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            pixels(preview.preview(), &expected, &original);
            // The last physical tile includes padding outside the unchanged layer frame.
            let layer = preview.preview().metadata().layer(0).unwrap();
            let source = preview.preview().source(layer.pixels().slot()).unwrap();
            let tile = source.prepare_tile((1, 1), &mut || Ok(())).unwrap();
            let bytes = tile.bytes().unwrap();
            let stride = usize::from(source.depth() / 8) * 4;
            for y in 0..256 {
                let start = if y < 14 { 44 } else { 0 };
                assert!(bytes[(y * 256 + start) * stride..(y + 1) * 256 * stride]
                    .iter().all(|v| *v == 0));
            }
            drop(tile);
            preview.abort(&session, &|| session.cleanup_check()).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            assert_eq!(state.native().unwrap().key(), native.key());
            assert_eq!(state.history_lengths(), [0, 0]);
        }
    }
}

#[test]
fn sparse_native_placement_expands_color_and_mask_without_copying_empty_pixels() {
    use std::sync::Arc;
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut original = crate::engine::Document::new_depth(8, 8, depth).unwrap();
        original.width = 16000;
        original.height = 9000;
        let mut raster = crate::raster::Raster::new_depth(15999, 9000, depth);
        raster.set16(300, 300, [12347, 23459, 34571, 0]);
        raster.set16(15998, 8999, [43117, 23519, 13177, 65535]);
        let padding = (38 * 256 + 127) * 4;
        if depth == 16 {
            Arc::make_mut(raster.samples16.get_mut(&(62, 35)).unwrap())[padding..padding + 4]
                .copy_from_slice(&[54321, 32109, 10987, 65535]);
        } else {
            Arc::make_mut(raster.tiles.get_mut(&(62, 35)).unwrap())[padding..padding + 4]
                .copy_from_slice(&[211, 125, 43, 255]);
        }
        original.layers[0].pixels = raster.clone();
        original.layers[0].mask = Some(crate::engine::Mask {
            enabled: true, linked: true, cache_key: crate::engine::id(),
            steps: vec![crate::engine::MaskStep {
                id: "mask-paint".into(), kind: "paint".into(), enabled: true, weight: 1., value: 0.,
                settings: serde_json::Value::Null, pixels: raster.clone(),
            }],
        });
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(original.clone(), "sparse-place".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let path = fixture.path().join("corner.png");
        let color = [51713, 12347, 34571, 65535];
        let png = if depth == 16 { crate::raster::png16(1, 1, &color).unwrap() }
            else { crate::raster::png(1, 1, &color.map(crate::raster::project16)).unwrap() };
        std::fs::write(&path, png).unwrap();
        let command = json!({"op":"image.place","path":path,"layer":original.layers[0].id,"new_layer":false,
            "mode":"replace","rect":[15999,8999,16000,9000]});
        let disk = runtime.disk().used().unwrap();
        let checks = std::cell::Cell::new(0usize);
        let preview = prepare(capture(&state, &[command.clone()]), &session, &|| {
            checks.set(checks.get() + 1);
            Ok(())
        }).unwrap();
        assert!(checks.get() < 20000, "Sparse placement should not walk every empty pixel row");
        let before_layer = native.metadata().layer(0).unwrap();
        let after_layer = preview.preview().metadata().layer(0).unwrap();
        let old_mask = before_layer.mask().unwrap().steps().next().unwrap().pixels();
        let new_mask = after_layer.mask().unwrap().steps().next().unwrap().pixels();
        for (old, new, mask) in [(before_layer.pixels(), after_layer.pixels(), false), (old_mask, new_mask, true)] {
            let before = native.source(old.slot()).unwrap();
            let after = preview.preview().source(new.slot()).unwrap();
            assert_eq!((after.width(), after.height()), (16000, 9000));
            let retained = after.physical_tile((1, 1)).unwrap();
            let source = before.physical_tile((1, 1)).unwrap();
            assert!(retained.owner().same_owner(source.owner()));
            assert_eq!(retained.reference(), source.reference());
            assert_eq!(after.physical_tiles().count(), 2);
            for (x, y, expected) in [
                (300, 300, raster.get16(300, 300)),
                (15998, 8999, raster.get16(15998, 8999)),
                (15999, 8998, [0; 4]),
                (15999, 8999, if mask { [0; 4] } else if depth == 16 { color } else { color.map(|v| u16::from(crate::raster::project16(v)) * 257) }),
            ] {
                let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
                assert_eq!(after.prepare_region(region, &mut || Ok(())).unwrap().pixel16(x, y).unwrap(), expected);
            }
            let edge = after.prepare_tile((62, 35), &mut || Ok(())).unwrap();
            let stride = if depth == 16 { 8 } else { 4 };
            assert!(edge.bytes().unwrap()[40 * 256 * stride..].iter().all(|v| *v == 0));
        }
        preview.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let stopped = prepare(capture(&state, &[command]), &session, &|| {
            if runtime.disk().used().unwrap() > disk { Err("Cancel sparse placement".into()) } else { Ok(()) }
        }).err().unwrap();
        assert!(stopped.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
    }
}

#[test]
fn native_png_file_and_inline_placement_keep_wide_frames_precision_and_history() {
    for (depth, incoming) in [(8, 8), (8, 16), (16, 8), (16, 16)] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 512 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let native = NativeDocument::new_blank(16000, 3, depth, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let original_layer = native.metadata().layers().next().unwrap().id().to_owned();
        let words = [12347, 23459, 34571, 0, 54321, 32109, 10987, 65535];
        let wide = fixture.path().join("native-wide.png");
        let small = fixture.path().join("native-small.png");
        for (path, width, height, values) in [
            (&wide, 16000, 2, words.repeat(16000)),
            (&small, 2, 1, words[4..].repeat(2)),
        ] {
            let bytes = if incoming == 16 { super::import_tests::native_png16(width, height, &values) }
                else { crate::raster::png(width, height, &values.iter().copied().map(crate::raster::project16).collect::<Vec<_>>()).unwrap() };
            std::fs::write(path, bytes).unwrap();
        }
        let encoded = STANDARD.encode(std::fs::read(&wide).unwrap());
        let clipboard_words = words.repeat(16000);
        let clipboard = crate::clipboard::Image {
            width: 16000, height: 2,
            bytes: clipboard_words.iter().copied().map(crate::raster::project16).collect(),
            samples16: (incoming == 16).then_some(clipboard_words),
            origin: Some([0, 1]),
        };
        for case in 0..7 {
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let command = match case {
                0 => json!({"op":"image.place","path":wide,"rect":[0,1,16000,3]}),
                1 => json!({"op":"image.paste","path":wide,"layer":original_layer,"origin":[0,1]}),
                2 => json!({"op":"image.place","path":wide,"layer":original_layer,"new_layer":false,"mode":"replace","rect":[0,1,16000,3]}),
                3 => json!({"op":"image.place","path":small,"rect":[0,1,16000,3]}),
                4 => json!({"op":"image.place","png":encoded,"rect":[0,1,16000,3]}),
                5 => json!({"op":"image.paste","png":encoded,"layer":original_layer,"origin":[0,1]}),
                _ => clipboard.command(&original_layer).unwrap(),
            };
            let expected = |x: usize| {
                let mut pixel: [u16; 4] = words[if case == 3 || x % 2 == 1 { 4..8 } else { 0..4 }].try_into().unwrap();
                if incoming == 8 { pixel = pixel.map(|v| u16::from(crate::raster::project16(v)) * 257); }
                pixel
            };
            let inspect = |document: &NativeDocument| {
                assert_eq!((document.metadata().width(), document.metadata().height()), (16000, 3));
                assert_eq!(document.metadata().depth(), depth.max(incoming));
                let layer = if case == 2 {
                    document.metadata().layers().find(|l| l.id() == original_layer).unwrap()
                } else { document.metadata().layers().find(|l| l.id() != original_layer && l.kind() == "paint").unwrap() };
                assert_eq!(layer.origin(), [0, if case == 2 { 0 } else { 1 }]);
                let pixels = layer.pixels();
                assert_eq!((pixels.width(), pixels.height()), (16000, if case == 2 { 3 } else { 2 }));
                let surface = document.source(pixels.slot()).unwrap();
                let y = if case == 2 { 1 } else { 0 };
                for x in [0, 1, 15998, 15999] {
                    let area = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
                    let view = surface.prepare_region(area, &mut || Ok(())).unwrap();
                    assert_eq!(view.pixel16(x, y).unwrap(), expected(x as usize));
                }
            };
            let disk = runtime.disk().used().unwrap();
            let preview = prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            inspect(preview.preview());
            preview.abort(&session, &|| session.cleanup_check()).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let bad = json!({"op":"image.place","path":small,"rect":[0,0,16001,1]});
            let refused = prepare(capture(&state, &[command.clone(), bad]), &session, &|| Ok(())).err().unwrap();
            assert!(refused.cleanup_error.is_none());
            assert_eq!(runtime.disk().used().unwrap(), disk);
            assert_eq!(state.native().unwrap().key(), native.key());
            assert_eq!(state.history_lengths(), [0, 0]);
            let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            let (base, history, delta) = prepared.into_parts();
            let commit = history.prepare_commit(&base, delta, HistoryRequest {
                actor: "human", label: "Wide placement", task: None, gesture: None,
            }, &|| Ok(())).unwrap();
            state.commit_native(gate(&[]), commit).unwrap().retire(&session, &|| session.cleanup_check()).unwrap();
            inspect(state.native().unwrap());
            for direction in [Direction::Undo, Direction::Redo] {
                let moved = state.native_history().unwrap().prepare_move(state.native().unwrap(), direction, "human", &|| Ok(())).unwrap();
                drop(state.move_native(gate(&[]), moved).unwrap());
                if matches!(direction, Direction::Undo) {
                    assert_eq!(state.native().unwrap().metadata().depth(), depth);
                    assert_eq!(state.native().unwrap().metadata().layers().len(), 1);
                    assert!(state.native().unwrap().source(state.native().unwrap().metadata().layer(0).unwrap().pixels().slot()).unwrap().physical_tiles().next().is_none());
                } else { inspect(state.native().unwrap()); }
            }
        }
    }
}

#[test]
fn wide_source_downsampling_uses_bounded_windows_and_matches_native_precision() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
        let original = crate::engine::Document::new_depth(8, 8, depth).unwrap();
        let words: Vec<u16> = (0..8192 * 3)
            .flat_map(|i| {
                [
                    (i * 37 % 65536) as u16,
                    23459,
                    34571,
                    if i % 3 == 0 { 0 } else { 49153 },
                ]
            })
            .collect();
        let png = if depth == 16 {
            crate::raster::png16(8192, 3, &words)
        } else {
            crate::raster::png(
                8192,
                3,
                &words
                    .iter()
                    .copied()
                    .map(crate::raster::project16)
                    .collect::<Vec<_>>(),
            )
        }
        .unwrap();
        let path = fixture.path().join("wide.png");
        std::fs::write(&path, png).unwrap();
        let command = json!({"op":"image.place","path":path,"rect":[-5,-3,262,254]});
        let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original.clone(), "wide-place".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let disk = runtime.disk().used().unwrap();
        let preview = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
        pixels(preview.preview(), &expected, &original);
        preview
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        drop((
            state,
            native,
            session,
            staged.seals,
            cache,
            runtime,
            connection,
        ));
    }
}

#[test]
fn placed_images_match_resident_scaling_frames_masks_precision_and_history() {
    for (depth, incoming) in [(8, 8), (8, 16), (16, 8), (16, 16)] {
        for case in 0..8 {
            let fixture = Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
            let mut original = super::settings_tests::document(depth);
            original
                .layers
                .iter_mut()
                .find(|l| l.id == "A")
                .unwrap()
                .locked = false;
            if case == 6 {
                original
                    .layers
                    .iter_mut()
                    .find(|l| l.id == "outside")
                    .unwrap()
                    .kind = "fill".into();
            }
            let path = file(&fixture, incoming);
            if case == 7 && depth == 8 && incoming == 16 {
                let words = [12347, 23459, 34571, 49153, 54321, 32109, 10987, 0]
                    .map(|v| u16::from(crate::raster::project16(v)) * 257);
                std::fs::write(&path, crate::raster::png16(2, 1, &words).unwrap()).unwrap();
            }
            if case == 4 {
                let pixels = &mut original
                    .layers
                    .iter_mut()
                    .find(|l| l.id == "outside")
                    .unwrap()
                    .pixels;
                if depth == 16 {
                    let tile = std::sync::Arc::make_mut(pixels.samples16.get_mut(&(3, 0)).unwrap());
                    let end = tile.len();
                    tile[end - 4..].copy_from_slice(&[12347, 23459, 34571, 45679]);
                } else {
                    let tile = std::sync::Arc::make_mut(pixels.tiles.get_mut(&(3, 0)).unwrap());
                    let end = tile.len();
                    tile[end - 4..].copy_from_slice(&[13, 57, 123, 231]);
                }
            }
            let mut command = match case {
                0 => json!({"op":"image.place","layer":"B","x":-4,"y":7}),
                1 => json!({"op":"image.place","layer":"b_child","rect":[-5,-3,260,4]}),
                2 => json!({"op":"image.place","layer":"outside","parent":"B","rect":[0,0,1,1]}),
                3 => {
                    json!({"op":"image.place","layer":"outside","parent":null,"rect":[0,0,267,257]})
                }
                4 => {
                    json!({"op":"image.place","layer":"outside","new_layer":false,"rect":[-2,1,0,2]})
                }
                5 => {
                    json!({"op":"image.place","layer":"a_base","new_layer":false,"mode":"replace","rect":[-5,-3,260,7]})
                }
                6 => {
                    json!({"op":"image.place","layer":"outside","new_layer":false,"rect":[-5,-3,260,7]})
                }
                _ => {
                    json!({"op":"image.place","layer":"outside","new_layer":false,"mode":"replace","x":-3,"y":0})
                }
            };
            if case == 7 {
                // A natural-size replacement with identical values keeps the original,
                // including when the incoming image promotes the whole document.
                let source = crate::image_import::decode_file(
                    &path,
                    &json!({}),
                    crate::image_import::BUDGET,
                )
                .unwrap();
                let l = original
                    .layers
                    .iter_mut()
                    .find(|l| l.id == "outside")
                    .unwrap();
                for x in 0..2 {
                    l.pixels.set16(x, 0, source.get16(x, 0));
                }
                l.pixels.retained = Some(Box::new(crate::retained::Original::from(&l.pixels)));
            }
            if case % 2 == 0 {
                command["path"] = json!(path);
            } else {
                command["png"] = json!(STANDARD.encode(std::fs::read(&path).unwrap()));
            }
            let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "placed-image".into(),
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
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let disk = runtime.disk().used().unwrap();
            let preview =
                prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            pixels(preview.preview(), &expected, &original);
            if case == 7 && depth >= incoming {
                let old = native
                    .metadata()
                    .layers()
                    .find(|l| l.id() == "outside")
                    .unwrap();
                let new = preview
                    .preview()
                    .metadata()
                    .layers()
                    .find(|l| l.id() == "outside")
                    .unwrap();
                assert_eq!(
                    native.source(old.pixels().slot()).unwrap().key(),
                    preview.preview().source(new.pixels().slot()).unwrap().key()
                );
            }
            if case == 4 {
                let l = preview
                    .preview()
                    .metadata()
                    .layers()
                    .find(|l| l.id() == "outside")
                    .unwrap();
                let source = preview.preview().source(l.pixels().slot()).unwrap();
                let tile = source.prepare_tile((3, 0), &mut || Ok(())).unwrap();
                let bytes = tile.bytes().unwrap();
                let expected = expected.layers.iter().find(|l| l.id == "outside").unwrap();
                if source.depth() == 16 {
                    let words = expected.pixels.samples16.get(&(3, 0)).unwrap();
                    for (bytes, word) in bytes.chunks_exact(2).zip(words.iter()) {
                        assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), *word);
                    }
                } else {
                    assert_eq!(
                        bytes,
                        expected.pixels.tiles.get(&(3, 0)).unwrap().as_slice()
                    );
                }
            }
            let before = native.metadata().selection_coverage().unwrap();
            let after = preview.preview().metadata().selection_coverage().unwrap();
            assert_eq!(
                native.source(before.mask().slot()).unwrap().key(),
                preview.preview().source(after.mask().slot()).unwrap().key()
            );
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            let (base, history, delta) = prepared.into_parts();
            let commit = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Place image",
                        task: None,
                        gesture: None,
                    },
                    &|| Ok(()),
                )
                .unwrap();
            state
                .commit_native(gate(&[]), commit)
                .unwrap()
                .retire(&session, &|| session.cleanup_check())
                .unwrap();
            for (direction, mut expected) in [
                (Direction::Undo, original.clone()),
                (Direction::Redo, expected),
            ] {
                let moved = state
                    .native_history()
                    .unwrap()
                    .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
                    .unwrap();
                drop(state.move_native(gate(&[]), moved).unwrap());
                expected.revision = state.view().revision();
                pixels(state.native().unwrap(), &expected, &original);
            }
            drop((
                state,
                native,
                session,
                staged.seals,
                cache,
                runtime,
                connection,
            ));
        }
    }
}

#[test]
fn placement_failures_retire_sources_and_late_leases_protect_the_complete_frame() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
    let original = super::settings_tests::document(16);
    let path = file(&fixture, 16);
    let valid = json!({"op":"image.place","path":path,"layer":"outside","new_layer":false,"rect":[-5,-3,260,7]});
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged =
        CapturedScene::from_parts(original.clone(), "place-refusal".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    for fields in [
        json!({"parent":null}),
        json!({"new_layer":2}),
        json!({"mode":"bad"}),
        json!({"layer":"b_child"}),
        json!({"layer":"a_base"}),
        json!({"layer":"B"}),
        json!({"layer":"missing"}),
        json!({"rect":[0,0,0,1]}),
        json!({"rect":[-100000,0,100000,1]}),
        json!({"frame":1}),
        json!({"png":"invalid"}),
    ] {
        let mut invalid = valid.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(Engine::preview_edits(original.clone(), &[invalid.clone()]).is_err());
        let refused = prepare(
            capture(&state, &[valid.clone(), invalid]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let refused = prepare(capture(&state, &[valid.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > disk {
            Err("placement interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(refused.reason.contains("placement interrupted"));
    assert!(refused.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), disk);
    let prepared = prepare(capture(&state, &[valid]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Place",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let leases = [crate::engine::Lease {
        id: "late".into(),
        owner: "other".into(),
        description: "Outside placement rectangle".into(),
        scopes: vec![crate::engine::Scope {
            target: Some("outside".into()),
            rect: Some([500, 0, 510, 5]),
        }],
        expires: 100,
    }];
    let refused = state.commit_native(gate(&leases), commit).unwrap_err();
    assert!(refused.reason().contains("Reserved"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
    assert_eq!(runtime.disk().used().unwrap(), disk);
    drop((
        state,
        native,
        session,
        staged.seals,
        cache,
        runtime,
        connection,
    ));
}
