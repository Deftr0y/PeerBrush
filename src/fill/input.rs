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
    fn capture(input: &RasterInput<'_>) -> Result<Option<Self>, String> {
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
    fn tile(
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
    baseline: Option<FillBaseline>,
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

/// Does not expand bounds. Coverage is consumed once by this clear kernel.
pub(crate) fn stage_clear<'d, 's>(
    input: &RasterInput<'d>,
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
                if baseline.is_none() && !touches(local, key) {
                    continue;
                }
                check()?;
                let before = input.prepare_tile(key, check)?;
                let mut after = if let Some(baseline) = &baseline {
                    baseline.tile(input, None, key, check)?
                } else {
                    before.bytes().clone()
                };
                if touches(local, key) {
                    let coverage = selection.prepare(input.tile_document_rect(key)?, check)?;
                    after = clear.render_tile(key, &after, check, |x, y| coverage.value(x, y))?;
                }
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
pub(crate) fn stage_fill<'d, 's>(
    input: &RasterInput<'d>,
    color: [u8; 4],
    requested: Option<[i32; 4]>,
    frame: Option<&InputFrame>,
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
    let local = [
        area[0] - origin[0],
        area[1] - origin[1],
        area[2] - origin[0],
        area[3] - origin[1],
    ];
    if clip(local, [0, 0, shape[0] as i32, shape[1] as i32]) != local {
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
        input.color(color),
        selection.polygon(),
        input.pool(),
    )?;
    if let Some(frame) = frame {
        let draft = stage_frame_with(input, frame, session, check, |key, raw, check| {
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
            if selection.has_coverage() {
                let coverage = selection.prepare(frame.tile_document_rect(key)?, check)?;
                restrict_frame_tile(input, frame, key, &before, &after, &coverage, check)
            } else {
                Ok(after)
            }
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
