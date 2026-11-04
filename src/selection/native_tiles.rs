//! Sequential native tile staging; completion grants no document mutation.
use crate::{
    large_document::{BlockRef, Error, Reservation, Result, ScratchBytes, Store, TileSpec},
    raster::{backed::Plane, surface::Surface, TILE},
};
use std::collections::BTreeMap;

pub(crate) struct Writer {
    width: u32,
    height: u32,
    depth: u16,
    columns: u32,
    rows: u32,
    next: u32,
    blocks: BTreeMap<(u32, u32), BlockRef>,
    store: Store,
    // Conservative provisional BTree index envelope. The Surface constructor
    // must retain its permanent descriptor charge before this guard drops.
    _index: Reservation,
    failed: bool,
}

pub(super) struct Rows {
    writer: Writer,
    next: u32,
    stripe: ScratchBytes,
    failed: bool,
}

impl Rows {
    pub fn new(width: u32, height: u32, depth: u16, store: Store) -> Result<Self> {
        let writer = Writer::new(width, height, depth, store)?;
        let bytes = writer.columns as usize * (TILE * TILE * 4) as usize * usize::from(depth / 8);
        let stripe = ScratchBytes::new(&writer.store.resident_budget(), bytes, bytes)?;
        Ok(Self {
            writer,
            next: 0,
            stripe,
            failed: false,
        })
    }
    /// Decode owners supply canonical native16 rows; native8 samples must be
    /// exact multiples257 and are stored at their original8bit depth.
    pub fn row(
        &mut self,
        y: u32,
        rgba16be: &[u8],
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        let invalid = self.failed
            || y != self.next
            || y >= self.writer.height
            || rgba16be.len() != self.writer.width as usize * 8;
        self.failed = true;
        if invalid {
            return Err(Error::Invalid(
                "Decoded native rows differ from their admitted source",
            ));
        }
        check()?;
        let stride = usize::from(self.writer.depth / 8) * 4;
        let tile_bytes = (TILE * TILE) as usize * stride;
        for (x, pixel) in rgba16be.chunks_exact(8).enumerate() {
            if x % 4096 == 0 {
                check()?;
            }
            let at = x / TILE as usize * tile_bytes
                + ((y % TILE) as usize * TILE as usize + x % TILE as usize) * stride;
            if self.writer.depth == 16 {
                self.stripe[at..at + 8].copy_from_slice(pixel);
            } else {
                for (channel, pair) in pixel.chunks_exact(2).enumerate() {
                    let word = u16::from_be_bytes([pair[0], pair[1]]);
                    if word % 257 != 0 {
                        return Err(Error::Invalid(
                            "Decoded8bit row contains nonnative channel bits",
                        ));
                    }
                    self.stripe[at + channel] = (word / 257) as u8;
                }
            }
        }
        if (y + 1) % TILE == 0 || y + 1 == self.writer.height {
            for x in 0..self.writer.columns {
                let at = x as usize * tile_bytes;
                self.writer
                    .tile(x, y / TILE, &self.stripe[at..at + tile_bytes], check)?;
            }
            self.stripe.fill(0);
        }
        check()?;
        self.next += 1;
        self.failed = false;
        Ok(())
    }
    pub fn finish(self, check: &mut dyn FnMut() -> Result<()>) -> Result<Surface> {
        if self.failed || self.next != self.writer.height {
            return Err(Error::Invalid("Decoded native rows are incomplete"));
        }
        self.writer.finish(check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        large_document::{DiskBudget, Limits, ResidentBudget},
        raster::backed::Region,
    };
    #[test]
    fn decoded_native_rows_keep_every_channel_at_original_depth_and_refuse_partial_or_non_native_input(
    ) {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
            let path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "peerbrush-native-row-output-{}",
                    crate::engine::id()
                ));
            let store = Store::create_with_disk_budget(
                &path,
                pool.clone(),
                DiskBudget::new(32 * 1024 * 1024, 4).unwrap(),
                Limits {
                    disk_bytes: 32 * 1024 * 1024,
                    max_blocks: 64,
                    max_resident_blocks: 8,
                    manifest_bytes: 1024 * 1024,
                },
            )
            .unwrap();
            let (w, h) = (257u32, 259u32);
            let pixel = |x: u32, y: u32| {
                std::array::from_fn::<_, 4, _>(|c| {
                    if depth == 8 {
                        ((x * 17 + y * 7 + c as u32 * 37) % 256) as u16 * 257
                    } else {
                        (x * 1931 + y * 937 + c as u32 * 16411) as u16
                    }
                })
            };
            let mut writer = Rows::new(w, h, depth, store.clone()).unwrap();
            let mut row = vec![0; w as usize * 8];
            for y in 0..h {
                for x in 0..w {
                    for (channel, word) in pixel(x, y).iter().enumerate() {
                        let at = x as usize * 8 + channel * 2;
                        row[at..at + 2].copy_from_slice(&word.to_be_bytes());
                    }
                }
                writer.row(y, &row, &mut || Ok(())).unwrap();
            }
            let actual = writer.finish(&mut || Ok(())).unwrap();
            assert_eq!(actual.depth(), depth);
            for top in (0..h).step_by(32) {
                let view = actual
                    .prepare_region(
                        Region::new([0, top as i32, w as i32, (top + 32).min(h) as i32]).unwrap(),
                        &mut || Ok(()),
                    )
                    .unwrap();
                for y in top..(top + 32).min(h) {
                    for x in 0..w {
                        assert_eq!(view.pixel16(x as i32, y as i32).unwrap(), pixel(x, y));
                    }
                }
            }
            let incomplete = Rows::new(w, h, depth, store.clone()).unwrap();
            assert!(incomplete.finish(&mut || Ok(())).is_err());
            if depth == 8 {
                let mut invalid = Rows::new(w, h, depth, store.clone()).unwrap();
                row.fill(0);
                row[1] = 1;
                assert!(invalid.row(0, &row, &mut || Ok(())).is_err());
                assert!(invalid
                    .row(0, &vec![0; w as usize * 8], &mut || Ok(()))
                    .is_err());
                assert!(invalid.finish(&mut || Ok(())).is_err());
            }
            drop(actual);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}

