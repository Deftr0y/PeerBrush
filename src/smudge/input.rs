//! Session-owned ordered smudge. No working dab is a document/history commit.
use super::native::{PreparedStrip, PreparedTile, WorkingSource};
use crate::{
    brush::{input::LocalStroke, Settings},
    edit_bounds::{
        input::{restrict_frame_tile, restrict_tile, InputFrame, RasterInput},
        output::{stage_frame_seed, BuiltTiles, FrameSeed, TileOutput},
    },
    large_document::{PinnedBytes, ResidentBudget},
    raster::TILE,
    workspace::source_runtime::SourceSession,
};

struct Working<'a, 'd, 's> {
    input: &'a RasterInput<'d>,
    output: WorkingOutput<'d, 's>,
    last_step: Option<usize>,
}
enum WorkingOutput<'d, 's> {
    Original(TileOutput<'d, 's>),
    Framed(FrameSeed<'d, 's>),
}
impl<'d, 's> WorkingOutput<'d, 's> {
    fn prepare_tile(
        &self,
        key: (u32, u32),
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String> {
        match self {
            Self::Original(output) => output.prepare_working_tile(key, check),
            Self::Framed(output) => output.prepare_tile(key, check),
        }
    }
    fn prepare_strip(
        &self,
        area: [i32; 4],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String> {
        match self {
            Self::Original(output) => output.prepare_working_strip(area, check),
            Self::Framed(output) => output.prepare_strip(area, check),
        }
    }
    fn replace_tile(
        &mut self,
        input: &RasterInput<'_>,
        key: (u32, u32),
        bytes: PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        match self {
            Self::Original(output) => {
                output.replace_tile(&input.prepare_tile(key, check)?, bytes, check)
            }
            Self::Framed(output) => output.replace_tile(key, bytes, check),
        }
    }
    fn abort(self) -> Result<(), String> {
        match self {
            Self::Original(output) => output.abort(),
            Self::Framed(output) => output.abort(),
        }
    }
    fn finish(
        self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<BuiltTiles<'d, 's>, String> {
        match self {
            Self::Framed(output) => output.finish(check).map(BuiltTiles::Draft),
            Self::Original(output) => output.finish(check).map_err(|refused| {
                let mut reason = refused.reason().to_owned();
                if let Err(cleanup) = refused.abort() {
                    reason.push_str("; native smudge cleanup retained inventory: ");
                    reason.push_str(&cleanup);
                }
                reason
            }),
        }
    }
}
impl Working<'_, '_, '_> {
    fn step(&mut self, step: usize) -> Result<(), String> {
        if self.last_step.is_some_and(|old| step < old) {
            return Err("Native smudge attempted to read an obsolete working step".into());
        }
        self.last_step = Some(step);
        Ok(())
    }
}
impl WorkingSource for Working<'_, '_, '_> {
    fn frame(&self) -> [u32; 2] {
        match &self.output {
            WorkingOutput::Original(_) => self.input.shape(),
            WorkingOutput::Framed(output) => output.shape(),
        }
    }
    fn depth(&self) -> u16 {
        self.input.depth()
    }
    fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.input.pool().same_pool(pool)
    }
    fn prepare_strip(
        &mut self,
        step: usize,
        area: [i32; 4],
        depth: u16,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedStrip, String> {
        self.step(step)?;
        if depth != self.depth() {
            return Err("Native smudge strip depth differs".into());
        }
        let bytes = self.output.prepare_strip(area, check)?;
        PreparedStrip::new(step, area, depth, bytes, self.input.pool())
    }
    fn prepare_tile(
        &mut self,
        step: usize,
        key: (u32, u32),
        depth: u16,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedTile, String> {
        self.step(step)?;
        if depth != self.depth() {
            return Err("Native smudge tile depth differs".into());
        }
        let bytes = self.output.prepare_tile(key, check)?;
        PreparedTile::new(step, key, depth, bytes, self.input.pool())
    }
    fn stage_tile(
        &mut self,
        step: usize,
        key: (u32, u32),
        bytes: PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.step(step)?;
        self.output.replace_tile(self.input, key, bytes, check)
    }
}

/// Original-frame entry point; reframed strokes use stage_in_frame explicitly.
pub(crate) fn stage<'d, 's>(
    input: &RasterInput<'d>,
    stroke: &LocalStroke,
    settings: Settings,
    color: [u8; 4],
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<BuiltTiles<'d, 's>, String> {
    stage_in_frame(input, stroke, settings, color, None, session, check)
}
pub(crate) fn stage_in_frame<'d, 's>(
    input: &RasterInput<'d>,
    stroke: &LocalStroke,
    settings: Settings,
    color: [u8; 4],
    frame: Option<&InputFrame>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<BuiltTiles<'d, 's>, String> {
    session.check()?;
    check()?;
    stroke.validate_frame(input, frame)?;
    if input.document().metadata().read_only() || input.layer_metadata().locked() {
        return Err("Native smudge requires an editable unlocked plane".into());
    }
    if !input.is_mask() && input.layer_metadata().kind() != "paint" {
        return Err("Smudge a paint layer or paint mask step".into());
    }
    if settings.opacity == 0. || settings.flow == 0. || settings.wetness == 0. {
        return Ok(BuiltTiles::Unchanged);
    }
    let mut guarded = || {
        session.check()?;
        check()
    };
    let plan = stroke.smudge(input, settings, input.color(color), &mut guarded)?;
    let mut working = Working {
        input,
        output: if let Some(frame) = frame {
            WorkingOutput::Framed(stage_frame_seed(
                input,
                frame,
                session,
                &mut guarded,
                |_, bytes, _| Ok(bytes.clone()),
            )?)
        } else {
            WorkingOutput::Original(TileOutput::new(input, session)?)
        },
        last_step: None,
    };
    let result = (|| {
        plan.run_in_place(&mut working, &mut guarded)?;
        let selection = input.selection()?;
        if selection.has_coverage() {
            let shape = working.frame();
            for ty in 0..shape[1].div_ceil(TILE) {
                for tx in 0..shape[0].div_ceil(TILE) {
                    guarded()?;
                    let key = (tx, ty);
                    if matches!(&working.output,WorkingOutput::Original(output) if !output.has_replacement(key))
                    {
                        continue;
                    }
                    let before = if let Some(frame) = frame {
                        let prepared = frame
                            .geometry()
                            .source_reads(key)?
                            .map(|area| input.prepare_region(area, &mut guarded))
                            .transpose()?;
                        frame.geometry().render_tile(key, &mut guarded, |x, y| {
                            prepared
                                .as_ref()
                                .ok_or("Native smudge frame baseline source was not prepared")?
                                .pixel16(x, y)
                        })?
                    } else {
                        input.prepare_tile(key, &mut guarded)?.bytes().clone()
                    };
                    let proposed = working.output.prepare_tile(key, &mut guarded)?;
                    let area = if let Some(frame) = frame {
                        frame.tile_document_rect(key)?
                    } else {
                        input.tile_document_rect(key)?
                    };
                    let coverage = selection.prepare(area, &mut guarded)?;
                    let after = if let Some(frame) = frame {
                        restrict_frame_tile(
                            input,
                            frame,
                            key,
                            &before,
                            &proposed,
                            &coverage,
                            &mut guarded,
                        )?
                    } else {
                        restrict_tile(input, key, &before, &proposed, &coverage, &mut guarded)?
                    };
                    working
                        .output
                        .replace_tile(input, key, after, &mut guarded)?;
                }
            }
        }
        guarded()
    })();
    if let Err(mut reason) = result {
        if let Err(cleanup) = working.output.abort() {
            reason.push_str("; native smudge cleanup retained inventory: ");
            reason.push_str(&cleanup);
        }
        return Err(reason);
    }
    working.output.finish(&mut guarded)
}
