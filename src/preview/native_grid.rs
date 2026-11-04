//! Preserve the established preview sample grid while preparing one document
//! tile at a time. A downsampled overview must not pin the entire source frame.
use crate::large_document::{Reservation, ResidentBudget};

type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Copy)]
struct Run {
    first: usize,
    end: usize,
    tile: i32,
}

pub(crate) struct Grid {
    xs: Vec<i32>,
    ys: Vec<i32>,
    columns: Vec<Run>,
    rows: Vec<Run>,
    frame: [i32; 2],
    rect: [i32; 4],
    _charge: Reservation,
}
pub(crate) struct Tile<'a> {
    pub(crate) source: [i32; 4],
    pub(crate) output: [usize; 4],
    pub(crate) xs: &'a [i32],
    pub(crate) ys: &'a [i32],
}
impl Grid {
    pub(crate) fn new(
        width: u32,
        height: u32,
        depth: u16,
        rect: Option<[i32; 4]>,
        edge: u32,
        budget: &ResidentBudget,
    ) -> Result<Self> {
        if width == 0 || height == 0 || width > 16000 || height > 16000 || ![8, 16].contains(&depth)
        {
            return Err("Invalid native preview frame".into());
        }
        let mut rect = rect.unwrap_or([0, 0, width as i32, height as i32]);
        rect[0] = rect[0].clamp(0, width as i32 - 1);
        rect[1] = rect[1].clamp(0, height as i32 - 1);
        rect[2] = rect[2].clamp(rect[0] + 1, width as i32);
        rect[3] = rect[3].clamp(rect[1] + 1, height as i32);
        let rw = rect[2] - rect[0];
        let rh = rect[3] - rect[1];
        let edge = edge.clamp(1, 8192);
        let scale = if depth == 8 {
            f64::from((edge as f32 / rw.max(rh) as f32).min(1.0))
        } else {
            (f64::from(edge) / f64::from(rw.max(rh))).min(1.0)
        };
        let dimension = |n: i32| {
            if depth == 8 {
                ((n as f32 * scale as f32).round() as usize).max(1)
            } else {
                ((f64::from(n) * scale).round() as usize).max(1)
            }
        };
        let (w, h) = (dimension(rw), dimension(rh));
        let bytes = (w + h)
            .checked_mul(std::mem::size_of::<i32>())
            .and_then(|n| n.checked_add(128 * std::mem::size_of::<Run>() + 512))
            .ok_or("Preview grid overflow")?;
        let charge = budget
            .reserve_bytes(bytes, 128 * 1024)
            .map_err(|e| e.to_string())?;
        let axis = |length: usize, start: i32| -> Result<Vec<i32>> {
            let mut out = Vec::new();
            out.try_reserve_exact(length)
                .map_err(|_| "Preview axis allocation refused")?;
            for at in 0..length {
                let delta = if depth == 8 {
                    (at as f32 / scale as f32) as i32
                } else {
                    (at as f64 / scale) as i32
                };
                out.push(start + delta);
            }
            Ok(out)
        };
        let xs = axis(w, rect[0])?;
        let ys = axis(h, rect[1])?;
        fn runs(axis: &[i32]) -> Result<Vec<Run>> {
            let mut out: Vec<Run> = Vec::new();
            out.try_reserve_exact(64)
                .map_err(|_| "Preview tile index allocation refused")?;
            for (at, &coordinate) in axis.iter().enumerate() {
                let tile = coordinate / 256;
                if let Some(last) = out.last_mut().filter(|r| r.tile == tile) {
                    last.end = at + 1;
                } else {
                    out.push(Run {
                        first: at,
                        end: at + 1,
                        tile,
                    });
                }
            }
            Ok(out)
        }
        let columns = runs(&xs)?;
        let rows = runs(&ys)?;
        Ok(Self {
            xs,
            ys,
            columns,
            rows,
            frame: [width as i32, height as i32],
            rect,
            _charge: charge,
        })
    }
    pub(crate) fn width(&self) -> usize {
        self.xs.len()
    }
    pub(crate) fn height(&self) -> usize {
        self.ys.len()
    }
    pub(crate) fn rect(&self) -> [i32; 4] {
        self.rect
    }
    pub(crate) fn visit(
        &self,
        check: &mut dyn FnMut() -> Result<()>,
        mut tile: impl FnMut(Tile<'_>) -> Result<()>,
    ) -> Result<()> {
        check()?;
        for row in &self.rows {
            for column in &self.columns {
                check()?;
                let x = column.tile * 256;
                let y = row.tile * 256;
                tile(Tile {
                    source: [
                        x,
                        y,
                        (x + 256).min(self.frame[0]),
                        (y + 256).min(self.frame[1]),
                    ],
                    output: [column.first, row.first, column.end, row.end],
                    xs: &self.xs[column.first..column.end],
                    ys: &self.ys[row.first..row.end],
                })?;
            }
        }
        check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Document;
    #[test]
    fn native_grids_render_exact_legacy_preview_samples_across_physical_tiles() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(257, 259, depth).unwrap();
            for y in 0..259 {
                for x in 0..257 {
                    if depth == 16 {
                        doc.layers[0].pixels.set16(
                            x,
                            y,
                            [12347 + x as u16, 33559 + y as u16, 51237, 34567],
                        );
                    } else {
                        doc.layers[0]
                            .pixels
                            .set(x, y, [(x * 17) as u8, (y * 29) as u8, 173, 137]);
                    }
                }
            }
            for rect in [None, Some([251, 249, 257, 259]), Some([-99, 13, 200, 300])] {
                for edge in [1, 31, 257, 8192] {
                    let (w, h, expected, area) = doc.preview(rect, edge, None, false).unwrap();
                    let pool = ResidentBudget::new(128 * 1024).unwrap();
                    let grid = Grid::new(257, 259, depth, rect, edge, &pool).unwrap();
                    assert_eq!(
                        (grid.width(), grid.height(), grid.rect()),
                        (w as usize, h as usize, area)
                    );
                    let mut bytes = vec![0; expected.len()];
                    let mut seen = vec![0u8; grid.width() * grid.height()];
                    grid.visit(&mut || Ok(()), |tile| {
                        assert!(
                            tile.source[2] - tile.source[0] <= 256
                                && tile.source[3] - tile.source[1] <= 256
                        );
                        for (dy, &y) in tile.ys.iter().enumerate() {
                            for (dx, &x) in tile.xs.iter().enumerate() {
                                assert!(
                                    x >= tile.source[0]
                                        && x < tile.source[2]
                                        && y >= tile.source[1]
                                        && y < tile.source[3]
                                );
                                let out =
                                    (tile.output[1] + dy) * grid.width() + tile.output[0] + dx;
                                seen[out] += 1;
                                let pixel = if depth == 16 {
                                    crate::depth16::display_pixel(doc.layers[0].pixels.get16(x, y))
                                } else {
                                    doc.layers[0].pixels.get(x, y)
                                };
                                bytes[out * 4..out * 4 + 4].copy_from_slice(&pixel);
                            }
                        }
                        Ok(())
                    })
                    .unwrap();
                    assert_eq!(bytes, expected, "depth{depth} edge{edge} rect{rect:?}");
                    assert!(seen.iter().all(|&v| v == 1));
                    drop(grid);
                    assert_eq!(pool.used(), 0);
                }
            }
        }
    }
    #[test]
    fn overview_is_partitioned_with_explicit_failure_and_final_cancellation() {
        let pool = ResidentBudget::new(128 * 1024).unwrap();
        let grid = Grid::new(16000, 16000, 16, None, 1536, &pool).unwrap();
        let mut regions = 0;
        let mut samples = 0;
        grid.visit(&mut || Ok(()), |tile| {
            regions += 1;
            samples += tile.xs.len() * tile.ys.len();
            Ok(())
        })
        .unwrap();
        assert_eq!(regions, 63 * 63);
        assert_eq!(samples, 1536 * 1536);
        let mut checks = 0;
        grid.visit(
            &mut || {
                checks += 1;
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();
        let last = checks;
        checks = 0;
        let mut completed = 0;
        assert!(grid
            .visit(
                &mut || {
                    checks += 1;
                    if checks == last {
                        Err("late cancellation".into())
                    } else {
                        Ok(())
                    }
                },
                |_| {
                    completed += 1;
                    Ok(())
                }
            )
            .is_err());
        assert_eq!(completed, regions);
        let mut visited = 0;
        assert!(grid
            .visit(&mut || Ok(()), |_| {
                visited += 1;
                Err("failed source preparation".into())
            })
            .is_err());
        assert_eq!(visited, 1);
        let tiny = ResidentBudget::new(1).unwrap();
        assert!(Grid::new(16000, 16000, 16, None, 1536, &tiny).is_err());
        assert_eq!(tiny.used(), 0);
    }
}