impl Writer {
    pub fn new(width: u32, height: u32, depth: u16, store: Store) -> Result<Self> {
        if width == 0 || height == 0 || width > 16000 || height > 16000 || ![8, 16].contains(&depth)
        {
            return Err(Error::Invalid("Invalid admitted native selection output"));
        }
        let columns = width.div_ceil(TILE);
        let rows = height.div_ceil(TILE);
        let index = store
            .resident_budget()
            .reserve_bytes(columns as usize * rows as usize * 2048, 8 * 1024 * 1024)?;
        Ok(Self {
            width,
            height,
            depth,
            columns,
            rows,
            next: 0,
            blocks: BTreeMap::new(),
            store,
            _index: index,
            failed: false,
        })
    }

    pub fn tile(
        &mut self,
        x: u32,
        y: u32,
        bytes: &[u8],
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        let spec = TileSpec::new(TILE as u16, TILE as u16, self.depth)?;
        let invalid = self.failed
            || self.next >= self.columns * self.rows
            || x != self.next % self.columns
            || y != self.next / self.columns
            || bytes.len() != spec.bytes();
        self.failed = true;
        if invalid {
            return Err(Error::Invalid(
                "Native selection tiles are incomplete or out of order",
            ));
        }
        check()?;
        let stride = usize::from(self.depth / 8) * 4;
        for yy in 0..TILE {
            if yy % 32 == 0 {
                check()?;
            }
            for xx in 0..TILE {
                if x * TILE + xx >= self.width || y * TILE + yy >= self.height {
                    let at = (yy * TILE + xx) as usize * stride;
                    if bytes[at..at + stride].iter().any(|v| *v != 0) {
                        return Err(Error::Invalid("Native selection edge padding is nonzero"));
                    }
                }
            }
        }
        if bytes.iter().any(|v| *v != 0) {
            self.blocks
                .insert((x, y), self.store.put(spec, bytes, check)?);
        }
        check()?;
        self.next += 1;
        self.failed = false;
        Ok(())
    }

    pub fn finish(self, check: &mut dyn FnMut() -> Result<()>) -> Result<Surface> {
        if self.failed || self.next != self.columns * self.rows {
            return Err(Error::Invalid("Native selection output did not complete"));
        }
        check()?;
        for block in self.blocks.values() {
            check()?;
            drop(self.store.pin(*block, check)?);
        }
        let source = if self.blocks.is_empty() {
            Surface::zero(self.width, self.height, self.depth)?
        } else {
            Surface::from_plane(Plane::from_blocks(
                self.width,
                self.height,
                self.depth,
                self.store,
                self.blocks,
            )?)?
        };
        check()?;
        Ok(source)
    }
}
