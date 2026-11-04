//! Memory-only foreground fills and clears on explicitly prepared native tiles.
use crate::{
    large_document::{PinnedBytes, ResidentBudget, ScratchBytes},
    raster::{blend, blend16, Pixel, TILE},
};

struct TileArea {
    width: u32,
    height: u32,
    depth: u16,
    area: [i32; 4],
    origin: [i32; 2],
    budget: ResidentBudget,
}
struct WorkingTile {
    bytes: ScratchBytes,
    area: [i32; 4],
    origin: [i32; 2],
}
impl TileArea {
    fn new(
        width: u32,
        height: u32,
        depth: u16,
        area: [i32; 4],
        origin: [i32; 2],
        budget: &ResidentBudget,
    ) -> Result<Self, String> {
        if width == 0
            || height == 0
            || width > 16000
            || height > 16000
            || ![8, 16].contains(&depth)
            || origin.iter().any(|v| v.unsigned_abs() > 100_000)
        {
            return Err("Invalid native fill/clear geometry".into());
        }
        let area = [
            area[0].max(0),
            area[1].max(0),
            area[2].min(width as i32),
            area[3].min(height as i32),
        ];
        Ok(Self {
            width,
            height,
            depth,
            area,
            origin,
            budget: budget.clone(),
        })
    }
    fn tiles(&self, enabled: bool) -> impl Iterator<Item = (u32, u32)> {
        let (x0, y0, x1, y1) =
            if !enabled || self.area[0] >= self.area[2] || self.area[1] >= self.area[3] {
                (0, 0, 0, 0)
            } else {
                (
                    self.area[0] as u32 / TILE,
                    self.area[1] as u32 / TILE,
                    (self.area[2] as u32).div_ceil(TILE),
                    (self.area[3] as u32).div_ceil(TILE),
                )
            };
        (y0..y1).flat_map(move |y| (x0..x1).map(move |x| (x, y)))
    }
    fn copy_tile(
        &self,
        key: (u32, u32),
        source: &PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<WorkingTile, String> {
        check()?;
        if !source.uses_budget(&self.budget) {
            return Err("Native tile source uses a different resident pool".into());
        }
        if key.0 >= self.width.div_ceil(TILE) || key.1 >= self.height.div_ceil(TILE) {
            return Err("Native tile is outside its frame".into());
        }
        let origin = [(key.0 * TILE) as i32, (key.1 * TILE) as i32];
        let area = [
            self.area[0].max(origin[0]),
            self.area[1].max(origin[1]),
            self.area[2].min(origin[0] + TILE as i32),
            self.area[3].min(origin[1] + TILE as i32),
        ];
        if area[0] >= area[2] || area[1] >= area[3] {
            return Err("Native tile is outside its prepared footprint".into());
        }
        let stride = if self.depth == 16 { 8 } else { 4 };
        let length = (TILE * TILE) as usize * stride;
        if source.len() != length {
            return Err("Native input tile length differs".into());
        }
        let mut bytes =
            ScratchBytes::new(&self.budget, length, length).map_err(|e| e.to_string())?;
        bytes.copy_from_slice(source);
        Ok(WorkingTile {
            bytes,
            area,
            origin,
        })
    }
}

pub(crate) struct PreparedFill<'a> {
    frame: TileArea,
    color: Pixel,
    polygon: Option<&'a [[f32; 2]]>,
}
impl<'a> PreparedFill<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        width: u32,
        height: u32,
        depth: u16,
        area: [i32; 4],
        origin: [i32; 2],
        color: Pixel,
        polygon: Option<&'a [[f32; 2]]>,
        budget: &ResidentBudget,
    ) -> Result<Self, String> {
        if polygon.is_some_and(|p| p.iter().flatten().any(|v| !v.is_finite())) {
            return Err("Invalid native foreground fill polygon".into());
        }
        Ok(Self {
            frame: TileArea::new(width, height, depth, area, origin, budget)?,
            color,
            polygon,
        })
    }
    pub(crate) fn tiles(&self) -> impl Iterator<Item = (u32, u32)> {
        self.frame.tiles(self.color[3] != 0)
    }
    pub(crate) fn render_tile(
        &self,
        key: (u32, u32),
        source: &PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String> {
        if self.color[3] == 0 {
            return Err("Transparent native fill has no prepared tiles".into());
        }
        let WorkingTile {
            mut bytes,
            area,
            origin,
        } = self.frame.copy_tile(key, source, check)?;
        let stride = if self.frame.depth == 16 { 8 } else { 4 };
        let color16 = self.color.map(|v| u16::from(v) * 257);
        for y in area[1]..area[3] {
            check()?;
            for x in area[0]..area[2] {
                if self.polygon.is_some_and(|p| {
                    !crate::selection::contains(
                        p,
                        (x + self.frame.origin[0]) as f32 + 0.5,
                        (y + self.frame.origin[1]) as f32 + 0.5,
                    )
                }) {
                    continue;
                }
                let at =
                    (((y - origin[1]) as u32 * TILE + (x - origin[0]) as u32) as usize) * stride;
                if self.frame.depth == 16 {
                    let old = std::array::from_fn(|i| {
                        u16::from_be_bytes([bytes[at + i * 2], bytes[at + i * 2 + 1]])
                    });
                    let result = if color16[3] == 65535 {
                        color16
                    } else {
                        blend16(old, color16, 1., "normal")
                    };
                    for (dst, value) in bytes[at..at + 8].chunks_exact_mut(2).zip(result) {
                        dst.copy_from_slice(&value.to_be_bytes());
                    }
                } else {
                    let old = bytes[at..at + 4].try_into().unwrap();
                    let result = if self.color[3] == 255 {
                        self.color
                    } else {
                        blend(old, self.color, 1., "normal")
                    };
                    bytes[at..at + 4].copy_from_slice(&result);
                }
            }
        }
        check()?;
        Ok(bytes.freeze())
    }
    /// Results delivered before an error remain provisional. Only the common
    /// engine transaction can publish after this and its source checks succeed.
    pub(crate) fn visit(
        &self,
        check: &mut dyn FnMut() -> Result<(), String>,
        mut prepare: impl FnMut((u32, u32)) -> Result<PinnedBytes, String>,
        mut provisional: impl FnMut((u32, u32), PinnedBytes) -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        for key in self.tiles() {
            check()?;
            let source = prepare(key)?;
            let output = self.render_tile(key, &source, check)?;
            check()?;
            provisional(key, output)?;
        }
        check()
    }
}

