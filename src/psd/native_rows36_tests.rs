use super::*;
use crate::{
    engine::{
        scene::{
            Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, Retained36,
            SceneIdentity, Warp36,
        },
        Document, Layer,
    },
    large_document::{DiskBudget, Limits, ResidentBudget, Store},
    psd::memory36::{Admitted, MetadataCharge},
    raster::{backed::Plane, Raster, Surface},
};
use std::sync::Arc;

struct Fixture {
    scene: Option<SceneSnapshot>,
    stores: Vec<Store>,
    paths: Vec<std::path::PathBuf>,
    pool: ResidentBudget,
}

#[test]
fn native_density_writes_the_original_mask_and_separate_density_after_reframing() {
    let check = || Ok(());
    for depth in [8, 16] {
        let mut doc = Document::new_depth(2, 1, depth).unwrap();
        for x in 0..2 {
            doc.layers[0].pixels.set16(x, 0, [0, 0, 0, 65535]);
        }
        let layer = doc.layers[0].id.clone();
        let mut doc = crate::engine::Engine::preview_edits(doc, &[
            serde_json::json!({"op":"mask.add","layer":layer,"value":0}),
            serde_json::json!({"op":"mask.step.add","layer":layer,"kind":"density","value":153}),
        ]).unwrap();
        let raw = if depth == 8 { [42 * 257, 80 * 257, 65535] } else { [923, 10319, 65535] };
        let mut source = Raster::new_depth(3, 1, depth);
        let paint = &mut doc.layers[0].mask.as_mut().unwrap().steps[1].pixels;
        for (x, sample) in raw.into_iter().enumerate() {
            let pixel = [sample, sample, sample, 65535];
            source.set16(x as i32, 0, pixel);
            if x > 0 {
                paint.set16(x as i32 - 1, 0, pixel);
            }
        }
        let mut retained = crate::retained::Original::from(&source);
        retained.matrix[4] = -1.;
        paint.retained = Some(Box::new(retained));
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let mut scratch = ScratchBytes::new(&fixture.pool, 65536, 65536).unwrap();
        for format in [super::super::structural::Format::Psd, super::super::structural::Format::Psb] {
            let plan = scene_save36::ScenePlan::new(fixture.scene(), format, &[], &check).unwrap();
            let mask = plan.standard().records()[0].mask.unwrap();
            assert_eq!(mask.bounds, [-1, 0, 2, 1]);
            assert_eq!(mask.default, 0);
            assert_eq!(mask.density, Some(153));
            let mut layers = Vec::new();
            let mut merged = Vec::new();
            plan.standard().write_layers(&mut layers, &mut rows, &mut scratch, &check, |_, _| Ok(()), |_, _| unreachable!()).unwrap();
            plan.standard().write_composite(&mut merged, &mut rows, &mut scratch, &check, |_, _| Ok(())).unwrap();
            let bytes = standard_file(format, depth, layers, merged);
            let index = super::super::structural::index_for_decode(&mut std::io::Cursor::new(&bytes), check).unwrap();
            let record = &index.layers[0];
            let channel = record.channels.iter().find(|channel| channel.id == -2).unwrap();
            let start = channel.span.offset as usize + 2;
            let native = values(&bytes[start..start + 3 * usize::from(depth / 8)], depth);
            assert_eq!(native, if depth == 8 { raw.map(|sample| sample / 257) } else { raw });
            let reopened = crate::psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            let mask = reopened.layers[0].mask.as_ref().unwrap();
            assert_eq!(mask.steps[2].value, 153.);
            assert_eq!(mask.steps[1].pixels.retained.as_ref().unwrap().pixels.get16(0, 0)[0], raw[0]);
        }
    }
}

#[test]
fn sparse_standard_rows_skip_absent_tiles_and_keep_hidden_native_color() {
    use std::cell::Cell;

    for depth in [8, 16] {
        let mut doc = Document::new_depth(512, 1024, depth).unwrap();
        doc.layers[0].pixels.set16(256, 256, [12347, 23459, 34571, 0]);
        doc.layers[0].pixels.set16(256, 768, [12347, 23459, 34571, 65535]);
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let stride = usize::from(depth / 8);
        let mut output = vec![0xa5; 512 * stride];
        let checks = Cell::new(0);
        let provenance = rows.provenance();
        let request = |first_row| RowRequest {
            provenance,
            source: RowSource::Layer(0),
            channel: 0,
            bounds: [0, 0, 512, 1024],
            first_row,
            rows: 1,
            depth,
        };
        let empty = request(0);
        let populated = request(256);
        rows.fill(empty, &mut output, &|| {
            checks.set(checks.get() + 1);
            Ok(())
        })
        .unwrap();
        assert!(output.iter().all(|&byte| byte == 0));
        assert_eq!(checks.get(), 2);
        rows.fill(populated, &mut output, &|| Ok(())).unwrap();
        let start = 256 * stride;
        if depth == 16 {
            assert_eq!(&output[start..start + stride], &12347u16.to_be_bytes());
        } else {
            assert_eq!(output[start], crate::raster::project16(12347));
        }
        let merged = |first_row| RowRequest {
            source: RowSource::MergedWhiteMatte,
            ..request(first_row)
        };
        rows.fill(merged(768), &mut output, &|| Ok(())).unwrap();
        checks.set(0);
        rows.fill(merged(512), &mut output, &|| {
            checks.set(checks.get() + 1);
            Ok(())
        })
        .unwrap();
        assert!(output.iter().all(|&byte| byte == 255));
        assert_eq!(checks.get(), 2);
    }
}

#[test]
fn wide_native_planar_stripes_reuse_pins_and_match_composite_with_partial_alpha() {
    let check = || Ok(());
    for depth in [8, 16] {
        let mut doc = Document::new_depth(769, 260, depth).unwrap();
        for (x, y, alpha) in [(3, 255, 0), (255, 255, 32455), (256, 256, 45678), (768, 258, 65535)] {
            doc.layers[0].pixels.set16(x, y, [12347, 23459, 34571, alpha]);
        }
        let fixture = Fixture::new(&doc);
        let mut reference = Rows36::new(fixture.scene()).unwrap();
        reference.single_paint = None;
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        assert!(rows.single_paint.is_some());
        let stride = usize::from(depth / 8);
        let provenance = rows.provenance();
        let request = |source, channel, first_row, count| RowRequest {
            provenance,
            source,
            channel,
            bounds: [0, 0, 769, 260],
            first_row,
            rows: count,
            depth,
        };
        let mut raw = vec![0; 769 * stride];
        let first = request(RowSource::Layer(0), 0, 255, 1);
        rows.fill(first, &mut raw, &check).unwrap();
        assert_eq!(rows.pinned_rows.as_ref().unwrap().y, 0);
        let expected = doc.layers[0].pixels.get16(3, 255)[0];
        let expected = if depth == 16 { expected } else { u16::from(crate::raster::project16(expected)) };
        assert_eq!(values(&raw[3 * stride..4 * stride], depth), [expected]);
        for channel in 0..4 {
            for (first, count) in [(254, 4), (258, 2)] {
                let request = request(RowSource::MergedWhiteMatte, channel, first, count);
                let mut actual = vec![0; 769 * stride * count as usize];
                let mut expected = actual.clone();
                rows.fill(request, &mut actual, &check).unwrap();
                reference.fill(request, &mut expected, &check).unwrap();
                assert_eq!(actual, expected);
                assert_eq!(rows.pinned_rows.as_ref().unwrap().y, 1);
            }
        }
        for (first, count) in [(254, 4), (258, 2)] {
            let mut actual = vec![0; 769 * stride * 4 * count as usize];
            let mut expected = actual.clone();
            rows.fill_composite_rgba(first, count, true, &mut actual, &check).unwrap();
            reference.fill_composite_rgba(first, count, true, &mut expected, &check).unwrap();
            assert_eq!(actual, expected);
            assert_eq!(rows.pinned_rows.as_ref().unwrap().y, 1);
            assert!(rows.composite.is_none());
        }
        let pending = request(RowSource::Layer(0), 1, 258, 1);
        let checks = std::cell::Cell::new(0);
        let error = rows.fill(pending, &mut raw, &|| {
            checks.set(checks.get() + 1);
            if checks.get() > 1 { Err("cancel pinned Photoshop row".into()) } else { Ok(()) }
        }).unwrap_err();
        assert_eq!(error, "cancel pinned Photoshop row");
        rows.finish_merged_pass();
        assert!(rows.pinned_rows.is_none());
    }
}

#[test]
fn merged_planar_pass_reuses_exact_native_samples_and_releases_its_frame() {
    let check = || Ok(());
    for depth in [8, 16] {
        let mut doc = Document::new_depth(2, 3, depth).unwrap();
        let pixel = if depth == 8 {
            [39, 147, 212, 111].map(|sample| sample * 257)
        } else {
            [9131, 31543, 57411, 34119]
        };
        doc.layers[0].pixels.set16(0, 1, pixel);
        doc.layers[0].pixels.set16(1, 2, [51 * 257, 92 * 257, 187 * 257, 65535]);
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let sample = usize::from(depth / 8);
        let fill = |rows: &mut Rows36<'_>| {
            (0..4).map(|channel| {
                let mut output = vec![0; 4 * sample];
                rows.fill(RowRequest {
                    provenance: rows.provenance(), source: RowSource::MergedWhiteMatte,
                    channel, bounds: [0, 0, 2, 3], first_row: 1, rows: 2, depth,
                }, &mut output, &check).unwrap();
                output
            }).collect::<Vec<_>>()
        };
        let expected = fill(&mut rows);
        rows.prepare_merged_pass(&check).unwrap();
        assert_eq!(rows.merged_pass.as_ref().unwrap().len(), 2 * 3 * 4 * sample);
        assert_eq!(fill(&mut rows), expected);
        let charged = fixture.pool.used();
        rows.finish_merged_pass();
        assert_eq!(charged - fixture.pool.used(), 2 * 3 * 4 * sample);

        let spare = fixture.pool.limit() - fixture.pool.used();
        let pressure = fixture.pool.reserve_bytes(spare - 16, spare).unwrap();
        rows.prepare_merged_pass(&check).unwrap();
        assert!(rows.merged_pass.is_none());
        drop(pressure);
        assert_eq!(fill(&mut rows), expected);
    }
}

fn standard_file(
    format: crate::psd::structural::Format, depth: u16, layers: Vec<u8>, merged: Vec<u8>,
) -> Vec<u8> {
    let mut bytes = b"8BPS".to_vec();
    let psb = format == crate::psd::structural::Format::Psb;
    bytes.extend((if psb { 2u16 } else { 1 }).to_be_bytes());
    bytes.extend([0; 6]);
    bytes.extend(4u16.to_be_bytes());
    bytes.extend(1u32.to_be_bytes());
    bytes.extend(2u32.to_be_bytes());
    bytes.extend(depth.to_be_bytes());
    bytes.extend(3u16.to_be_bytes());
    bytes.extend([0; 8]);
    if psb { bytes.extend((layers.len() as u64).to_be_bytes()); }
    else { bytes.extend((layers.len() as u32).to_be_bytes()); }
    bytes.extend(layers);
    bytes.extend(merged);
    bytes
}

