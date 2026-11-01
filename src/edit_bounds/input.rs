//! Generation-bound read projections for native paint workers. No projection
//! grants edit permission, source publication, or an Engine history operation.
use super::native::FrameExpansion;
use crate::{
    engine::native::{DocumentKey, LayerMetadata, NativeDocument, RasterMetadata, RasterSlot},
    large_document::{Error, PinnedBytes, ResidentBudget, ScratchBytes},
    raster::{backed::Region, project16, Surface, SurfaceKey, SurfaceRegion, TILE},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InputBinding {
    generation: DocumentKey,
    slot: RasterSlot,
    surface: SurfaceKey,
    shape: [u32; 2],
    depth: u16,
    origin: [i32; 2],
}
impl InputBinding {
    pub(crate) fn generation(self) -> DocumentKey {
        self.generation
    }
    pub(crate) fn slot(self) -> RasterSlot {
        self.slot
    }
    pub(crate) fn source(self) -> SurfaceKey {
        self.surface
    }
    pub(crate) fn shape(self) -> [u32; 2] {
        self.shape
    }
    pub(crate) fn depth(self) -> u16 {
        self.depth
    }
}

pub(crate) struct RasterInput<'a> {
    document: &'a NativeDocument,
    layer: LayerMetadata<'a>,
    pixels: RasterMetadata<'a>,
    surface: &'a Surface,
    binding: InputBinding,
    mask: bool,
}
impl<'a> RasterInput<'a> {
    pub(crate) fn layer(document: &'a NativeDocument, layer: &str) -> Result<Self, String> {
        Self::resolve(document, layer, None)
    }
    pub(crate) fn mask_step(
        document: &'a NativeDocument,
        layer: &str,
        step: &str,
    ) -> Result<Self, String> {
        Self::resolve(document, layer, Some(step))
    }
    fn resolve(
        document: &'a NativeDocument,
        layer_id: &str,
        step: Option<&str>,
    ) -> Result<Self, String> {
        let layer = document
            .metadata()
            .layers()
            .find(|l| l.id() == layer_id)
            .ok_or("Unknown native paint layer")?;
        let pixels = if let Some(step_id) = step {
            layer
                .mask()
                .ok_or("Native layer has no mask")?
                .steps()
                .find(|s| s.id() == step_id && s.kind() == "paint")
                .ok_or("Unknown native mask step")?
                .pixels()
        } else {
            layer.pixels()
        };
        let surface = document.source(pixels.slot())?;
        let shape = [pixels.width(), pixels.height()];
        if shape != [surface.width(), surface.height()] || pixels.depth() != surface.depth() {
            return Err("Native paint metadata differs from its captured source".into());
        }
        let binding = InputBinding {
            generation: document.key(),
            slot: pixels.slot(),
            surface: surface.key(),
            shape,
            depth: pixels.depth(),
            origin: layer.origin(),
        };
        Ok(Self {
            document,
            layer,
            pixels,
            surface,
            binding,
            mask: step.is_some(),
        })
    }
    pub(crate) fn binding(&self) -> InputBinding {
        self.binding
    }
    pub(crate) fn validate(&self, binding: InputBinding) -> Result<(), String> {
        if self.binding != binding {
            return Err("Native paint input belongs to another source generation or frame".into());
        }
        Ok(())
    }
    pub(crate) fn document(&self) -> &'a NativeDocument {
        self.document
    }
    pub(crate) fn metadata(&self) -> RasterMetadata<'a> {
        self.pixels
    }
    pub(crate) fn layer_metadata(&self) -> LayerMetadata<'a> {
        self.layer
    }
    pub(crate) fn shape(&self) -> [u32; 2] {
        self.binding.shape
    }
    pub(crate) fn origin(&self) -> [i32; 2] {
        self.binding.origin
    }
    pub(crate) fn depth(&self) -> u16 {
        self.binding.depth
    }
    pub(crate) fn pool(&self) -> &ResidentBudget {
        self.document.budget()
    }
    pub(crate) fn is_mask(&self) -> bool {
        self.mask
    }
    pub(crate) fn color(&self, color: [u8; 4]) -> [u8; 4] {
        if self.mask {
            [color[0], color[0], color[0], color[3]]
        } else {
            color
        }
    }
    pub(crate) fn local_rect(&self, area: [i32; 4]) -> Result<[i32; 4], String> {
        translate(area, self.origin(), true)
    }
    pub(crate) fn document_rect(&self, area: [i32; 4]) -> Result<[i32; 4], String> {
        translate(area, self.origin(), false)
    }
    pub(crate) fn local_clip(&self) -> Result<Option<[i32; 4]>, String> {
        self.document
            .metadata()
            .selection()
            .map(|r| self.local_rect(r))
            .transpose()
    }
    pub(crate) fn tile_document_rect(&self, key: (u32, u32)) -> Result<[i32; 4], String> {
        translate(tile_rect(self.shape(), key)?, self.origin(), false)
    }
    /// The worker must call outside Engine/UI locks. Full physical padding is
    /// preserved; an absent descriptor is explicit known zero, never a read error.
    pub(crate) fn prepare_tile(
        &self,
        key: (u32, u32),
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<InputTile, String> {
        check()?;
        let tile = self
            .surface
            .prepare_tile(key, &mut || check().map_err(|_| Error::Cancelled))
            .map_err(|e| e.to_string())?;
        let bytes = tile
            .copy_to_scratch(self.pool())
            .map_err(|e| e.to_string())?;
        check()?;
        Ok(InputTile {
            binding: self.binding,
            key,
            bytes: bytes.freeze(),
        })
    }
    pub(crate) fn prepare_region(
        &self,
        area: [i32; 4],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedInput<'a>, String> {
        check()?;
        let region = Region::new(area).map_err(|e| e.to_string())?;
        let pixels = self
            .surface
            .prepare_region(region, &mut || check().map_err(|_| Error::Cancelled))
            .map_err(|e| e.to_string())?;
        check()?;
        Ok(PreparedInput {
            binding: self.binding,
            pixels,
            _document: self.document,
        })
    }
    /// Healing needs a complete384-square halo, slightly larger than one native
    /// region. Stage bounded strips into a separately charged linear owner;
    /// subsequent integral-table callbacks remain memory-only and fallible.
    pub(crate) fn prepare_halo(
        &self,
        area: [i32; 4],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedPixels<'a>, String> {
        self.prepare_linear(area, 384, 384 * 384, check)
    }
    pub(crate) fn prepare_strip(
        &self,
        area: [i32; 4],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedPixels<'a>, String> {
        self.prepare_linear(area, 16000, crate::raster::backed::MAX_REGION_PIXELS, check)
    }
    fn prepare_linear(
        &self,
        area: [i32; 4],
        edge: i64,
        max_pixels: usize,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedPixels<'a>, String> {
        check()?;
        let width = i64::from(area[2]) - i64::from(area[0]);
        let height = i64::from(area[3]) - i64::from(area[1]);
        if width <= 0
            || height <= 0
            || width > edge
            || height > edge
            || width
                .checked_mul(height)
                .is_none_or(|n| n > max_pixels as i64)
            || area.iter().any(|v| v.unsigned_abs() > 100000)
        {
            return Err(
                "Native paint linear footprint exceeds its explicit halo/strip admission".into(),
            );
        }
        let bpp = if self.depth() == 16 { 8 } else { 4 };
        let length = width as usize * height as usize * bpp;
        let mut bytes =
            ScratchBytes::new(self.pool(), length, length).map_err(|e| e.to_string())?;
        let rows = (crate::raster::backed::MAX_REGION_PIXELS / width as usize).max(1) as i32;
        let mut top = area[1];
        while top < area[3] {
            check()?;
            let bottom = (top + rows).min(area[3]);
            let prepared = self.prepare_region([area[0], top, area[2], bottom], check)?;
            for y in top..bottom {
                check()?;
                for x in area[0]..area[2] {
                    let at =
                        ((y - area[1]) as usize * width as usize + (x - area[0]) as usize) * bpp;
                    for (c, value) in prepared.pixel16(x, y)?.into_iter().enumerate() {
                        if self.depth() == 16 {
                            bytes[at + c * 2..at + c * 2 + 2].copy_from_slice(&value.to_be_bytes());
                        } else {
                            bytes[at + c] = project16(value);
                        }
                    }
                }
            }
            top = bottom;
        }
        check()?;
        Ok(PreparedPixels {
            binding: self.binding,
            area,
            bytes: bytes.freeze(),
            _document: self.document,
        })
    }
    pub(crate) fn frame_expansion(
        &self,
        requested_document: [i32; 4],
    ) -> Result<Option<FrameExpansion>, String> {
        let metadata = self.document.metadata();
        FrameExpansion::for_edit(
            [metadata.width(), metadata.height()],
            self.shape(),
            self.origin(),
            requested_document,
            metadata.selection(),
            self.depth(),
            self.pool(),
        )
    }
    pub(crate) fn project_frame(
        &self,
        requested_document: [i32; 4],
    ) -> Result<Option<InputFrame>, String> {
        self.frame_expansion(requested_document).map(|plan| {
            plan.map(|geometry| InputFrame {
                binding: self.binding,
                layer: self.layer.pixels().slot(),
                geometry,
            })
        })
    }
    pub(crate) fn selection(&self) -> Result<SelectionInput<'a>, String> {
        SelectionInput::new(self.document)
    }
}
pub(crate) struct InputFrame {
    binding: InputBinding,
    layer: RasterSlot,
    geometry: FrameExpansion,
}
impl InputFrame {
    pub(crate) fn binding(&self) -> InputBinding {
        self.binding
    }
    pub(crate) fn geometry(&self) -> &FrameExpansion {
        &self.geometry
    }
    pub(crate) fn tile_document_rect(&self, key: (u32, u32)) -> Result<[i32; 4], String> {
        translate(
            tile_rect(self.geometry.new_shape(), key)?,
            self.geometry.new_origin(),
            false,
        )
    }
    pub(crate) fn for_plane(&self, input: &RasterInput<'_>) -> Result<Self, String> {
        if self.binding.generation != input.binding.generation
            || self.layer != input.layer.pixels().slot()
            || self.geometry.old_origin() != input.origin()
        {
            return Err("Native frame plane belongs to another layer or source generation".into());
        }
        Ok(Self {
            binding: input.binding,
            layer: self.layer,
            geometry: self
                .geometry
                .for_plane(input.shape(), input.depth(), input.pool())?,
        })
    }
}
fn tile_rect(shape: [u32; 2], key: (u32, u32)) -> Result<[i32; 4], String> {
    if key.0 >= shape[0].div_ceil(TILE) || key.1 >= shape[1].div_ceil(TILE) {
        return Err("Native tile is outside the projected frame".into());
    }
    Ok([
        (key.0 * TILE) as i32,
        (key.1 * TILE) as i32,
        ((key.0 + 1) * TILE).min(shape[0]) as i32,
        ((key.1 + 1) * TILE).min(shape[1]) as i32,
    ])
}
pub(crate) struct InputTile {
    binding: InputBinding,
    key: (u32, u32),
    bytes: PinnedBytes,
}
pub(crate) struct PreparedPixels<'a> {
    binding: InputBinding,
    area: [i32; 4],
    bytes: PinnedBytes,
    _document: &'a NativeDocument,
}
impl PreparedPixels<'_> {
    pub(crate) fn binding(&self) -> InputBinding {
        self.binding
    }
    pub(crate) fn pixel16(&self, x: i32, y: i32) -> Result<[u16; 4], String> {
        if x < self.area[0] || y < self.area[1] || x >= self.area[2] || y >= self.area[3] {
            return Err("Sample is outside the prepared native paint pixels".into());
        }
        let index = (y - self.area[1]) as usize * (self.area[2] - self.area[0]) as usize
            + (x - self.area[0]) as usize;
        Ok(std::array::from_fn(|c| {
            if self.binding.depth == 16 {
                u16::from_be_bytes([
                    self.bytes[index * 8 + c * 2],
                    self.bytes[index * 8 + c * 2 + 1],
                ])
            } else {
                u16::from(self.bytes[index * 4 + c]) * 257
            }
        }))
    }
    pub(crate) fn into_bytes(self) -> PinnedBytes {
        self.bytes
    }
}
impl InputTile {
    pub(crate) fn binding(&self) -> InputBinding {
        self.binding
    }
    pub(crate) fn key(&self) -> (u32, u32) {
        self.key
    }
    pub(crate) fn bytes(&self) -> &PinnedBytes {
        &self.bytes
    }
}
fn translate(area: [i32; 4], origin: [i32; 2], subtract: bool) -> Result<[i32; 4], String> {
    let mut translated = [0; 4];
    for i in 0..4 {
        translated[i] = if subtract {
            area[i].checked_sub(origin[i % 2])
        } else {
            area[i].checked_add(origin[i % 2])
        }
        .ok_or("Native paint coordinate overflow")?;
    }
    Ok(translated)
}