pub(crate) struct PreparedClear {
    frame: TileArea,
}
impl PreparedClear {
    pub(crate) fn new(
        width: u32,
        height: u32,
        depth: u16,
        local_area: [i32; 4],
        origin: [i32; 2],
        budget: &ResidentBudget,
    ) -> Result<Self, String> {
        Ok(Self {
            frame: TileArea::new(width, height, depth, local_area, origin, budget)?,
        })
    }
    pub(crate) fn tiles(&self) -> impl Iterator<Item = (u32, u32)> {
        self.frame.tiles(true)
    }
    /// Coverage is already prepared in document coordinates. Sampling must be
    /// memory-only and fail on an undeclared footprint, never fetch backing here.
    pub(crate) fn render_tile(
        &self,
        key: (u32, u32),
        source: &PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
        mut coverage: impl FnMut(i32, i32) -> Result<f32, String>,
    ) -> Result<PinnedBytes, String> {
        let WorkingTile {
            mut bytes,
            area,
            origin,
        } = self.frame.copy_tile(key, source, check)?;
        let stride = if self.frame.depth == 16 { 8 } else { 4 };
        for y in area[1]..area[3] {
            check()?;
            for x in area[0]..area[2] {
                let amount = coverage(x + self.frame.origin[0], y + self.frame.origin[1])?;
                if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
                    return Err("Invalid prepared selection confidence".into());
                }
                let remaining = 1. - amount as f64;
                if remaining >= 1. {
                    continue;
                }
                let at =
                    (((y - origin[1]) as u32 * TILE + (x - origin[0]) as u32) as usize) * stride;
                if self.frame.depth == 16 {
                    let alpha = u16::from_be_bytes([bytes[at + 6], bytes[at + 7]]);
                    let result = (alpha as f64 * remaining).round() as u16;
                    if result == 0 {
                        bytes[at..at + 8].fill(0);
                    } else {
                        bytes[at + 6..at + 8].copy_from_slice(&result.to_be_bytes());
                    }
                } else {
                    let result = (bytes[at + 3] as f64 * remaining).round() as u8;
                    if result == 0 {
                        bytes[at..at + 4].fill(0);
                    } else {
                        bytes[at + 3] = result;
                    }
                }
            }
        }
        check()?;
        Ok(bytes.freeze())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clear_uses_exact_soft_confidence_and_preserves_physical_padding() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(2 * 1024 * 1024).unwrap();
            let length = 256 * 256 * if depth == 16 { 8 } else { 4 };
            let mut bytes = ScratchBytes::new(&pool, length, length).unwrap();
            bytes.fill(173);
            let source = bytes.freeze();
            let clear = PreparedClear::new(1, 1, depth, [0, 0, 1, 1], [17, -23], &pool).unwrap();
            assert_eq!(clear.tiles().collect::<Vec<_>>(), vec![(0, 0)]);
            let output = clear
                .render_tile((0, 0), &source, &mut || Ok(()), |x, y| {
                    assert_eq!([x, y], [17, -23]);
                    Ok(128f32 / 255.)
                })
                .unwrap();
            if depth == 16 {
                let expected = (u16::from_be_bytes([173, 173]) as f64
                    * (1. - (128f32 / 255.) as f64))
                    .round() as u16;
                assert_eq!(&output[..6], &source[..6]);
                assert_eq!(&output[6..8], &expected.to_be_bytes());
                assert_eq!(&output[8..], &source[8..]);
            } else {
                let expected = (173f64 * (1. - (128f32 / 255.) as f64)).round() as u8;
                assert_eq!(&output[..3], &source[..3]);
                assert_eq!(output[3], expected);
                assert_eq!(&output[4..], &source[4..]);
            }
            drop(output);
            drop(source);
            drop(clear);
            assert_eq!(pool.used(), 0);
        }
    }
    #[test]
    fn bad_or_unavailable_coverage_returns_no_partial_native_clear() {
        let pool = ResidentBudget::new(2 * 1024 * 1024).unwrap();
        let mut bytes = ScratchBytes::new(&pool, 512 * 1024, 512 * 1024).unwrap();
        bytes.fill(173);
        let source = bytes.freeze();
        let clear = PreparedClear::new(256, 256, 16, [0, 0, 256, 256], [0, 0], &pool).unwrap();
        let mut calls = 0;
        assert!(clear
            .render_tile((0, 0), &source, &mut || Ok(()), |_, _| {
                calls += 1;
                if calls == 3 {
                    Err("Unprepared coverage".into())
                } else {
                    Ok(0.5)
                }
            })
            .is_err());
        assert!(clear
            .render_tile((0, 0), &source, &mut || Ok(()), |_, _| Ok(1.1))
            .is_err());
        assert_eq!(pool.used(), 512 * 1024);
        assert!(source.iter().all(|v| *v == 173));
    }
    #[test]
    fn edge_padding_and_native_low_bits_survive_clipped_foreground_fill() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(2 * 1024 * 1024).unwrap();
            let stride = if depth == 16 { 8 } else { 4 };
            let mut raw = ScratchBytes::new(
                &pool,
                (TILE * TILE) as usize * stride,
                (TILE * TILE) as usize * stride,
            )
            .unwrap();
            raw.fill(173);
            let source = raw.freeze();
            let fill = PreparedFill::new(
                257,
                259,
                depth,
                [256, 256, 257, 259],
                [0, 0],
                [71, 137, 211, 127],
                None,
                &pool,
            )
            .unwrap();
            assert_eq!(fill.tiles().collect::<Vec<_>>(), vec![(1, 1)]);
            let output = fill.render_tile((1, 1), &source, &mut || Ok(())).unwrap();
            for y in 0..256 {
                for x in 0..256 {
                    let at = (y * 256 + x) * stride;
                    if x == 0 && y < 3 {
                        assert_ne!(&output[at..at + stride], &source[at..at + stride]);
                    } else {
                        assert_eq!(&output[at..at + stride], &source[at..at + stride]);
                    }
                }
            }
            drop(output);
            drop(source);
            drop(fill);
            assert_eq!(pool.used(), 0);
        }
    }
    #[test]
    fn failed_source_sink_or_final_cancel_cannot_complete_provisional_visit() {
        let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let source = ScratchBytes::new(&pool, 512 * 1024, 512 * 1024)
            .unwrap()
            .freeze();
        let fill = PreparedFill::new(
            512,
            256,
            16,
            [0, 0, 512, 256],
            [0, 0],
            [17, 71, 201, 255],
            None,
            &pool,
        )
        .unwrap();
        let mut emitted = 0;
        let result = fill.visit(
            &mut || Ok(()),
            |key| {
                if key.0 == 1 {
                    Err("Source pin failed".into())
                } else {
                    Ok(source.clone())
                }
            },
            |_, _| {
                emitted += 1;
                Ok(())
            },
        );
        assert_eq!(result.err().unwrap(), "Source pin failed");
        assert_eq!(emitted, 1);
        assert_eq!(pool.used(), 512 * 1024);
        let result = fill.visit(
            &mut || Ok(()),
            |_| Ok(source.clone()),
            |_, _| Err("Candidate refused".into()),
        );
        assert_eq!(result.err().unwrap(), "Candidate refused");
        assert_eq!(pool.used(), 512 * 1024);
        let mut checks = 0;
        assert!(fill
            .render_tile((0, 0), &source, &mut || {
                checks += 1;
                if checks > 10 {
                    Err("Cancelled".into())
                } else {
                    Ok(())
                }
            })
            .is_err());
        assert_eq!(pool.used(), 512 * 1024);
        assert!(source.iter().all(|v| *v == 0));
    }
}
