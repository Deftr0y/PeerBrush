//! Captured native composite rows share prepared layer effects and masks.
//! Document filters prepare their finite-support composite input separately.
use super::{native, native_grid, native_png, Format, Output, PreviewGrid, Resampling};
use crate::{
    compositor::native::{
        prepare_filters_with_layers, LayerWindow, Native16, Native8, Plan, RasterTiles, Stored,
        Topology,
    },
    engine::scene::SceneSnapshot,
    large_document::ScratchBytes,
    raster::TILE,
};
use std::sync::Arc;

type Result<T> = std::result::Result<T, String>;

pub(crate) struct Rows<'a> {
    scene: &'a SceneSnapshot,
    topology: Arc<Topology<'a>>,
    stored: Option<&'a Stored<'a>>,
    occupied: Option<RasterTiles>,
}
impl<'a> Rows<'a> {
    pub(crate) fn new(scene: &'a SceneSnapshot, check: &dyn Fn() -> Result<()>) -> Result<Self> {
        Self::for_target(scene, None, check)
    }
    fn for_target(
        scene: &'a SceneSnapshot,
        target: Option<&str>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        Self::from_topology(Topology::for_target(scene, target)?, None, check)
    }
    pub(super) fn from_topology(
        topology: Arc<Topology<'a>>,
        stored: Option<&'a Stored<'a>>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        let scene = topology.scene();
        if ![8, 16].contains(&scene.metadata().depth())
            || !(1..=16000).contains(&scene.metadata().width())
            || !(1..=16000).contains(&scene.metadata().height())
        {
            return Err("Composite rows require native RGB8 or RGB16".into());
        }
        if let Some(stored) = stored {
            stored.check_tree(&topology)?;
        }
        let occupied = topology.composite_tiles(None, &mut || check())?;
        Ok(Self {
            scene,
            topology,
            stored,
            occupied,
        })
    }
    pub(crate) fn with_prepared(
        scene: &'a SceneSnapshot,
        target: Option<&str>,
        stored: &'a Stored<'a>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        Self::from_topology(Topology::for_target(scene, target)?, Some(stored), check)
    }

