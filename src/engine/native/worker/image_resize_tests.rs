use super::*;
use super::{
    effect_create_tests::compare_raster,
    grouping_tests::gate,
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use serde_json::json;

#[test]
fn sparse_large_resize_admits_only_sampled_tiles_and_preserves_native_color_and_mask() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 16 * 1024 * 1024);
        let mut original = Document::new_depth(2, 2, depth).unwrap();
        original.width = 16000;
        original.height = 16000;
        let mut raster = crate::raster::Raster::new_depth(16000, 16000, depth);
        raster.set16(0, 0, [12347, 23459, 34571, 65535]);
        raster.set16(1, 0, [54321, 32109, 10987, 0]);
        raster.set16(15999, 15999, [43117, 23519, 13177, 65535]);
        original.layers[0].pixels = raster.clone();
        original.layers[0].mask = Some(crate::engine::Mask {
            enabled: true, linked: true, cache_key: crate::engine::id(),
            steps: vec![crate::engine::MaskStep {
                id: "paint-mask".into(), kind: "paint".into(), enabled: true, weight: 1., value: 0.,
                settings: serde_json::Value::Null, pixels: raster.clone(),
            }],
        });
        let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
        let mut staged = CapturedScene::from_parts(original, "sparse-resize".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let input = native.source(native.metadata().layer(0).unwrap().pixels().slot()).unwrap();
        assert!(input.has_physical_tiles([i32::MIN, i32::MIN, 1, 1]));
        assert!(!input.has_physical_tiles([256, 256, 15872, 15872]));
        let work = crate::geometry::native::resize_work(input, [15999, 15999], &|| Ok(())).unwrap();
        assert!(work > 0 && work < 1_000_000);
        let disk = runtime.disk().used().unwrap();
        let command = json!({"op":"image.resize","width":15999,"height":15999});
        let preview = prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
        let layer = preview.preview().metadata().layer(0).unwrap();
        for plane in [layer.pixels(), layer.mask().unwrap().steps().next().unwrap().pixels()] {
            let source = preview.preview().source(plane.slot()).unwrap();
            assert_eq!((source.width(), source.height(), source.depth()), (15999, 15999, depth));
            assert_eq!(source.physical_tiles().count(), 2);
            for tile in source.physical_tiles() {
                let (tx, ty) = tile.coordinate();
                let area = [(tx * 256) as i32, (ty * 256) as i32,
                    ((tx + 1) * 256).min(15999) as i32, ((ty + 1) * 256).min(15999) as i32];
                let region = source.prepare_region(crate::raster::backed::Region::new(area).unwrap(), &mut || Ok(())).unwrap();
                for y in area[1]..area[3] {
                    for x in area[0]..area[2] {
                        let sx = crate::geometry::input::coordinate(x as u32, 16000, 15999);
                        let sy = crate::geometry::input::coordinate(y as u32, 16000, 15999);
                        let expected = if depth == 16 {
                            crate::raster::sampling::sample16(sx, sy, |x, y| Ok::<_, String>(raster.get16(x, y))).unwrap()
                        } else {
                            crate::raster::sampling::sample(sx, sy, |x, y| Ok::<_, String>(raster.get(x, y))).unwrap()
                                .map(|v| u16::from(v) * 257)
                        };
                        assert_eq!(region.pixel16(x, y).unwrap(), expected);
                    }
                }
            }
            assert!(!source.has_physical_tiles([256, 256, 15872, 15872]));
            assert!(source.has_physical_tiles([15998, 15998, i32::MAX, i32::MAX]));
            assert!(!source.has_physical_tiles([15999, 15999, i32::MAX, i32::MAX]));
        }
        preview.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let stopped = prepare(capture(&state, &[command]), &session, &|| {
            if runtime.disk().used().unwrap() > disk { Err("Cancel sparse resize".into()) }
            else { Ok(()) }
        }).err().unwrap();
        assert!(stopped.reason.contains("Cancel sparse resize"));
        assert!(stopped.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
    }
}