#[test]
fn native_solid_fill_color_overlay_is_a_standard_editable_style() {
    use crate::psd::{row_stream::Source, structural::{self, Format}};
    use std::io::Cursor;

    let check = || Ok(());
    let base = [47, 103, 211, 255];
    let tint = [185, 69, 127, 255];
    let full = [tint[0], tint[1], tint[2]].map(|v| f64::from(v) / 255.);
    let partial = [0.1875, 0.625, 0.90625];
    for (depth, opacity, color, blend, enabled, linked, source_format) in [
        (8, 1., full, "normal", true, None, crate::psd::COLOR_OVERLAY_FORMAT),
        (16, 1., full, "normal", true, None, crate::psd::COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "normal", true, None, crate::psd::PRECISE_COLOR_OVERLAY_FORMAT),
        (16, 0.625, partial, "normal", true, None, crate::psd::PRECISE_COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "multiply", true, None, crate::psd::BLENDED_COLOR_OVERLAY_FORMAT),
        (16, 0.625, partial, "vivid_light", true, None, crate::psd::BLENDED_COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "multiply", true, Some(true), crate::psd::MASKED_COLOR_OVERLAY_FORMAT),
        (16, 0.625, partial, "vivid_light", true, Some(true), crate::psd::MASKED_COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "multiply", false, None, crate::psd::COLOR_OVERLAY_STATE_FORMAT),
        (16, 0.625, partial, "vivid_light", true, Some(false), crate::psd::COLOR_OVERLAY_STATE_FORMAT),
        (8, 0.625, partial, "multiply", true, None, crate::psd::COLOR_OVERLAY_LAYER_FORMAT),
        (16, 0.625, partial, "vivid_light", true, Some(true), crate::psd::COLOR_OVERLAY_LAYER_FORMAT),
        (8, 0.625, partial, "multiply", true, None, crate::psd::RASTER_COLOR_OVERLAY_FORMAT),
        (16, 0.625, partial, "vivid_light", true, None, crate::psd::RASTER_COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "multiply", true, Some(false), crate::psd::MULTI_COLOR_OVERLAY_FORMAT),
        (16, 0.625, partial, "vivid_light", true, None, crate::psd::MULTI_COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "multiply", true, None, crate::psd::HARD_SHADOW_FORMAT),
        (16, 0.625, partial, "normal", true, None, crate::psd::HARD_SHADOW_FORMAT),
        (8, 0.625, partial, "multiply", true, None, crate::psd::GLOBAL_LIGHT_STYLE_FORMAT),
        (16, 0.625, partial, "normal", true, None, crate::psd::GLOBAL_LIGHT_STYLE_FORMAT),
        (8, 0.625, partial, "multiply", true, None, crate::psd::SHADOW_STACK_FORMAT),
        (16, 0.625, partial, "normal", true, None, crate::psd::SHADOW_STACK_FORMAT),
        (8, 0.625, partial, "normal", true, None, crate::psd::STROKE_STYLE_FORMAT),
        (16, 0.625, partial, "normal", true, None, crate::psd::STROKE_STYLE_FORMAT),
        (8, 0.625, partial, "normal", true, Some(true), crate::psd::GRADIENT_OVERLAY_FORMAT),
        (16, 0.625, partial, "normal", true, Some(false), crate::psd::GRADIENT_OVERLAY_FORMAT),
        (8, 0.625, partial, "normal", true, Some(true), crate::psd::PATTERN_OVERLAY_FORMAT),
        (16, 0.625, partial, "normal", true, Some(false), crate::psd::PATTERN_OVERLAY_FORMAT),
        (8, 0.625, partial, "normal", true, Some(true), crate::psd::MULTI_GRADIENT_OVERLAY_FORMAT),
        (16, 0.625, partial, "normal", true, Some(false), crate::psd::MULTI_GRADIENT_OVERLAY_FORMAT),
        (8, 0.625, partial, "normal", true, Some(true), crate::psd::PLACED_PATTERN_OVERLAY_FORMAT),
        (16, 0.625, partial, "normal", true, Some(false), crate::psd::PLACED_PATTERN_OVERLAY_FORMAT),
        (8, 0.625, partial, "normal", true, None, crate::psd::MULTI_INNER_SHADOW_FORMAT),
        (16, 0.625, partial, "normal", true, None, crate::psd::MULTI_INNER_SHADOW_FORMAT),
        (8, 0.625, partial, "normal", true, None, crate::psd::MULTI_INSIDE_STROKE_FORMAT),
        (16, 0.625, partial, "normal", true, None, crate::psd::MULTI_INSIDE_STROKE_FORMAT),
    ] {
        let mut doc = Document::new_depth(2, 1, depth).unwrap();
        doc.layers[0].kind = "fill".into();
        doc.layers[0].color = base;
        let shadow_stack = source_format == crate::psd::SHADOW_STACK_FORMAT;
        let inner_stack = source_format == crate::psd::MULTI_INNER_SHADOW_FORMAT;
        let stroke_stack = source_format == crate::psd::MULTI_INSIDE_STROKE_FORMAT;
        let stroke = source_format == crate::psd::STROKE_STYLE_FORMAT;
        let gradient = source_format == crate::psd::GRADIENT_OVERLAY_FORMAT;
        let placed = source_format == crate::psd::PLACED_PATTERN_OVERLAY_FORMAT;
        let pattern = placed || source_format == crate::psd::PATTERN_OVERLAY_FORMAT;
        let gradients = source_format == crate::psd::MULTI_GRADIENT_OVERLAY_FORMAT;
        let has_pattern = pattern || gradients && depth == 16;
        let multi = source_format == crate::psd::MULTI_COLOR_OVERLAY_FORMAT || (shadow_stack || stroke || gradient || pattern) && depth == 16;
        let global_light = source_format == crate::psd::GLOBAL_LIGHT_STYLE_FORMAT;
        let shadow = global_light || source_format == crate::psd::HARD_SHADOW_FORMAT;
        let raster = stroke || stroke_stack || shadow || shadow_stack || inner_stack || gradient || pattern || gradients || source_format == crate::psd::RASTER_COLOR_OVERLAY_FORMAT || multi && depth == 16;
        if raster {
            doc.layers[0].kind = "paint".into();
            doc.layers[0].x = -1;
            doc.layers[0].pixels = crate::psd::styles::tests::color_overlay_raster(depth);
            if stroke || stroke_stack {
                let mut pixel = doc.layers[0].pixels.get16(1, 0);
                pixel[3] = 65535;
                doc.layers[0].pixels.set16(1, 0, pixel);
            }
        }
        doc.layers[0].mask = linked.map(|linked| {
            let mut mask = crate::psd::styles::tests::color_overlay_mask(depth);
            mask.linked = linked;
            mask
        });
        doc.layers[0].effects.push(crate::effects::Effect {
            id: crate::engine::id(), kind: "color_overlay".into(), enabled, weight: 1.,
            settings: serde_json::json!({"opacity":opacity, "blend":blend, "color":color}).into(),
        });
        if multi { crate::psd::styles::tests::extend_color_overlay_stack(&mut doc.layers[0]); }
        if shadow { doc.layers[0].effects[0] = crate::psd::styles::tests::standard_shadow_fixture(depth); }
        if global_light {
            let angle = if depth == 8 { 45u32 } else { 180u32 };
            doc.psd_global_angle = Some(angle.to_be_bytes());
            Arc::make_mut(&mut doc.layers[0].effects[0].settings)["global_light"] = serde_json::json!([angle, -12.5]);
        }
        if shadow_stack {
            use crate::psd::styles::tests::standard_shadow_fixture;
            doc.layers[0].effects.extend([standard_shadow_fixture(8), standard_shadow_fixture(16)]);
        }
        if inner_stack {
            doc.layers[0].effects = crate::psd::styles::tests::standard_inner_shadow_stack().into();
            if depth == 16 {
                doc.psd_global_angle = Some(45u32.to_be_bytes());
                Arc::make_mut(&mut doc.layers[0].effects[0].settings)["global_light"] = serde_json::json!([45, -12.5]);
            }
        }
        if stroke_stack {
            doc.layers[0].effects = crate::psd::styles::tests::standard_inside_stroke_stack().into();
        }
        if stroke {
            use crate::psd::styles::tests::{standard_shadow_fixture, standard_stroke_fixture};
            if depth == 8 { doc.layers[0].effects[0] = standard_stroke_fixture("outside"); }
            else { doc.layers[0].effects.extend([standard_shadow_fixture(8), standard_stroke_fixture("inside"), standard_shadow_fixture(16)]); }
        }
        if gradient {
            let overlay = crate::psd::styles::tests::standard_gradient_overlay_fixture(depth);
            if multi { doc.layers[0].effects.insert(0, overlay); }
            else { doc.layers[0].effects[0] = overlay; }
        }
        if pattern {
            use crate::psd::styles::tests::{standard_gradient_overlay_fixture, standard_pattern_overlay_fixture};
            let mut overlay = standard_pattern_overlay_fixture(depth);
            if placed {
                let settings = Arc::make_mut(&mut overlay.settings);
                settings["phase_x"] = serde_json::json!(3.);
                settings["phase_y"] = serde_json::json!(-2.);
                settings["bitmap_angle"] = serde_json::json!(if depth == 16 { -90. } else { 0. });
            }
            if multi {
                doc.layers[0].effects.insert(0, standard_gradient_overlay_fixture(depth));
                doc.layers[0].effects.insert(0, overlay);
            } else {
                doc.layers[0].effects[0] = overlay;
            }
        }
        if gradients {
            use crate::psd::styles::tests::{standard_gradient_stack_fixture, standard_pattern_overlay_fixture};
            let stack = standard_gradient_stack_fixture(depth);
            if has_pattern {
                let pattern = standard_pattern_overlay_fixture(depth);
                drop(doc.layers[0].effects.splice(0..0, std::iter::once(pattern).chain(stack)));
            } else {
                doc.layers[0].effects = stack.into();
            }
        }
        if source_format == crate::psd::COLOR_OVERLAY_LAYER_FORMAT {
            doc.layers[0].opacity = 101. / 255.;
            if depth == 16 {
                doc.layers[0].blend = "multiply".into();
                doc.layers[0].psd_metadata.push(crate::psd::LayerMetadata {
                    key: "infx".into(), data: vec![1, 0, 0, 0],
                });
            }
            let mut backdrop = Layer::new("Backdrop", "fill", 2, 1);
            backdrop.pixels = Raster::new_depth(2, 1, depth);
            backdrop.color = [17, 204, 71, 255];
            doc.layers.push(backdrop);
        }
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let mut scratch = ScratchBytes::new(&fixture.pool, 65536, 65536).unwrap();
        let mut actual = ScratchBytes::new(&fixture.pool, 65536, 65536).unwrap();
        for format in [Format::Psd, Format::Psb] {
            let plan = scene_save36::ScenePlan::new(fixture.scene(), format, &[], &check).unwrap();
            let record = plan.standard().records().last().unwrap();
            assert_eq!(record.source, RowSource::OriginalLayer(0));
            assert_eq!(record.solid, (!raster).then_some(base));
            if raster { assert_eq!(record.bounds, [-1, 0, 2, 1]); }
            assert_eq!(record.standard_style.unwrap().source_format(), source_format);
            assert_eq!(record.mask.map(|mask| mask.linked), linked);
            let mut layers = Vec::new();
            let mut merged = Vec::new();
            plan.standard().write_layers(&mut layers, &mut rows, &mut scratch, &check, |_, _| Ok(()), |_, _| unreachable!()).unwrap();
            plan.standard().write_composite(&mut merged, &mut rows, &mut scratch, &check, |_, _| Ok(())).unwrap();
            let mut bytes = standard_file(format, depth, layers, merged);
            if let Some(angle) = doc.psd_global_angle {
                let resource = crate::psd::global_angle_resource(&angle);
                bytes.splice(30..34, (resource.len() as u32).to_be_bytes().into_iter().chain(resource));
            }
            let reopened = crate::psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            assert_eq!(reopened.psd_global_angle, doc.psd_global_angle);
            if raster {
                assert_eq!(reopened.layers[0].x, -1);
                for x in 0..3 {
                    assert_eq!(reopened.layers[0].pixels.get16(x, 0), doc.layers[0].pixels.get16(x, 0));
                }
            } else {
                assert_eq!(reopened.layers[0].color, base);
            }
            assert_eq!(reopened.layers[0].opacity, doc.layers[0].opacity);
            assert_eq!(reopened.layers[0].blend, doc.layers[0].blend);
            assert_eq!(reopened.layers[0].effects.len(), doc.layers[0].effects.len());
            for (actual, expected) in reopened.layers[0].effects.iter().zip(&doc.layers[0].effects) {
                assert_eq!(actual.kind, expected.kind);
                assert_eq!(actual.settings, expected.settings);
                assert_eq!(actual.enabled, expected.enabled);
            }
            assert_eq!(reopened.layers[0].mask.as_ref().map(|mask| mask.linked), linked);
            assert_eq!(reopened.export_png().unwrap(), doc.export_png().unwrap());

            let index = structural::index(&mut Cursor::new(&bytes), structural::Limits::current(), check).unwrap();
            let asset = index.global_tags.iter().find(|tag| tag.key == *b"Patt");
            assert_eq!(asset.is_some(), has_pattern);
            let tags = &index.layers.last().unwrap().tags;
            let style_key = if multi || gradients || inner_stack || stroke_stack { *b"lmfx" } else { *b"lfx2" };
            let style = tags.iter().find(|tag| tag.key == style_key).unwrap();
            assert_eq!(tags.iter().filter(|tag| matches!(&tag.key, b"lfx2" | b"lmfx")).count(), 1);
            let mask_mode = tags.iter().find(|tag| tag.key == *b"lmgm");
            assert_eq!(mask_mode.is_some(), linked == Some(false));
            let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
            let replay = scene_save36::ScenePlan::for_import(fixture.scene(), &source, &index, &[], &check).unwrap();
            replay.standard().compare(&source, &index, &mut rows, &mut scratch, &mut actual, &check, |_, _| Ok(())).unwrap();
            drop(source);
            if let Some(asset) = asset {
                let at = (asset.data.offset + asset.data.bytes - 1) as usize;
                bytes[at] ^= 1;
                let changed = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
                assert!(replay.standard().compare(&changed, &index, &mut rows, &mut scratch, &mut actual, &check, |_, _| Ok(())).is_err());
                bytes[at] ^= 1;
            }
            if let Some(mode) = mask_mode {
                let offset = mode.data.offset as usize;
                assert_eq!(&bytes[offset..offset + 4], &[0; 4]);
                bytes[offset] = 1;
                let changed = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
                assert!(replay.standard().compare(&changed, &index, &mut rows, &mut scratch, &mut actual, &check, |_, _| Ok(())).is_err());
                bytes[offset] = 0;
            }
            bytes[(style.data.offset + style.data.bytes - 1) as usize] ^= 1;
            let changed = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
            assert!(replay.standard().compare(&changed, &index, &mut rows, &mut scratch, &mut actual, &check, |_, _| Ok(())).is_err());
        }
        if stroke {
            if depth == 8 { doc.layers[0].effects.push(crate::psd::styles::tests::standard_shadow_fixture(16)); }
            else { Arc::make_mut(&mut doc.layers[0].effects[4].settings)["position"] = serde_json::json!("outside"); }
        } else if shadow_stack {
            let last = doc.layers[0].effects.len() - 1;
            doc.layers[0].effects.swap(0, last);
        } else if shadow {
            Arc::make_mut(&mut doc.layers[0].effects[0].settings)["size"] = serde_json::json!(5.);
        } else if gradient {
            let settings = Arc::make_mut(&mut doc.layers[0].effects[0].settings);
            settings["color_stops"][1]["midpoint"] = serde_json::json!(0.625);
        } else if pattern {
            Arc::make_mut(&mut doc.layers[0].effects[0].settings)["phase_x"] = serde_json::json!(0.5);
        } else if gradients {
            doc.layers[0].effects[usize::from(has_pattern)].weight = 0.5;
        } else if source_format == crate::psd::COLOR_OVERLAY_LAYER_FORMAT {
            if let Some(grouped) = doc.layers[0].psd_metadata.iter_mut().find(|tag| tag.key == "infx") {
                grouped.data[0] = 0;
            } else {
                doc.layers[0].opacity = 0.5;
            }
        } else {
            doc.layers[0].effects[0].weight = 0.5;
        }
        let fallback = Fixture::new(&doc);
        let plan = scene_save36::ScenePlan::new(fallback.scene(), Format::Psb, &[], &check).unwrap();
        assert!(plan.standard().records().iter().all(|record| record.standard_style.is_none()));
    }
}

