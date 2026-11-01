//! Original glyph positions and whole-glyph coverage phase, with bounded scratch.
use super::{Check, NativeRegion};
use crate::{
    engine::scene::{Content36, Source36},
    fonts::prepared::FontOwner,
    large_document::{PinnedBytes, ResidentBudget, ScratchBytes},
    raster::{backed::Region, project16, Pixel16},
    source::TextLayout,
};
use ab_glyph::{point, Font, Glyph, PxScale, ScaleFont};
use std::sync::Arc;
struct ReadyGlyph {
    bounds: ab_glyph::Rect,
    coverage: PinnedBytes,
}
pub(super) struct TextPlan {
    font: Arc<FontOwner>,
    glyphs: Vec<ReadyGlyph>,
    color: Pixel16,
    frame: [u32; 2],
}
impl TextPlan {
    pub(super) fn is_known_zero(&self) -> bool {
        self.glyphs.is_empty() || self.color[3] == 0
    }
    pub(super) fn new(
        source: &Source36,
        font: Arc<FontOwner>,
        pool: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<Self, String> {
        let Content36::Text {
            text,
            layout,
            size,
            line_height,
            letter_spacing,
            align,
            color,
            ..
        } = &source.content
        else {
            return Err("Text preparation requires text metadata".into());
        };
        let _curves = pool
            .reserve_bytes(8 * 1024 * 1024, 8 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let f = font.font();
        let mut work = 0;
        let mut glyphs = Vec::new();
        glyphs
            .try_reserve_exact(16384)
            .map_err(|_| "Glyph placement allocation failed")?;
        if glyphs
            .capacity()
            .checked_mul(std::mem::size_of::<ReadyGlyph>())
            .is_none_or(|n| n > 4 * 1024 * 1024 - 4096)
        {
            return Err("Glyph placement capacity exceeded metadata admission".into());
        }
        if *layout == TextLayout::Shaped {
            let placed = {
                let _working = [
                    pool.reserve_bytes(64 * 1024 * 1024, 64 * 1024 * 1024)
                        .map_err(|e| e.to_string())?,
                    pool.reserve_bytes(64 * 1024 * 1024, 64 * 1024 * 1024)
                        .map_err(|e| e.to_string())?,
                    pool.reserve_bytes(64 * 1024 * 1024, 64 * 1024 * 1024)
                        .map_err(|e| e.to_string())?,
                ];
                font.preflight_shaping(check)?;
                // This callback precedes shaping's outline validation allocation.
                super::super::shaping::layout_parts(
                    source.width,
                    text,
                    match &source.content {
                        Content36::Text { font, .. } => font,
                        _ => unreachable!(),
                    },
                    *size,
                    *line_height,
                    *letter_spacing,
                    align,
                    f,
                    &mut |id| font.preflight_glyph(id, check),
                )?
            };
            for glyph in placed {
                push(&mut glyphs, &font, glyph, &mut work, pool, check)?;
            }
        } else {
            let scaled = f.as_scaled(PxScale::from(*size));
            for (row, line) in text.split('\n').enumerate() {
                let mut advance = 0.;
                let mut previous = None;
                for ch in line.chars() {
                    let g = scaled.glyph_id(ch);
                    if let Some(p) = previous {
                        advance += scaled.kern(p, g);
                        advance += *letter_spacing;
                    }
                    advance += scaled.h_advance(g);
                    previous = Some(g);
                }
                let start = match align.as_str() {
                    "center" => (source.width as f32 - advance) * 0.5,
                    "right" => source.width as f32 - advance,
                    _ => 0.,
                };
                let mut x = start;
                let mut previous = None;
                let y = scaled.ascent() + row as f32 * scaled.height() * line_height;
                for ch in line.chars() {
                    let gid = scaled.glyph_id(ch);
                    if let Some(p) = previous {
                        x += scaled.kern(p, gid);
                        x += *letter_spacing;
                    }
                    font.preflight_glyph(gid, check)?;
                    if gid.0 == 0 || (!super::super::spacing(ch) && f.outline(gid).is_none()) {
                        return Err("This text needs an unsupported glyph or outline".into());
                    }
                    push(
                        &mut glyphs,
                        &font,
                        gid.with_scale_and_position(*size, point(x, y)),
                        &mut work,
                        pool,
                        check,
                    )?;
                    x += scaled.h_advance(gid);
                    previous = Some(gid);
                }
            }
        }
        check()?;
        Ok(Self {
            font,
            glyphs,
            color: *color,
            frame: [source.width, source.height],
        })
    }
    pub(super) fn render(
        &self,
        region: Region,
        depth: u16,
        pool: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<NativeRegion, String> {
        if !self.font.uses_budget(pool) {
            return Err("Glyph snapshot belongs to another resident pool".into());
        }
        let stride = if depth == 16 { 8 } else { 4 };
        let bytes = region.pixels() * stride;
        let mut output = ScratchBytes::new(pool, bytes, bytes).map_err(|e| e.to_string())?;
        let b = region.bounds();
        let width = (b[2] - b[0]) as usize;
        for ready in &self.glyphs {
            check()?;
            let bounds = ready.bounds;
            if bounds.max.x < b[0] as f32
                || bounds.max.y < b[1] as f32
                || bounds.min.x >= b[2] as f32
                || bounds.min.y >= b[3] as f32
            {
                continue;
            }
            if !ready.coverage.uses_budget(pool) {
                return Err("Glyph coverage belongs to another resident pool".into());
            }
            let left = (bounds.min.x as i32).max(0).max(b[0]);
            let top = (bounds.min.y as i32).max(0).max(b[1]);
            let right = (bounds.max.x as i32).min(self.frame[0] as i32).min(b[2]);
            let bottom = (bounds.max.y as i32).min(self.frame[1] as i32).min(b[3]);
            let glyph_width = bounds.width() as usize;
            for y in top..bottom {
                check()?;
                for x in left..right {
                    let offset = ((y - bounds.min.y as i32) as usize * glyph_width
                        + (x - bounds.min.x as i32) as usize)
                        * 4;
                    let coverage = f32::from_bits(u32::from_be_bytes(
                        ready.coverage[offset..offset + 4].try_into().unwrap(),
                    ));
                    let mut p = self.color;
                    p[3] = (f32::from(p[3]) * coverage).round() as u16;
                    if p[3] == 0 {
                        continue;
                    }
                    let at = ((y - b[1]) as usize * width + (x - b[0]) as usize) * stride;
                    let old = std::array::from_fn(|c| {
                        if depth == 16 {
                            u16::from_be_bytes(
                                output[at + c * 2..at + c * 2 + 2].try_into().unwrap(),
                            )
                        } else {
                            u16::from(output[at + c]) * 257
                        }
                    });
                    let result = crate::raster::blend16(old, p, 1., "normal");
                    if depth == 16 {
                        for (c, v) in result.into_iter().enumerate() {
                            output[at + c * 2..at + c * 2 + 2].copy_from_slice(&v.to_be_bytes());
                        }
                    } else {
                        output[at..at + 4].copy_from_slice(&result.map(project16));
                    }
                }
            }
        }
        check()?;
        Ok(NativeRegion {
            region,
            depth,
            bytes: output.freeze(),
        })
    }
}

fn push(
    glyphs: &mut Vec<ReadyGlyph>,
    font: &FontOwner,
    glyph: Glyph,
    work: &mut u64,
    pool: &ResidentBudget,
    check: Check<'_>,
) -> Result<(), String> {
    let f = font.font();
    check()?;
    font.preflight_glyph(glyph.id, check)?;
    if glyph.id.0 == 0 {
        return Err("The selected font needs an unsupported glyph".into());
    }
    if let Some(outline) = f.outline_glyph(glyph.clone()) {
        let bounds = outline.px_bounds();
        *work = super::super::outline_work(bounds, *work)?;
        let pixels = (bounds.width() as usize)
            .checked_mul(bounds.height() as usize)
            .ok_or("Glyph rasterizer shape overflow")?;
        if pixels.checked_add(4).is_none_or(|n| n > 4 * 1024 * 1024) {
            return Err("Glyph coverage exceeds bounded rasterizer admission".into());
        }
        if glyphs.len() >= 16384 {
            return Err("Text exceeds the glyph placement budget".into());
        }
        let length = pixels
            .checked_mul(4)
            .ok_or("Glyph coverage footprint overflow")?;
        let mut coverage = ScratchBytes::new(pool, length, length).map_err(|e| e.to_string())?;
        let _raster = pool
            .reserve_bytes(16 * 1024 * 1024 + 16, 16 * 1024 * 1024 + 16)
            .map_err(|e| e.to_string())?;
        let mut failure = None;
        let width = bounds.width() as usize;
        outline.draw(|x, y, value| {
            if x == 0 && failure.is_none() {
                failure = check().err();
            }
            if failure.is_some() {
                return;
            }
            if !value.is_finite() {
                failure = Some("Glyph rasterizer emitted nonfinite coverage".into());
                return;
            }
            let at = (y as usize * width + x as usize) * 4;
            coverage[at..at + 4].copy_from_slice(&value.to_bits().to_be_bytes());
        });
        if let Some(error) = failure {
            return Err(error);
        }
        check()?;
        glyphs.push(ReadyGlyph {
            bounds,
            coverage: coverage.freeze(),
        });
    }
    Ok(())
}
