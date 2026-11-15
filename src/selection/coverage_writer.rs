//! A completed scalar row stream stages each full native tile once. The
//! resulting selection remains a candidate until common engine publication.
use super::{backed::CoverageSource, contours::Contours, rows::CompleteRows};
use crate::{
    large_document::{
        BlockRef, Error, Reservation, ResidentBudget, Result, ScratchBytes, Store, TileSpec,
    },
    raster::{backed::Plane, surface::Surface, TILE},
};
use std::collections::BTreeMap;

const TILE_BYTES: usize = (TILE * TILE * 4) as usize;
const MAX_COLUMNS: usize = 16000u32.div_ceil(TILE) as usize;

/// Ordered scalar sources share the same completion/contour/native-tile path
/// as flood output. Completion grants no source or document publication.
pub(crate) fn stage_rows(
    width: u32,
    height: u32,
    store: Store,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
    read: impl FnMut(u32, &mut [u8]) -> std::result::Result<(), String>,
) -> std::result::Result<CoverageSource, String> {
    stage_feathered_rows(width, height, 0, store, pool, check, None, read, |_, _| {
        Ok(())
    })
}

pub(crate) fn stage_rows_guarded(
    width: u32,
    height: u32,
    store: Store,
    pool: &ResidentBudget,
    active: &dyn Fn() -> Result<()>,
    owner: &dyn Fn() -> Result<()>,
    read: impl FnMut(u32, &mut [u8]) -> std::result::Result<(), String>,
) -> std::result::Result<CoverageSource, String> {
    stage_feathered_rows(
        width, height, 0, store, pool, active, Some(owner), read, |_, _| Ok(()),
    )
}

pub(crate) fn stage_feathered_rows(
    width: u32,
    height: u32,
    feather: u32,
    store: Store,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
    owner: Option<&dyn Fn() -> Result<()>>,
    mut read: impl FnMut(u32, &mut [u8]) -> std::result::Result<(), String>,
    mut combine: impl FnMut(u32, &mut [u8]) -> std::result::Result<(), String>,
) -> std::result::Result<CoverageSource, String> {
    if feather > 64 {
        return Err("Selection feather exceeds its bounded radius".into());
    }
    if let Some(owner) = owner {
        owner().map_err(|e| e.to_string())?;
    }
    let writer = Writer::new(width, height, store, pool).map_err(|e| e.to_string())?;
    if feather == 0 {
        return unfiltered(writer, check, owner, read, combine);
    }
    let mut writer = writer;
    let w = width as usize;
    let ring_bytes = w
        .checked_mul(2 * feather as usize + 1)
        .ok_or("Selection feather size overflow")?;
    let mut ring = ScratchBytes::new(pool, ring_bytes, ring_bytes).map_err(|e| e.to_string())?;
    let mut sums = ScratchBytes::new(pool, w * 4, w * 4).map_err(|e| e.to_string())?;
    let mut input = ScratchBytes::new(pool, w, w).map_err(|e| e.to_string())?;
    let mut output = ScratchBytes::new(pool, w, w).map_err(|e| e.to_string())?;
    let mut combined = ScratchBytes::new(pool, w, w).map_err(|e| e.to_string())?;
    let mut ordered = super::rows::Feather::new(
        width,
        height,
        feather,
        &mut ring,
        &mut sums,
        &mut input,
        &mut output,
    )
    .map_err(str::to_owned)?;
    let complete = ordered.run(
        || check().map_err(|e| e.to_string()),
        |y, row| read(y, row),
        |y, row| {
            combined.copy_from_slice(row);
            combine(y, &mut combined)?;
            writer
                .row_guarded(y, &combined, &mut || check(), owner)
                .map_err(|e| e.to_string())
        },
    )?;
    writer
        .finish_guarded(&complete, &mut || check(), owner)
        .map_err(|e| e.to_string())
}