#[test]
fn native_readback_accepts_legacy_mixed_overlay_records() {
    use crate::psd::{row_stream::Source, structural::{self, Format}};
    use std::io::Cursor;

    let check = || Ok(());
    let mut doc = Document::new_depth(2, 1, 16).unwrap();
    doc.layers[0].kind = "fill".into();
    doc.layers[0].color = [47, 103, 211, 255];
    doc.layers[0].effects.push(crate::effects::Effect {
        id: crate::engine::id(), kind: "color_overlay".into(), enabled: true, weight: 1.,
        settings: serde_json::json!({"opacity":1., "blend":"normal", "color":[1., 0., 0.]}).into(),
    });
    let mut layer = Layer::new("Partial Overlay", "paint", 3, 1);
    layer.x = -1;
    layer.pixels = Raster::new_depth(3, 1, 16);
    for x in 0..3 {
        layer.pixels.set16(x, 0, [0, 65535, 0, 65535]);
    }
    layer.effects.push(crate::effects::Effect {
        id: crate::engine::id(), kind: "color_overlay".into(), enabled: true, weight: 1.,
        settings: serde_json::json!({"opacity":0.625, "blend":"normal", "color":[0., 1., 0.]}).into(),
    });
    let mut gradient = layer.clone();
    gradient.id = crate::engine::id();
    gradient.name = "Gradient Overlay".into();
    let mut overlay = crate::psd::styles::tests::standard_gradient_overlay_fixture(16);
    let settings = Arc::make_mut(&mut overlay.settings);
    settings["blend"] = serde_json::json!("normal");
    for stop in settings["color_stops"].as_array_mut().unwrap() {
        stop["color"] = serde_json::json!([0., 1., 0.]);
    }
    let mut disabled = overlay.clone();
    disabled.id = crate::engine::id();
    disabled.enabled = false;
    let mut pattern = crate::psd::styles::tests::standard_pattern_overlay_fixture(16);
    pattern.enabled = false;
    gradient.effects = vec![pattern, overlay, disabled];
    doc.layers.extend([layer, gradient]);

    // A different weight selects the rendered layout. Both weights leave
    // the same samples here because each overlay and its base share a color.
    let mut legacy = doc.clone();
    for layer in &mut legacy.layers[1..] {
        layer.effects[0].weight = 0.5;
    }
    let written = Fixture::new(&legacy);
    let plan = scene_save36::ScenePlan::new(written.scene(), Format::Psb, &[], &check).unwrap();
    assert!(plan.standard().records()[0].standard_style.is_none());
    assert!(plan.standard().records()[1].standard_style.is_none());
    assert_eq!(plan.standard().records()[2].standard_style.unwrap().source_format(), crate::psd::COLOR_OVERLAY_FORMAT);
    let mut layers = Vec::new();
    let mut merged = Vec::new();
    let mut rows = Rows36::new(written.scene()).unwrap();
    let mut scratch = ScratchBytes::new(&written.pool, 65536, 65536).unwrap();
    plan.standard().write_layers(&mut layers, &mut rows, &mut scratch, &check, |_, _| Ok(()), |_, _| unreachable!()).unwrap();
    plan.standard().write_composite(&mut merged, &mut rows, &mut scratch, &check, |_, _| Ok(())).unwrap();
    let bytes = standard_file(Format::Psb, 16, layers, merged);
    let index = structural::index(&mut Cursor::new(&bytes), structural::Limits::current(), check).unwrap();
    let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
    let original = Fixture::new(&doc);
    let replay = scene_save36::ScenePlan::for_import(original.scene(), &source, &index, &[], &check).unwrap();
    assert_eq!(replay.standard().records()[0].bounds, [0, 0, 2, 1]);
    assert_eq!(replay.standard().records()[1].bounds, [0, 0, 2, 1]);
    let mut rows = Rows36::new(original.scene()).unwrap();
    let mut scratch = ScratchBytes::new(&original.pool, 65536, 65536).unwrap();
    let mut actual = ScratchBytes::new(&original.pool, 65536, 65536).unwrap();
    replay.standard().compare(&source, &index, &mut rows, &mut scratch, &mut actual, &check, |_, _| Ok(())).unwrap();
}

