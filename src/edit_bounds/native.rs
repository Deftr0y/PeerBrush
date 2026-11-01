//! Explicit bounded frame expansion. Geometry grants no document/source authority.
use crate::{
    large_document::{PinnedBytes, ResidentBudget, ScratchBytes},
    raster::{project16, Pixel16, TILE},
};

pub(crate) struct FrameExpansion {
    old_shape: [u32; 2],
    old_origin: [i32; 2],
    new_shape: [u32; 2],
    new_origin: [i32; 2],
    shift: [i32; 2],
    depth: u16,
    pool: ResidentBudget,
}
impl FrameExpansion {
    pub(crate) fn old_shape(&self) -> [u32; 2] {
        self.old_shape
    }
    pub(crate) fn old_origin(&self) -> [i32; 2] {
        self.old_origin
    }
    pub(crate) fn new_shape(&self) -> [u32; 2] {
        self.new_shape
    }
    pub(crate) fn new_origin(&self) -> [i32; 2] {
        self.new_origin
    }
    pub(crate) fn shift(&self) -> [i32; 2] {
        self.shift
    }
    /// Matches the resident edit-bound union after command geometry has been
    /// resolved in document coordinates. Live admission remains the engine's
    /// responsibility; this internal shape limit does not admit a16K document.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_edit(
        canvas: [u32; 2],
        old_shape: [u32; 2],
        old_origin: [i32; 2],
        requested_document: [i32; 4],
        selection_bounds: Option<[i32; 4]>,
        depth: u16,
        pool: &ResidentBudget,
    ) -> Result<Option<Self>, String> {
        if canvas
            .into_iter()
            .chain(old_shape)
            .any(|v| v == 0 || v > 16000)
            || old_origin.iter().any(|v| v.unsigned_abs() > 100000)
            || ![8, 16].contains(&depth)
        {
            return Err("Invalid native edit frame".into());
        }
        let canvas_rect = [0, 0, canvas[0] as i32, canvas[1] as i32];
        let clip = selection_bounds.unwrap_or(canvas_rect);
        let area = [
            requested_document[0].max(clip[0]).max(0),
            requested_document[1].max(clip[1]).max(0),
            requested_document[2].min(clip[2]).min(canvas_rect[2]),
            requested_document[3].min(clip[3]).min(canvas_rect[3]),
        ];
        let old = [
            old_origin[0],
            old_origin[1],
            old_origin[0] + old_shape[0] as i32,
            old_origin[1] + old_shape[1] as i32,
        ];
        if area[0] >= area[2]
            || area[1] >= area[3]
            || (area[0] >= old[0] && area[1] >= old[1] && area[2] <= old[2] && area[3] <= old[3])
        {
            return Ok(None);
        }
        let bounds = [
            old[0].min(0),
            old[1].min(0),
            old[2].max(canvas_rect[2]),
            old[3].max(canvas_rect[3]),
        ];
        let new_shape = [
            (bounds[2] - bounds[0]) as u32,
            (bounds[3] - bounds[1]) as u32,
        ];
        if new_shape.iter().any(|v| *v > 16000) {
            return Err("Expanded native edit frame exceeds the bounded source shape".into());
        }
        Ok(Some(Self {
            old_shape,
            old_origin,
            new_shape,
            new_origin: [bounds[0], bounds[1]],
            shift: [old[0] - bounds[0], old[1] - bounds[1]],
            depth,
            pool: pool.clone(),
        }))
    }
    pub(crate) fn tiles(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        (0..self.new_shape[1].div_ceil(TILE))
            .flat_map(move |y| (0..self.new_shape[0].div_ceil(TILE)).map(move |x| (x, y)))
    }
    fn destination(&self, key: (u32, u32)) -> Result<[i32; 4], String> {
        if key.0 >= self.new_shape[0].div_ceil(TILE) || key.1 >= self.new_shape[1].div_ceil(TILE) {
            return Err("Native frame tile is outside its declared footprint".into());
        }
        Ok([
            (key.0 * TILE) as i32,
            (key.1 * TILE) as i32,
            ((key.0 + 1) * TILE).min(self.new_shape[0]) as i32,
            ((key.1 + 1) * TILE).min(self.new_shape[1]) as i32,
        ])
    }
    /// Exact old-source local rectangle, at most one destination tile's area.
    /// None means explicit geometric zero, never an unavailable backing source.
    pub(crate) fn source_reads(&self, key: (u32, u32)) -> Result<Option<[i32; 4]>, String> {
        let area = self.destination(key)?;
        let source = [
            (area[0] - self.shift[0]).max(0),
            (area[1] - self.shift[1]).max(0),
            (area[2] - self.shift[0]).min(self.old_shape[0] as i32),
            (area[3] - self.shift[1]).min(self.old_shape[1] as i32),
        ];
        Ok((source[0] < source[2] && source[1] < source[3]).then_some(source))
    }
    /// The source callback reads prepared memory only and returns errors for
    /// missing pixels. Original physical padding is outside the logical source;
    /// it cannot become editable pixels when the storage frame changes.
    pub(crate) fn render_tile(
        &self,
        key: (u32, u32),
        check: &mut dyn FnMut() -> Result<(), String>,
        mut pixel: impl FnMut(i32, i32) -> Result<Pixel16, String>,
    ) -> Result<PinnedBytes, String> {
        check()?;
        let source = self.source_reads(key)?;
        let length = (TILE * TILE) as usize * if self.depth == 16 { 8 } else { 4 };
        let mut output =
            ScratchBytes::new(&self.pool, length, length).map_err(|e| e.to_string())?;
        if let Some(source) = source {
            for y in source[1]..source[3] {
                check()?;
                for x in source[0]..source[2] {
                    let value = pixel(x, y)?;
                    let local_x = (x + self.shift[0]) as u32 - key.0 * TILE;
                    let local_y = (y + self.shift[1]) as u32 - key.1 * TILE;
                    let index = (local_y * TILE + local_x) as usize;
                    if self.depth == 16 {
                        for (c, value) in value.into_iter().enumerate() {
                            output[index * 8 + c * 2..index * 8 + c * 2 + 2]
                                .copy_from_slice(&value.to_be_bytes());
                        }
                    } else {
                        for (c, value) in value.into_iter().enumerate() {
                            output[index * 4 + c] = project16(value);
                        }
                    }
                }
            }
        }
        check()?;
        Ok(output.freeze())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expansion_keeps_offcanvas_samples_low_bits_hidden_rgb_and_zero_padding() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
            let plan = FrameExpansion::for_edit(
                [273, 265],
                [17, 13],
                [200, -3],
                [0, 0, 5, 5],
                None,
                depth,
                &pool,
            )
            .unwrap()
            .unwrap();
            assert_eq!(plan.old_origin, [200, -3]);
            assert_eq!(plan.new_origin, [0, -3]);
            assert_eq!(plan.new_shape, [273, 268]);
            assert_eq!(plan.shift, [200, 0]);
            assert_eq!(plan.source_reads((0, 0)).unwrap(), Some([0, 0, 17, 13]));
            assert_eq!(plan.source_reads((1, 1)).unwrap(), None);
            for key in plan.tiles() {
                let bytes = plan
                    .render_tile(key, &mut || Ok(()), |x, y| {
                        assert!((0..17).contains(&x) && (0..13).contains(&y));
                        Ok([
                            12347 + x as u16 * 17,
                            33559 + y as u16 * 31,
                            51237,
                            if x % 3 == 0 { 0 } else { 45679 },
                        ])
                    })
                    .unwrap();
                for y in 0..TILE {
                    for x in 0..TILE {
                        let gx = (key.0 * TILE + x) as i32;
                        let gy = (key.1 * TILE + y) as i32;
                        let expected = if (200..217).contains(&gx) && (0..13).contains(&gy) {
                            let sx = gx - 200;
                            [
                                12347 + sx as u16 * 17,
                                33559 + gy as u16 * 31,
                                51237,
                                if sx % 3 == 0 { 0 } else { 45679 },
                            ]
                        } else {
                            [0; 4]
                        };
                        let index = (y * TILE + x) as usize;
                        if depth == 16 {
                            for c in 0..4 {
                                assert_eq!(
                                    u16::from_be_bytes([
                                        bytes[index * 8 + c * 2],
                                        bytes[index * 8 + c * 2 + 1]
                                    ]),
                                    expected[c]
                                );
                            }
                        } else {
                            for c in 0..4 {
                                assert_eq!(bytes[index * 4 + c], project16(expected[c]));
                            }
                        }
                    }
                }
            }
            assert_eq!(pool.used(), 0);
        }
    }
    #[test]
    fn covered_empty_and_excessive_expansions_have_no_source_or_allocation_authority() {
        let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        assert!(FrameExpansion::for_edit(
            [273, 265],
            [17, 13],
            [200, -3],
            [201, 1, 209, 5],
            None,
            16,
            &pool
        )
        .unwrap()
        .is_none());
        assert!(FrameExpansion::for_edit(
            [273, 265],
            [17, 13],
            [200, -3],
            [0, 0, 5, 5],
            Some([8, 8, 20, 20]),
            16,
            &pool
        )
        .unwrap()
        .is_none());
        assert!(FrameExpansion::for_edit(
            [16000, 16000],
            [16000, 16000],
            [-1, -1],
            [15999, 15999, 16000, 16000],
            None,
            16,
            &pool
        )
        .is_err());
        assert!(FrameExpansion::for_edit(
            [1, 1],
            [1, 1],
            [i32::MAX, 0],
            [0, 0, 1, 1],
            None,
            16,
            &pool
        )
        .is_err());
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn unavailable_remap_sample_or_cancel_returns_no_output_and_releases_scratch() {
        let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let plan = FrameExpansion::for_edit(
            [273, 265],
            [17, 13],
            [200, -3],
            [0, 0, 5, 5],
            None,
            16,
            &pool,
        )
        .unwrap()
        .unwrap();
        assert!(plan
            .render_tile((0, 0), &mut || Ok(()), |_, _| Err(
                "Missing prepared old source".into()
            ))
            .is_err());
        assert_eq!(pool.used(), 0);
        let mut checks = 0;
        assert!(plan
            .render_tile(
                (0, 0),
                &mut || {
                    checks += 1;
                    if checks > 3 {
                        Err("Cancelled frame expansion".into())
                    } else {
                        Ok(())
                    }
                },
                |_, _| Ok([12347, 33559, 51237, 45679])
            )
            .is_err());
        assert_eq!(pool.used(), 0);
    }
}