fn unfiltered(
    mut writer: Writer,
    active: &dyn Fn() -> Result<()>,
    owner: Option<&dyn Fn() -> Result<()>>,
    read: impl FnMut(u32, &mut [u8]) -> std::result::Result<(), String>,
    mut combine: impl FnMut(u32, &mut [u8]) -> std::result::Result<(), String>,
) -> std::result::Result<CoverageSource, String> {
    let width = writer.width as usize;
    let mut row = ScratchBytes::new(&writer.store.resident_budget(), width, width)
        .map_err(|e| e.to_string())?;
    let complete = super::rows::unfiltered(
        writer.width,
        writer.height,
        &mut row,
        || active().map_err(|e| e.to_string()),
        read,
        |y, row| {
            combine(y, row)?;
            writer
                .row_guarded(y, row, &mut || active(), owner)
                .map_err(|e| e.to_string())
        },
    )?;
    writer
        .finish_guarded(&complete, &mut || active(), owner)
        .map_err(|e| e.to_string())
}
pub(super) struct Writer {
    width: u32,
    height: u32,
    next: u32,
    columns: u32,
    stripe: ScratchBytes,
    dirty: [bool; MAX_COLUMNS],
    contours: Contours,
    blocks: BTreeMap<(u32, u32), BlockRef>,
    store: Store,
    _index: Reservation,
    failed: bool,
}

impl Writer {
    pub fn new(width: u32, height: u32, store: Store, pool: &ResidentBudget) -> Result<Self> {
        if width == 0
            || height == 0
            || width > 16000
            || height > 16000
            || !store.resident_budget().same_pool(pool)
        {
            return Err(Error::Invalid(
                "Selection tile writer requires exact shape and shared payload ledger",
            ));
        }
        let columns = width.div_ceil(TILE);
        let bytes = columns as usize * TILE_BYTES;
        Ok(Self {
            width,
            height,
            next: 0,
            columns,
            stripe: ScratchBytes::new(pool, bytes, bytes)?,
            dirty: [false; MAX_COLUMNS],
            contours: Contours::new(width, height, pool)?,
            blocks: BTreeMap::new(),
            store,
            _index: pool.reserve_bytes(
                columns as usize * height.div_ceil(TILE) as usize * 2048,
                8 * 1024 * 1024,
            )?,
            failed: false,
        })
    }
    pub fn row(
        &mut self,
        y: u32,
        values: &[u8],
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        self.row_guarded(y, values, check, None)
    }

    fn row_guarded(
        &mut self,
        y: u32,
        values: &[u8],
        check: &mut dyn FnMut() -> Result<()>,
        owner: Option<&dyn Fn() -> Result<()>>,
    ) -> Result<()> {
        let invalid = self.failed
            || y != self.next
            || y >= self.height
            || values.len() != self.width as usize;
        self.failed = true;
        if invalid {
            return Err(Error::Invalid(
                "Selection tile rows differ from their exact source",
            ));
        }
        check()?;
        self.contours.row(y, values, check)?;
        let zero = [0; TILE as usize];
        for (column, samples) in values.chunks(TILE as usize).enumerate() {
            if column * TILE as usize % 4096 == 0 {
                check()?;
            }
            // Each stripe starts zero and each row is written once.
            if samples == &zero[..samples.len()] {
                continue;
            }
            self.dirty[column] = true;
            let at = column * TILE_BYTES + ((y % TILE) * TILE) as usize * 4;
            for (pixel, &value) in self.stripe[at..at + samples.len() * 4].chunks_exact_mut(4).zip(samples) {
                pixel.fill(value);
            }
        }
        if (y + 1) % TILE == 0 || y + 1 == self.height {
            let spec = TileSpec::new(TILE as u16, TILE as u16, 8)?;
            for (x, dirty) in self.dirty[..self.columns as usize].iter_mut().enumerate() {
                check()?;
                if !*dirty {
                    continue;
                }
                let at = x * TILE_BYTES;
                let tile = &mut self.stripe[at..at + TILE_BYTES];
                let block = if let Some(owner) = owner {
                    self.store.put_guarded(spec, tile, check, &mut || owner())?
                } else {
                    self.store.put(spec, tile, check)?
                };
                #[cfg(test)]
                tests::observe_completed_tile();
                self.blocks.insert((x as u32, y / TILE), block);
                tile.fill(0);
                *dirty = false;
            }
        }
        check()?;
        self.next += 1;
        self.failed = false;
        Ok(())
    }

