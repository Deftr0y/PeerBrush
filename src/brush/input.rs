//! Charged local geometry derived from one immutable native input generation.
use super::{native::PreparedBrush, Settings};
use crate::{
    edit_bounds::input::{InputBinding, InputFrame, RasterInput},
    large_document::{Reservation, ResidentBudget},
    retouch::native::PreparedRetouch,
    smudge::native::PreparedSmudge,
};

/// Charged temporary document-space geometry from the shared command parser.
/// Its capacities stay admitted until shifted LocalStroke data owns its charge.
pub(crate) struct StrokePoints {
    points: Vec<[f32; 2]>,
    pressures: Option<Vec<f32>>,
    _charge: Reservation,
}
impl StrokePoints {
    pub(crate) fn from_parameters(
        parameters: &serde_json::Value,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        let count = parameters["points"]
            .as_array()
            .filter(|p| !p.is_empty() && p.len() <= 10000)
            .ok_or("Stroke needs 1-10000 points")?
            .len();
        let bytes = count
            .checked_mul(std::mem::size_of::<[f32; 2]>() + 2 * std::mem::size_of::<f32>())
            .and_then(|n| n.checked_add(256))
            .ok_or("Native parsed stroke admission overflow")?;
        let charge = pool
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let (points, pressures) = super::points_from_command(parameters)?;
        let actual = points
            .capacity()
            .checked_mul(std::mem::size_of::<[f32; 2]>())
            .and_then(|n| {
                pressures.as_ref().map_or(Some(n), |p| {
                    p.capacity()
                        .checked_mul(std::mem::size_of::<f32>())
                        .and_then(|b| n.checked_add(b))
                })
            })
            .ok_or("Native parsed stroke capacity overflow")?;
        if actual > bytes {
            return Err("Native parsed stroke allocator exceeded admission".into());
        }
        check()?;
        Ok(Self {
            points,
            pressures,
            _charge: charge,
        })
    }
    pub(crate) fn points(&self) -> &[[f32; 2]] {
        &self.points
    }
    pub(crate) fn pressures(&self) -> Option<&[f32]> {
        self.pressures.as_deref()
    }
    pub(crate) fn local(
        &self,
        input: &RasterInput<'_>,
        frame: Option<&InputFrame>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<LocalStroke, String> {
        if let Some(frame) = frame {
            LocalStroke::for_frame(input, frame, self.points(), self.pressures(), check)
        } else {
            LocalStroke::new(input, self.points(), self.pressures(), check)
        }
    }
}

pub(crate) struct LocalStroke {
    binding: InputBinding,
    shape: [u32; 2],
    origin: [i32; 2],
    points: Vec<[f32; 2]>,
    pressures: Option<Vec<f32>>,
    polygon: Option<Vec<[f32; 2]>>,
    clip: Option<[i32; 4]>,
    // Keep admission until the actual Vec capacities above have been released.
    _charge: Reservation,
}

/// Worker-only preparation. Output remains unpublished; the common dispatcher
/// must retain the capture/session and atomically adopt every related plane.
pub(crate) fn stage<'d, 's>(
    input: &RasterInput<'d>,
    stroke: &LocalStroke,
    settings: Settings,
    color: [u8; 4],
    erase: bool,
    frame: Option<&InputFrame>,
    session: &'s crate::workspace::source_runtime::SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<crate::edit_bounds::output::BuiltTiles<'d, 's>, String> {
    use crate::edit_bounds::{
        input::{restrict_frame_tile, restrict_tile},
        output::{stage_frame_with, BuiltTiles, TileOutput},
    };
    session.check()?;
    check()?;
    if input.document().metadata().read_only() {
        return Err("Native document is read-only".into());
    }
    stroke.validate_frame(input, frame)?;
    if !input.is_mask()
        && matches!(
            input.layer_metadata().kind(),
            "group" | "fill" | "adjustment"
        )
    {
        return Err("Paint on a paint layer or its mask".into());
    }
    if settings.opacity == 0. || settings.flow == 0. || color[3] == 0 {
        return Ok(BuiltTiles::Unchanged);
    }
    let plan = stroke.brush(input, settings, color, erase, check)?;
    let selection = input.selection()?;
    if let Some(frame) = frame {
        let draft = stage_frame_with(input, frame, session, check, |key, before, check| {
            if plan
                .tiles()
                .binary_search_by_key(&(key.1, key.0), |&(x, y)| (y, x))
                .is_err()
            {
                return Ok(before.clone());
            }
            let after = plan.render_tile(key, before, check)?;
            if selection.has_coverage() {
                let coverage = selection.prepare(frame.tile_document_rect(key)?, check)?;
                restrict_frame_tile(input, frame, key, before, &after, &coverage, check)
            } else {
                Ok(after)
            }
        })?;
        return Ok(BuiltTiles::Draft(draft));
    }
    let mut output = TileOutput::new(input, session)?;
    let result = (|| {
        for &key in plan.tiles() {
            check()?;
            let before = input.prepare_tile(key, check)?;
            let mut after = plan.render_tile(key, before.bytes(), check)?;
            if selection.has_coverage() {
                let coverage = selection.prepare(input.tile_document_rect(key)?, check)?;
                after = restrict_tile(input, key, before.bytes(), &after, &coverage, check)?;
            }
            output.replace_tile(&before, after, check)?;
        }
        check()
    })();
    if let Err(mut error) = result {
        if let Err(cleanup) = output.abort() {
            error.push_str("; native brush cleanup retained inventory: ");
            error.push_str(&cleanup);
        }
        return Err(error);
    }
    output.finish(check).map_err(|refused| {
        let mut reason = refused.reason().to_owned();
        if let Err(cleanup) = refused.abort() {
            reason.push_str("; native brush cleanup retained inventory: ");
            reason.push_str(&cleanup);
        }
        reason
    })
}
impl LocalStroke {
    pub(crate) fn new(
        input: &RasterInput<'_>,
        points_document: &[[f32; 2]],
        pressures: Option<&[f32]>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::in_frame(
            input,
            input.shape(),
            input.origin(),
            points_document,
            pressures,
            check,
        )
    }
    pub(crate) fn for_frame(
        input: &RasterInput<'_>,
        frame: &InputFrame,
        points_document: &[[f32; 2]],
        pressures: Option<&[f32]>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        input.validate(frame.binding())?;
        Self::in_frame(
            input,
            frame.geometry().new_shape(),
            frame.geometry().new_origin(),
            points_document,
            pressures,
            check,
        )
    }
    fn in_frame(
        input: &RasterInput<'_>,
        shape: [u32; 2],
        origin: [i32; 2],
        points_document: &[[f32; 2]],
        pressures: Option<&[f32]>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if points_document.is_empty()
            || points_document.len() > 10000
            || points_document
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 100000.)
            || pressures.is_some_and(|p| {
                p.len() != points_document.len()
                    || p.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
            })
        {
            return Err("Invalid native document-space stroke".into());
        }
        let selection = input.selection()?;
        let polygon = selection.polygon();
        let point_bytes = points_document
            .len()
            .checked_add(polygon.map_or(0, |p| p.len()))
            .and_then(|n| n.checked_mul(std::mem::size_of::<[f32; 2]>()))
            .ok_or("Native local stroke footprint overflow")?;
        let admitted = point_bytes
            .checked_add(pressures.map_or(0, std::mem::size_of_val))
            .and_then(|n| n.checked_mul(2))
            .and_then(|n| n.checked_add(128))
            .ok_or("Native local stroke footprint overflow")?;
        let charge = input
            .pool()
            .reserve_bytes(admitted, admitted)
            .map_err(|e| e.to_string())?;
        let shift = origin;
        let copy = |source: &[[f32; 2]]| -> Result<Vec<[f32; 2]>, String> {
            let mut values = Vec::new();
            values
                .try_reserve_exact(source.len())
                .map_err(|_| "Native local stroke allocation failed")?;
            for p in source {
                values.push([p[0] - shift[0] as f32, p[1] - shift[1] as f32]);
            }
            Ok(values)
        };
        let points = copy(points_document)?;
        let polygon = polygon.map(copy).transpose()?;
        let pressures = pressures
            .map(|source| {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(source.len())
                    .map_err(|_| "Native pressure allocation failed")?;
                values.extend_from_slice(source);
                Ok::<_, String>(values)
            })
            .transpose()?;
        let actual = points
            .capacity()
            .checked_add(polygon.as_ref().map_or(0, |p| p.capacity()))
            .and_then(|n| n.checked_mul(std::mem::size_of::<[f32; 2]>()))
            .and_then(|n| {
                pressures.as_ref().map_or(Some(n), |p| {
                    p.capacity()
                        .checked_mul(std::mem::size_of::<f32>())
                        .and_then(|b| n.checked_add(b))
                })
            })
            .ok_or("Native stroke capacity overflow")?;
        if actual > admitted {
            return Err("Native stroke allocator exceeded its admission".into());
        }
        check()?;
        Ok(Self {
            binding: input.binding(),
            shape,
            origin,
            points,
            pressures,
            polygon,
            clip: selection
                .bounds()
                .map(|r| {
                    let mut local = [0; 4];
                    for i in 0..4 {
                        local[i] = r[i]
                            .checked_sub(origin[i % 2])
                            .ok_or("Native stroke clip overflow")?;
                    }
                    Ok::<_, String>(local)
                })
                .transpose()?,
            _charge: charge,
        })
    }
    pub(crate) fn binding(&self) -> InputBinding {
        self.binding
    }
    pub(crate) fn validate_frame(
        &self,
        input: &RasterInput<'_>,
        frame: Option<&InputFrame>,
    ) -> Result<(), String> {
        input.validate(self.binding)?;
        let (shape, origin) = if let Some(frame) = frame {
            input.validate(frame.binding())?;
            (frame.geometry().new_shape(), frame.geometry().new_origin())
        } else {
            (input.shape(), input.origin())
        };
        if self.shape != shape || self.origin != origin {
            return Err("Native stroke belongs to another proposed frame".into());
        }
        Ok(())
    }
    pub(crate) fn brush<'a>(
        &'a self,
        input: &RasterInput<'_>,
        mut settings: Settings,
        color: [u8; 4],
        erase: bool,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedBrush<'a>, String> {
        input.validate(self.binding)?;
        settings.texture_origin = self.origin;
        PreparedBrush::new(
            self.shape[0],
            self.shape[1],
            input.depth(),
            &self.points,
            self.pressures.as_deref(),
            settings,
            input.color(color),
            erase,
            self.clip,
            self.polygon.as_deref(),
            input.pool(),
            check,
        )
    }
    pub(crate) fn retouch<'a>(
        &'a self,
        input: &RasterInput<'_>,
        settings: Settings,
        offset_document: [f32; 2],
        heal_radius: Option<u32>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedRetouch<'a>, String> {
        input.validate(self.binding)?;
        PreparedRetouch::new(
            self.shape[0],
            self.shape[1],
            input.depth(),
            &self.points,
            self.pressures.as_deref(),
            settings,
            self.origin,
            offset_document,
            heal_radius,
            self.clip,
            self.polygon.as_deref(),
            input.pool(),
            check,
        )
    }
    pub(crate) fn smudge<'a>(
        &'a self,
        input: &RasterInput<'_>,
        mut settings: Settings,
        color: [u8; 4],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedSmudge<'a>, String> {
        input.validate(self.binding)?;
        settings.texture_origin = self.origin;
        PreparedSmudge::new(
            self.shape[0],
            self.shape[1],
            input.depth(),
            &self.points,
            self.pressures.as_deref(),
            settings,
            input.color(color),
            self.clip,
            self.polygon.as_deref(),
            input.pool(),
            check,
        )
    }
}
