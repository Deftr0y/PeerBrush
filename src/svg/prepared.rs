//! Static SVG admission with the existing exact full-origin rasterizer.
//! A translated cropped canvas changes tiny-skia coverage for affine paths.
//! Until a global-phase streaming renderer is qualified, large output refuses.
use super::{tiny_skia, usvg};
use crate::{
    large_document::{Reservation, ResidentBudget, ScratchBytes},
    raster::backed::Region,
    source::prepared::NativeRegion,
};
use std::cell::RefCell;

struct Projection {
    frame: [u32; 2],
    matrix: [f32; 6],
    pixmap: tiny_skia::Pixmap,
    _charges: Vec<Reservation>,
}
pub(crate) struct PreparedSvg {
    tree: usvg::Tree,
    intrinsic: [u32; 2],
    projection: RefCell<Option<Projection>>,
    _charge: Reservation,
}
impl PreparedSvg {
    pub(crate) fn new(
        markup: &str,
        intrinsic: [u32; 2],
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        let charge = pool
            .reserve_bytes(64 * 1024 * 1024, 64 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let tree = super::tree_bounded(markup, true)?;
        if intrinsic
            != [
                tree.size().width().ceil() as u32,
                tree.size().height().ceil() as u32,
            ]
        {
            return Err("SVG viewport must match its editable source frame".into());
        }
        check()?;
        Ok(Self {
            tree,
            intrinsic,
            projection: RefCell::new(None),
            _charge: charge,
        })
    }
    pub(crate) fn render(
        &self,
        frame: [u32; 2],
        matrix: [f32; 6],
        region: Region,
        depth: u16,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<NativeRegion, String> {
        if !self._charge.uses_budget(pool) || ![8, 16].contains(&depth) {
            return Err("SVG projection precision or resident pool differs".into());
        }
        check()?;
        let b = region.bounds();
        if frame.iter().any(|v| *v == 0 || *v > 16000)
            || b[0] < 0
            || b[1] < 0
            || b[2] > frame[0] as i32
            || b[3] > frame[1] as i32
        {
            return Err("SVG output region differs from its captured frame".into());
        }
        let area = u64::from(frame[0]) * u64::from(frame[1]);
        // shortcut: at most4MP full-origin SVG scratch; replace only with a
        // qualified renderer preserving global AA/gradient phase and isolation.
        if area > 4 * 1024 * 1024 {
            return Err("Large SVG needs a qualified global-phase streaming renderer; saved appearance and definition must be retained".into());
        }
        let mut cache = self
            .projection
            .try_borrow_mut()
            .map_err(|_| "SVG projection is busy")?;
        if !cache
            .as_ref()
            .is_some_and(|p| p.frame == frame && p.matrix == matrix)
        {
            drop(cache.take());
            let [a, bb, c, d, x, y] = matrix;
            let transform = tiny_skia::Transform::from_scale(
                self.intrinsic[0] as f32 / self.tree.size().width(),
                self.intrinsic[1] as f32 / self.tree.size().height(),
            )
            .post_concat(tiny_skia::Transform::from_row(a, bb, c, d, x, y));
            let (mut paths, mut segments, mut nesting) = (0, 0, 0);
            super::complexity(self.tree.root(), 0, &mut paths, &mut segments, &mut nesting)?;
            for clip in self.tree.clip_paths() {
                super::complexity(clip.root(), 0, &mut paths, &mut segments, &mut nesting)?;
            }
            let (mut memory, mut largest) = (0, area);
            let clips =
                ((nesting + 2) as u64).saturating_mul((self.tree.clip_paths().len() + 1) as u64);
            super::surfaces(
                self.tree.root(),
                transform,
                0,
                clips,
                frame,
                &mut memory,
                &mut largest,
            )?;
            let bytes = area
                .saturating_mul(20)
                .saturating_add(memory)
                .saturating_add(1024 * 1024);
            if largest.saturating_mul(paths.max(segments.div_ceil(32)).max(1) as u64) > 256_000_000
                || bytes > 128 * 1024 * 1024
            {
                return Err("SVG exceeds bounded full-origin rendering work or memory".into());
            }
            let mut charges = Vec::new();
            let mut remaining = bytes as usize;
            while remaining != 0 {
                let n = remaining.min(64 * 1024 * 1024);
                charges.push(pool.reserve_bytes(n, n).map_err(|e| e.to_string())?);
                remaining -= n;
            }
            check()?;
            let mut pixmap = tiny_skia::Pixmap::new(frame[0], frame[1])
                .ok_or("Cannot allocate admitted SVG projection")?;
            resvg::render(&self.tree, transform, &mut pixmap.as_mut());
            check()?;
            *cache = Some(Projection {
                frame,
                matrix,
                pixmap,
                _charges: charges,
            });
        }
        let projection = cache.as_ref().ok_or("Prepared SVG projection is missing")?;
        let stride = if depth == 16 { 8 } else { 4 };
        let length = region.pixels() * stride;
        let mut output = ScratchBytes::new(pool, length, length).map_err(|e| e.to_string())?;
        let width = (b[2] - b[0]) as usize;
        for (row, part) in output.chunks_mut(width * stride).enumerate() {
            check()?;
            let start = (row + b[1] as usize) * frame[0] as usize + b[0] as usize;
            for (out, pixel) in part
                .chunks_exact_mut(stride)
                .zip(&projection.pixmap.pixels()[start..start + width])
            {
                let p = pixel.demultiply();
                let rgba = [p.red(), p.green(), p.blue(), p.alpha()];
                if depth == 16 {
                    for (target, v) in out.chunks_exact_mut(2).zip(rgba) {
                        target.copy_from_slice(&(u16::from(v) * 257).to_be_bytes());
                    }
                } else {
                    out.copy_from_slice(&rgba);
                }
            }
        }
        check()?;
        NativeRegion::from_bytes(region, depth, output.freeze())
    }
}