    pub(crate) fn region_is_transparent(&self, bounds: [i32; 4]) -> bool {
        self.occupied.as_ref().is_some_and(|tiles| !tiles.intersects(bounds))
    }
    pub(crate) fn scene(&self) -> &'a SceneSnapshot {
        self.scene
    }
    pub(super) fn alpha_bounds(&self, check: &dyn Fn() -> Result<()>) -> Result<[i32; 4]> {
        let mut output = native::canvas(self.scene, Format::Png);
        output.target = self.topology.target().map(str::to_owned);
        let full = output.document_rect;
        let width = output.width;
        let height = output.height;
        let occupied = self.occupied.as_ref();
        let stride = usize::from(output.bit_depth / 2);
        let capacity = if occupied.is_some() {
            (width.min(TILE) * height.min(TILE)) as usize * stride
        } else {
            (width * native_png::stripe_rows(width, height)) as usize * stride
        };
        let mut transfer = ScratchBytes::new(self.scene.budget(), capacity, capacity)
            .map_err(|e| e.to_string())?;
        let mut bounds = [full[2], full[3], 0, 0];
        let mut scan = |rect: [i32; 4]| -> Result<()> {
            output.document_rect = rect;
            output.width = (rect[2] - rect[0]) as u32;
            output.height = (rect[3] - rect[1]) as u32;
            let stripe = native_png::stripe_rows(output.width, output.height);
            let row_bytes = output.width as usize * stride;
            for first in (0..output.height).step_by(stripe as usize) {
                check()?;
                let count = stripe.min(output.height - first);
                let bytes = &mut transfer[..count as usize * row_bytes];
                self.fill(
                    &output,
                    output.bit_depth,
                    Resampling::Nearest,
                    first,
                    count,
                    bytes,
                    check,
                )?;
                for (y, row) in bytes.chunks_exact(row_bytes).enumerate() {
                    let y = rect[1] + first as i32 + y as i32;
                    for (x, pixel) in row.chunks_exact(stride).enumerate() {
                        if pixel[3 * stride / 4..].iter().any(|&byte| byte != 0) {
                            let x = rect[0] + x as i32;
                            bounds = [
                                bounds[0].min(x),
                                bounds[1].min(y),
                                bounds[2].max(x + 1),
                                bounds[3].max(y + 1),
                            ];
                        }
                    }
                }
            }
            Ok(())
        };
        if let Some(occupied) = occupied {
            for rect in occupied.rects() {
                scan(rect)?;
            }
        } else {
            scan(full)?;
        }
        check()?;
        if bounds[2] <= bounds[0] || bounds[3] <= bounds[1] {
            return Err("Target is transparent; choose canvas bounds to export an empty image".into());
        }
        Ok(bounds)
    }
    /// The caller admits this provisional output buffer before allocation and
    /// must not encode/publish a partial stripe after any failure.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fill(
        &self,
        output: &Output,
        depth: u16,
        resampling: Resampling,
        first_row: u32,
        rows: u32,
        bytes: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        self.fill_sampled(output, depth, resampling, first_row, rows, None, bytes, check)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn fill_sampled(
        &self,
        output: &Output,
        depth: u16,
        resampling: Resampling,
        first_row: u32,
        rows: u32,
        preview: Option<PreviewGrid>,
        bytes: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        check()?;
        if output.target.as_deref() != self.topology.target() {
            return Err("Export target differs from its captured tree provider".into());
        }
        if depth != self.scene.metadata().depth() || output.native_source_depth != depth {
            return Err("Export rows differ from the captured native precision".into());
        }
        let stride = usize::from(depth / 2);
        let output_width = preview.map_or(output.width, |grid| grid.width) as usize;
        let expected = output_width
            .checked_mul(rows as usize)
            .and_then(|n| n.checked_mul(stride))
            .ok_or("Native export row size overflow")?;
        if bytes.len() != expected {
            return Err("Native export row buffer has the wrong exact length".into());
        }
        let mut grid = native_grid::Grid::sampled(output, resampling, first_row, rows, preview)?;
        let width = self.scene.metadata().width() as i32;
        let height = self.scene.metadata().height() as i32;
        let clip = |[l, t, r, b]: [i32; 4]| [l.max(0), t.max(0), r.min(width), b.min(height)];
        let empty = |bounds: [i32; 4]| {
            bounds[0] >= bounds[2] || bounds[1] >= bounds[3]
                || self.region_is_transparent(bounds)
        };
        let stripe_support = grid.support([0, first_row, output_width as u32, first_row + rows]);
        if empty(clip(stripe_support)) {
            bytes.fill(0);
            return check();
        }
        while let Some(window) = grid.next()? {
            check()?;
            let bounds = clip(window.source);
            if empty(bounds) {
                let [l, t, r, b] = window.output;
                for y in t..b {
                    let offset = (y - first_row) as usize * output_width;
                    bytes[(offset + l as usize) * stride..(offset + r as usize) * stride].fill(0);
                }
                continue;
            }
            let mut fill = |sample: &dyn Fn(i32, i32) -> Result<[u16; 4]>| -> Result<()> {
                for y in window.output[1]..window.output[3] {
                    check()?;
                    for x in window.output[0]..window.output[2] {
                        let pixel =
                            native_grid::sample(grid.position(x, y)?, resampling, |x, y| {
                                if x < 0 || y < 0 || x >= width || y >= height {
                                    Ok([0; 4])
                                } else {
                                    sample(x, y)
                                }
                            })?;
                        let at = (((y - first_row) as usize * output_width) + x as usize)
                            * stride;
                        if depth == 8 {
                            for c in 0..4 {
                                bytes[at + c] = u8::try_from(pixel[c])
                                    .map_err(|_| "Native8 sample exceeds its channel precision")?;
                            }
                        } else {
                            for c in 0..4 {
                                bytes[at + c * 2..at + c * 2 + 2]
                                    .copy_from_slice(&pixel[c].to_be_bytes());
                            }
                        }
                    }
                }
                Ok(())
            };
            let region = crate::raster::backed::Region::new(bounds).map_err(|e| e.to_string())?;
            if let Some(filtered) =
                prepare_filters_with_layers(&self.topology, region, self.stored, &mut || check())?
            {
                if depth == 8 {
                    fill(&|x, y| Ok(filtered.pixel(x, y)?.map(u16::from)))?;
                } else {
                    fill(&|x, y| filtered.pixel16(x, y))?;
                }
                continue;
            }
            let prepared =
                LayerWindow::prepare(&self.topology, region, self.stored, &mut || check())?;
            if depth == 8 {
                let plan =
                    Plan::<Native8, _>::with_topology(self.topology.clone(), &prepared, region)?;
                fill(&|x, y| Ok(plan.pixel(x, y)?.map(u16::from)))?;
            } else {
                let plan =
                    Plan::<Native16, _>::with_topology(self.topology.clone(), &prepared, region)?;
                fill(&|x, y| plan.pixel(x, y))?;
            }
        }
        check()
    }
}