#[test]
fn native_pattern_fill_and_overlay_share_one_bound_standard_asset() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use crate::psd::{row_stream::Source, structural::{self, Format as PhotoshopFormat}};
    use std::io::Cursor;

    let check = || Ok(());
    let depth = 16u16;
    let mut doc = Document::new_depth(2, 1, depth).unwrap();
    let tile = crate::raster::png16(2, 1, &[12347, 23459, 34571, 40001, 19277, 30671, 40991, 65535]).unwrap();
    doc.layers[0].kind = "fill".into();
    doc.layers[0].color = [255; 4];
    doc.layers[0].effects.push(crate::effects::Effect {
        id: crate::engine::id(), kind: "pattern_overlay".into(), enabled: true, weight: 1.,
        settings: serde_json::json!({"pattern":"bitmap","bitmap_png":STANDARD.encode(tile),
            "sampling":"nearest","opacity":1.,"use_pattern_alpha":true,
            "phase_x":1.,"phase_y":-2.,"bitmap_angle":-90.}).into(),
    });
    let mut overlay = doc.layers[0].effects[0].clone();
    overlay.id = crate::engine::id();
    let settings = Arc::make_mut(&mut overlay.settings);
    settings.as_object_mut().unwrap().remove("use_pattern_alpha");
    settings["phase_x"] = serde_json::json!(-1.);
    settings["phase_y"] = serde_json::json!(3.);
    settings["bitmap_angle"] = serde_json::json!(180.);
    let mut paint = Layer::new("Pattern Overlay", "paint", 2, 1);
    paint.pixels = Raster::new_depth(2, 1, depth);
    paint.pixels.set16(0, 0, [24689, 42043, 56791, 43021]);
    paint.effects.push(overlay);
    doc.layers.insert(0, paint);
    let fixture = Fixture::new(&doc);
    let mut rows = Rows36::new(fixture.scene()).unwrap();
    let mut scratch = ScratchBytes::new(&fixture.pool, 65536, 65536).unwrap();
    let mut actual = ScratchBytes::new(&fixture.pool, 65536, 65536).unwrap();
    for format in [PhotoshopFormat::Psd, PhotoshopFormat::Psb] {
        let plan = scene_save36::ScenePlan::new(fixture.scene(), format, &[], &check).unwrap();
        assert!(plan.standard().records()[0].pattern.is_some());
        let style = plan.standard().records()[1].standard_style.unwrap();
        assert_eq!(style.source_format(), crate::psd::PLACED_PATTERN_OVERLAY_FORMAT);
        let mut layers = Vec::new();
        let mut merged = Vec::new();
        plan.standard().write_layers(&mut layers, &mut rows, &mut scratch, &check, |_, _| Ok(()), |_, _| unreachable!()).unwrap();
        plan.standard().write_composite(&mut merged, &mut rows, &mut scratch, &check, |_, _| Ok(())).unwrap();
        let mut bytes = standard_file(format, depth, layers, merged);
        let index = structural::index(&mut Cursor::new(&bytes), structural::Limits::current(), check).unwrap();
        let fill = index.layers[0].tags.iter().find(|tag| tag.key == *b"PtFl").unwrap();
        let fill_at = fill.data.offset as usize;
        let descriptor = crate::psd::styles::descriptor_block(&bytes[fill_at..fill_at + fill.data.bytes as usize]).unwrap();
        let phase = &descriptor["items"]["phase"]["value"]["items"];
        assert_eq!(phase["Hrzn"]["value"].as_f64(), Some(1.));
        assert_eq!(phase["Vrtc"]["value"].as_f64(), Some(-2.));
        assert_eq!(descriptor["items"]["Angl"]["value"][1].as_f64(), Some(-90.));
        let pattern = index.global_tags.iter().find(|tag| tag.key == *b"Patt").unwrap();
        let at = pattern.data.offset as usize;
        let payload = &bytes[at..at + pattern.data.bytes as usize];
        assert_eq!(crate::psd::patterns::decode(payload, "Patt", &Default::default()).unwrap().len(), 1);
        let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
        let replay = scene_save36::ScenePlan::for_import(fixture.scene(), &source, &index, &[], &check).unwrap();
        replay.standard().compare(&source, &index, &mut rows, &mut scratch, &mut actual, &check, |_, _| Ok(())).unwrap();
        drop(source);
        let mut changed = bytes.clone();
        changed[fill_at + fill.data.bytes as usize - 1] ^= 1;
        let changed = Source::new(Cursor::new(&changed), changed.len() as u64).unwrap();
        assert!(replay.standard().compare(&changed, &index, &mut rows, &mut scratch, &mut actual, &check, |_, _| Ok(())).is_err());
        bytes[pattern.data.offset as usize + 8] ^= 1;
        let changed = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
        assert!(replay.standard().compare(&changed, &index, &mut rows, &mut scratch, &mut actual, &check, |_, _| Ok(())).is_err());
    }
}
impl Fixture {
    fn new(doc: &Document) -> Self {
        Self::build(doc, false)
    }
    fn build(doc: &Document, foreign: bool) -> Self {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(32 * 1024 * 1024, 4).unwrap();
        let mut paths = Vec::new();
        let mut stores = Vec::new();
        for _ in 0..if foreign { 2 } else { 1 } {
            let path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("pb-standard-rows-{}", crate::engine::id()));
            stores.push(
                Store::create_with_disk_budget(
                    &path,
                    pool.clone(),
                    disk.clone(),
                    Limits {
                        disk_bytes: 32 * 1024 * 1024,
                        max_blocks: 64,
                        max_resident_blocks: 16,
                        manifest_bytes: 65536,
                    },
                )
                .unwrap(),
            );
            paths.push(path);
        }
        struct Capture<'a> {
            store: &'a Store,
            entries: Vec<Surface>,
        }
        impl RasterCollector for Capture<'_> {
            fn resident(&mut self, raster: &Raster) -> Result<RasterRef36> {
                let mut current = raster.clone();
                current.retained = None;
                let surface = Surface::stage_raster(&current, self.store.clone(), &mut || Ok(()))
                    .map_err(|e| e.to_string())?;
                self.entries.push(surface);
                let id = self.entries.len() as u32;
                let retained = raster
                    .retained
                    .as_ref()
                    .map(|original| -> Result<_> {
                        Ok(Box::new(Retained36 {
                            pixels: self.resident(&original.pixels)?,
                            matrix: original.matrix,
                            projective: original.projective,
                            warps: original.warps.iter().map(Warp36::capture).collect(),
                        }))
                    })
                    .transpose()?;
                Ok(RasterRef36 {
                    id,
                    width: raster.width,
                    height: raster.height,
                    depth: raster.depth,
                    retained,
                })
            }
            fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
                Err("Fixture accepts resident oracle capture only".into())
            }
        }
        let mut capture = Capture {
            store: &stores[0],
            entries: vec![],
        };
        let metadata = Document36::capture(doc, &mut capture).unwrap();
        if foreign {
            let source =
                Surface::stage_raster(&doc.layers[0].pixels, stores[1].clone(), &mut || Ok(()))
                    .unwrap();
            let blocks = source
                .physical_tiles()
                .map(|t| (t.coordinate(), t.reference()))
                .collect();
            capture.entries[0] = Surface::from_plane(
                Plane::from_blocks(
                    doc.layers[0].pixels.width,
                    doc.layers[0].pixels.height,
                    doc.bit_depth,
                    stores[0].clone(),
                    blocks,
                )
                .unwrap(),
            )
            .unwrap();
        }
        let owner = MetadataCharge::reserve(&pool, 1024 * 1024, &|| Ok(())).unwrap();
        let pending = PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), owner),
            RasterRoster::new(capture.entries).unwrap(),
            pool.clone(),
            disk,
        )
        .unwrap();
        let identity = SceneIdentity {
            project_id: "standard-row-fixture".into(),
            document_id: doc.id.clone(),
            revision: doc.revision,
        };
        let scene = SceneSnapshot::from_native(
            identity,
            crate::engine::native::NativeDocument::from_pending(pending).unwrap(),
            vec![],
            None,
            None,
        )
        .unwrap();
        Self {
            scene: Some(scene),
            stores,
            paths,
            pool,
        }
    }
    fn scene(&self) -> &SceneSnapshot {
        self.scene.as_ref().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.scene.take());
        self.stores.clear();
        assert_eq!(self.pool.used(), 0);
        let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        for path in &self.paths {
            let target = std::fs::canonicalize(path).unwrap();
            assert_eq!(target.parent(), Some(parent.as_path()));
            assert!(target
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pb-standard-rows-"));
            std::fs::remove_dir_all(target).unwrap();
        }
    }
}
fn doc(depth: u16) -> Document {
    let mut doc = Document::new_depth(4, 3, depth).unwrap();
    if depth == 16 {
        doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 0]);
        doc.layers[0]
            .pixels
            .set16(1, 0, [50001, 30003, 10001, 32769]);
    } else {
        doc.layers[0].pixels.set(0, 0, [17, 99, 201, 0]);
        doc.layers[0].pixels.set(1, 0, [222, 144, 33, 127]);
    }
    doc
}
fn request(
    rows: &Rows36<'_>,
    source: RowSource,
    channel: i16,
    bounds: [i32; 4],
    first_row: u32,
    count: u32,
) -> RowRequest {
    RowRequest {
        provenance: rows.provenance(),
        source,
        channel,
        bounds,
        first_row,
        rows: count,
        depth: rows.scene.metadata().depth(),
    }
}
fn values(bytes: &[u8], depth: u16) -> Vec<u16> {
    if depth == 16 {
        bytes
            .chunks_exact(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .collect()
    } else {
        bytes.iter().map(|v| u16::from(*v)).collect()
    }
}
fn sample(raster: &Raster, x: i32, y: i32, depth: u16) -> [u16; 4] {
    if depth == 16 {
        raster.get16(x, y)
    } else {
        raster.get(x, y).map(u16::from)
    }
}

#[test]
fn native_clipped_fill_uses_the_base_shape_in_standard_composite_rows() {
    for depth in [8, 16] {
        let (doc, expected) = crate::psd::fill_opacity::clipped_fixture(depth);
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let maximum = if depth == 8 { 255 } else { 65535 };
        let alpha = u32::from(expected[3]);
        for (index, channel) in [0, 1, 2, 3].into_iter().enumerate() {
            let request = request(&rows, RowSource::MergedWhiteMatte, channel, [0, 0, 2, 1], 0, 1);
            let mut output = vec![0; 2 * usize::from(depth / 8)];
            rows.fill(request, &mut output, &|| Ok(())).unwrap();
            let samples = if channel == 3 {
                [expected[3], 0]
            } else {
                let mixed = (u32::from(expected[index]) * alpha + maximum * (maximum - alpha)
                    + maximum / 2) / maximum;
                [mixed as u16, maximum as u16]
            };
            assert_eq!(values(&output, depth), samples);
        }
    }
}

#[test]
fn native_blend_if_preserves_source_and_writes_standard_ranges_and_composite() {
    use crate::psd::{blend_ranges, structural::Format};
    for depth in [8, 16] {
        let mut doc = Document::new_depth(2, 1, depth).unwrap();
        let layer = &mut doc.layers[0];
        layer.pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
        layer.pixels.set16(1, 0, [45679, 23459, 34571, 65535]);
        layer.blend = "linear_dodge".into();
        crate::psd::fill_opacity::update(&mut layer.psd_metadata, &serde_json::json!({"fill_opacity":128})).unwrap();
        let pairs = [blend_ranges::DEFAULT, [128, 128, 255, 255, 0, 0, 255, 255],
            blend_ranges::DEFAULT, blend_ranges::DEFAULT];
        blend_ranges::update(&mut layer.psd_metadata, &serde_json::json!({"blend_if":pairs})).unwrap();
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let layer = &doc.layers[0];
        let white = if depth == 8 { 255u32 } else { 65535 };
        let [first, second] = [0, 1].map(|x| sample(&layer.pixels, x, 0, depth)[0]);
        let alpha = white / 255 * 128;
        let matte = (u32::from(second) * alpha + white * (white - alpha) + white / 2) / white;
        for (source, expected) in [
            (RowSource::MergedWhiteMatte, [white as u16, matte as u16]),
            (RowSource::OriginalLayer(0), [first, second]),
        ] {
            let request = request(&rows, source, 0, [0, 0, 2, 1], 0, 1);
            let mut output = vec![0; usize::from(depth / 4)];
            rows.fill(request, &mut output, &|| Ok(())).unwrap();
            assert_eq!(values(&output, depth), expected);
        }
        for format in [Format::Psd, Format::Psb] {
            let plan = scene_save36::ScenePlan::new(fixture.scene(), format, &[], &|| Ok(())).unwrap();
            let mut scratch = ScratchBytes::new(&fixture.pool, 512 * 1024, 512 * 1024).unwrap();
            let mut bytes = Vec::new();
            plan.standard()
                .write_layers(&mut bytes, &mut rows, &mut scratch, &|| Ok(()), |_, _| Ok(()), |_, _| unreachable!())
                .unwrap();
            let mut expected = 32u32.to_be_bytes().to_vec();
            expected.extend_from_slice(pairs.as_flattened());
            assert!(bytes.windows(expected.len()).any(|window| window == expected));
            assert!(!bytes.windows(8).any(|window| window == b"8BIM@rng"));
        }
    }
}

#[test]
fn native_folder_blend_if_writes_only_the_opening_record_and_gates_merged_rows() {
    use crate::psd::{blend_ranges, structural::Format, LayerMetadata as PsdLayerMetadata, CLOSED_FOLDER, CLOSING_LAYER_ID};
    for depth in [8, 16] {
        let mut doc = Document::new_depth(2, 1, depth).unwrap();
        doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
        doc.layers[0].pixels.set16(1, 0, [45679, 23459, 34571, 65535]);
        let mut folder = Layer::new("Folder", "group", 2, 1);
        folder.pixels.convert_depth(depth);
        let mut backdrop = Layer::new("Backdrop", "paint", 2, 1);
        backdrop.pixels.convert_depth(depth);
        backdrop.pixels.set16(1, 0, [54321, 32123, 11223, 65535]);
        doc.layers[0].parent = Some(folder.id.clone());
        let pairs = [blend_ranges::DEFAULT, [0, 0, 100, 100, 0, 0, 255, 255],
            blend_ranges::DEFAULT, blend_ranges::DEFAULT];
        blend_ranges::update(&mut folder.psd_metadata, &serde_json::json!({"blend_if":pairs})).unwrap();
        folder.psd_metadata.extend([
            PsdLayerMetadata { key: "lyid".into(), data: 17u32.to_be_bytes().to_vec() },
            PsdLayerMetadata { key: CLOSED_FOLDER.into(), data: 2u32.to_be_bytes().to_vec() },
            PsdLayerMetadata { key: CLOSING_LAYER_ID.into(), data: 23u32.to_be_bytes().to_vec() },
        ]);
        doc.layers.insert(0, folder);
        doc.layers.push(backdrop);

        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let merged = request(&rows, RowSource::MergedWhiteMatte, 0, [0, 0, 2, 1], 0, 1);
        let mut output = vec![0; usize::from(depth / 4)];
        rows.fill(merged, &mut output, &|| Ok(())).unwrap();
        assert_eq!(values(&output, depth), [
            sample(&doc.layers[1].pixels, 0, 0, depth)[0],
            sample(&doc.layers[2].pixels, 1, 0, depth)[0],
        ]);
        for format in [Format::Psd, Format::Psb] {
            let plan = scene_save36::ScenePlan::new(fixture.scene(), format, &[], &|| Ok(())).unwrap();
            let records = plan.standard().records().iter().filter(|record|
                record.metadata.iter().any(|metadata| metadata.key == blend_ranges::TAG));
            assert_eq!(records.map(|record| record.section).collect::<Vec<_>>(), [2]);
            let ids = plan.standard().records().iter().filter_map(|record|
                record.metadata.iter().find(|metadata| metadata.key == *b"lyid")
                    .map(|metadata| (record.section, u32::from_be_bytes(metadata.bytes.try_into().unwrap())))
            ).collect::<Vec<_>>();
            assert_eq!(ids, [(3, 23), (2, 17)]);
            let mut scratch = ScratchBytes::new(&fixture.pool, 512 * 1024, 512 * 1024).unwrap();
            let mut bytes = Vec::new();
            plan.standard().write_layers(&mut bytes, &mut rows, &mut scratch, &|| Ok(()), |_, _| Ok(()), |_, _| unreachable!())
                .unwrap();
            assert_eq!(bytes.windows(8).filter(|window| *window == b"8BIMlyid").count(), 2);
            assert!(bytes.windows(16).any(|window| window == b"8BIMlsct\0\0\0\x04\0\0\0\x02"));
            let mut expected = 32u32.to_be_bytes().to_vec();
            expected.extend_from_slice(pairs.as_flattened());
            assert_eq!(bytes.windows(expected.len()).filter(|window| *window == expected).count(), 1);
        }
    }
}

#[test]
fn native_save_emits_one_disabled_clipping_flag_on_the_base() {
    use crate::psd::structural::Format;
    for format in [Format::Psd, Format::Psb] {
        let (mut doc, _) = crate::psd::fill_opacity::clipped_fixture(16);
        let base = &mut doc.layers[1].psd_metadata;
        crate::psd::fill_opacity::update(base, &serde_json::json!({"fill_opacity":255})).unwrap();
        crate::psd::clipping::update(base, &serde_json::json!({"blend_clipped_as_group":false})).unwrap();
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let plan = scene_save36::ScenePlan::new(fixture.scene(), format, &[], &|| Ok(())).unwrap();
        let mut scratch = ScratchBytes::new(&fixture.pool, 512 * 1024, 512 * 1024).unwrap();
        let mut bytes = Vec::new();
        plan.standard()
            .write_layers(&mut bytes, &mut rows, &mut scratch, &|| Ok(()), |_, _| Ok(()), |_, _| unreachable!())
            .unwrap();
        let tag = b"8BIMclbl";
        let offsets: Vec<_> = bytes.windows(tag.len()).enumerate()
            .filter_map(|(offset, window)| (window == tag).then_some(offset))
            .collect();
        assert_eq!(offsets.len(), 1);
        let offset = offsets[0] + tag.len();
        assert_eq!(&bytes[offset..offset + 8], &[0, 0, 0, 4, 0, 0, 0, 0]);
    }
}

#[test]
fn editable_sources_read_the_captured_native_raster_without_resolving_fonts() {
    use crate::fonts::{self, FontError, InstalledFont};
    use serde_json::json;
    let font = InstalledFont {
        family: "Saved text fixture".into(),
        postscript: "PeerBrushSavedText-Regular".into(),
        style: "normal".into(),
        weight: 400,
        stretch: 5,
        index: 0,
        sha256: "0".repeat(64),
    };
    let guard = fonts::test_fonts(vec![(font.clone(), Err(FontError::Missing))]);
    for depth in [8, 16] {
        for content in [
            json!({"kind":"text","text":"Saved appearance","font":font,"size":12.,
                "line_height":1.2,"align":"left","color":[12501,23003,34507,65535]}),
            json!({"kind":"shape","shape":"rectangle","bounds":[0.,0.,4.,3.],
                "points":[],"closed":true,"fill":[12501,23003,34507,65535],
                "stroke":[0,0,0,0],"stroke_width":0.}),
            json!({"kind":"svg","svg":"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"3\"><rect width=\"4\" height=\"3\" fill=\"red\"/></svg>"}),
            json!({"kind":"bezier","nodes":[{"point":[0.,0.]},{"point":[4.,0.]},{"point":[2.,3.]}],
                "closed":true,"fill":[12501,23003,34507,65535],"stroke":[0,0,0,0],"stroke_width":0.}),
        ] {
            for styled in [false, true] {
                let mut doc = doc(depth);
                doc.layers[0].source = Some(Arc::new(
                    serde_json::from_value::<crate::source::Source>(
                        json!({"width":4,"height":3,"content":content}),
                    )
                    .unwrap(),
                ));
                // The saved appearance intentionally differs from a fresh render.
                // Read-only consumers must preserve it even with missing fonts.
                if styled {
                    doc.layers[0].effects.push(crate::effects::Effect {
                        id: crate::engine::id(),
                        kind: "invert".into(),
                        enabled: true,
                        weight: 0.67,
                        settings: json!({}).into(),
                    });
                }
                let definition = doc.layers[0].source.clone();
                let fixture = Fixture::new(&doc);
                let captured = json!(fixture.scene().metadata().layers().next().unwrap().source());
                let mut rows = Rows36::new(fixture.scene()).unwrap();
                let low = (depth == 8).then(|| crate::effects::prepare(&doc, &[None]).unwrap());
                let high = (depth == 16).then(|| crate::depth16::prepare(&doc).unwrap());
                let composite8 = (depth == 8).then(|| crate::filters::prepare8(&doc).unwrap());
                let composite16 = (depth == 16).then(|| crate::filters::prepare16(&doc).unwrap());
                for source in [
                    RowSource::OriginalLayer(0),
                    RowSource::Layer(0),
                    RowSource::BakedComposite,
                ] {
                    for channel in [-1, 0, 1, 2] {
                        let c = if channel == -1 { 3 } else { channel as usize };
                        let r = request(&rows, source, channel, [0, 0, 4, 3], 0, 3);
                        let mut bytes = vec![0xcd; 12 * usize::from(depth / 8)];
                        rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
                        let expected = (0..3)
                            .flat_map(|y| (0..4).map(move |x| (x, y)))
                            .map(|(x, y)| {
                                if source == RowSource::BakedComposite {
                                    if depth == 16 {
                                        composite16.as_ref().unwrap().get(x, y)[c]
                                    } else {
                                        u16::from(composite8.as_ref().unwrap().get(x, y)[c])
                                    }
                                } else if source == RowSource::Layer(0) && styled {
                                    if depth == 16 {
                                        high.as_ref().unwrap()[0].as_ref().unwrap().get(x, y)[c]
                                    } else {
                                        u16::from(
                                            low.as_ref().unwrap()[0].as_ref().unwrap().get(x, y)[c],
                                        )
                                    }
                                } else {
                                    sample(&doc.layers[0].pixels, x, y, depth)[c]
                                }
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(
                            values(&bytes, depth),
                            expected,
                            "{depth} {source:?} {styled}"
                        );
                    }
                }
                for target in [None, Some(doc.layers[0].id.as_str())] {
                    let frame = crate::render::native_preview::test_frame(
                        fixture.scene(),
                        None,
                        4,
                        target,
                        false,
                        &mut || Ok(()),
                    )
                    .unwrap();
                    assert_eq!(
                        &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                        &doc.preview(None, 4, target, false).unwrap().2
                    );
                }
                assert_eq!(
                    captured,
                    json!(fixture.scene().metadata().layers().next().unwrap().source())
                );
                assert_eq!(doc.layers[0].source, definition);
                assert_eq!(guard.requests(), 0);
            }
        }
    }
}
#[test]
fn actual_raw_current_and_original_channels_preserve_hidden_rgb_offsets_and_native_low_bits() {
    for depth in [8, 16] {
        let mut doc = doc(depth);
        doc.layers[0].x = -2;
        doc.layers[0].y = -1;
        let mut original = Raster::new_depth(2, 2, depth);
        original.set16(0, 0, [65535; 4]);
        doc.layers[0].pixels.retained = Some(Box::new(crate::retained::Original::from(&original)));
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        assert!(std::ptr::eq(rows.scene(), fixture.scene()));
        for source in [RowSource::Layer(0), RowSource::OriginalLayer(0)] {
            for channel in [-1, 0, 1, 2] {
                let r = request(&rows, source, channel, [-2, -1, 2, 2], 0, 3);
                let mut output = vec![0xcd; 12 * usize::from(depth / 8)];
                rows.fill(r, &mut output, &|| Ok(())).unwrap();
                let expected = (0..3)
                    .flat_map(|y| (0..4).map(move |x| (x, y)))
                    .map(|(x, y)| {
                        sample(&doc.layers[0].pixels, x, y, depth)
                            [if channel == -1 { 3 } else { channel as usize }]
                    })
                    .collect::<Vec<_>>();
                assert_eq!(values(&output, depth), expected);
            }
        }
    }
}
#[test]
fn procedural_fill_channels_use_colour_and_preserve_transparent_colour_at_both_depths() {
    for depth in [8, 16] {
        let mut doc = doc(depth);
        doc.layers[0].kind = "fill".into();
        doc.layers[0].color = [17, 99, 201, 0];
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        for source in [RowSource::Layer(0), RowSource::OriginalLayer(0)] {
            for channel in [-1, 0, 1, 2] {
                let r = request(&rows, source, channel, [0, 0, 4, 3], 1, 2);
                let mut bytes = vec![0xcd; 8 * usize::from(depth / 8)];
                rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
                let expected = u16::from(
                    doc.layers[0].color[if channel == -1 { 3 } else { channel as usize }],
                ) * if depth == 16 { 257 } else { 1 };
                assert!(values(&bytes, depth).iter().all(|v| *v == expected));
            }
        }
    }
}
#[test]
fn real_composite_and_standard_white_matte_match_legacy_native_channels() {
    for depth in [8, 16] {
        let mut doc = doc(depth);
        doc.layers[0].effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "levels".into(),
            enabled: true,
            weight: 0.71,
            settings: serde_json::json!({"output_black":0.17,"output_white":0.83}).into(),
        });
        let mut fill = Layer::new("backdrop", "fill", 4, 3);
        fill.pixels = Raster::new_depth(4, 3, depth);
        fill.color = [23, 77, 141, 113];
        fill.opacity = 0.47;
        doc.layers.push(fill);
        doc.filters.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "blur".into(),
            enabled: true,
            weight: 0.63,
            settings: serde_json::json!({"radius":2.3}).into(),
        });
        doc.filters.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "levels".into(),
            enabled: true,
            weight: 0.63,
            settings: serde_json::json!({"output_black":0.79,"output_white":0.21}).into(),
        });
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let high = if depth == 16 {
            Some(crate::filters::prepare16(&doc).unwrap())
        } else {
            None
        };
        let low = if depth == 8 {
            Some(crate::filters::prepare8(&doc).unwrap())
        } else {
            None
        };
        for channel in [-1, 0, 1, 2] {
            let r = request(
                &rows,
                RowSource::OriginalLayer(0),
                channel,
                [0, 0, 4, 3],
                0,
                1,
            );
            let mut bytes = vec![0; 4 * usize::from(depth / 8)];
            rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
            let expected: Vec<_> = (0..4)
                .map(|x| {
                    let p = if depth == 16 {
                        doc.layers[0].pixels.get16(x, 0)
                    } else {
                        doc.layers[0].pixels.get(x, 0).map(u16::from)
                    };
                    p[if channel == -1 { 3 } else { channel as usize }]
                })
                .collect();
            assert_eq!(values(&bytes, depth), expected);
        }
        for source in [RowSource::BakedComposite, RowSource::MergedWhiteMatte] {
            let alpha = if source == RowSource::MergedWhiteMatte {
                3
            } else {
                -1
            };
            for channel in [alpha, 0, 1, 2] {
                for (first, count) in [(0, 1), (1, 2)] {
                    let r = request(&rows, source, channel, [0, 0, 4, 3], first, count);
                    let mut bytes = vec![0xcd; 4 * count as usize * usize::from(depth / 8)];
                    rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
                    let mut expected = Vec::new();
                    for y in first..first + count {
                        for x in 0..4 {
                            let p = if let Some(high) = &high {
                                high.get(x, y as i32)
                            } else {
                                low.as_ref().unwrap().get(x, y as i32).map(u16::from)
                            };
                            let c = if channel == -1 { 3 } else { channel as usize };
                            let mut value = p[c];
                            if source == RowSource::MergedWhiteMatte && c < 3 {
                                let maximum = if depth == 16 { 65535u64 } else { 255 };
                                value = ((u64::from(value) * u64::from(p[3])
                                    + maximum * (maximum - u64::from(p[3]))
                                    + maximum / 2)
                                    / maximum) as u16;
                            }
                            expected.push(value);
                        }
                    }
                    assert_eq!(
                        values(&bytes, depth),
                        expected,
                        "{depth} {source:?} {channel}"
                    );
                }
            }
        }
    }
}
#[test]
fn raw_adjustment_canvas_padding_reads_current_physical_source_without_a_derived_substitute() {
    let mut doc = doc(16);
    doc.layers[0].kind = "adjustment".into();
    doc.layers[0].pixels = Raster::new_depth(2, 1, 16);
    doc.layers[0]
        .pixels
        .set16(0, 0, [12347, 23459, 34571, 45679]);
    doc.layers[0].x = 1;
    doc.layers[0].y = 1;
    let fixture = Fixture::new(&doc);
    let mut rows = Rows36::new(fixture.scene()).unwrap();
    let r = request(&rows, RowSource::OriginalLayer(0), 0, [0, 0, 4, 3], 0, 3);
    let mut bytes = vec![0xcd; 24];
    rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
    let expected = (0..3)
        .flat_map(|y| (0..4).map(move |x| (x, y)))
        .map(|(x, y)| doc.layers[0].pixels.get16(x - 1, y - 1)[0])
        .collect::<Vec<_>>();
    assert_eq!(values(&bytes, 16), expected);
    let r = request(&rows, RowSource::Layer(0), 0, [1, 1, 3, 2], 0, 1);
    let mut bytes = vec![0xcd; 4];
    assert!(rows.fill(r, &mut bytes, &|| Ok(())).is_err());
    assert_eq!(bytes, [0xcd; 4]);
}
#[test]
fn styled_current_channels_expand_to_canvas_while_original_channels_keep_the_cropped_source() {
    for depth in [8, 16] {
        let mut doc = doc(depth);
        let layer = &mut doc.layers[0];
        layer.x = 1;
        layer.y = 1;
        layer.pixels = Raster::new_depth(2, 1, depth);
        layer.pixels.set16(0, 0, [12347, 23459, 34571, 0]);
        layer.pixels.set16(1, 0, [50001, 30003, 10001, 32769]);
        layer.psd_metadata.push(crate::psd::LayerMetadata {
            key: "infx".into(), data: vec![1, 0, 0, 0],
        });
        layer.psd_metadata.push(crate::psd::LayerMetadata {
            key: "lclr".into(), data: vec![0, 11, 0, 0, 0, 0, 0, 0],
        });
        layer.psd_metadata.push(crate::psd::LayerMetadata {
            key: "sn2P".into(), data: vec![0, 0, 0, 1],
        });
        layer.psd_metadata.push(crate::psd::LayerMetadata {
            key: "lyid".into(), data: 17u32.to_be_bytes().to_vec(),
        });
        layer.locked = true;
        layer.psd_metadata.push(crate::psd::LayerMetadata {
            key: "lspf".into(), data: 0x8000_0000u32.to_be_bytes().to_vec(),
        });
        for (kind, settings) in [
            ("blur", serde_json::json!({"radius":1.3})),
            ("stroke", serde_json::json!({"size":1.7})),
            ("blur", serde_json::json!({"radius":2.3})),
        ] {
            layer.effects.push(crate::effects::Effect {
                id: crate::engine::id(),
                kind: kind.into(),
                enabled: true,
                weight: 0.67,
                settings: settings.into(),
            });
        }
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let low = if depth == 8 {
            Some(crate::effects::prepare(&doc, &[None]).unwrap())
        } else {
            None
        };
        let high = if depth == 16 {
            Some(crate::depth16::prepare(&doc).unwrap())
        } else {
            None
        };
        for channel in [-1, 0, 1, 2] {
            let c = if channel == -1 { 3 } else { channel as usize };
            let r = request(&rows, RowSource::Layer(0), channel, [0, 0, 4, 3], 0, 3);
            let mut bytes = vec![0; 12 * usize::from(depth / 8)];
            rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
            let expected: Vec<_> = (0..3)
                .flat_map(|y| (0..4).map(move |x| (x, y)))
                .map(|(x, y)| {
                    if let Some(high) = &high {
                        high[0].as_ref().unwrap().get(x, y)[c]
                    } else {
                        u16::from(low.as_ref().unwrap()[0].as_ref().unwrap().get(x, y)[c])
                    }
                })
                .collect();
            assert_eq!(values(&bytes, depth), expected);
            let r = request(
                &rows,
                RowSource::OriginalLayer(0),
                channel,
                [1, 1, 3, 2],
                0,
                1,
            );
            let mut bytes = vec![0; 2 * usize::from(depth / 8)];
            rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
            assert_eq!(
                values(&bytes, depth),
                (0..2)
                    .map(|x| sample(&doc.layers[0].pixels, x, 0, depth)[c])
                    .collect::<Vec<_>>()
            );
        }
        for format in [crate::psd::structural::Format::Psd, crate::psd::structural::Format::Psb] {
            let plan = scene_save36::ScenePlan::new(fixture.scene(), format, &[], &|| Ok(())).unwrap();
            let mut scratch = ScratchBytes::new(&fixture.pool, 512 * 1024, 512 * 1024).unwrap();
            let mut layers = Vec::new();
            plan.standard()
                .write_layers(&mut layers, &mut rows, &mut scratch, &|| Ok(()), |_, _| Ok(()), |_, _| unreachable!())
                .unwrap();
            for tag in [
                b"8BIMinfx\0\0\0\x04\x01\0\0\0".as_slice(),
                b"8BIMlclr\0\0\0\x08\0\x0b\0\0\0\0\0\0".as_slice(),
                b"8BIMsn2P\0\0\0\x04\0\0\0\x01".as_slice(),
                b"8BIMlyid\0\0\0\x04\0\0\0\x11".as_slice(),
                b"8BIMlspf\0\0\0\x04\x80\0\0\0".as_slice(),
            ] {
                assert_eq!(layers.windows(tag.len()).filter(|entry| *entry == tag).count(), 1);
            }
            assert_eq!(layers.windows(8).filter(|entry| *entry == b"8BIMlspf").count(), 1);
        }
    }
}
#[test]
fn disabled_raw_mask_keeps_native_samples_while_unprepared_colour_still_refuses() {
    for depth in [8, 16] {
        let mut doc = doc(depth);
        let mut pixels = Raster::new_depth(4, 3, depth);
        if depth == 16 {
            pixels.set16(1, 0, [12347, 0, 0, 65535]);
        } else {
            pixels.set(1, 0, [37, 0, 0, 255]);
        }
        doc.layers[0].mask = Some(crate::engine::Mask {
            enabled: false,
            linked: true,
            cache_key: crate::engine::id(),
            steps: vec![crate::engine::MaskStep {
                id: "paint-mask".into(),
                kind: "paint".into(),
                enabled: true,
                value: 0.,
                weight: 1.,
                settings: serde_json::Value::Null,
                pixels,
            }],
        });
        doc.layers[0].effects.push(crate::effects::Effect {
            id: "effect".into(),
            kind: "drop_shadow".into(),
            enabled: true,
            weight: 1.,
            settings: serde_json::json!({}).into(),
        });
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let mut bytes = vec![0xcd; 4 * usize::from(depth / 8)];
        let r = request(&rows, RowSource::OriginalLayer(0), -2, [0, 0, 4, 3], 0, 1);
        rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
        if depth == 16 {
            assert_eq!(
                bytes,
                [65535u16, 12347, 65535, 65535]
                    .map(u16::to_be_bytes)
                    .concat()
            );
        } else {
            assert_eq!(bytes, [255, 37, 255, 255]);
        }
        bytes.fill(0xcd);
        let r = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
        assert!(rows
            .fill(r, &mut bytes, &|| Ok(()))
            .unwrap_err()
            .contains("regional"));
        assert!(bytes.iter().all(|v| *v == 0xcd));
        let r = request(&rows, RowSource::OriginalLayer(0), 0, [0, 0, 4, 3], 0, 1);
        rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
        let r = request(&rows, RowSource::BakedComposite, 0, [0, 0, 4, 3], 0, 1);
        assert!(rows.fill(r, &mut bytes, &|| Ok(())).is_err());
        drop(rows);
        drop(fixture);
        let mut doc = doc;
        doc.layers[0].effects.clear();
        doc.layers[0].source = Some(
            (crate::source::Source {
                width: 4,
                height: 3,
                matrix: [1., 0., 0., 1., 0., 0.],
                content: crate::source::Content::Svg {
                    svg: "<svg/>".into(),
                },
            })
            .into(),
        );
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let r = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
        assert!(rows.fill(r, &mut bytes, &|| Ok(())).is_err());
    }
}
#[test]
fn malformed_foreign_target_and_cancelled_requests_preserve_source_and_output() {
    let doc = doc(16);
    let fixture = Fixture::new(&doc);
    let other = Fixture::new(&doc);
    let mut rows = Rows36::new(fixture.scene()).unwrap();
    let foreign = Rows36::new(other.scene()).unwrap();
    let good = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
    let mut requests = vec![
        RowRequest {
            provenance: foreign.provenance(),
            ..good
        },
        RowRequest { channel: 4, ..good },
        RowRequest {
            source: RowSource::Layer(9),
            ..good
        },
        RowRequest {
            bounds: [1, 0, 5, 3],
            ..good
        },
        RowRequest { depth: 8, ..good },
        RowRequest {
            first_row: u32::MAX,
            ..good
        },
        RowRequest { rows: 0, ..good },
        RowRequest {
            source: RowSource::MergedWhiteMatte,
            bounds: [-1, 0, 3, 3],
            ..good
        },
    ];
    for request in requests.drain(..) {
        let mut bytes = [0xcd; 8];
        assert!(rows.fill(request, &mut bytes, &|| Ok(())).is_err());
        assert_eq!(bytes, [0xcd; 8]);
    }
    let mut bytes = [0xcd; 8];
    assert_eq!(
        rows.fill(good, &mut bytes, &|| Err("cancelled source guard".into()))
            .unwrap_err(),
        "cancelled source guard"
    );
    assert_eq!(bytes, [0xcd; 8]);
    assert!(rows.fill(good, &mut bytes[..7], &|| Ok(())).is_err());
    assert_eq!(doc.layers[0].pixels.get16(0, 0), [12347, 23459, 34571, 0]);
}
#[test]
fn genuine_foreign_block_and_resident_refusal_never_become_zero_pixels() {
    let doc = doc(16);
    let fixture = Fixture::build(&doc, true);
    let mut rows = Rows36::new(fixture.scene()).unwrap();
    let r = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
    let mut bytes = [0xcd; 8];
    assert!(rows.fill(r, &mut bytes, &|| Ok(())).is_err());
    assert_eq!(bytes, [0xcd; 8]);
    drop(rows);
    drop(fixture);
    let fixture = Fixture::new(&doc);
    let mut rows = Rows36::new(fixture.scene()).unwrap();
    let r = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
    let free = fixture.pool.limit() - fixture.pool.used();
    let held = fixture.pool.reserve_bytes(free, free).unwrap();
    assert!(rows.fill(r, &mut bytes, &|| Ok(())).is_err());
    assert_eq!(bytes, [0xcd; 8]);
    drop(held);
    rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
    assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 12347);
}

