//! Retained input selection from an immutable generation-bound native document.
use crate::{
    engine::native::{NativeDocument, RasterMetadata, RasterSlot, RetainedMetadata},
    large_document::{Reservation, ResidentBudget, Result as SourceResult},
    raster::Surface,
    transform::native::{self, Mapping, Tile, Work},
};

pub(crate) struct Original<'a> {
    document: &'a NativeDocument,
    current: RasterSlot,
    pixels: RasterSlot,
    retained: Option<RetainedMetadata<'a>>,
    mapping: Mapping,
}

struct Stages {
    values: Vec<crate::warp::Stage>,
    _charge: Reservation,
}
fn copy_stages(
    retained: Option<RetainedMetadata<'_>>,
    budget: &ResidentBudget,
) -> Result<Stages, String> {
    let warps = retained.map_or(&[][..], |original| original.warps());
    if warps.len() > crate::warp::MAX_STAGES {
        return Err("Retained Warp stage limit exceeded".into());
    }
    let charge = budget
        .reserve_bytes(16 * 1024, 16 * 1024)
        .map_err(|e| e.to_string())?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(warps.len())
        .map_err(|_| "Native Warp preparation allocation refused")?;
    if values
        .capacity()
        .checked_mul(std::mem::size_of::<crate::warp::Stage>())
        .is_none_or(|bytes| bytes > charge.bytes())
    {
        return Err("Native Warp preparation exceeds its admitted metadata".into());
    }
    for stage in warps {
        values.push(crate::warp::Stage {
            before: stage.before,
            grid: crate::warp::Grid {
                rect: stage.grid.rect,
                points: stage.grid.points,
            },
        });
    }
    Ok(Stages {
        values,
        _charge: charge,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{
            scene::{
                Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, Retained36,
            },
            Document,
        },
        large_document::{DiskBudget, ResidentBudget},
        psd::memory36::{Admitted, MetadataCharge},
    };
    use std::sync::Arc;

    struct Collector(Vec<Surface>);
    impl RasterCollector for Collector {
        fn resident(&mut self, raster: &crate::raster::Raster) -> Result<RasterRef36, String> {
            self.0
                .push(Surface::zero(raster.width, raster.height, raster.depth).unwrap());
            Ok(RasterRef36 {
                id: self.0.len() as u32,
                width: raster.width,
                height: raster.height,
                depth: raster.depth,
                retained: None,
            })
        }
        fn surface(&mut self, _: &Surface) -> Result<RasterRef36, String> {
            Err("Unexpected fixture source".into())
        }
    }
    fn document(pool: &ResidentBudget, retained: bool) -> NativeDocument {
        let source = Document::new_depth(8, 8, 16).unwrap();
        let mut collector = Collector(Vec::new());
        let mut metadata = Document36::capture(&source, &mut collector).unwrap();
        if retained {
            collector.0.push(Surface::zero(6, 7, 16).unwrap());
            metadata.layers[0].pixels.retained = Some(Box::new(Retained36 {
                pixels: RasterRef36 {
                    id: collector.0.len() as u32,
                    width: 6,
                    height: 7,
                    depth: 16,
                    retained: None,
                },
                matrix: [1., 0., 0., 1., 2., 3.],
                projective: None,
                warps: Vec::new(),
            }));
        }
        let owner = MetadataCharge::reserve(pool, 64 * 1024, &|| Ok(())).unwrap();
        NativeDocument::from_pending(
            PendingScene::new(
                Admitted::from_parts(Arc::new(metadata), owner),
                RasterRoster::new(collector.0).unwrap(),
                pool.clone(),
                DiskBudget::new(1024 * 1024, 8).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn projection_keeps_original_placement_and_generation_owner() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let document = document(&pool, true);
        let layer = document.metadata().layers().next().unwrap().id();
        let baseline = pool.used();
        let input = Original::layer(&document, layer).unwrap();
        assert_eq!(input.document().key(), document.key());
        assert_ne!(input.current_slot(), input.original_slot());
        assert_eq!(
            [
                input.source().unwrap().width(),
                input.source().unwrap().height()
            ],
            [6, 7]
        );
        assert_eq!(input.mapping().matrix(), [1., 0., 0., 1., 2., 3.]);
        assert_eq!(input.metadata().unwrap().matrix(), input.mapping().matrix());
        assert_eq!(pool.used(), baseline);
        let mut work = Work::new(1024 * 1024).unwrap();
        assert_eq!(
            input.bounds(true, &mut work, &|| Ok(())).unwrap(),
            [2., 3., 8., 10.]
        );
        assert_eq!(
            input.bounds(false, &mut work, &|| Ok(())).unwrap(),
            [2., 3., 8., 10.]
        );
        assert!(input
            .bounds(false, &mut work, &|| Err(
                crate::large_document::Error::Cancelled
            ))
            .is_err());
        assert_eq!(pool.used(), baseline);
        let tile = input
            .render_tile([8, 8], (0, 0), &mut work, &|| Ok(()))
            .unwrap();
        assert_eq!(tile.source(), input.source().unwrap().key());
        assert!(tile.bytes().iter().all(|b| *b == 0));
        drop(tile);
        drop(input);
        drop(document);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn foreign_generation_metadata_refuses_without_losing_owners() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let first = document(&pool, false);
        let second = document(&pool, true);
        let foreign = first.metadata().layers().next().unwrap().pixels();
        let baseline = pool.used();
        assert!(Original::raster(&second, foreign).is_err());
        assert!(Original::layer(&second, "missing").is_err());
        assert_eq!(pool.used(), baseline);
        assert!(first.source(foreign.slot()).is_ok());
        drop(first);
        drop(second);
        assert_eq!(pool.used(), 0);
    }
}
impl<'a> Original<'a> {
    pub(crate) fn layer(document: &'a NativeDocument, layer: &str) -> Result<Self, String> {
        let pixels = document
            .metadata()
            .layers()
            .find(|l| l.id() == layer)
            .ok_or("Unknown native transform layer")?
            .pixels();
        Self::raster(document, pixels)
    }
    pub(crate) fn raster(
        document: &'a NativeDocument,
        current: RasterMetadata<'a>,
    ) -> Result<Self, String> {
        let current_slot = current.slot();
        document.source(current_slot)?;
        let retained = current.retained();
        let metadata = retained.map_or(current, |original| original.pixels());
        if metadata.retained().is_some() {
            return Err("Nested native transform originals are unsupported".into());
        }
        let pixels = metadata.slot();
        let source = document.source(pixels)?;
        if source.depth() != current.depth() {
            return Err("Native retained source precision differs".into());
        }
        // Copy only bounded fixed-size placement records. Original pixels and
        // all metadata owners stay in the borrowed NativeDocument generation.
        let stages = copy_stages(retained, document.budget())?;
        let matrix = retained.map_or(super::IDENTITY, |original| original.matrix());
        let projective = retained.and_then(|original| original.projective());
        let mapping = Mapping::retained(
            source.width(),
            source.height(),
            source.depth(),
            matrix,
            projective,
            &stages.values,
            document.budget(),
        )?;
        drop(stages);
        Ok(Self {
            document,
            current: current_slot,
            pixels,
            retained,
            mapping,
        })
    }
    pub(crate) fn document(&self) -> &'a NativeDocument {
        self.document
    }
    pub(crate) fn current_slot(&self) -> RasterSlot {
        self.current
    }
    pub(crate) fn original_slot(&self) -> RasterSlot {
        self.pixels
    }
    pub(crate) fn metadata(&self) -> Option<RetainedMetadata<'a>> {
        self.retained
    }
    pub(crate) fn source(&self) -> Result<&'a Surface, String> {
        self.document.source(self.pixels)
    }
    pub(crate) fn mapping(&self) -> &Mapping {
        &self.mapping
    }
    pub(crate) fn bounds(
        &self,
        full: bool,
        work: &mut Work,
        check: &dyn Fn() -> SourceResult<()>,
    ) -> Result<[f64; 4], String> {
        check().map_err(|e| e.to_string())?;
        let source = self.source()?;
        let frame = [0, 0, source.width() as i32, source.height() as i32];
        let pixels = if full {
            frame
        } else {
            native::source_bounds(
                source,
                self.document.budget(),
                self.document.disk_budget(),
                work,
                check,
            )?
            .unwrap_or(frame)
        };
        let stages = copy_stages(self.retained, self.document.budget())?;
        let _prepared = if stages.values.is_empty() {
            None
        } else {
            Some(
                self.document
                    .budget()
                    .reserve_bytes(64 * 1024, 64 * 1024)
                    .map_err(|e| e.to_string())?,
            )
        };
        let matrix = self
            .retained
            .map_or(super::IDENTITY, |original| original.matrix());
        let projective = self.retained.and_then(|original| original.projective());
        super::geometry::validate(
            source.width(),
            source.height(),
            source.depth(),
            source.depth(),
            matrix,
            projective,
            &stages.values,
        )?;
        let bounds = super::geometry::bounds(pixels, matrix, projective, &stages.values)?;
        check().map_err(|e| e.to_string())?;
        Ok(bounds)
    }
    pub(crate) fn render_tile(
        &self,
        frame: [u32; 2],
        coordinate: (u32, u32),
        work: &mut Work,
        check: &dyn Fn() -> SourceResult<()>,
    ) -> Result<Tile, String> {
        native::render_tile(
            self.source()?,
            &self.mapping,
            frame,
            coordinate,
            None,
            self.document.budget(),
            self.document.disk_budget(),
            work,
            check,
        )
    }
}