pub(crate) struct PreparedInput<'a> {
    binding: InputBinding,
    pixels: SurfaceRegion,
    _document: &'a NativeDocument,
}
impl PreparedInput<'_> {
    pub(crate) fn binding(&self) -> InputBinding {
        self.binding
    }
    pub(crate) fn pixel16(&self, x: i32, y: i32) -> Result<[u16; 4], String> {
        self.pixels.pixel16(x, y).map_err(|e| e.to_string())
    }
}

/// Geometry and contours stay borrowed from the admitted document. Bounds on
/// coverage select priority only; sampling uses its physical origin and extent.
pub(crate) struct SelectionInput<'a> {
    document: &'a NativeDocument,
    bounds: Option<[i32; 4]>,
    polygon: Option<&'a [[f32; 2]]>,
    coverage: Option<(&'a Surface, [i32; 2])>,
}
impl<'a> SelectionInput<'a> {
    pub(crate) fn new(document: &'a NativeDocument) -> Result<Self, String> {
        let metadata = document.metadata();
        let bounds = metadata.selection();
        let polygon = metadata
            .selection_polygon()
            .filter(|p| crate::selection::bounds(p).ok() == bounds);
        let coverage = metadata
            .selection_coverage()
            .filter(|c| Some(c.bounds()) == bounds)
            .map(|c| Ok::<_, String>((document.source(c.mask().slot())?, c.origin())))
            .transpose()?;
        Ok(Self {
            document,
            bounds,
            polygon,
            coverage,
        })
    }
    pub(crate) fn polygon(&self) -> Option<&'a [[f32; 2]]> {
        self.polygon
    }
    pub(crate) fn bounds(&self) -> Option<[i32; 4]> {
        self.bounds
    }
    pub(crate) fn has_coverage(&self) -> bool {
        self.coverage.is_some()
    }
    pub(crate) fn prepare(
        &self,
        area_document: [i32; 4],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedSelection<'a>, String> {
        check()?;
        let width = i64::from(area_document[2]) - i64::from(area_document[0]);
        let height = i64::from(area_document[3]) - i64::from(area_document[1]);
        if width <= 0
            || height <= 0
            || width
                .checked_mul(height)
                .is_none_or(|n| n > crate::raster::backed::MAX_REGION_PIXELS as i64)
        {
            return Err("Native selection footprint exceeds its bounded sampling area".into());
        }
        let coverage = self
            .coverage
            .map(|(surface, origin)| {
                let requested = translate(area_document, origin, true)?;
                let local = [
                    requested[0].max(0),
                    requested[1].max(0),
                    requested[2].min(surface.width() as i32),
                    requested[3].min(surface.height() as i32),
                ];
                let pixels = if local[0] < local[2] && local[1] < local[3] {
                    Some(
                        surface
                            .prepare_region(
                                Region::new(local).map_err(|e| e.to_string())?,
                                &mut || check().map_err(|_| Error::Cancelled),
                            )
                            .map_err(|e| e.to_string())?,
                    )
                } else {
                    None
                };
                Ok::<_, String>(PreparedCoverage {
                    pixels,
                    origin,
                    shape: [surface.width(), surface.height()],
                })
            })
            .transpose()?;
        check()?;
        Ok(PreparedSelection {
            area: area_document,
            bounds: self.bounds,
            polygon: self.polygon,
            coverage,
            _document: self.document,
        })
    }
}
pub(crate) struct PreparedSelection<'a> {
    area: [i32; 4],
    bounds: Option<[i32; 4]>,
    polygon: Option<&'a [[f32; 2]]>,
    coverage: Option<PreparedCoverage>,
    _document: &'a NativeDocument,
}
struct PreparedCoverage {
    pixels: Option<SurfaceRegion>,
    origin: [i32; 2],
    shape: [u32; 2],
}
impl PreparedSelection<'_> {
    pub(crate) fn value(&self, x: i32, y: i32) -> Result<f32, String> {
        let r = self.area;
        if x < r[0] || y < r[1] || x >= r[2] || y >= r[3] {
            return Err("Selection sample is outside the native paint footprint".into());
        }
        if let Some(coverage) = &self.coverage {
            let local = [
                i64::from(x) - i64::from(coverage.origin[0]),
                i64::from(y) - i64::from(coverage.origin[1]),
            ];
            if local[0] < 0
                || local[1] < 0
                || local[0] >= i64::from(coverage.shape[0])
                || local[1] >= i64::from(coverage.shape[1])
            {
                return Ok(0.);
            }
            return coverage
                .pixels
                .as_ref()
                .ok_or("Prepared coverage misses an in-source sample")?
                .pixel(local[0] as i32, local[1] as i32)
                .map(|p| p[0] as f32 / 255.)
                .map_err(|e| e.to_string());
        }
        let Some(bounds) = self.bounds else {
            return Ok(1.);
        };
        Ok(f32::from(u8::from(
            x >= bounds[0]
                && y >= bounds[1]
                && x < bounds[2]
                && y < bounds[3]
                && self
                    .polygon
                    .is_none_or(|p| crate::selection::contains(p, x as f32 + 0.5, y as f32 + 0.5)),
        )))
    }
}

