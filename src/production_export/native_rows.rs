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
        if scene.metadata().depth() != 8 && scene.metadata().depth() != 16 {
            return Err("Composite rows require native RGB8 or RGB16".into());
        }
        if scene
            .metadata()
            .filters()
            .any(|filter| filter.enabled() && filter.weight() > 0.)
        {
            return Err("Document filters require their explicit regional provider".into());
        }
        Ok(Self {
            scene,
            topology: Topology::new(scene)?,
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
        if output.target.is_some() {
            return Err("Isolated export requires its explicit captured tree provider".into());
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
            let prepared = RawLayers::prepare(self.scene, window.source, &mut || {
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
}