#[test]
fn large_image_resize_resamples_sparse_liquify_confidence_at_the_canvas_limit() {
    use crate::{raster::{backed::Region, Raster}, selection::Coverage};
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let mut original = Document::new_depth(4000, 4000, 16).unwrap();
    let mut mask = Raster::new(4000, 4000);
    mask.set(3967, 3967, [137; 4]);
    mask.set(3968, 3968, [231; 4]);
    mask.set(3999, 3999, [197; 4]);
    let bounds = [3967, 3967, 4000, 4000];
    original.selection = Some(bounds);
    original.selection_coverage = Some(Coverage {
        mask: mask.clone(), bounds, origin: [0, 0], contours: vec![],
    });
    let layer = &mut original.layers[0];
    layer.x = 3960;
    layer.y = 3960;
    layer.pixels = Raster::new_depth(40, 40, 16);
    layer.pixels.set16(39, 39, [12347, 23459, 34571, 65535]);
    let layer = layer.id.clone();
    let original = Engine::preview_edits(original, &[
        json!({"op":"liquify.stroke","layer":layer,"points":[[3967.,3967.],[3968.,3968.]],"radius":2.,"strength":0.4}),
    ]).unwrap();
    let cache = LegacyTileCache::new(&runtime.resident(), 16).unwrap();
    let mut staged = CapturedScene::from_parts(original, "large-resize-coverage".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(())).unwrap();
    let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let before = native.metadata().layer(0).unwrap().effects().next().unwrap().clips().next().unwrap().1;
    assert_eq!([before.mask().width(), before.mask().height()], [4000, 4000]);
    let source = native.source(before.mask().slot()).unwrap();
    let work = crate::geometry::native::coverage_resize_work(source, [16000, 16000], &|| Ok(())).unwrap();
    assert!((1..4_000_000).contains(&work));

    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let preview = prepare(capture(&state, &[
        json!({"op":"image.resize","width":16000,"height":16000}),
    ]), &session, &|| Ok(())).unwrap();
    let layer = preview.preview().metadata().layer(0).unwrap();
    let after = layer.effects().next().unwrap().clips().next().unwrap().1;
    assert_eq!(after.origin(), before.origin().map(|v| v * 4));
    let source = preview.preview().source(after.mask().slot()).unwrap();
    assert_eq!((source.width(), source.height(), source.depth()), (16000, 16000, 8));
    assert!(!source.has_physical_tiles([0, 0, 15600, 15600]));
    for [l, t, r, b] in [[15862, 15862, 15882, 15882], [15992, 15992, 16000, 16000]] {
        let view = source.prepare_region(Region::new([l, t, r, b]).unwrap(), &mut || Ok(())).unwrap();
        for y in t..b {
            for x in l..r {
                let expected = crate::geometry::input::coverage(
                    x as u32, y as u32, [4000, 4000], [16000, 16000],
                    |x, y| Ok::<_, String>(mask.get(x, y)[0]),
                ).unwrap();
                assert_eq!(view.pixel(x, y).unwrap(), [expected; 4]);
            }
        }
    }
    assert_eq!(layer.pixels().depth(), 16);
    preview.abort(&session, &|| session.cleanup_check()).unwrap();
    assert_eq!(runtime.disk().used().unwrap(), disk);
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
}

#[test]
fn dense_resize_still_refuses_excess_sample_work_before_staging_outputs() {
    let fixture = Fixture::new();
    let (runtime, _connection) =
        fixture.runtime_with_budgets(128 * 1024 * 1024, 8 * 1024 * 1024);
    let original = super::layer_paste_tests::shared_large_document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 4).unwrap();
    let mut staged = CapturedScene::from_parts(original, "dense-resize-work".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(())).unwrap();
    let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let stopped = prepare(capture(&state, &[
        json!({"op":"image.resize","width":15999,"height":511}),
    ]), &session, &|| Ok(())).err().unwrap();
    assert!(stopped.reason.contains("128-million-sample"), "{}", stopped.reason);
    assert!(stopped.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), disk);
    assert_eq!(state.native().unwrap().key(), native.key());
}

#[test]
fn full_frame_dense_resize_reaches_staging_and_can_cancel_atomically() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 16 * 1024 * 1024);
    let mut original = Document::new_depth(2, 2, 16).unwrap();
    original.width = 16000;
    original.height = 16000;
    let mut raster = crate::raster::Raster::new_depth(16000, 16000, 16);
    let edge = crate::raster::TILE;
    let tile = std::sync::Arc::new([23459, 41963, 12347, 65535].repeat((edge * edge) as usize));
    for x in 0..16000u32.div_ceil(edge) {
        for y in 0..16000u32.div_ceil(edge) {
            raster.samples16.insert((x, y), tile.clone());
        }
    }
    original.layers[0].pixels = raster;
    let cache = LegacyTileCache::new(&runtime.resident(), 4).unwrap();
    let mut staged = CapturedScene::from_parts(original, "dense-full-frame-resize".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(())).unwrap();
    let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let stopped = prepare(capture(&state, &[json!({"op":"image.resize","width":15999,"height":16000})]), &session, &|| {
        if runtime.disk().used().unwrap() > disk {
            Err("Cancel admitted dense resize".into())
        } else {
            Ok(())
        }
    }).err().unwrap();
    assert!(stopped.reason.contains("Cancel admitted dense resize"), "{}", stopped.reason);
    assert!(stopped.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), disk);
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
}