impl super::native_png::CapturedExportRows for Rows<'_> {
    fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
    fn fill(
        &mut self,
        request: super::native_png::RowRequest<'_>,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let expected = match request.output.format {
            super::Format::Png | super::Format::Jxl => {
                super::native_png::binding(self.scene, request.output, request.resampling, check)?
            }
            super::Format::Webp => {
                super::native_webp::binding(self.scene, request.output, request.resampling, check)?
            }
        };
        if request.provenance != expected {
            return Err("Native row request differs from its captured encoding plan".into());
        }
        Rows::fill(
            self,
            request.output,
            request.sample_depth,
            request.resampling,
            request.first_row,
            request.rows,
            output,
            check,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::native::Layers;
    use crate::production_export::{Control, Format, Image};

    #[test]
    fn sampled_export_preview_matches_full_output_pixels_at_both_depths() {
        for depth in [8, 16] {
            let mut doc = crate::engine::Document::new_depth(1024, 83, depth).unwrap();
            for y in 8..68 {
                for x in 230..318 {
                    doc.layers[0].pixels.set16(x, y, [12347, 23003, 34507, 40001]);
                }
            }
            let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
            let rows = Rows::new(&scene, &|| Ok(())).unwrap();
            let mut output = native::canvas(&scene, Format::Png);
            output.document_rect = [-13, -7, 1030, 90];
            output.width = 4099;
            output.height = 291;
            let preview = PreviewGrid::new(&output, 103);
            let stride = usize::from(depth / 2);
            for method in [Resampling::Nearest, Resampling::Bilinear] {
                let mut sampled = vec![0xa5; preview.width as usize * preview.height as usize * stride];
                rows.fill_sampled(
                    &output, depth, method, 0, preview.height, Some(preview), &mut sampled, &|| Ok(()),
                ).unwrap();
                let mut full = vec![0; output.width as usize * stride];
                for y in 0..preview.height {
                    rows.fill(
                        &output, depth, method, preview.source(y, output.height) as u32, 1, &mut full, &|| Ok(()),
                    ).unwrap();
                    for x in 0..preview.width {
                        let at = (y * preview.width + x) as usize * stride;
                        let source = preview.source(x, output.width) * stride;
                        assert_eq!(&sampled[at..at + stride], &full[source..source + stride]);
                    }
                }
                assert!(sampled[..stride].iter().all(|&byte| byte == 0));
                assert!(sampled.iter().any(|&byte| byte != 0));
            }
            drop(rows);
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }

    #[test]
    fn empty_export_windows_are_zeroed_without_losing_bilinear_edge_samples() {
        use std::cell::Cell;

        for depth in [8, 16] {
            let mut doc = crate::engine::Document::new_depth(512, 8, depth).unwrap();
            let color = [12347, 23459, 34571, 65535];
            doc.layers[0].pixels.set16(256, 3, color);
            doc.layers[0].mask = Some(crate::engine::Mask {
                enabled: true,
                linked: true,
                cache_key: crate::engine::id(),
                steps: vec![crate::engine::MaskStep {
                    id: crate::engine::id(),
                    kind: "fill".into(),
                    enabled: true,
                    weight: 1.,
                    value: 128.,
                    pixels: crate::raster::Raster::new_depth(512, 8, depth),
                    settings: serde_json::Value::Null,
                }],
            });
            let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
            let mut output = native::canvas(&scene, Format::Png);
            output.document_rect = [-1, 0, 513, 8];
            output.width = 1028;
            let stride = usize::from(depth / 2);
            let mut sparse = vec![0xa5; output.width as usize * 8 * stride];
            let mut full = sparse.clone();
            let mut rows = Rows::new(&scene, &|| Ok(())).unwrap();
            assert_eq!(
                rows.occupied.as_ref().unwrap().rects().collect::<Vec<_>>(),
                [[256, 0, 512, 8]]
            );
            let sparse_checks = Cell::new(0);
            rows.fill(&output, depth, Resampling::Bilinear, 0, 8, &mut sparse, &|| {
                sparse_checks.set(sparse_checks.get() + 1);
                Ok(())
            }).unwrap();
            let mut padded = output.clone();
            padded.document_rect[1] = -8;
            padded.height = 16;
            let mut empty = vec![0xa5; padded.width as usize * 7 * stride];
            let empty_checks = Cell::new(0);
            rows.fill(&padded, depth, Resampling::Bilinear, 0, 7, &mut empty, &|| {
                empty_checks.set(empty_checks.get() + 1);
                Ok(())
            }).unwrap();
            assert!(empty.iter().all(|&byte| byte == 0));
            assert_eq!(empty_checks.get(), 2);
            rows.occupied = None;
            let full_checks = Cell::new(0);
            rows.fill(&output, depth, Resampling::Bilinear, 0, 8, &mut full, &|| {
                full_checks.set(full_checks.get() + 1);
                Ok(())
            }).unwrap();
            assert_eq!(sparse, full);
            assert!(sparse_checks.get() < full_checks.get());
            assert!(sparse[..stride * output.width as usize].iter().all(|&v| v == 0));
            let at = (3 * output.width as usize + 514) * stride;
            if depth == 16 {
                assert_eq!(
                    &sparse[at..at + stride],
                    &[12347u16, 23459, 34571, 24672].map(u16::to_be_bytes).concat()
                );
            } else {
                let [r, g, b, _] = color.map(crate::raster::project16);
                assert_eq!(&sparse[at..at + stride], &[r, g, b, 96]);
            }
            drop(rows);
            drop(scene);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn hard_drop_shadow_crosses_sparse_tiles_without_full_frame_staging() {
        use serde_json::json;
        for depth in [8, 16] {
            let mut doc = crate::engine::Document::new_depth(2048, 16, depth).unwrap();
            doc.layers[0].pixels.set16(255, 5, [12347, 23459, 34571, 65535]);
            doc.layers[0].effects.push(crate::effects::Effect {
                id: crate::engine::id(),
                kind: "drop_shadow".into(),
                enabled: true,
                weight: 1.,
                settings: json!({"size": 0., "distance": 8.25, "angle": 0.,
                    "color": [1., 0., 0.], "opacity": 1.}).into(),
            });
            let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
            let mut rows = Rows::new(&scene, &|| Ok(())).unwrap();
            assert!(!Stored::composite_needed(&rows.topology, None));
            assert_eq!(rows.occupied.as_ref().unwrap().rects().collect::<Vec<_>>(),
                [[0, 0, 256, 16], [256, 0, 512, 16]]);
            assert_eq!(rows.alpha_bounds(&|| Ok(())).unwrap(), [255, 5, 265, 6]);

            let stride = usize::from(depth / 2);
            let output = native::canvas(&scene, Format::Png);
            let mut sparse = vec![0xa5; 2048 * stride];
            let mut dense = sparse.clone();
            rows.fill(&output, depth, Resampling::Nearest, 5, 1, &mut sparse, &|| Ok(())).unwrap();
            let fringe = &sparse[263 * stride..265 * stride];
            if depth == 16 {
                assert_eq!(fringe, &[255, 255, 0, 0, 0, 0, 191, 255, 255, 255, 0, 0, 0, 0, 64, 0]);
            } else {
                assert_eq!(fringe, &[255, 0, 0, 191, 255, 0, 0, 64]);
            }
            rows.occupied = None;
            rows.fill(&output, depth, Resampling::Nearest, 5, 1, &mut dense, &|| Ok(())).unwrap();
            assert_eq!(sparse, dense);

            let rgba8 = if depth == 16 {
                sparse.chunks_exact(2).map(|word| {
                    crate::raster::project16(u16::from_be_bytes([word[0], word[1]]))
                }).collect()
            } else {
                sparse
            };
            assert_eq!(rgba8, doc.preview(Some([0, 5, 2048, 6]), 2048, None, false).unwrap().2);
            drop(rows);
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }

    #[test]
    fn sparse_effect_rows_keep_off_canvas_blur_and_filter_halos_at_native_depth() {
        use serde_json::json;
        for depth in [8, 16] {
            let mut doc = crate::engine::Document::new_depth(1024, 16, depth).unwrap();
            let layer = doc.layers[0].id.clone();
            doc.layers[0].x = -256;
            for y in 5..11 {
                for x in [254, 255, 510, 511] {
                    doc.layers[0].pixels.set16(x, y, [65535, 51721, 40219, 65535]);
                }
            }
            let doc = crate::engine::Engine::preview_edits(doc, &[
                json!({"op":"effect.add","layer":layer,"kind":"blur","settings":{"radius":2.}}),
                json!({"op":"filter.add","kind":"bloom","settings":{"threshold":0.,"spread":3.,"strength":1.}}),
            ]).unwrap();
            let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
            let output = native::canvas(&scene, Format::Png);
            let stride = usize::from(depth / 2);
            let mut sparse = vec![0xa5; 1024 * 16 * stride];
            let mut dense = sparse.clone();
            let mut rows = Rows::new(&scene, &|| Ok(())).unwrap();
            assert_eq!(rows.occupied.as_ref().unwrap().rects().collect::<Vec<_>>(),
                [[0, 0, 256, 16], [256, 0, 512, 16]]);
            let crop = rows.alpha_bounds(&|| Ok(())).unwrap();
            assert_eq!(crop[0], 0, "Off-canvas blur must reach the visible canvas");
            assert!(crop[2] > 256, "Blur and Bloom must reach the next tile");
            rows.fill(&output, depth, Resampling::Nearest, 0, 16, &mut sparse, &|| Ok(())).unwrap();
            rows.occupied = None;
            assert_eq!(rows.alpha_bounds(&|| Ok(())).unwrap(), crop);
            rows.fill(&output, depth, Resampling::Nearest, 0, 16, &mut dense, &|| Ok(())).unwrap();
            assert_eq!(sparse, dense);
            drop(rows);
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);

            // A positive lower bound can create hidden RGB even on an empty
            // composite. A sparse zero would silently change standard bytes.
            let doc = crate::engine::Engine::preview_edits(doc, &[
                json!({"op":"filter.add","kind":"channel_clamp","settings":{"channel":"r","minimum":0.25}}),
            ]).unwrap();
            let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
            let rows = Rows::new(&scene, &|| Ok(())).unwrap();
            assert!(rows.occupied.is_none());
            let mut output = native::canvas(&scene, Format::Png);
            output.width = 1;
            output.height = 1;
            output.document_rect = [900, 8, 901, 9];
            let mut word = [0; 8];
            rows.fill(&output, depth, Resampling::Nearest, 0, 1, &mut word[..stride], &|| Ok(())).unwrap();
            // Native8 red is 64, and the native16 word is 0x4000.
            assert_eq!(word, [64, 0, 0, 0, 0, 0, 0, 0]);
            drop(rows);
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }

    #[test]
    fn sparse_alpha_crop_composes_only_occupied_canvas_tiles_at_original_depth() {
        use crate::engine::Document;
        use crate::raster::Raster;
        use std::cell::Cell;

        for depth in [8, 16] {
            let mut doc = Document::new_depth(1, 1, depth).unwrap();
            doc.width = 16000;
            doc.height = 16000;
            let mut pixels = Raster::new_depth(16000, 16000, depth);
            pixels.set16(1, 1, [12347, 23459, 34571, 0]);
            pixels.set16(255, 2, [12347, 23459, 34571, 65535]);
            let alpha = if depth == 16 { 1 } else { 257 };
            pixels.set16(15998, 15999, [12347, 23459, 34571, alpha]);
            pixels.set16(15999, 15999, [12347, 23459, 34571, 65535]);
            doc.layers[0].pixels = pixels;
            doc.layers[0].x = 1;
            doc.layers[0].y = -1;
            let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
            for target in [None, Some(doc.layers[0].id.as_str())] {
                let rows = Rows::for_target(&scene, target, &|| Ok(())).unwrap();
                let occupied = rows.occupied.as_ref().unwrap();
                assert_eq!(occupied.rects().count(), 4);
                let checks = Cell::new(0);
                let bounds = rows.alpha_bounds(&|| {
                    checks.set(checks.get() + 1);
                    if checks.get() > 12000 {
                        return Err("Sparse crop exceeded its occupied work envelope".into());
                    }
                    Ok(())
                }).unwrap();
                assert_eq!(bounds, [256, 1, 16000, 15999]);
            }
            drop(scene);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn actual_native_composite_stripes_match_full_legacy_crop_padding_and_resampling() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        for depth in [8, 16] {
            let mut doc = crate::compositor::native::tests::doc(depth, true, "screen");
            doc.layers[0].effects.push(crate::effects::Effect {
                id: crate::engine::id(),
                kind: "invert".into(),
                enabled: true,
                weight: 0.43,
                settings: serde_json::json!({}).into(),
            });
            let words = [12347, 33559, 51237, 65535, 54321, 12345, 45679, 27139];
            let png = if depth == 16 {
                crate::raster::png16(2, 1, &words).unwrap()
            } else {
                crate::raster::png(2, 1, &words.map(crate::raster::project16)).unwrap()
            };
            let mut settings = crate::effects::defaults("pattern_overlay");
            settings["pattern"] = serde_json::json!("bitmap");
            settings["bitmap_png"] = serde_json::json!(STANDARD.encode(png));
            settings["bitmap_scale"] = serde_json::json!(1.7);
            settings["bitmap_angle"] = serde_json::json!(23.);
            doc.layers[0].effects.push(crate::effects::Effect {
                id: crate::engine::id(),
                kind: "pattern_overlay".into(),
                enabled: true,
                weight: 0.67,
                settings: settings.into(),
            });
            crate::adjustment::add(&mut doc, &serde_json::json!({"kind":"invert"})).unwrap();
            doc.layers[0].opacity = 0.61;
            for (kind, settings) in [
                ("blur", serde_json::json!({"radius":1.7})),
                ("bloom", serde_json::json!({"spread":2.3})),
            ] {
                doc.filters.push(crate::effects::Effect {
                    id: crate::engine::id(),
                    kind: kind.into(),
                    enabled: true,
                    weight: 0.63,
                    settings: settings.into(),
                });
            }
            let expected = Image::render(&doc, None, &Control::default()).unwrap();
            let (scene, path) = crate::compositor::native::tests::capture_kind_with_budget(
                &doc,
                true,
                crate::large_document::ResidentBudget::new(96 * 1024 * 1024).unwrap(),
            );
            let provider = Rows::new(&scene, &|| Ok(())).unwrap();
            assert!(std::ptr::eq(provider.scene(), &scene));
            for method in [Resampling::Nearest, Resampling::Bilinear] {
                let output = Output {
                    target: None,
                    target_name: String::new(),
                    variant: String::new(),
                    filename: "test.png".into(),
                    document_rect: [-3, -2, 12, 10],
                    width: 23,
                    height: 19,
                    format: Format::Png,
                    native_source_depth: depth,
                    bit_depth: depth,
                    scale_xy: [23. / 15., 19. / 12.],
                };
                let stride = usize::from(depth / 2);
                for (first, count) in [(0, 7), (7, 5), (12, 7)] {
                    let mut bytes = vec![0xcd; output.width as usize * count as usize * stride];
                    provider
                        .fill(&output, depth, method, first, count, &mut bytes, &|| Ok(()))
                        .unwrap();
                    let grid = native_grid::Grid::new(&output, method, first, count).unwrap();
                    for y in first..first + count {
                        for x in 0..output.width {
                            let [sx, sy] = grid.position(x, y).unwrap();
                            let p = expected.resampled(sx, sy, method);
                            let at = ((y - first) * output.width + x) as usize * stride;
                            let actual = if depth == 8 {
                                bytes[at..at + 4]
                                    .iter()
                                    .map(|v| u16::from(*v))
                                    .collect::<Vec<_>>()
                            } else {
                                bytes[at..at + 8]
                                    .chunks_exact(2)
                                    .map(|v| u16::from_be_bytes(v.try_into().unwrap()))
                                    .collect()
                            };
                            assert_eq!(actual, p, "{depth} {method:?} {x},{y}");
                        }
                    }
                }
                let mut bytes = vec![0; output.width as usize * stride];
                assert_eq!(
                    provider
                        .fill(&output, depth, method, 0, 1, &mut bytes, &|| Err(
                            "source cancelled".into()
                        ))
                        .unwrap_err(),
                    "source cancelled"
                );
                assert!(provider
                    .fill(&output, depth, method, 0, 1, &mut bytes[..1], &|| Ok(()))
                    .is_err());
                let mut isolated = output.clone();
                isolated.target = Some(doc.layers[0].id.clone());
                assert!(provider
                    .fill(&isolated, depth, method, 0, 1, &mut bytes, &|| Ok(()))
                    .is_err());
            }
            drop(provider);
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }
    #[test]
    fn isolated_hidden_root_excludes_ancestors_siblings_and_document_filters() {
        for depth in [8, 16] {
            let mut doc = crate::compositor::native::tests::doc(depth, true, "screen");
            doc.layers[0].visible = false;
            let effect = crate::effects::Effect {
                id: crate::engine::id(),
                kind: "invert".into(),
                enabled: true,
                weight: 0.67,
                settings: serde_json::json!({}).into(),
            };
            doc.layers[3].effects.push(effect.clone());
            doc.filters.push(effect);
            let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
            assert!(Rows::new(&scene, &|| Ok(())).is_ok());
            assert!(Rows::for_target(&scene, Some(&doc.layers[2].id), &|| Ok(())).is_err());
            assert!(Rows::for_target(&scene, Some("missing"), &|| Ok(())).is_err());
            let free = scene.budget().limit() - scene.budget().used();
            let held = scene.budget().reserve_bytes(free, free).unwrap();
            assert!(Rows::for_target(&scene, Some(&doc.layers[0].id), &|| Ok(())).is_err());
            assert_eq!(scene.budget().used(), scene.budget().limit());
            drop(held);
            for index in [0, 1, 4] {
                let target = &doc.layers[index].id;
                let expected = Image::render(&doc, Some(target), &Control::default()).unwrap();
                let provider = Rows::for_target(&scene, Some(target), &|| Ok(())).unwrap();
                if index == 0 {
                    let region = crate::raster::backed::Region::new([0, 0, 9, 7]).unwrap();
                    let prepared =
                        Layers::for_topology(&provider.topology, region, &mut || Ok(())).unwrap();
                    let other = Topology::for_target(&scene, Some(&doc.layers[4].id)).unwrap();
                    if depth == 8 {
                        assert!(
                            Plan::<Native8, _>::with_topology(other, &prepared, region).is_err()
                        );
                    } else {
                        assert!(
                            Plan::<Native16, _>::with_topology(other, &prepared, region).is_err()
                        );
                    }
                }
                let output = Output {
                    target: Some(target.clone()),
                    target_name: String::new(),
                    variant: String::new(),
                    filename: "test.png".into(),
                    document_rect: [-2, -1, 11, 9],
                    width: 17,
                    height: 13,
                    format: Format::Png,
                    native_source_depth: depth,
                    bit_depth: depth,
                    scale_xy: [17. / 13., 13. / 10.],
                };
                let stride = usize::from(depth / 2);
                for method in [Resampling::Nearest, Resampling::Bilinear] {
                    let mut bytes = vec![0xcd; 17 * 13 * stride];
                    provider
                        .fill(&output, depth, method, 0, 13, &mut bytes, &|| Ok(()))
                        .unwrap();
                    let grid = native_grid::Grid::new(&output, method, 0, 13).unwrap();
                    for y in 0..13 {
                        for x in 0..17 {
                            let [sx, sy] = grid.position(x, y).unwrap();
                            let expected = expected.resampled(sx, sy, method);
                            let at = (y * 17 + x) as usize * stride;
                            let actual: Vec<u16> = if depth == 8 {
                                bytes[at..at + 4].iter().map(|v| u16::from(*v)).collect()
                            } else {
                                bytes[at..at + 8]
                                    .chunks_exact(2)
                                    .map(|v| u16::from_be_bytes(v.try_into().unwrap()))
                                    .collect()
                            };
                            assert_eq!(actual, expected, "{depth} root{index} {method:?} {x},{y}");
                        }
                    }
                }
            }
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }
    #[test]
    fn actual_captured_compositor_encodes_native_png_and_projected_webp_with_exact_pixels() {
        for depth in [8, 16] {
            let doc = crate::compositor::native::tests::doc(depth, true, "screen");
            let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
            for target in [None, Some(doc.layers[0].id.clone())] {
                let expected = Image::render(&doc, target.as_deref(), &Control::default()).unwrap();
                for format in [Format::Png, Format::Webp] {
                    let output = Output {
                        target: target.clone(),
                        target_name: String::new(),
                        variant: String::new(),
                        filename: format!("test.{}", format.extension()),
                        document_rect: [-3, -2, 12, 10],
                        width: 23,
                        height: 19,
                        format,
                        native_source_depth: depth,
                        bit_depth: if format == Format::Webp { 8 } else { depth },
                        scale_xy: [23. / 15., 19. / 12.],
                    };
                    let settings = super::super::Settings {
                        format,
                        ..Default::default()
                    };
                    let mut provider = Rows::for_target(&scene, target.as_deref(), &|| Ok(())).unwrap();
                    let mut encoded = Vec::new();
                    if format == Format::Png {
                        let counted = super::super::native_png::write_to(
                            &scene,
                            &output,
                            &settings,
                            &mut provider,
                            &mut std::io::sink(),
                            1024 * 1024,
                            &|| Ok(()),
                        )
                        .unwrap();
                        let written = super::super::native_png::write_to(
                            &scene,
                            &output,
                            &settings,
                            &mut provider,
                            &mut encoded,
                            counted.bytes(),
                            &|| Ok(()),
                        )
                        .unwrap();
                        counted.same_encoding(&written, &|| Ok(())).unwrap();
                    } else {
                        let counted = super::super::native_webp::write_to(
                            &scene,
                            &output,
                            &settings,
                            &mut provider,
                            &mut std::io::sink(),
                            1024 * 1024,
                            &|| Ok(()),
                        )
                        .unwrap();
                        let written = super::super::native_webp::write_to(
                            &scene,
                            &output,
                            &settings,
                            &mut provider,
                            &mut encoded,
                            counted.bytes(),
                            &|| Ok(()),
                        )
                        .unwrap();
                        counted.same_encoding(&written, &|| Ok(())).unwrap();
                    }
                    let decoded = image::load_from_memory_with_format(
                        &encoded,
                        if format == Format::Png {
                            image::ImageFormat::Png
                        } else {
                            image::ImageFormat::WebP
                        },
                    )
                    .unwrap()
                    .to_rgba16();
                    let grid =
                        native_grid::Grid::new(&output, settings.resampling, 0, output.height)
                            .unwrap();
                    for y in 0..output.height {
                        for x in 0..output.width {
                            let [sx, sy] = grid.position(x, y).unwrap();
                            let mut pixel = expected.resampled(sx, sy, settings.resampling);
                            if format == Format::Webp && depth == 16 {
                                pixel =
                                    pixel.map(|value| u16::from(crate::raster::project16(value)));
                            }
                            if output.bit_depth == 8 {
                                pixel = pixel.map(|value| value * 257);
                            }
                            assert_eq!(
                                decoded.get_pixel(x, y).0,
                                pixel,
                                "{depth} {format:?} {x},{y}"
                            );
                        }
                    }
                    use super::super::native_png::{CapturedExportRows, RowRequest};
                    let mut bytes = vec![0; output.width as usize * usize::from(depth / 2)];
                    assert!(CapturedExportRows::fill(
                        &mut provider,
                        RowRequest {
                            output: &output,
                            sample_depth: depth,
                            resampling: settings.resampling,
                            first_row: 0,
                            rows: 1,
                            provenance: [0; 32]
                        },
                        &mut bytes,
                        &|| Ok(())
                    )
                    .is_err());
                }
            }
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }
    #[test]
    fn padded_exterior_beyond_legacy_preview_coordinates_never_becomes_a_source_read() {
        for depth in [8, 16] {
            let doc = crate::compositor::native::tests::doc(depth, true, "screen");
            let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
            let provider = Rows::new(&scene, &|| Ok(())).unwrap();
            for rect in [
                [16000, 0, 24000, 8],
                [-116000, -7, -110000, 8],
                [110000, 0, 116000, 9],
            ] {
                let output = Output {
                    target: None,
                    target_name: String::new(),
                    variant: String::new(),
                    filename: "padded.png".into(),
                    document_rect: rect,
                    width: 17,
                    height: 7,
                    format: Format::Png,
                    native_source_depth: depth,
                    bit_depth: depth,
                    scale_xy: [1., 1.],
                };
                for method in [Resampling::Nearest, Resampling::Bilinear] {
                    let mut bytes = vec![0xff; 17 * 7 * usize::from(depth / 2)];
                    provider
                        .fill(&output, depth, method, 0, 7, &mut bytes, &|| Ok(()))
                        .unwrap();
                    assert!(bytes.iter().all(|v| *v == 0));
                }
            }
            drop(provider);
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }
}