    pub fn finish(
        self,
        complete: &CompleteRows,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<CoverageSource> {
        self.finish_guarded(complete, check, None)
    }

    fn finish_guarded(
        self,
        complete: &CompleteRows,
        check: &mut dyn FnMut() -> Result<()>,
        owner: Option<&dyn Fn() -> Result<()>>,
    ) -> Result<CoverageSource> {
        if self.failed || self.next != self.height || complete.shape() != (self.width, self.height)
        {
            return Err(Error::Invalid(
                "Selection scalar rows did not complete their exact source",
            ));
        }
        check()?;
        let (bounds, contours) = self.contours.finish(check)?;
        // Re-observe every generated reference while retaining at most one
        // native pin. Adjacent duplicate references reuse that live pin.
        let mut previous = None;
        for reference in self.blocks.values() {
            check()?;
            let fingerprint = reference.fingerprint();
            if previous
                .as_ref()
                .is_some_and(|(key, _)| *key == fingerprint)
            {
                continue;
            }
            drop(previous.take());
            let pin = if let Some(owner) = owner {
                self.store.pin_guarded(*reference, check, &mut || owner())?
            } else {
                self.store.pin(*reference, check)?
            };
            previous = Some((fingerprint, pin));
        }
        check()?;
        let surface = if self.blocks.is_empty() {
            Surface::zero(self.width, self.height, 8)?
        } else {
            Surface::from_plane(Plane::from_blocks(
                self.width,
                self.height,
                8,
                self.store,
                self.blocks,
            )?)?
        };
        let output = CoverageSource::from_frozen(surface, bounds, [0, 0], contours)?;
        if let Some(owner) = owner {
            owner()?;
        }
        check()?;
        Ok(output)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };
    thread_local! {
        static COMPLETED_TILES: RefCell<Option<Rc<Cell<usize>>>> = const { RefCell::new(None) };
    }
    pub(crate) struct CompletedTiles {
        count: Rc<Cell<usize>>,
        previous: Option<Rc<Cell<usize>>>,
    }
    impl CompletedTiles {
        pub(crate) fn observe() -> Self {
            let count = Rc::new(Cell::new(0));
            let previous = COMPLETED_TILES.with(|slot| slot.replace(Some(count.clone())));
            Self { count, previous }
        }
        pub(crate) fn count(&self) -> usize {
            self.count.get()
        }
    }
    impl Drop for CompletedTiles {
        fn drop(&mut self) {
            COMPLETED_TILES.with(|slot| *slot.borrow_mut() = self.previous.take());
        }
    }
    pub(super) fn observe_completed_tile() {
        COMPLETED_TILES.with(|slot| {
            if let Some(count) = slot.borrow().as_ref() {
                count.set(count.get() + 1);
            }
        });
    }
    use crate::{
        raster::{backed::Region, Raster},
        selection::rows::Feather,
    };
    fn store(pool: ResidentBudget) -> (Store, std::path::PathBuf) {
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-coverage-writer-{}", crate::engine::id()));
        let store = Store::create(
            &path,
            pool,
            crate::large_document::Limits {
                disk_bytes: 32 * 1024 * 1024,
                max_blocks: 64,
                max_resident_blocks: 32,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        (store, path)
    }
    #[test]
    fn actual_soft_row_stream_stages_native_surface_exactly_across_tile_edges() {
        for radius in [0usize, 3] {
            let (w, h) = (257u32, 259u32);
            let pool = ResidentBudget::new(96 * 1024 * 1024).unwrap();
            let (store, path) = store(pool.clone());
            let mut writer = Writer::new(w, h, store.clone(), &pool).unwrap();
            let mut ring = vec![0; (2 * radius + 1) * w as usize];
            let mut sums = vec![0; w as usize * 4];
            let mut input = vec![0; w as usize];
            let mut row = vec![0; w as usize];
            let mut expected = Raster::new(w, h);
            let complete = Feather::new(
                w,
                h,
                radius as u32,
                &mut ring,
                &mut sums,
                &mut input,
                &mut row,
            )
            .unwrap()
            .run(
                || Ok(()),
                |y, row| {
                    for (x, v) in row.iter_mut().enumerate() {
                        *v = if (3..253).contains(&x)
                            && (2..257).contains(&y)
                            && !(x > 60 && x < 190 && y > 60 && y < 190)
                        {
                            255
                        } else {
                            0
                        };
                    }
                    Ok(())
                },
                |y, row| {
                    for (x, &v) in row.iter().enumerate() {
                        expected.set(x as i32, y as i32, [v; 4]);
                    }
                    writer
                        .row(y, row, &mut || Ok(()))
                        .map_err(|e| e.to_string())
                },
            )
            .unwrap();
            let source = writer.finish(&complete, &mut || Ok(())).unwrap();
            let legacy = crate::selection::from_mask(expected).unwrap();
            assert_eq!(source.bounds(), legacy.bounds);
            for y in 0..h {
                let view = source
                    .prepare_region(
                        Region::new([0, y as i32, w as i32, y as i32 + 1]).unwrap(),
                        &pool,
                        &mut || Ok(()),
                    )
                    .unwrap();
                for x in 0..w {
                    assert_eq!(
                        view.value(x as i32, y as i32).unwrap(),
                        legacy.value(x as i32, y as i32)
                    );
                }
            }
            assert_eq!(source.contours().len(), 2);
            drop(source);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    #[test]
    fn unfeathered_rows_keep_soft_gaps_and_join_contours_across_tiles() {
        let width = 3 * TILE + 1;
        let pool = ResidentBudget::new(96 * 1024 * 1024).unwrap();
        let (store, path) = store(pool.clone());
        let source = stage_feathered_rows(
            width,
            TILE + 3,
            0,
            store.clone(),
            &pool,
            &|| Ok(()),
            None,
            |y, row| {
                row.fill(0);
                if y == 0 {
                    row[TILE as usize - 1..=TILE as usize].fill(255);
                }
                Ok(())
            },
            |y, row| {
                if y == 1 {
                    row[TILE as usize - 1] = 191;
                    row[width as usize - 1] = 63;
                }
                if y == TILE {
                    row[TILE as usize + 1] = 111;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            source.bounds(),
            [TILE as i32 - 1, 0, width as i32, TILE as i32 + 1]
        );
        assert_eq!(source.contours().len(), 1);
        assert_eq!(source.mask().physical_tiles().len(), 4);
        let view = source
            .mask()
            .prepare_region(Region::new([0, 0, width as i32, 3]).unwrap(), &mut || Ok(()))
            .unwrap();
        for (x, y, value) in [
            (TILE as i32 - 1, 0, 255),
            (TILE as i32, 0, 255),
            (TILE as i32 - 1, 1, 191),
            (TILE as i32 - 1, 2, 0),
            (2 * TILE as i32, 1, 0),
            (width as i32 - 1, 1, 63),
        ] {
            assert_eq!(view.pixel(x, y).unwrap(), [value; 4]);
        }
        let edge = TILE as i32;
        let tail = source
            .mask()
            .prepare_region(
                Region::new([edge - 1, edge, edge + 2, edge + 2]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        assert_eq!(tail.pixel(edge - 1, edge).unwrap(), [0; 4]);
        assert_eq!(tail.pixel(edge, edge).unwrap(), [0; 4]);
        assert_eq!(tail.pixel(edge + 1, edge).unwrap(), [111; 4]);
        assert_eq!(tail.pixel(edge + 1, edge + 1).unwrap(), [0; 4]);
        drop((view, tail, source, store));
        assert_eq!(pool.used(), 0);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn failed_row_or_final_guard_never_produces_a_selection_source() {
        let pool = ResidentBudget::new(96 * 1024 * 1024).unwrap();
        let (store, path) = store(pool.clone());
        let mut writer = Writer::new(4, 2, store.clone(), &pool).unwrap();
        writer.row(0, &[255; 4], &mut || Ok(())).unwrap();
        assert!(writer
            .row(1, &[255; 4], &mut || Err(Error::Cancelled))
            .is_err());
        assert!(writer.row(1, &[255; 4], &mut || Ok(())).is_err());
        let mut ring = [0; 4];
        let mut sums = [0; 16];
        let mut input = [0; 4];
        let mut output = [0; 4];
        let complete = Feather::new(4, 2, 0, &mut ring, &mut sums, &mut input, &mut output)
            .unwrap()
            .run(
                || Ok(()),
                |_, row| {
                    row.fill(255);
                    Ok(())
                },
                |_, _| Ok(()),
            )
            .unwrap();
        assert!(writer.finish(&complete, &mut || Ok(())).is_err());
        let mut writer = Writer::new(4, 2, store.clone(), &pool).unwrap();
        writer.row(0, &[255; 4], &mut || Ok(())).unwrap();
        writer.row(1, &[255; 4], &mut || Ok(())).unwrap();
        assert!(writer.finish(&complete, &mut || Err(Error::Stale)).is_err());
        drop(store);
        assert_eq!(pool.used(), 0);
        std::fs::remove_dir_all(path).unwrap();
    }
}