fn pixels(native: &NativeDocument, expected: &Document, original: &Document) {
    super::import_tests::pixels(native, expected, original);
    for (layer, expected) in native.metadata().layers().zip(&expected.layers) {
        for (effect, expected) in layer.effects().zip(&expected.effects) {
            for (stroke, coverage) in effect.clips() {
                let expected: crate::selection::Coverage = serde_json::from_value(
                    expected.settings["strokes"][stroke as usize]["coverage"].clone(),
                )
                .unwrap();
                compare_raster(native, coverage.mask(), &expected.mask);
            }
        }
    }
}

fn exercise(original: Document, sizes: &[[u32; 2]]) {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(512 * 1024 * 1024, 256 * 1024 * 1024);
    let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
    let mut staged =
        CapturedScene::from_parts(original.clone(), "image-resize".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let disk = runtime.disk().used().unwrap();
    for (index, size) in sizes.iter().enumerate() {
        let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let mut commands = vec![
            json!({"op":if index%2==0 {"image.resize"} else {"resize"},"width":size[0],"height":size[1]}),
        ];
        if index == sizes.len() - 1 && *size != [original.width, original.height] {
            commands
                .push(json!({"op":"image.resize","width":original.width,"height":original.height}));
        }
        let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
        expected.revision = original.revision + 1;
        let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
        pixels(preview.preview(), &expected, &original);
        for (before, after) in native
            .metadata()
            .layers()
            .zip(preview.preview().metadata().layers())
        {
            if [before.pixels().width(), before.pixels().height()]
                == [after.pixels().width(), after.pixels().height()]
                && commands.len() == 1
                && (before.source().is_none() || *size == [original.width, original.height])
            {
                assert_eq!(
                    native.source(before.pixels().slot()).unwrap().key(),
                    preview
                        .preview()
                        .source(after.pixels().slot())
                        .unwrap()
                        .key()
                );
            }
        }
        if *size == [original.width, original.height] {
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        preview
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Resize image",
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
        drop(state);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    drop((native, session, staged.seals, cache, runtime, connection));
}

#[test]
fn native_resize_matches_the_resident_sampler_across_source_band_edges() {
    for depth in [8, 16] {
        let mut original = Document::new_depth(720, 740, depth).unwrap();
        let pixels = &mut original.layers[0].pixels;
        for y in (0..2).chain(252..260).chain(508..516).chain(738..740) {
            for x in (0..2).chain(252..260).chain(508..516).chain(718..720) {
                let red = (x * 39 + y * 31) as u16;
                let alpha = if (x + y) % 3 == 0 { 0 } else { 35009 };
                pixels.set16(x, y, [red, 23117, 41963, alpha]);
            }
        }
        // A half-scale tile requires more than one admitted source region.
        exercise(original, &[[360, 370], [719, 739], [720, 740]]);
    }
}

#[test]
fn native_resize_checks_cancellation_per_pinned_row_and_ownership_before_writing() {
    use std::cell::Cell;

    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let mut raster = crate::raster::Raster::new_depth(128, 128, 16);
    raster.set16(3, 4, [12347, 23459, 34571, 65535]);
    let input = session.fresh_fragment().unwrap();
    let plane = crate::raster::backed::Plane::from_resident(
        &raster,
        input.store().clone(),
        &mut || Ok(()),
    ).unwrap();
    let source = crate::raster::Surface::from_plane(plane).unwrap();
    let output = session.fresh_fragment().unwrap();
    let disk = runtime.disk().used().unwrap();
    let rows = Cell::new(0);
    let full = Cell::new(0);

    let cancelled = crate::geometry::native::resized(&source, [256, 256], output.store(),
        &|| { full.set(full.get() + 1); Ok(()) },
        &|| {
            rows.set(rows.get() + 1);
            if rows.get() == 32 { Err("Cancel pinned resize row".into()) } else { Ok(()) }
        },
    );
    assert_eq!(cancelled.err().as_deref(), Some("Cancel pinned resize row"));
    assert_eq!(rows.get(), 32);
    assert!((1..32).contains(&full.get()));
    assert_eq!(runtime.disk().used().unwrap(), disk);

    rows.set(0);
    let refused = crate::geometry::native::resized(&source, [256, 256], output.store(),
        &|| if rows.get() == 256 { Err(crate::large_document::Error::Changed) } else { Ok(()) },
        &|| { rows.set(rows.get() + 1); Ok(()) },
    );
    assert_eq!(rows.get(), 256);
    assert_eq!(refused.err(), Some(crate::large_document::Error::Changed.to_string()));
    assert_eq!(runtime.disk().used().unwrap(), disk);
    drop(source);
    session.abort_fragment(output, &mut || Ok(())).unwrap();
    session.abort_fragment(input, &mut || Ok(())).unwrap();
}

#[test]
fn native_image_resize_matches_sources_asymmetric_masks_spatial_effects_and_historic_clips() {
    for depth in [8, 16] {
        exercise(
            super::canvas_tests::document(depth),
            &[[4, 4], [11, 11], [8, 8], [16, 16]],
        );
        let mut doc = Document::new_depth(3, 3, depth).unwrap();
        doc.layers[0].pixels = crate::raster::Raster::new_depth(1, 1, depth);
        doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 0]);
        doc.layers[0].pixels.retained = Some(Box::new(crate::retained::Original::from(
            &doc.layers[0].pixels,
        )));
        exercise(doc, &[[4, 4], [2, 5], [6, 6]]);
    }
}

#[test]
fn extreme_downsampling_keeps_exact_native_samples_with_two_row_source_windows() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(8192, 3, depth).unwrap();
        for y in 0..3 {
            for x in 0..8192 {
                doc.layers[0].pixels.set16(
                    x,
                    y,
                    [
                        (x as u16).wrapping_mul(37),
                        23459 + (y as u16) * 113,
                        34571,
                        if x % 7 == 0 { 0 } else { 49153 },
                    ],
                );
            }
        }
        exercise(doc, &[[267, 257], [8192, 3]]);
    }
}

