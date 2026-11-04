//! Full-origin SVG bands feed the existing guarded provisional tile writer.
//! No complete framebuffer, cropped CTM, random write or publication authority.
use super::{
    prepared::PreparedSource,
    stage::{self, TileProducer},
};
use crate::{
    engine::native::{Cancellation, NativeDocument, StagedOutput},
    large_document::ScratchBytes,
    raster::TILE,
    workspace::source_runtime::SourceSession,
};

// Explicit operation admission, independent of output band/tile counts. Failure
// preserves current pixels/history; this is not a global document-cap change.
const RETAINED_BYTES: usize = 16 * 1024 * 1024;
const MAX_INVOCATIONS: usize = 16384;
const MAX_WORK: u64 = 1_000_000_000;

struct Frontier<'a> {
    plan: resvg::rows::RowPlan<'a>,
    frame: [u32; 2],
    stride: usize,
    bytes: ScratchBytes,
    next: u32,
}
impl Frontier<'_> {
    fn load(
        &mut self,
        ty: u32,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let top = ty
            .checked_mul(TILE)
            .ok_or("SVG frontier coordinate overflow")?;
        let end = (top + TILE).min(self.frame[1]);
        if self.plan.next_y() != top {
            return Err("SVG frontier band sequence differs".into());
        }
        self.bytes.fill(0);
        while self.plan.next_y() < end {
            check()?;
            let start = self.plan.next_y();
            let count = self.plan.maximum_rows().min(end - start);
            let image = self.plan.next(count)?;
            check()?;
            if image.width() != self.frame[0]
                || image.y() != start
                || image.row_count() != count
                || image.pixels().len() != self.frame[0] as usize * count as usize
            {
                return Err("SVG renderer returned an incomplete frontier band".into());
            }
            for (row, pixels) in image
                .pixels()
                .chunks_exact(self.frame[0] as usize)
                .enumerate()
            {
                check()?;
                let begin = ((start - top) as usize + row) * self.frame[0] as usize * self.stride;
                for (target, pixel) in self.bytes
                    [begin..begin + self.frame[0] as usize * self.stride]
                    .chunks_exact_mut(self.stride)
                    .zip(pixels)
                {
                    // Exact original svg::render demultiply, then native16
                    // byte*257 uplift. Do not interpolate premultiplied bytes.
                    let p = pixel.demultiply();
                    let rgba = [p.red(), p.green(), p.blue(), p.alpha()];
                    if self.stride == 8 {
                        for (word, value) in target.chunks_exact_mut(2).zip(rgba) {
                            word.copy_from_slice(&(u16::from(value) * 257).to_be_bytes());
                        }
                    } else {
                        target.copy_from_slice(&rgba);
                    }
                }
            }
            check()?;
        }
        Ok(())
    }
}
impl TileProducer for Frontier<'_> {
    fn fill_tile(
        &mut self,
        tx: u32,
        ty: u32,
        tile: &mut [u8],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let columns = self.frame[0].div_ceil(TILE);
        if ty.checked_mul(columns).and_then(|n| n.checked_add(tx)) != Some(self.next)
            || tx >= columns
            || ty >= self.frame[1].div_ceil(TILE)
            || tile.len() != TILE as usize * TILE as usize * self.stride
        {
            return Err("SVG frontier tile sequence or geometry differs".into());
        }
        check()?;
        if tx == 0 {
            self.load(ty, check)?;
        }
        // The actual tile writer supplies a new zeroed admitted native tile.
        // Clear explicitly so outside-canvas edge padding is never inherited.
        tile.fill(0);
        let width = (self.frame[0] - tx * TILE).min(TILE) as usize;
        let rows = (self.frame[1] - ty * TILE).min(TILE) as usize;
        for row in 0..rows {
            check()?;
            let source = (row * self.frame[0] as usize + tx as usize * TILE as usize) * self.stride;
            let target = row * TILE as usize * self.stride;
            tile[target..target + width * self.stride]
                .copy_from_slice(&self.bytes[source..source + width * self.stride]);
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or("SVG frontier tile count overflow")?;
        check()
    }
    fn complete(&self) -> Result<(), String> {
        if self.next != self.frame[0].div_ceil(TILE) * self.frame[1].div_ceil(TILE)
            || !self.plan.is_complete()
        {
            return Err("SVG output frontier was not fully consumed".into());
        }
        Ok(())
    }
}

pub(super) fn render(
    source: &PreparedSource<'_>,
    frame: [u32; 2],
    depth: u16,
    base: &NativeDocument,
    session: &SourceSession,
    cancellation: &Cancellation,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    session.check()?;
    cancellation.check()?;
    check()?;
    let pool = session.resident();
    if !source.uses_budget(&pool)
        || !base.budget().same_pool(&pool)
        || !base.disk_budget().same_pool(&session.disk())
        || ![8, 16].contains(&depth)
        || frame.iter().any(|n| *n == 0 || *n > 16000)
    {
        return Err("SVG staging source, precision, frame or pool differs".into());
    }
    let (svg, matrix) = source
        .svg_plan()
        .ok_or("SVG staging lacks its prepared definition")?;
    let tree = svg.tree();
    let intrinsic = svg.intrinsic();
    let [a, b, c, d, x, y] = matrix;
    let transform = resvg::tiny_skia::Transform::from_scale(
        intrinsic[0] as f32 / tree.size().width(),
        intrinsic[1] as f32 / tree.size().height(),
    )
    .post_concat(resvg::tiny_skia::Transform::from_row(a, b, c, d, x, y));
    let stride = if depth == 16 { 8 } else { 4 };
    let size = (frame[0] as usize)
        .checked_mul(frame[1].min(TILE) as usize)
        .and_then(|n| n.checked_mul(stride))
        .ok_or("SVG frontier allocation overflow")?;
    let bytes = ScratchBytes::new(&pool, size, size).map_err(|e| e.to_string())?;
    let budget = crate::svg::row_budget::Budget::new(&pool, cancellation.clone());
    let plan = resvg::rows::RowPlan::new(
        tree,
        transform,
        frame,
        resvg::rows::PlanLimits {
            retained_bytes: RETAINED_BYTES,
            maximum_invocations: MAX_INVOCATIONS,
            work: MAX_WORK,
        },
        &budget,
    )?;
    let mut frontier = Frontier {
        plan,
        frame,
        stride,
        bytes,
        next: 0,
    };
    stage::render_inner(
        source,
        frame,
        depth,
        base,
        session,
        Some(&mut frontier),
        &mut || {
            cancellation.check()?;
            check()
        },
    )
}