/// Exact legacy post-edit soft-coverage restriction. Clear's own coverage kernel
/// must not call this a second time. Pixel reads here are prepared memory only.
pub(crate) fn restrict_tile(
    input: &RasterInput<'_>,
    key: (u32, u32),
    before: &PinnedBytes,
    after: &PinnedBytes,
    selection: &PreparedSelection<'_>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<PinnedBytes, String> {
    restrict_geometry(
        input,
        input.shape(),
        input.origin(),
        key,
        before,
        after,
        selection,
        check,
    )
}
pub(crate) fn restrict_frame_tile(
    input: &RasterInput<'_>,
    frame: &InputFrame,
    key: (u32, u32),
    before: &PinnedBytes,
    after: &PinnedBytes,
    selection: &PreparedSelection<'_>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<PinnedBytes, String> {
    input.validate(frame.binding())?;
    restrict_geometry(
        input,
        frame.geometry().new_shape(),
        frame.geometry().new_origin(),
        key,
        before,
        after,
        selection,
        check,
    )
}
fn restrict_geometry(
    input: &RasterInput<'_>,
    shape: [u32; 2],
    origin: [i32; 2],
    key: (u32, u32),
    before: &PinnedBytes,
    after: &PinnedBytes,
    selection: &PreparedSelection<'_>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<PinnedBytes, String> {
    check()?;
    let bpp = if input.depth() == 16 { 8 } else { 4 };
    let length = (TILE * TILE) as usize * bpp;
    if !before.uses_budget(input.pool())
        || !after.uses_budget(input.pool())
        || before.len() != length
        || after.len() != length
        || key.0 >= shape[0].div_ceil(TILE)
        || key.1 >= shape[1].div_ceil(TILE)
    {
        return Err("Soft-selection tile input differs from its admitted native frame".into());
    }
    if selection._document.key() != input.binding.generation {
        return Err("Soft selection belongs to another native generation".into());
    }
    if selection.coverage.is_none() {
        return Ok(after.clone());
    }
    let mut output = ScratchBytes::new(input.pool(), length, length).map_err(|e| e.to_string())?;
    output.copy_from_slice(after);
    for y in key.1 * TILE..((key.1 + 1) * TILE).min(shape[1]) {
        check()?;
        for x in key.0 * TILE..((key.0 + 1) * TILE).min(shape[0]) {
            let t = selection.value(x as i32 + origin[0], y as i32 + origin[1])?;
            if t == 1. {
                continue;
            }
            let at = ((y % TILE) * TILE + x % TILE) as usize * bpp;
            let read = |bytes: &[u8]| -> [u16; 4] {
                std::array::from_fn(|c| {
                    if input.depth() == 16 {
                        u16::from_be_bytes([bytes[at + c * 2], bytes[at + c * 2 + 1]])
                    } else {
                        u16::from(bytes[at + c]) * 257
                    }
                })
            };
            let old = read(before);
            let new = read(after);
            if old == new {
                continue;
            }
            let alpha = old[3] as f64 * (1. - t as f64) + new[3] as f64 * t as f64;
            let mut result = [0u16; 4];
            result[3] = alpha.round() as u16;
            for c in 0..3 {
                result[c] = if alpha > 0. {
                    ((old[c] as f64 * old[3] as f64 * (1. - t as f64)
                        + new[c] as f64 * new[3] as f64 * t as f64)
                        / alpha)
                        .round() as u16
                } else {
                    0
                };
            }
            for c in 0..4 {
                if input.depth() == 16 {
                    output[at + c * 2..at + c * 2 + 2].copy_from_slice(&result[c].to_be_bytes());
                } else {
                    output[at + c] = project16(result[c]);
                }
            }
        }
    }
    check()?;
    Ok(output.freeze())
}

#[cfg(test)]
#[path = "input/tests.rs"]
mod tests;
