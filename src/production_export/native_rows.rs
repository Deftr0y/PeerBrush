//! Captured native composite rows. Current derived providers must be prepared
//! explicitly; this base provider refuses them rather than changing the image.
use super::{native_grid, Output, Resampling};
use crate::{
    compositor::native::{Native16, Native8, Plan, RawLayers, Topology},
    engine::scene::SceneSnapshot,
    large_document::Error,
};
use std::sync::Arc;

type Result<T> = std::result::Result<T, String>;

pub(crate) struct Rows<'a> {
    scene: &'a SceneSnapshot,
    topology: Arc<Topology<'a>>,
}
impl<'a> Rows<'a> {
    pub(crate) fn new(scene: &'a SceneSnapshot) -> Result<Self> {
        Self::for_target(scene, None)
    }
    pub(crate) fn for_target(scene: &'a SceneSnapshot, target: Option<&str>) -> Result<Self> {
        if scene.metadata().depth() != 8 && scene.metadata().depth() != 16 {
            return Err("Composite rows require native RGB8 or RGB16".into());
        }
        if target.is_none()
            && scene
                .metadata()
                .filters()
                .any(|filter| filter.enabled() && filter.weight() > 0.)
        {
            return Err("Document filters require their explicit regional provider".into());
        }
        Ok(Self {
            scene,
            topology: Topology::for_target(scene, target)?,
        })
    }
    pub(crate) fn scene(&self) -> &'a SceneSnapshot {
        self.scene
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
        check()?;
        if output.target.as_deref() != self.topology.target() {
            return Err("Export target differs from its captured tree provider".into());
        }
        if depth != self.scene.metadata().depth() || output.native_source_depth != depth {
            return Err("Export rows differ from the captured native precision".into());
        }
        let stride = usize::from(depth / 2);
        let expected = (output.width as usize)
            .checked_mul(rows as usize)
            .and_then(|n| n.checked_mul(stride))
            .ok_or("Native export row size overflow")?;
        if bytes.len() != expected {
            return Err("Native export row buffer has the wrong exact length".into());
        }
        let mut grid = native_grid::Grid::new(output, resampling, first_row, rows)?;
        let width = self.scene.metadata().width() as i32;
        let height = self.scene.metadata().height() as i32;
        while let Some(window) = grid.next()? {
            check()?;
            let mut failed_check = None;
            let prepared = RawLayers::for_topology(&self.topology, window.source, &mut || {
                check().map_err(|reason| {
                    failed_check = Some(reason);
                    Error::Cancelled
                })
            })
            .map_err(|reason| failed_check.unwrap_or(reason))?;
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
                        let at = (((y - first_row) as usize * output.width as usize) + x as usize)
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
            if depth == 8 {
                let plan = Plan::<Native8, _>::with_topology(
                    self.topology.clone(),
                    &prepared,
                    window.source,
                )?;
                fill(&|x, y| Ok(plan.pixel(x, y)?.map(u16::from)))?;
            } else {
                let plan = Plan::<Native16, _>::with_topology(
                    self.topology.clone(),
                    &prepared,
                    window.source,
                )?;
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
    use crate::production_export::{Control, Format, Image};
    #[test]
    fn actual_native_composite_stripes_match_full_legacy_crop_padding_and_resampling() {
        for depth in [8, 16] {
            let doc = crate::compositor::native::tests::doc(depth, true, "screen");
            let expected = Image::render(&doc, None, &Control::default()).unwrap();
            let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
            let provider = Rows::new(&scene).unwrap();
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
            assert!(Rows::new(&scene).is_err());
            assert!(Rows::for_target(&scene, Some(&doc.layers[2].id)).is_err());
            assert!(Rows::for_target(&scene, Some("missing")).is_err());
            let free = scene.budget().limit() - scene.budget().used();
            let held = scene.budget().reserve_bytes(free, free).unwrap();
            assert!(Rows::for_target(&scene, Some(&doc.layers[0].id)).is_err());
            assert_eq!(scene.budget().used(), scene.budget().limit());
            drop(held);
            for index in [0, 1, 4] {
                let target = &doc.layers[index].id;
                let expected = Image::render(&doc, Some(target), &Control::default()).unwrap();
                let provider = Rows::for_target(&scene, Some(target)).unwrap();
                if index == 0 {
                    let region = crate::raster::backed::Region::new([0, 0, 9, 7]).unwrap();
                    let prepared =
                        RawLayers::for_topology(&provider.topology, region, &mut || Ok(()))
                            .unwrap();
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
                    let mut provider = Rows::for_target(&scene, target.as_deref()).unwrap();
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
}