#[test]
fn resize_errors_cancellation_and_late_leases_retire_all_partial_outputs() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(512 * 1024 * 1024, 256 * 1024 * 1024);
    let original = super::canvas_tests::document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "resize-refusal".into(),
        vec![],
        None,
        None,
    )
    .stage(&runtime, &cache, &|| Ok(()))
    .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let valid = json!({"op":"image.resize","width":16,"height":16});
    for command in [
        json!({"op":"image.resize","width":0,"height":8}),
        json!({"op":"image.resize","width":8.5,"height":8}),
        json!({"op":"image.resize","width":8193,"height":8}),
        json!({"op":"image.resize","width":16,"height":4}),
        json!({"op":"image.resize","width":1000,"height":1000}),
    ] {
        assert!(Engine::preview_edits(original.clone(), &[command.clone()]).is_err());
        let refused = prepare(
            capture(&state, &[valid.clone(), command]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let locked = [
        json!({"op":"layer.update","layer":"A","locked":true}),
        valid.clone(),
    ];
    assert!(Engine::preview_edits(original.clone(), &locked).is_err());
    assert!(prepare(capture(&state, &locked), &session, &|| Ok(())).is_err());
    let cancelled = prepare(capture(&state, &[valid.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > disk {
            Err("resize interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(cancelled.reason.contains("resize interrupted"));
    assert!(cancelled.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), disk);
    let prepared = prepare(capture(&state, &[valid]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Resize image",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let leases = [crate::engine::Lease {
        id: "late".into(),
        owner: "other".into(),
        description: "Mask".into(),
        scopes: vec![crate::engine::Scope::layer("a_base")],
        expires: 100,
    }];
    let refused = state.commit_native(gate(&leases), commit).unwrap_err();
    assert!(refused.reason().contains("Reserved"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(runtime.disk().used().unwrap(), disk);
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
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

#[test]
fn cancellation_after_resized_coverage_sealing_releases_the_last_source_owner() {
    use std::cell::Cell;
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let original = Document::new_depth(8, 8, 16).unwrap();
    let target = original.layers[0].id.clone();
    let original = Engine::preview_edits(original, &[
        json!({"op":"selection","kind":"ellipse","rect":[1,1,7,7],"feather":1}),
        json!({"op":"liquify.stroke","layer":target,"points":[[2.,2.],[4.,4.]],"radius":2.,"strength":0.4}),
    ]).unwrap();
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(original, "resize-seal".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let command = json!({"op":"image.resize","width":16,"height":16});
    let baseline = runtime.disk().used().unwrap();
    let peak = Cell::new(baseline);
    let preview = prepare(capture(&state, &[command.clone()]), &session, &|| {
        let disk = runtime.disk().used().unwrap();
        if disk > peak.get() {
            peak.set(disk);
        }
        Ok(())
    })
    .unwrap();
    // The empty paint source writes no blocks; the last growth is the
    // resized coverage checkpoint, before its final seal guard returns.
    assert!(peak.get() > baseline);
    preview
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    let refused = prepare(capture(&state, &[command]), &session, &|| {
        if runtime.disk().used().unwrap() >= peak.get() {
            Err("coverage seal interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(refused.reason.contains("coverage seal interrupted"));
    assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    assert_eq!(state.native().unwrap().key(), native.key());
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
