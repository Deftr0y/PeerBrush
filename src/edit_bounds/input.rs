//! Generation-bound read projections for native paint workers. No projection
//! grants edit permission, source publication, or an Engine history operation.
use super::native::FrameExpansion;
use crate::{
    engine::native::{
        CapturedEdit, Command, CommandToken, DocumentKey, LayerMetadata, Method, NativeDocument,
        RasterMetadata, RasterSlot,
    },
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
/// Producer guard for one exact immutable command in the captured descriptor.
/// It cannot replace scopes or authorize a different source/frame.
pub(crate) struct WriteGuard<'c> {
    _capture: &'c CapturedEdit,
    cancellation: crate::engine::native::Cancellation,
    command: Command<'c>,
    binding: InputBinding,
    shape: [u32; 2],
    origin: [i32; 2],
    writes: std::cell::Cell<Option<[i32; 4]>>,
}
impl<'c> WriteGuard<'c> {
    pub(crate) fn new(
        captured: &'c CapturedEdit,
        command: Command<'c>,
        input: &RasterInput<'_>,
        frame: Option<&InputFrame>,
    ) -> Result<Self, String> {
        let cancellation = captured.cancellation();
        cancellation.check()?;
        if captured.base_key() != input.binding.generation
            || !captured
                .descriptor()
                .is_some_and(|d| d.commands().any(|c| c.token() == command.token()))
        {
            return Err("Native paint command is not owned by its captured descriptor".into());
        }
        let parameters = command.parameters();
        let operation = parameters["op"]
            .as_str()
            .ok_or("Native paint command has no operation")?;
        let method = match operation {
            "paint" | "shape" | "gradient" | "fill" | "adjust" | "pixels.replace"
            | "image.patch" => Method::Paint,
            "paint.fill" => Method::Fill,
            "paint.clear_selection" => Method::Clear,
            "smudge" => Method::Smudge,
            "clone" | "heal" => Method::Retouch,
            _ => return Err("Native paint guard requires a paint operator".into()),
        };
        if command.method() != method {
            return Err("Native paint method differs from its descriptor".into());
        }
        let target = parameters["layer"]
            .as_str()
            .ok_or("Native paint command has no target")?;
        let mask = parameters
            .get("mask")
            .map_or(Ok(false), |v| v.as_bool().ok_or("mask must be boolean"))?;
        if mask != input.is_mask() {
            return Err("Native paint command targets a different color or mask plane".into());
        }
        if !mask && input.layer_metadata().source().is_some() {
            return Err("Edit this layer's text/vector properties, paint its mask, or explicitly rasterize it first".into());
        }
        if target != input.layer_metadata().id() {
            if method != Method::Clear || mask {
                return Err("Native paint command targets another layer".into());
            }
            let document = captured.document();
            let parent = document
                .metadata()
                .layers()
                .find(|layer| layer.id() == target)
                .ok_or("Unknown native clear folder")?;
            if parent.kind() != "group" {
                return Err("Resolved native clear target is not a folder child".into());
            }
            let mut current = input.layer_metadata().parent();
            let mut found = false;
            for _ in 0..=16 {
                let Some(id) = current else {
                    break;
                };
                if id == target {
                    found = true;
                    break;
                }
                current = document
                    .metadata()
                    .layers()
                    .find(|layer| layer.id() == id)
                    .and_then(|layer| layer.parent());
            }
            if !found {
                return Err("Resolved native clear target is not a folder child".into());
            }
        }
        if mask
            && RasterInput::from_parameters(captured.document(), parameters)?.binding()
                != input.binding()
        {
            return Err("Native paint command targets another raw mask step".into());
        }
        let (shape, origin) = if let Some(frame) = frame {
            input.validate(frame.binding())?;
            if frame.command != Some(command.token()) {
                return Err("Native frame was not planned from this immutable command".into());
            }
            (frame.geometry().new_shape(), frame.geometry().new_origin())
        } else {
            (input.shape(), input.origin())
        };
        Ok(Self {
            _capture: captured,
            cancellation,
            command,
            binding: input.binding,
            shape,
            origin,
            writes: std::cell::Cell::new(None),
        })
    }
    pub(crate) fn write_bounds(&self) -> Option<[i32; 4]> {
        self.writes.get()
    }
    fn record_write(&self, bounds: [i32; 4]) {
        self.writes.set(Some(match self.writes.get() {
            None => bounds,
            Some(old) => [
                old[0].min(bounds[0]),
                old[1].min(bounds[1]),
                old[2].max(bounds[2]),
                old[3].max(bounds[3]),
            ],
        }));
    }
    pub(crate) fn parameters(&self) -> &'c serde_json::Value {
        self.command.parameters()
    }
    pub(crate) fn check_active(&self) -> Result<(), String> {
        self.cancellation.check()
    }
    pub(crate) fn token(&self) -> CommandToken {
        self.command.token()
    }
    pub(crate) fn capture(&self) -> &CapturedEdit {
        self._capture
    }
    pub(crate) fn validate(
        &self,
        input: &RasterInput<'_>,
        frame: Option<&InputFrame>,
        operations: &[&str],
    ) -> Result<(), String> {
        self.check_active()?;
        input.validate(self.binding)?;
        let (shape, origin) = if let Some(frame) = frame {
            input.validate(frame.binding())?;
            (frame.geometry().new_shape(), frame.geometry().new_origin())
        } else {
            (input.shape(), input.origin())
        };
        if shape != self.shape
            || origin != self.origin
            || !self.parameters()["op"]
                .as_str()
                .is_some_and(|operation| operations.contains(&operation))
        {
            return Err("Native paint guard belongs to another operator or proposed frame".into());
        }
        Ok(())
    }
    pub(crate) fn check_tile(
        &self,
        input: &RasterInput<'_>,
        frame: Option<&InputFrame>,
        key: (u32, u32),
        before: &PinnedBytes,
        after: &PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.validate(
            input,
            frame,
            &[
                "paint",
                "shape",
                "gradient",
                "fill",
                "adjust",
                "pixels.replace",
                "image.patch",
                "paint.fill",
                "paint.clear_selection",
                "smudge",
                "clone",
                "heal",
            ],
        )?;
        if let Some(bounds) = changed_bounds(input, frame, key, before, after, check)? {
            self.command
                .check_write(input.document(), input.binding.slot, bounds)?;
            self.check_active()?;
            self.record_write(bounds);
        }
        self.check_active()
    }
    pub(crate) fn check_child_tile(
        &self,
        input: &RasterInput<'_>,
        key: (u32, u32),
        before: &PinnedBytes,
        after: &PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.validate(input, None, &["paint.fill", "fill"])?;
        if input.is_mask() || input.layer_metadata().kind() != "group" {
            return Err("Native child fill requires its captured folder target".into());
        }
        let metadata = input.document().metadata();
        if let Some(bounds) = changed_geometry(
            input.pool(),
            metadata.depth(),
            [metadata.width(), metadata.height()],
            [0, 0],
            key,
            before,
            after,
            check,
        )? {
            // The existing folder slot is only the authorization target. Its
            // unused plane is never replaced; the engine inserts a new child.
            self.command
                .check_write(input.document(), input.binding.slot, bounds)?;
            self.check_active()?;
            self.record_write(bounds);
        }
        self.check_active()
    }
    pub(crate) fn validate_fill_frame(
        &self,
        input: &RasterInput<'_>,
        frame: &InputFrame,
    ) -> Result<(), String> {
        self.check_active()?;
        input.validate(frame.binding())?;
        if self._capture.base_key() != input.binding.generation
            || input.is_mask()
            || input.layer_metadata().kind() != "fill"
            || input.layer_metadata().source().is_some()
            || self.parameters()["mask"] != true
            || self.parameters()["layer"].as_str() != Some(input.layer_metadata().id())
            || !matches!(
                self.parameters()["op"].as_str(),
                Some(
                    "paint"
                        | "smudge"
                        | "clone"
                        | "heal"
                        | "paint.fill"
                        | "shape"
                        | "gradient"
                        | "fill"
                )
            )
            || frame.command != Some(self.token())
            || frame.geometry().new_shape() != self.shape
            || frame.geometry().new_origin() != self.origin
        {
            return Err(
                "Native fill baseline is not an ancillary frame of this captured mask command"
                    .into(),
            );
        }
        Ok(())
    }
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
    pub(crate) fn origin(self) -> [i32; 2] {
        self.origin
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
pub(crate) enum RetouchInput<'a> {
    Raster(RasterInput<'a>),
    Merged(&'a NativeDocument),
    ComposedMask {
        document: &'a NativeDocument,
        layer: LayerMetadata<'a>,
    },
}

fn read_with_guard<T>(
    check: &mut dyn FnMut() -> Result<(), String>,
    read: impl FnOnce(
        &mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> crate::large_document::Result<T>,
) -> Result<T, String> {
    let mut failure = None;
    let result = read(&mut || {
        check().map_err(|reason| {
            failure = Some(reason);
            Error::Cancelled
        })
    });
    match failure {
        Some(reason) => Err(reason),
        None => result.map_err(|error| error.to_string()),
    }
}

impl<'a> RasterInput<'a> {
    /// Resolve only immutable captured metadata. Disabled paint steps remain
    /// editable; a missing target step never falls back to a different plane.
    pub(crate) fn from_parameters(
        document: &'a NativeDocument,
        parameters: &serde_json::Value,
    ) -> Result<Self, String> {
        let target = parameters["layer"]
            .as_str()
            .ok_or("Choose a target layer")?;
        let mask = parameters
            .get("mask")
            .map_or(Ok(false), |v| v.as_bool().ok_or("mask must be boolean"))?;
        if !mask {
            return Self::layer(document, target);
        }
        let layer = document
            .metadata()
            .layers()
            .find(|l| l.id() == target)
            .ok_or("Unknown native paint layer")?;
        let mask = layer.mask().ok_or("Layer has no mask")?;
        let step = if let Some(step) = parameters.get("step").filter(|step| !step.is_null()) {
            step.as_str().ok_or("Mask step must be an ID")?
        } else {
            mask.steps()
                .filter(|step| step.kind() == "paint")
                .last()
                .ok_or("Add a paint step to the mask")?
                .id()
        };
        Self::mask_step(document, target, step)
    }
    pub(crate) fn retouch_source(
        &self,
        parameters: &serde_json::Value,
    ) -> Result<RetouchInput<'a>, String> {
        let boolean = |key: &str| {
            parameters.get(key).map_or(Ok(false), |v| {
                v.as_bool().ok_or_else(|| format!("{key} must be boolean"))
            })
        };
        let mask = boolean("mask")?;
        let merged = boolean("sample_merged")?;
        if parameters.get("source_step").is_some() && (!mask || merged) {
            return Err("Source steps require layer mask sampling".into());
        }
        if merged {
            if mask {
                return Err("Mask retouching samples a layer mask, not merged artwork".into());
            }
            return Ok(RetouchInput::Merged(self.document));
        }
        let target = parameters
            .get("source_layer")
            .map(|v| v.as_str().ok_or("Source layer must be an ID"))
            .transpose()?
            .unwrap_or(self.layer.id());
        let layer = self
            .document
            .metadata()
            .layers()
            .find(|l| l.id() == target)
            .ok_or("Unknown retouch source layer")?;
        if mask {
            if let Some(step) = parameters.get("source_step") {
                return Self::mask_step(
                    self.document,
                    target,
                    step.as_str().ok_or("Source step must be an ID")?,
                )
                .map(RetouchInput::Raster);
            }
            layer.mask().ok_or("Source layer has no mask")?;
            return Ok(RetouchInput::ComposedMask {
                document: self.document,
                layer,
            });
        }
        if layer.kind() != "paint" {
            return Err("Choose a paint source layer or sample merged artwork".into());
        }
        Self::layer(self.document, target).map(RetouchInput::Raster)
    }
    pub(crate) fn layer(document: &'a NativeDocument, layer: &str) -> Result<Self, String> {
        Self::resolve(document, layer, None, false)
    }
    pub(crate) fn mask_step(
        document: &'a NativeDocument,
        layer: &str,
        step: &str,
    ) -> Result<Self, String> {
        Self::resolve(document, layer, Some(step), true)
    }
    /// Read-only projection for remapping every raw mask plane with its layer.
    /// WriteGuard still requires a paint step for the actual draw target.
    pub(crate) fn mask_plane(
        document: &'a NativeDocument,
        layer: &str,
        step: &str,
    ) -> Result<Self, String> {
        Self::resolve(document, layer, Some(step), false)
    }
    fn resolve(
        document: &'a NativeDocument,
        layer_id: &str,
        step: Option<&str>,
        paint_only: bool,
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
                .find(|s| s.id() == step_id && (!paint_only || s.kind() == "paint"))
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
    pub(crate) fn surface(&self) -> &'a Surface {
        self.surface
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
        let tile = read_with_guard(check, |guard| self.surface.prepare_tile(key, guard))?;
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
        let pixels = read_with_guard(check, |guard| self.surface.prepare_region(region, guard))?;
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
    /// Inclusive floating queries in document coordinates, with the exact
    /// bilinear integer support admitted separately. Only physical source
    /// intersections are read; exterior zero follows from captured geometry.
    pub(crate) fn prepare_samples(
        &self,
        queries: [f32; 4],
        frame: Option<&InputFrame>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedSamples<'a>, String> {
        check()?;
        if queries.iter().any(|v| !v.is_finite() || v.abs() > 800_000.)
            || queries[0] > queries[2]
            || queries[1] > queries[3]
        {
            return Err("Invalid native sampling query footprint".into());
        }
        let local = [
            queries[0] - self.origin()[0] as f32,
            queries[1] - self.origin()[1] as f32,
            queries[2] - self.origin()[0] as f32,
            queries[3] - self.origin()[1] as f32,
        ];
        let support = [
            local[0].floor() as i32,
            local[1].floor() as i32,
            local[2].floor() as i32 + 2,
            local[3].floor() as i32 + 2,
        ];
        if support[2] - support[0] > 385 || support[3] - support[1] > 385 {
            return Err("Native bilinear footprint exceeds one complete healing halo".into());
        }
        let mut bounds = [0, 0, self.shape()[0] as i32, self.shape()[1] as i32];
        if let Some(frame) = frame {
            self.validate(frame.binding())?;
            let shift = frame.geometry().shift();
            let shape = frame.geometry().new_shape();
            bounds = [
                bounds[0].max(-shift[0]),
                bounds[1].max(-shift[1]),
                bounds[2].min(shape[0] as i32 - shift[0]),
                bounds[3].min(shape[1] as i32 - shift[1]),
            ];
        }
        let clipped = [
            support[0].max(bounds[0]),
            support[1].max(bounds[1]),
            support[2].min(bounds[2]),
            support[3].min(bounds[3]),
        ];
        let pixels = if clipped[0] < clipped[2] && clipped[1] < clipped[3] {
            Some(self.prepare_linear(clipped, 385, 385 * 385, check)?)
        } else {
            None
        };
        check()?;
        Ok(PreparedSamples {
            binding: self.binding,
            queries,
            bounds,
            pixels,
        })
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
                command: None,
            })
        })
    }
    pub(crate) fn fill_frame(&self, command: Command<'_>) -> Result<Option<InputFrame>, String> {
        if !matches!(
            command.parameters()["op"].as_str(),
            Some("paint.fill" | "shape" | "gradient" | "fill" | "pixels.replace")
        ) {
            return Err(
                "Native area frame requires a fill, primitive or pixel replacement command".into(),
            );
        }
        let metadata = self.document.metadata();
        let requested = command
            .parameters()
            .get("rect")
            .and_then(crate::engine::rect)
            .or(metadata.selection())
            .unwrap_or([0, 0, metadata.width() as i32, metadata.height() as i32]);
        let mut frame = self.command_frame(requested)?;
        if let Some(frame) = &mut frame {
            frame.command = Some(command.token());
        }
        Ok(frame)
    }
    pub(crate) fn stroke_frame(
        &self,
        command: Command<'_>,
        points: &crate::brush::input::StrokePoints,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Option<InputFrame>, String> {
        check()?;
        let parameters = command.parameters();
        if !matches!(
            parameters["op"].as_str(),
            Some("paint" | "smudge" | "clone" | "heal")
        ) || !points.matches_command(command.token())
        {
            return Err("Native stroke frame requires its same admitted command points".into());
        }
        let settings = crate::brush::Settings::from_command(parameters)?;
        let transparent = parameters["op"] == "paint"
            && crate::engine::raster_color(parameters, self.depth())?[3] == 0;
        if settings.opacity == 0. || settings.flow == 0. || transparent {
            return Ok(None);
        }
        let radius = settings.radius + 1.;
        let values = points.points();
        let requested = [
            (values.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min) - radius).floor() as i32,
            (values.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min) - radius).floor() as i32,
            (values
                .iter()
                .map(|p| p[0])
                .fold(f32::NEG_INFINITY, f32::max)
                + radius)
                .ceil() as i32,
            (values
                .iter()
                .map(|p| p[1])
                .fold(f32::NEG_INFINITY, f32::max)
                + radius)
                .ceil() as i32,
        ];
        let mut frame = self.command_frame(requested)?;
        if let Some(frame) = &mut frame {
            frame.command = Some(command.token());
        }
        check()?;
        Ok(frame)
    }
    fn command_frame(&self, requested: [i32; 4]) -> Result<Option<InputFrame>, String> {
        if self.is_mask() {
            let color = Self::layer(self.document, self.layer.id())?;
            color
                .project_frame(requested)?
                .map(|frame| frame.for_plane(self))
                .transpose()
        } else {
            self.project_frame(requested)
        }
    }
    pub(crate) fn selection(&self) -> Result<SelectionInput<'a>, String> {
        SelectionInput::new(self.document)
    }
}
pub(crate) struct InputFrame {
    binding: InputBinding,
    layer: RasterSlot,
    geometry: FrameExpansion,
    command: Option<CommandToken>,
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
            command: self.command,
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

/// Actual semantic baseline->proposal writes, excluding unchanged physical
/// padding. Call before staging the SAME immutable proposal through its token.
/// This box is input to the engine command guard, never a permission itself.
pub(crate) fn changed_bounds(
    input: &RasterInput<'_>,
    frame: Option<&InputFrame>,
    key: (u32, u32),
    before: &PinnedBytes,
    after: &PinnedBytes,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Option<[i32; 4]>, String> {
    check()?;
    let (shape, origin) = if let Some(frame) = frame {
        input.validate(frame.binding())?;
        (frame.geometry().new_shape(), frame.geometry().new_origin())
    } else {
        (input.shape(), input.origin())
    };
    changed_geometry(
        input.pool(),
        input.depth(),
        shape,
        origin,
        key,
        before,
        after,
        check,
    )
}
fn changed_geometry(
    pool: &ResidentBudget,
    depth: u16,
    shape: [u32; 2],
    origin: [i32; 2],
    key: (u32, u32),
    before: &PinnedBytes,
    after: &PinnedBytes,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Option<[i32; 4]>, String> {
    check()?;
    let bpp = if depth == 16 { 8 } else { 4 };
    let length = (TILE * TILE) as usize * bpp;
    if key.0 >= shape[0].div_ceil(TILE)
        || key.1 >= shape[1].div_ceil(TILE)
        || !before.uses_budget(pool)
        || !after.uses_budget(pool)
        || before.len() != length
        || after.len() != length
    {
        return Err("Native write comparison differs from its captured owner or frame".into());
    }
    let left = key.0 * TILE;
    let top = key.1 * TILE;
    let stride = TILE as usize * bpp;
    let mut bounds: Option<[i32; 4]> = None;
    for (y, (old, new)) in before
        .chunks_exact(stride)
        .zip(after.chunks_exact(stride))
        .enumerate()
    {
        check()?;
        if old == new {
            continue;
        }
        if top + y as u32 >= shape[1] {
            return Err("Native edit changed bottom physical padding".into());
        }
        for (x, (old, new)) in old.chunks_exact(bpp).zip(new.chunks_exact(bpp)).enumerate() {
            if old == new {
                continue;
            }
            if left + x as u32 >= shape[0] {
                return Err("Native edit changed right physical padding".into());
            }
            let x = origin[0]
                .checked_add((left + x as u32) as i32)
                .ok_or("Native write coordinate overflow")?;
            let y = origin[1]
                .checked_add((top + y as u32) as i32)
                .ok_or("Native write coordinate overflow")?;
            let right = x.checked_add(1).ok_or("Native write coordinate overflow")?;
            let bottom = y.checked_add(1).ok_or("Native write coordinate overflow")?;
            bounds = Some(bounds.map_or([x, y, right, bottom], |old| {
                [
                    old[0].min(x),
                    old[1].min(y),
                    old[2].max(right),
                    old[3].max(bottom),
                ]
            }));
        }
    }
    check()?;
    Ok(bounds)
}
pub(crate) fn check_proposal(
    guard: Option<&WriteGuard<'_>>,
    input: &RasterInput<'_>,
    frame: Option<&InputFrame>,
    key: (u32, u32),
    before: &PinnedBytes,
    after: &PinnedBytes,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    if let Some(guard) = guard {
        return guard.check_tile(input, frame, key, before, after, check);
    }
    #[cfg(test)]
    {
        Ok(())
    }
    #[cfg(not(test))]
    {
        Err("Native paint output requires its captured command write guard".into())
    }
}
pub(crate) struct PreparedPixels<'a> {
    binding: InputBinding,
    area: [i32; 4],
    bytes: PinnedBytes,
    _document: &'a NativeDocument,
}
pub(crate) struct PreparedSamples<'a> {
    binding: InputBinding,
    queries: [f32; 4],
    bounds: [i32; 4],
    pixels: Option<PreparedPixels<'a>>,
}
impl PreparedSamples<'_> {
    pub(crate) fn sample16(&self, x: f32, y: f32) -> Result<[u16; 4], String> {
        if !x.is_finite()
            || !y.is_finite()
            || x < self.queries[0]
            || y < self.queries[1]
            || x > self.queries[2]
            || y > self.queries[3]
        {
            return Err("Retouch sample is outside its declared source queries".into());
        }
        let x = x - self.binding.origin[0] as f32;
        let y = y - self.binding.origin[1] as f32;
        let bounds = self.bounds;
        if x <= bounds[0] as f32 - 1.
            || y <= bounds[1] as f32 - 1.
            || x >= bounds[2] as f32
            || y >= bounds[3] as f32
        {
            return Ok([0; 4]);
        }
        crate::retouch::native::bilinear(x, y, |x, y| {
            if x < bounds[0] || y < bounds[1] || x >= bounds[2] || y >= bounds[3] {
                return Ok([0; 4]);
            }
            self.pixels
                .as_ref()
                .ok_or("Native sampling source is not prepared")?
                .pixel16(x, y)
        })
    }
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
    pub(crate) fn copy_row(&self, y: i32, output: &mut [u8]) -> Result<(), String> {
        self.pixels.copy_row(y, output).map_err(|e| e.to_string())
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
    /// A conservative pixel footprint. Older masks may contain samples outside
    /// their recorded selection bounds, so use their physical tile locations.
    pub(crate) fn support(&self) -> Option<[i32; 4]> {
        use crate::{engine::reframe_scope::union, raster::TILE};
        let Some((surface, [ox, oy])) = self.coverage else {
            return self.bounds;
        };
        surface
            .physical_tiles()
            .map(|tile| {
                let (x, y) = tile.coordinate();
                [
                    ox + (x * TILE) as i32,
                    oy + (y * TILE) as i32,
                    ox + ((x + 1) * TILE).min(surface.width()) as i32,
                    oy + ((y + 1) * TILE).min(surface.height()) as i32,
                ]
            })
            .reduce(union)
    }
    pub(crate) fn has_coverage(&self) -> bool {
        self.coverage.is_some()
    }
    pub(crate) fn may_cover(&self, area_document: [i32; 4]) -> Result<bool, String> {
        self.coverage.map_or(Ok(true), |(surface, origin)| {
            Ok(surface.has_physical_tiles(translate(area_document, origin, true)?))
        })
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
                    let region = Region::new(local).map_err(|e| e.to_string())?;
                    Some(read_with_guard(check, |guard| surface.prepare_region(region, guard))?)
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
    restrict_document_geometry(
        input.document(),
        input.depth(),
        shape,
        origin,
        key,
        before,
        after,
        selection,
        check,
    )
}
pub(crate) fn restrict_child_tile(
    input: &RasterInput<'_>,
    key: (u32, u32),
    before: &PinnedBytes,
    after: &PinnedBytes,
    selection: &PreparedSelection<'_>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<PinnedBytes, String> {
    if input.is_mask() || input.layer_metadata().kind() != "group" {
        return Err("Native child coverage requires its folder target".into());
    }
    let metadata = input.document().metadata();
    restrict_document_geometry(
        input.document(),
        metadata.depth(),
        [metadata.width(), metadata.height()],
        [0, 0],
        key,
        before,
        after,
        selection,
        check,
    )
}
fn restrict_document_geometry(
    document: &NativeDocument,
    depth: u16,
    shape: [u32; 2],
    origin: [i32; 2],
    key: (u32, u32),
    before: &PinnedBytes,
    after: &PinnedBytes,
    selection: &PreparedSelection<'_>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<PinnedBytes, String> {
    check()?;
    let bpp = if depth == 16 { 8 } else { 4 };
    let length = (TILE * TILE) as usize * bpp;
    if !before.uses_budget(document.budget())
        || !after.uses_budget(document.budget())
        || before.len() != length
        || after.len() != length
        || key.0 >= shape[0].div_ceil(TILE)
        || key.1 >= shape[1].div_ceil(TILE)
    {
        return Err("Soft-selection tile input differs from its admitted native frame".into());
    }
    if selection._document.key() != document.key() {
        return Err("Soft selection belongs to another native generation".into());
    }
    if selection.coverage.is_none() {
        return Ok(after.clone());
    }
    let mut output =
        ScratchBytes::new(document.budget(), length, length).map_err(|e| e.to_string())?;
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
                    if depth == 16 {
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
                if depth == 16 {
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