#[test]
fn explicit_zero_and_exact_128_kipixel_admission_do_not_expand_the_stripe() {
    for depth in [8, 16] {
        let doc = Document::new_depth(1024, 128, depth).unwrap();
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        for channel in [-1, 0, 1, 2] {
            let r = request(&rows, RowSource::Zero, channel, [0, 0, 1024, 128], 0, 128);
            let mut bytes = vec![0xcd; 128 * 1024 * usize::from(depth / 8)];
            rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
            assert!(bytes.iter().all(|v| *v == 0));
        }
        let r = request(&rows, RowSource::Zero, 0, [0, 0, 1024, 129], 0, 129);
        let mut bytes = vec![0xcd; 129 * 1024 * usize::from(depth / 8)];
        assert!(rows
            .fill(r, &mut bytes, &|| Ok(()))
            .unwrap_err()
            .contains("128 KiPixel"));
        assert!(bytes.iter().all(|v| *v == 0xcd));
    }
}

#[test]
fn cancellation_during_actual_source_pin_restores_shared_admission_without_sampling() {
    for depth in [8, 16] {
        let doc = doc(depth);
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let r = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
        let mut bytes = vec![0; 4 * usize::from(depth / 8)];
        rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
        let baseline = fixture.pool.used();
        let payload = fixture.pool.payload_used();
        let disk = fixture.scene().disk_budget().used().unwrap();
        let reached = std::cell::Cell::new(false);
        bytes.fill(0xcd);
        let error = rows
            .fill(r, &mut bytes, &|| {
                if fixture.pool.payload_used() > payload {
                    reached.set(true);
                    Err("cancelled while source payload admitted".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(reached.get());
        assert_eq!(error, "cancelled while source payload admitted");
        assert!(bytes.iter().all(|v| *v == 0xcd));
        assert_eq!(fixture.pool.used(), baseline);
        assert_eq!(fixture.scene().disk_budget().used().unwrap(), disk);
    }
}

#[test]
fn actual_psd_psb_standard_emission_and_comparison_use_bounded_captured_rows() {
    use crate::psd::{
        row_stream::Source,
        structural::{self, Format as PhotoshopFormat, Header},
    };
    use std::io::Cursor;
    for format in [PhotoshopFormat::Psd, PhotoshopFormat::Psb] {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(1024, 256, depth).unwrap();
            doc.layers[0]
                .pixels
                .set16(1, 0, [12347, 23459, 34571, 32769]);
            crate::psd::fill_opacity::update(
                &mut doc.layers[0].psd_metadata,
                &serde_json::json!({"fill_opacity":128}),
            ).unwrap();
            let fixture = Fixture::new(&doc);
            let mut rows = Rows36::new(fixture.scene()).unwrap();
            // Bound the coexisting in-memory test container/sections/record indexes
            // before any vector grows. This fixture creates no destination files.
            let _outputs = fixture
                .pool
                .reserve_bytes(16 * 1024 * 1024, 16 * 1024 * 1024)
                .unwrap();
            let header = Header {
                format,
                width: 1024,
                height: 256,
                depth,
                channels: 4,
            };
            let scene_plan = scene_save36::ScenePlan::new(fixture.scene(), format, &[], &|| Ok(())).unwrap();
            let plan = scene_plan.standard();
            let mut scratch = ScratchBytes::new(&fixture.pool, 512 * 1024, 512 * 1024).unwrap();
            let mut layers = Vec::new();
            let mut composite = Vec::new();
            let written = plan
                .write_layers(
                    &mut layers,
                    &mut rows,
                    &mut scratch,
                    &|| Ok(()),
                    |_, _| Ok(()),
                    |_, _| unreachable!(),
                )
                .unwrap();
            let merged = plan
                .write_composite(&mut composite, &mut rows, &mut scratch, &|| Ok(()), |_, _| Ok(()))
                .unwrap();
            assert_eq!(written.bytes, layers.len() as u64);
            assert_eq!(merged.bytes, composite.len() as u64);
            let mut bytes = b"8BPS".to_vec();
            bytes.extend(
                (if format == PhotoshopFormat::Psb {
                    2u16
                } else {
                    1u16
                })
                .to_be_bytes(),
            );
            bytes.extend([0; 6]);
            bytes.extend(4u16.to_be_bytes());
            bytes.extend(header.height.to_be_bytes());
            bytes.extend(header.width.to_be_bytes());
            bytes.extend(depth.to_be_bytes());
            bytes.extend(3u16.to_be_bytes());
            bytes.extend([0; 8]);
            if format == PhotoshopFormat::Psb {
                bytes.extend((layers.len() as u64).to_be_bytes());
            } else {
                bytes.extend((layers.len() as u32).to_be_bytes());
            }
            bytes.extend(&layers);
            bytes.extend(&composite);
            let index = structural::index(
                &mut Cursor::new(&bytes),
                structural::Limits::current(),
                || Ok(()),
            )
            .unwrap();
            let fill = index.layers[0].tags.iter().find(|tag| tag.key == *b"iOpa").unwrap();
            let offset = fill.data.offset as usize;
            assert_eq!(&bytes[offset..offset + 4], &[128, 0, 0, 0]);
            let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
            let mut actual = ScratchBytes::new(&fixture.pool, 512 * 1024, 512 * 1024).unwrap();
            let compared = plan
                .compare(
                    &source,
                    &index,
                    &mut rows,
                    &mut scratch,
                    &mut actual,
                    &|| Ok(()),
                    |_, _| Ok(()),
                )
                .unwrap();
            assert_eq!(compared.samples(), 8 * 1024 * 256);
        }
    }
}

#[test]
fn clipped_adjustment_standard_rows_preserve_original_samples_and_reopen_native_layout_generations() {
    for linked in [false, true] {
        for blend in ["normal", "multiply"] {
            for kind in ["invert", "levels", "levels_rgb", "levels_combined", "curves", "curves_rgb", "curves_shifted", "curves_spline", "posterize", "threshold", "legacy_brightness", "brightness_contrast", "exposure", "channel_mixer", "black_white", "selective_color", "vibrance", "photo_filter", "gradient_map", "photoshop_color_balance", "photoshop_hue_saturation", "color_lookup", "color_lookup_cube", "color_lookup_3dl", "color_lookup_look"] {
                check_clipped_adjustment_standard_rows(linked, blend, kind);
            }
        }
    }
    for blend in ["linear_light", "vivid_light", "pin_light", "hard_mix", "darker_color", "color"] {
        check_clipped_adjustment_standard_rows(true, blend, "invert");
    }
}

fn check_clipped_adjustment_standard_rows(linked: bool, blend: &str, kind: &str) {
    use crate::psd::{
        row_stream::Source,
        structural::{self, Format as PhotoshopFormat},
    };
    use std::io::Cursor;
    for depth in [8, 16] {
        if matches!(kind, "curves_spline" | "levels_combined" | "legacy_brightness" | "brightness_contrast" | "exposure" | "channel_mixer" | "black_white" | "selective_color" | "vibrance" | "photo_filter" | "gradient_map" | "photoshop_color_balance" | "photoshop_hue_saturation") && depth != 8 {
            continue;
        }
        let mut doc = Document::new_depth(2, 1, depth).unwrap();
        doc.psd_global_metadata = crate::psd::global_metadata::tests::fixtures_with_credentials_trailer();
        doc.layers[0]
            .pixels
            .set16(0, 0, [12347, 23459, 34571, 65535]);
        doc.layers[0].effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "invert".into(),
            enabled: true,
            weight: 0.43,
            settings: serde_json::json!({}).into(),
        });
        doc.layers[0].effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "blur".into(),
            enabled: true,
            weight: 0.57,
            settings: serde_json::json!({"radius":1.3}).into(),
        });
        let base = doc.layers[0].id.clone();
        let settings = match kind {
            "levels" => serde_json::json!({"output_black":1.,"output_white":0.,
                "preset":{"kind":1,"name":"Levels proof"}}),
            "levels_rgb" => serde_json::json!({"input_mapping":"exact","channels":[{"output_black":1.,"output_white":0.},{"gamma":1.25},{}]}),
            "levels_combined" => serde_json::json!({"input_mapping":"exact","composition":"photoshop_byte",
                "black":48./255.,"white":135./255.,"output_black":74./255.,"output_white":141./255.,
                "channels":[{"black":81./255.,"output_black":99./255.,"output_white":192./255.},{"white":128./255.},{}]}),
            "curves" => serde_json::json!({"interpolation":"linear","points":[[0.,17./255.],[1.,239./255.]],
                "preset":{"kind":2,"name":"Curves proof"}}),
            "curves_shifted" => serde_json::json!({"interpolation":"linear","points":[[51./255.,17./255.],[204./255.,239./255.]]}),
            "curves_spline" => serde_json::json!({"interpolation":"photoshop","points":[[0.,0.],[60./255.,35./255.],[190./255.,225./255.],[1.,1.]]}),
            "posterize" => serde_json::json!({"levels":3,"mapping":"photoshop_native"}),
            "threshold" => serde_json::json!({"level":115}),
            "legacy_brightness" => serde_json::json!({"brightness":13,"contrast":-21,"mean":254}),
            "brightness_contrast" => serde_json::json!({"brightness":117,"contrast":-21,"mean":254}),
            "exposure" => serde_json::json!({"exposure":0.75,"offset":0.03125,"gamma":1.125,
                "preset":{"kind":2,"name":"Exposure proof"}}),
            "channel_mixer" => serde_json::json!({"monochrome":true,"gray":[33,33,34,7],
                "preset":{"kind":2,"name":"Grayscale proof"}}),
            "black_white" => serde_json::json!({"cyans":-17,"use_tint":true,"tint_color":[225.0000003,193,158],
                "preset":{"kind":2,"name":"Tint proof"}}),
            "selective_color" => serde_json::json!({"method":"absolute","greens":[25,-50,15,9],
                "neutrals":[15,-25,35,-5],"blacks":[-15,18,-20,-17]}),
            "vibrance" => serde_json::json!({"vibrance":25,"saturation":-15}),
            "photo_filter" => serde_json::json!({"color_space":"xyz","color":[12288,14336,4096],
                "density":37,"preserve_luminosity":true}),
            "gradient_map" => serde_json::json!({"method":"linear","reverse":true,"name":"Ramp proof","color_stops":[
                {"position":0.,"color":[0.,0.25,0.125]},
                {"position":0.375,"color":[0.875,0.125,0.5],"midpoint":0.61},
                {"position":1.,"color":[1.,1.,1.],"midpoint":0.47}
            ]}),
            "photoshop_color_balance" => serde_json::json!({"shadows":[75,-51,-9],"midtones":[100,-100,52],
                "highlights":[31,13,-13],"preserve_luminosity":true}),
            "photoshop_hue_saturation" => serde_json::json!({"master":[-135,24,-12],
                "preset":{"kind":1,"name":"Hue proof"}}),
            "color_lookup" => crate::effects::color_lookup::from_profile(
                &crate::color_profile::device_link::tests::channel_link(depth == 16), "Link proof",
            ).unwrap(),
            "color_lookup_cube" => crate::effects::color_lookup::from_cube(
                crate::effects::color_lookup::tests::CUBE, &[], "Cube proof", "Cube proof.cube",
            ).unwrap(),
            "color_lookup_3dl" => {
                use crate::effects::color_lookup::{self, LutFormat, tests::THREE_DL};
                let mut settings = color_lookup::from_table(LutFormat::ThreeDl, THREE_DL, &[], "3DL proof", "Proof.3dl").unwrap();
                settings["data_order"] = serde_json::json!("bgr");
                settings
            }
            "color_lookup_look" => {
                use crate::effects::color_lookup::{self, LutFormat, tests::look_fixture};
                color_lookup::from_table(LutFormat::Look, look_fixture().as_bytes(), &[], "Look proof", "Proof.look").unwrap()
            }
            "curves_rgb" => serde_json::json!({"interpolation":"linear","points":[[0.,0.],[1.,1.]],"channels":[
                {"interpolation":"linear","points":[[0.,1.],[1.,0.]]},
                {"interpolation":"linear","points":[[0.,17./255.],[1.,239./255.]]},
                {"interpolation":"linear","points":[[0.,0.],[1.,1.]]},
            ]}),
            _ => serde_json::json!({}),
        };
        let kind = if kind.starts_with("curves_") { "curves" } else if kind.starts_with("levels_") { "levels" }
            else if kind.starts_with("color_lookup_") { "color_lookup" } else { kind };
        crate::adjustment::add(&mut doc, &serde_json::json!({"kind":kind, "layer":base,"settings":settings})).unwrap();
        doc.layers[0].blend = blend.into();
        doc.layers[0].pixels = Raster::new_depth(2, 1, depth);
        doc.layers[0].pixels.set16(1, 0, [17001, 29003, 43007, 0]);
        let mut mask = Raster::new_depth(2, 1, depth);
        mask.set16(1, 0, [12347, 12347, 12347, 65535]);
        let mut full = Raster::new_depth(4, 3, depth);
        full.set16(2, 1, [12347, 12347, 12347, 65535]);
        full.set16(0, 1, [23131, 23131, 23131, 65535]);
        let mut original = crate::retained::Original::from(&full);
        original.matrix[4] = -1.;
        original.matrix[5] = -1.;
        mask.retained = Some(Box::new(original));
        let mut fill = crate::engine::Layer::new("Solid Color", "fill", 2, 1);
        fill.pixels = Raster::new_depth(2, 1, depth);
        fill.color = [42, 137, 231, 255];
        fill.psd_metadata.push(crate::psd::LayerMetadata {
            key: "vmsk".into(),
            data: crate::psd::vector_mask::inverted_empty_mask_fixture(),
        });
        fill.psd_metadata.extend([
            crate::psd::LayerMetadata { key: "tsly".into(), data: vec![1, 0, 0, 0] },
            crate::psd::LayerMetadata { key: "vmgm".into(), data: vec![1, 0, 0, 0] },
        ]);
        let mut fill_mask = Raster::new_depth(2, 1, depth);
        fill_mask.set16(0, 0, [12347, 12347, 12347, 65535]);
        fill.mask = Some(crate::engine::Mask {
            enabled: true,
            linked: true,
            cache_key: crate::engine::id(),
            steps: vec![crate::engine::MaskStep {
                id: crate::engine::id(),
                kind: "paint".into(),
                enabled: true,
                value: 0.,
                weight: 1.,
                settings: serde_json::Value::Null,
                pixels: fill_mask,
            }],
        });
        doc.layers.push(fill);
        doc.layers[0].mask = Some(crate::engine::Mask {
            enabled: false,
            linked,
            cache_key: crate::engine::id(),
            steps: vec![crate::engine::MaskStep {
                id: crate::engine::id(),
                kind: "paint".into(),
                enabled: true,
                value: 0.,
                weight: 1.,
                settings: serde_json::Value::Null,
                pixels: mask,
            }],
        });
        let mut gradient = Layer::new("Gradient Fill", "fill", 2, 1);
        gradient.pixels = Raster::new_depth(2, 1, depth);
        gradient.color = [255; 4];
        gradient.effects.push(crate::effects::Effect {
            id: crate::engine::id(), kind: "gradient_overlay".into(), enabled: true, weight: 1.,
            settings: crate::psd::gradient_fill::tests::settings().into(),
        });
        doc.layers.push(gradient);
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let _outputs = fixture
            .pool
            .reserve_bytes(1024 * 1024, 1024 * 1024)
            .unwrap();
        let mut scratch = ScratchBytes::new(&fixture.pool, 65536, 65536).unwrap();
        let mut actual = ScratchBytes::new(&fixture.pool, 32000, 32000).unwrap();
        for format in [PhotoshopFormat::Psd, PhotoshopFormat::Psb] {
            let resident = if format == PhotoshopFormat::Psd {
                crate::psd::encode(&doc)
            } else {
                crate::psd::encode_psb(&doc)
            }
            .unwrap();
            let original_index = structural::index(
                &mut Cursor::new(&resident),
                structural::Limits::current(),
                || Ok(()),
            )
            .unwrap();
            let original_source = Source::new(Cursor::new(&resident), resident.len() as u64).unwrap();
            // No nvrt was emitted by earlier Source36 writers. Reconstruct that
            // exact baked/hidden-source plan, then independently index its bytes.
            for generation in 0..3 {
                let legacy = generation == 0;
                let expanded = generation == 2;
                let mut original_index = original_index.clone();
                if legacy {
                    for layer in &mut original_index.layers {
                        layer.tags.retain(|t| !crate::psd::adjustments::is_tone_tag(&t.key)
                            && !matches!(&t.key, b"nvrt" | b"lmgm" | b"SoCo" | b"GdFl"));
                        layer.channels.retain(|channel| channel.id != -3);
                    }
                }
                if !expanded {
                    for channel in original_index
                        .layers
                        .iter_mut()
                        .flat_map(|layer| &mut layer.channels)
                    {
                        if channel.id == -2 {
                            channel.dimensions = Some([2, 1]);
                        }
                    }
                }
                let plan = scene_save36::ScenePlan::for_import(
                    fixture.scene(),
                    &original_source,
                    &original_index,
                    &[],
                    &|| Ok(()),
                )
                .unwrap();
                assert_eq!(plan.has_fallback(), legacy);
                let mut layers = Vec::new();
                let mut merged = Vec::new();
                plan.standard()
                    .write_layers(
                        &mut layers,
                        &mut rows,
                        &mut scratch,
                        &|| Ok(()),
                        |_, _| Ok(()),
                        |_, _| unreachable!(),
                    )
                    .unwrap();
                plan.standard()
                    .write_composite(&mut merged, &mut rows, &mut scratch, &|| Ok(()), |_, _| Ok(()))
                    .unwrap();
                let mut bytes = b"8BPS".to_vec();
                bytes.extend(
                    (if format == PhotoshopFormat::Psb {
                        2u16
                    } else {
                        1
                    })
                    .to_be_bytes(),
                );
                bytes.extend([0; 6]);
                bytes.extend(4u16.to_be_bytes());
                bytes.extend(1u32.to_be_bytes());
                bytes.extend(2u32.to_be_bytes());
                bytes.extend(depth.to_be_bytes());
                bytes.extend(3u16.to_be_bytes());
                bytes.extend([0; 8]);
                if format == PhotoshopFormat::Psb {
                    bytes.extend((layers.len() as u64).to_be_bytes());
                } else {
                    bytes.extend((layers.len() as u32).to_be_bytes());
                }
                bytes.extend(&layers);
                bytes.extend(&merged);
                let index = structural::index(
                    &mut Cursor::new(&bytes),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                let masked = index
                    .layers
                    .iter()
                    .find(|layer| layer.mask.bytes == 20)
                    .unwrap();
                let channel = masked
                    .channels
                    .iter()
                    .find(|channel| channel.id == -2)
                    .unwrap();
                assert_eq!(
                    channel.dimensions,
                    Some(if expanded { [4, 3] } else { [2, 1] })
                );
                assert_eq!(
                    bytes[masked.mask.offset as usize + 17],
                    crate::psd::standard_mask_flags(false, legacy || linked)
                );
                assert_eq!(masked.tags.iter().any(|tag| &tag.key == b"lmgm"), !legacy && !linked);
                assert_eq!(masked.clipping, 1);
                let adjustment_key = match kind {
                    "invert" => b"nvrt",
                    "curves" => b"curv",
                    "posterize" => b"post",
                    "threshold" => b"thrs",
                    "legacy_brightness" => b"brit",
                    "brightness_contrast" => b"CgEd",
                    "exposure" => b"expA",
                    "channel_mixer" => b"mixr",
                    "black_white" => b"blwh",
                    "selective_color" => b"selc",
                    "vibrance" => b"vibA",
                    "photo_filter" => b"phfl",
                    "gradient_map" => b"grdm",
                    "photoshop_color_balance" => b"blnc",
                    "photoshop_hue_saturation" => b"hue2",
                    "color_lookup" => b"clrL",
                    _ => b"levl",
                };
                assert_eq!(masked.tags.iter().any(|tag| &tag.key == adjustment_key), !legacy);
                if kind == "brightness_contrast" {
                    assert_eq!(masked.tags.iter().any(|tag| &tag.key == b"brit"), !legacy);
                }
                if matches!(kind, "levels" | "curves" | "exposure" | "channel_mixer" | "photoshop_hue_saturation") {
                    assert_eq!(masked.tags.iter().any(|tag| &tag.key == b"CgEd"), !legacy);
                }
                assert_eq!(masked.blend_mode, scene_save36::blend(blend).unwrap());
                assert_eq!(index.layers.iter().any(|layer| {
                    layer.tags.iter().any(|tag| tag.key == *b"SoCo")
                }), !legacy);
                assert_eq!(index.layers.iter().any(|layer| {
                    layer.tags.iter().any(|tag| tag.key == *b"GdFl")
                }), !legacy);
                let paired = index.layers.iter().find(|layer| layer.tags.iter().any(|tag| tag.key == *b"vmsk")).unwrap();
                assert_eq!(paired.mask.bytes, if legacy { 20 } else { 36 });
                assert_eq!(paired.channels.iter().any(|channel| channel.id == -3), !legacy);
                let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
                let replay = scene_save36::ScenePlan::for_import(fixture.scene(), &source, &index, &[], &|| Ok(()))
                    .unwrap();
                assert_eq!(replay.has_fallback(), legacy);
                let compared = replay
                    .standard()
                    .compare(
                        &source,
                        &index,
                        &mut rows,
                        &mut scratch,
                        &mut actual,
                        &|| Ok(()),
                        |_, _| Ok(()),
                    )
                    .unwrap();
                compared.check(replay.standard(), &|| Ok(())).unwrap();
                if !legacy {
                    let reopened = crate::psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only, "{:?}", reopened.warnings);
                    assert_eq!(reopened.layers[0].kind, "adjustment");
                    assert_eq!(
                        reopened.layers[0].effects[0].settings.get("preset"),
                        doc.layers[0].effects[0].settings.get("preset")
                    );
                    assert_eq!(reopened.layers[0].blend, blend);
                    assert_eq!(reopened.layers[2].kind, "fill");
                    assert_eq!(reopened.layers[2].color, doc.layers[2].color);
                    assert_eq!(reopened.layers[2].psd_metadata, doc.layers[2].psd_metadata);
                    assert_eq!(
                        reopened.layers[0].clip_to.as_deref(),
                        Some(reopened.layers[1].id.as_str())
                    );
                    let mask = reopened.layers[0].mask.as_ref().unwrap();
                    assert!(!mask.enabled && mask.linked == linked);
                    assert_eq!(
                        mask.steps[1].pixels.get16(1, 0),
                        doc.layers[0].mask.as_ref().unwrap().steps[0]
                            .pixels
                            .get16(1, 0)
                    );
                    if expanded {
                        assert_eq!(
                            mask.steps[1]
                                .pixels
                                .retained
                                .as_ref()
                                .unwrap()
                                .pixels
                                .get16(0, 1),
                            full.get16(0, 1)
                        );
                    }
                    assert_eq!(reopened.psd_global_metadata, doc.psd_global_metadata);
                    assert_eq!(
                        reopened.layers[0].pixels.get16(1, 0),
                        doc.layers[0].pixels.get16(1, 0)
                    );
                    assert_eq!(reopened.export_png().unwrap(), doc.export_png().unwrap());
                }
            }
        }
    }
}
