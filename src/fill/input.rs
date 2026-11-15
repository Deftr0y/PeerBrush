//! Resolved fill/clear workers. Semantic fill baselines are nominal metadata
//! sources, not permission to change a layer or publish outside captured scopes.
use super::native::{PreparedClear, PreparedFill};
use crate::{
    edit_bounds::{
        input::{restrict_frame_tile, restrict_tile, InputBinding, InputFrame, RasterInput},
        output::{stage_frame_with, BuiltTiles, TileOutput},
    },
    large_document::{PinnedBytes, ScratchBytes},
    raster::TILE,
    workspace::source_runtime::SourceSession,
};

/// Only this module can capture the literal visual source of an actual fill
/// layer. The coordinator retains this proof separately from the draw envelope.
pub(crate) struct FillBaseline {
    binding: InputBinding,
    color: [u8; 4],
}
impl FillBaseline {
    pub(crate) fn capture(input: &RasterInput<'_>) -> Result<Option<Self>, String> {
        if input.is_mask() || input.layer_metadata().kind() != "fill" {
            return Ok(None);
        }
        if input.layer_metadata().source().is_some() {
            return Err("Rasterize the editable source before modifying fill color pixels".into());
        }
        Ok(Some(Self {
            binding: input.binding(),
            color: input.layer_metadata().color(),
        }))
    }
    pub(crate) fn binding(&self) -> InputBinding {
        self.binding
    }
    pub(crate) fn color(&self) -> [u8; 4] {
        self.color
    }
    pub(crate) fn logical_shape(&self) -> [u32; 2] {
        self.binding.shape()
    }
    pub(crate) fn logical_origin(&self) -> [i32; 2] {
        self.binding.origin()
    }
    pub(crate) fn tile(
        &self,
        input: &RasterInput<'_>,
        frame: Option<&InputFrame>,
        key: (u32, u32),
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String> {
        input.validate(self.binding)?;
        check()?;
        let (shape, shift) = if let Some(frame) = frame {
            input.validate(frame.binding())?;
            (frame.geometry().new_shape(), frame.geometry().shift())
        } else {
            (input.shape(), [0, 0])
        };
        if key.0 >= shape[0].div_ceil(TILE) || key.1 >= shape[1].div_ceil(TILE) {
            return Err("Fill baseline tile is outside its proposed frame".into());
        }
        let bpp = if input.depth() == 16 { 8 } else { 4 };
        let mut bytes = ScratchBytes::new(
            input.pool(),
            (TILE * TILE) as usize * bpp,
            (TILE * TILE) as usize * bpp,
        )
        .map_err(|e| e.to_string())?;
        let left = (key.0 * TILE) as i32;
        let top = (key.1 * TILE) as i32;
        let area = [
            left.max(shift[0]),
            top.max(shift[1]),
            (left + TILE as i32)
                .min(shape[0] as i32)
                .min(shift[0] + input.shape()[0] as i32),
            (top + TILE as i32)
                .min(shape[1] as i32)
                .min(shift[1] + input.shape()[1] as i32),
        ];
        for y in area[1]..area[3] {
            check()?;
            for x in area[0]..area[2] {
                let at = ((y - top) as usize * TILE as usize + (x - left) as usize) * bpp;
                for c in 0..4 {
                    if input.depth() == 16 {
                        bytes[at + c * 2..at + c * 2 + 2]
                            .copy_from_slice(&(u16::from(self.color[c]) * 257).to_be_bytes());
                    } else {
                        bytes[at + c] = self.color[c];
                    }
                }
            }
        }
        check()?;
        Ok(bytes.freeze())
    }
}
pub(crate) struct FillStage<'d, 's> {
    pub output: BuiltTiles<'d, 's>,
    pub(crate) baseline: Option<FillBaseline>,
}
impl FillStage<'_, '_> {
    pub(crate) fn baseline(&self) -> Option<&FillBaseline> {
        self.baseline.as_ref()
    }
    pub(crate) fn materializes_fill(&self) -> bool {
        self.baseline.is_some()
    }
}
impl<'d, 's> FillStage<'d, 's> {
    pub(crate) fn into_parts(self) -> (BuiltTiles<'d, 's>, Option<FillBaseline>) {
        (self.output, self.baseline)
    }
}
fn clip(area: [i32; 4], bounds: [i32; 4]) -> [i32; 4] {
    [
        area[0].max(bounds[0]),
        area[1].max(bounds[1]),
        area[2].min(bounds[2]),
        area[3].min(bounds[3]),
    ]
}
fn empty(area: [i32; 4]) -> bool {
    area[0] >= area[2] || area[1] >= area[3]
}
fn touches(area: [i32; 4], key: (u32, u32)) -> bool {
    !empty(area)
        && area[0] < (key.0 * TILE + TILE) as i32
        && area[1] < (key.1 * TILE + TILE) as i32
        && area[2] > (key.0 * TILE) as i32
        && area[3] > (key.1 * TILE) as i32
}
fn finish<'d, 's>(
    output: TileOutput<'d, 's>,
    metadata: bool,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<BuiltTiles<'d, 's>, String> {
    let result = if metadata {
        output.finish_for_metadata(check)
    } else {
        output.finish(check)
    };
    result.map_err(|refused| {
        let mut reason = refused.reason().to_owned();
        if let Err(cleanup) = refused.abort() {
            reason.push_str("; native fill cleanup retained inventory: ");
            reason.push_str(&cleanup);
        }
        reason
    })
}
fn abort_error(output: TileOutput<'_, '_>, mut error: String) -> String {
    if let Err(cleanup) = output.abort() {
        error.push_str("; native fill cleanup retained inventory: ");
        error.push_str(&cleanup);
    }
    error
}
fn validate(
    input: &RasterInput<'_>,
    session: &SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    session.check()?;
    check()?;
    if input.document().metadata().read_only() {
        return Err("Native document is read-only".into());
    }
    if input.layer_metadata().locked() {
        return Err("A selected layer or folder child is locked".into());
    }
    Ok(())
}
/// Semantic-equivalent color prebaseline for a mask command that expands a
/// procedural fill. The coordinator pairs this kind transition and every mask
/// output in one batch; it is not a color draw or a separate publication.
pub(crate) fn stage_visual_frame<'d, 's>(
    input: &RasterInput<'d>,
    frame: &InputFrame,
    guard: &crate::edit_bounds::input::WriteGuard<'_>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<FillStage<'d, 's>, String> {
    guard.validate_fill_frame(input, frame)?;
    validate(input, session, check)?;
    let baseline =
        FillBaseline::capture(input)?.ok_or("Native frame has no procedural fill baseline")?;
    let mut checked = || {
        guard.check_active()?;
        check()?;
        guard.check_active()
    };
    let output = stage_frame_with(
        input,
        frame,
        session,
        &mut checked,
        None,
        |key, _raw, check| baseline.tile(input, Some(frame), key, check),
    )?;
    Ok(FillStage {
        output: BuiltTiles::Draft(output),
        baseline: Some(baseline),
    })
}

/// New folder child pixels. The existing group slot is only a captured scope
/// target; its source plane is never used as the child's baseline or replaced.
pub(crate) fn stage_child_fill<'d, 's>(
    input: &RasterInput<'d>,
    guard: &crate::edit_bounds::input::WriteGuard<'_>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<BuiltTiles<'d, 's>, String> {
    guard.validate(input, None, &["paint.fill", "fill"])?;
    validate(input, session, check)?;
    if input.is_mask() || input.layer_metadata().kind() != "group" {
        return Err("Native foreground-fill insertion requires its captured folder".into());
    }
    let metadata = input.document().metadata();
    let selection = input.selection()?;
    let canvas = [0, 0, metadata.width() as i32, metadata.height() as i32];
    let parameters = guard.parameters();
    let legacy = parameters["op"] == "fill";
    let color = if legacy {
        crate::engine::color(parameters).map(super::expand_byte)
    } else {
        super::foreground_color(parameters, metadata.depth())?
    };
    let requested = if legacy {
        None
    } else {
        parameters.get("rect").and_then(crate::engine::rect)
    };
    let mut area = clip(requested.or(selection.bounds()).unwrap_or(canvas), canvas);
    if let Some(bounds) = selection.bounds() {
        area = clip(area, bounds);
    }
    if empty(area) && !legacy {
        return Ok(BuiltTiles::Unchanged);
    }
    let plan = PreparedFill::new(
        metadata.width(),
        metadata.height(),
        metadata.depth(),
        area,
        [0, 0],
        color,
        selection.polygon(),
        input.pool(),
    )?;
    let plan = if legacy { plan.literal() } else { plan };
    let mut checked = || {
        guard.check_active()?;
        session.check()?;
        check()?;
        guard.check_active()
    };
    let mut seed = crate::edit_bounds::output::FrameSeed::empty_canvas(
        input.document(),
        session,
        &mut checked,
    )?;
    let result = (|| {
        for key in plan.tiles() {
            checked()?;
            let length = (TILE * TILE) as usize * if metadata.depth() == 16 { 8 } else { 4 };
            let before = ScratchBytes::new(input.pool(), length, length)
                .map_err(|e| e.to_string())?
                .freeze();
            let mut after = plan.render_tile(key, &before, &mut checked)?;
            if selection.has_coverage() {
                let region = [
                    (key.0 * TILE) as i32,
                    (key.1 * TILE) as i32,
                    ((key.0 + 1) * TILE).min(metadata.width()) as i32,
                    ((key.1 + 1) * TILE).min(metadata.height()) as i32,
                ];
                let coverage = selection.prepare(region, &mut checked)?;
                after = crate::edit_bounds::input::restrict_child_tile(
                    input,
                    key,
                    &before,
                    &after,
                    &coverage,
                    &mut checked,
                )?;
            }
            guard.check_child_tile(input, key, &before, &after, &mut checked)?;
            seed.replace_tile(key, after, &mut checked)?;
        }
        checked()
    })();
    if let Err(mut reason) = result {
        if let Err(cleanup) = seed.abort() {
            reason.push_str("; native child-fill cleanup retained inventory: ");
            reason.push_str(&cleanup);
        }
        return Err(reason);
    }
    seed.finish(&mut checked).map(BuiltTiles::Draft)
}

/// Does not expand bounds. Coverage is consumed once by this clear kernel.
#[cfg(test)]
pub(crate) fn stage_clear<'d, 's>(
    input: &RasterInput<'d>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<FillStage<'d, 's>, String> {
    stage_clear_impl(input, None, session, check)
}
pub(crate) fn stage_clear_guarded<'d, 's>(
    input: &RasterInput<'d>,
    guard: &crate::edit_bounds::input::WriteGuard<'_>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<FillStage<'d, 's>, String> {
    guard.validate(input, None, &["paint.clear_selection"])?;
    let mut checked = || {
        guard.check_active()?;
        check()?;
        guard.check_active()
    };
    stage_clear_impl(input, Some(guard), session, &mut checked)
}
fn stage_clear_impl<'d, 's>(
    input: &RasterInput<'d>,
    guard: Option<&crate::edit_bounds::input::WriteGuard<'_>>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<FillStage<'d, 's>, String> {
    validate(input, session, check)?;
    let layer = input.layer_metadata();
    if !input.is_mask() && (!matches!(layer.kind(), "paint" | "fill") || layer.source().is_some()) {
        return Err("Rasterize editable text/vector or adjustment layers before clearing selected color pixels".into());
    }
    let selection = input.selection()?;
    let bounds = selection.bounds().ok_or("Select pixels before clearing")?;
    if empty(bounds) {
        return Ok(FillStage {
            output: BuiltTiles::Unchanged,
            baseline: None,
        });
    }
    let metadata = input.document().metadata();
    let area = clip(
        bounds,
        [0, 0, metadata.width() as i32, metadata.height() as i32],
    );
    let local = clip(
        input.local_rect(area)?,
        [0, 0, input.shape()[0] as i32, input.shape()[1] as i32],
    );
    let clear = PreparedClear::new(
        input.shape()[0],
        input.shape()[1],
        input.depth(),
        local,
        input.origin(),
        input.pool(),
    )?;
    let baseline = FillBaseline::capture(input)?;
    let mut output = TileOutput::new(input, session)?;
    let result = (|| {
        let shape = input.shape();
        for ty in 0..shape[1].div_ceil(TILE) {
            for tx in 0..shape[0].div_ceil(TILE) {
                let key = (tx, ty);
                let selected = if touches(local, key) {
                    let overlap = clip(input.tile_document_rect(key)?, area);
                    selection.may_cover(overlap)?.then_some(overlap)
                } else {
                    None
                };
                if baseline.is_none()
                    && (selected.is_none() || input.surface().physical_tile(key).is_none())
                {
                    continue;
                }
                check()?;
                let before = input.prepare_tile(key, check)?;
                let semantic_before = if let Some(baseline) = &baseline {
                    baseline.tile(input, None, key, check)?
                } else {
                    before.bytes().clone()
                };
                let mut after = semantic_before.clone();
                if let Some(overlap) = selected {
                    let coverage = selection.prepare(overlap, check)?;
                    after = clear.render_tile(key, &after, check, |x, y| coverage.value(x, y))?;
                }
                crate::edit_bounds::input::check_proposal(
                    guard,
                    input,
                    None,
                    key,
                    &semantic_before,
                    &after,
                    check,
                )?;
                output.replace_tile(&before, after, check)?;
            }
        }
        check()
    })();
    if let Err(error) = result {
        return Err(abort_error(output, error));
    }
    Ok(FillStage {
        output: finish(output, baseline.is_some(), check)?,
        baseline,
    })
}

/// Resolved raster foreground fill. The common coordinator owns folder child
/// insertion and the atomic kind/plane transition for a nominal fill baseline.
#[cfg(test)]
pub(crate) fn stage_fill<'d, 's>(
    input: &RasterInput<'d>,
    color: [u8; 4],
    requested: Option<[i32; 4]>,
    frame: Option<&InputFrame>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<FillStage<'d, 's>, String> {
    let color = input.color(color).map(super::expand_byte);
    stage_fill_impl(input, color, requested, frame, None, session, check)
}
pub(crate) fn stage_fill_guarded<'d, 's>(
    input: &RasterInput<'d>,
    frame: Option<&InputFrame>,
    guard: &crate::edit_bounds::input::WriteGuard<'_>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<FillStage<'d, 's>, String> {
    guard.validate(input, frame, &["paint.fill"])?;
    let parameters = guard.parameters();
    let color = super::foreground_color(parameters, input.depth())?;
    let requested = parameters.get("rect").and_then(crate::engine::rect);
    let mut checked = || {
        guard.check_active()?;
        check()?;
        guard.check_active()
    };
    stage_fill_impl(
        input,
        color,
        requested,
        frame,
        Some(guard),
        session,
        &mut checked,
    )
}
fn stage_fill_impl<'d, 's>(
    input: &RasterInput<'d>,
    color: [u16; 4],
    requested: Option<[i32; 4]>,
    frame: Option<&InputFrame>,
    guard: Option<&crate::edit_bounds::input::WriteGuard<'_>>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<FillStage<'d, 's>, String> {
    validate(input, session, check)?;
    if !input.is_mask()
        && (!matches!(input.layer_metadata().kind(), "paint" | "fill")
            || input.layer_metadata().source().is_some())
    {
        return Err(
            "Resolve folder insertion or source rasterization before raster foreground fill".into(),
        );
    }
    let metadata = input.document().metadata();
    let selection = input.selection()?;
    let canvas = [0, 0, metadata.width() as i32, metadata.height() as i32];
    let mut area = clip(requested.or(selection.bounds()).unwrap_or(canvas), canvas);
    if let Some(bounds) = selection.bounds() {
        area = clip(area, bounds);
    }
    if empty(area) {
        return Ok(FillStage {
            output: BuiltTiles::Unchanged,
            baseline: None,
        });
    }
    let baseline = FillBaseline::capture(input)?;
    if color[3] == 0 && frame.is_none() && baseline.is_none() {
        return Ok(FillStage {
            output: BuiltTiles::Unchanged,
            baseline: None,
        });
    }
    let (shape, origin) = if let Some(frame) = frame {
        input.validate(frame.binding())?;
        (frame.geometry().new_shape(), frame.geometry().new_origin())
    } else {
        (input.shape(), input.origin())
    };
    let requested_local = [
        area[0] - origin[0],
        area[1] - origin[1],
        area[2] - origin[0],
        area[3] - origin[1],
    ];
    let local = clip(requested_local, [0, 0, shape[0] as i32, shape[1] as i32]);
    if !input.is_mask() && local != requested_local {
        return Err(
            "Native foreground fill requires the complete captured destination frame".into(),
        );
    }
    let fill = PreparedFill::new(
        shape[0],
        shape[1],
        input.depth(),
        local,
        origin,
        color,
        selection.polygon(),
        input.pool(),
    )?;
    if let Some(frame) = frame {
        let draft = stage_frame_with(input, frame, session, check, None, |key, raw, check| {
            let before = if let Some(baseline) = &baseline {
                baseline.tile(input, Some(frame), key, check)?
            } else {
                raw.clone()
            };
            let after = if color[3] > 0 && touches(local, key) {
                fill.render_tile(key, &before, check)?
            } else {
                before.clone()
            };
            let after = if selection.has_coverage() {
                let coverage = selection.prepare(frame.tile_document_rect(key)?, check)?;
                restrict_frame_tile(input, frame, key, &before, &after, &coverage, check)
            } else {
                Ok(after)
            }?;
            crate::edit_bounds::input::check_proposal(
                guard,
                input,
                Some(frame),
                key,
                &before,
                &after,
                check,
            )?;
            Ok(after)
        })?;
        return Ok(FillStage {
            output: BuiltTiles::Draft(draft),
            baseline,
        });
    }
    let mut output = TileOutput::new(input, session)?;
    let result = (|| {
        for ty in 0..shape[1].div_ceil(TILE) {
            for tx in 0..shape[0].div_ceil(TILE) {
                let key = (tx, ty);
                if baseline.is_none() && !touches(local, key) {
                    continue;
                }
                let raw = input.prepare_tile(key, check)?;
                let before = if let Some(baseline) = &baseline {
                    baseline.tile(input, None, key, check)?
                } else {
                    raw.bytes().clone()
                };
                let mut after = if color[3] > 0 && touches(local, key) {
                    fill.render_tile(key, &before, check)?
                } else {
                    before.clone()
                };
                if selection.has_coverage() {
                    let coverage = selection.prepare(input.tile_document_rect(key)?, check)?;
                    after = restrict_tile(input, key, &before, &after, &coverage, check)?;
                }
                crate::edit_bounds::input::check_proposal(
                    guard, input, None, key, &before, &after, check,
                )?;
                output.replace_tile(&raw, after, check)?;
            }
        }
        check()
    })();
    if let Err(error) = result {
        return Err(abort_error(output, error));
    }
    Ok(FillStage {
        output: finish(output, baseline.is_some(), check)?,
        baseline,
    })
}
