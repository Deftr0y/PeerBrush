//! Session-owned ordered smudge. No working dab is a document/history commit.
use super::native::{PreparedStrip, PreparedTile, WorkingSource};
use crate::{
    brush::{input::LocalStroke, Settings},
    edit_bounds::{
        input::{restrict_tile, RasterInput},
        output::{BuiltTiles, TileOutput},
    },
    large_document::{PinnedBytes, ResidentBudget},
    raster::TILE,
    workspace::source_runtime::SourceSession,
};

struct Working<'a, 'd, 's> {
    input: &'a RasterInput<'d>,
    output: TileOutput<'d, 's>,
    last_step: Option<usize>,
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
        self.input.shape()
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
        let bytes = self.output.prepare_working_strip(area, check)?;
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
        let bytes = self.output.prepare_working_tile(key, check)?;
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
        let original = self.input.prepare_tile(key, check)?;
        self.output.replace_tile(&original, bytes, check)
    }
}

/// The stroke must have been prepared for this original frame. A reframed
/// stroke is explicitly refused until its complete working frame is staged.
pub(crate) fn stage<'d, 's>(
    input: &RasterInput<'d>,
    stroke: &LocalStroke,
    settings: Settings,
    color: [u8; 4],
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<BuiltTiles<'d, 's>, String> {
    session.check()?;
    check()?;
    stroke.validate_frame(input, None)?;
    if input.document().metadata().read_only() || input.layer_metadata().locked() {
        return Err("Native smudge requires an editable unlocked plane".into());
    }
    if !input.is_mask() && input.layer_metadata().kind() != "paint" {
        return Err("Smudge a paint layer or paint mask step".into());
    }
    let mut guarded = || {
        session.check()?;
        check()
    };
    let plan = stroke.smudge(input, settings, input.color(color), &mut guarded)?;
    let mut working = Working {
        input,
        output: TileOutput::new(input, session)?,
        last_step: None,
    };
    let result = (|| {
        plan.run_in_place(&mut working, &mut guarded)?;
        let selection = input.selection()?;
        if selection.has_coverage() {
            let shape = input.shape();
            for ty in 0..shape[1].div_ceil(TILE) {
                for tx in 0..shape[0].div_ceil(TILE) {
                    guarded()?;
                    let key = (tx, ty);
                    if !working.output.has_replacement(key) {
                        continue;
                    }
                    let before = input.prepare_tile(key, &mut guarded)?;
                    let proposed = working.output.prepare_working_tile(key, &mut guarded)?;
                    let coverage =
                        selection.prepare(input.tile_document_rect(key)?, &mut guarded)?;
                    let after = restrict_tile(
                        input,
                        key,
                        before.bytes(),
                        &proposed,
                        &coverage,
                        &mut guarded,
                    )?;
                    working.output.replace_tile(&before, after, &mut guarded)?;
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
    working.output.finish(&mut guarded).map_err(|refused| {
        let mut reason = refused.reason().to_owned();
        if let Err(cleanup) = refused.abort() {
            reason.push_str("; native smudge cleanup retained inventory: ");
            reason.push_str(&cleanup);
        }
        reason
    })
}
