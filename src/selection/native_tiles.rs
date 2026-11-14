//! Sequential native tile staging; completion grants no document mutation.
use crate::{
    large_document::{BlockRef, Error, Reservation, Result, ScratchBytes, Store, TileSpec},
    raster::{backed::Plane, surface::Surface, TILE},
};
use std::collections::BTreeMap;
use std::ops::Range;

const MAX_COLUMNS: usize = 16000u32.div_ceil(TILE) as usize;

pub(crate) struct Writer {
    width: u32,
    height: u32,
    depth: u16,
    columns: u32,
    rows: u32,
    next: u32,
    bottom_up: bool,
    blocks: BTreeMap<(u32, u32), BlockRef>,
    black: [Option<BlockRef>; 4],
    store: Store,
    // Conservative provisional BTree index envelope. The Surface constructor
    // must retain its permanent descriptor charge before this guard drops.
    _index: Reservation,
    failed: bool,
}

pub(crate) struct Rows {
    writer: Writer,
    next: u32,
    stripe: ScratchBytes,
    dirty: [bool; MAX_COLUMNS],
    failed: bool,
}

impl Rows {
    pub fn new(width: u32, height: u32, depth: u16, store: Store) -> Result<Self> {
        Self::from_scanlines(width, height, depth, store, false)
    }

    /// Accept sequential decoder rows and write each stripe at its final height.
    pub fn from_scanlines(
        width: u32,
        height: u32,
        depth: u16,
        store: Store,
        bottom_up: bool,
    ) -> Result<Self> {
        let mut writer = Writer::new(width, height, depth, store)?;
        writer.bottom_up = bottom_up;
        let bytes = writer.columns as usize * (TILE * TILE * 4) as usize * usize::from(depth / 8);
        let stripe = ScratchBytes::new(&writer.store.resident_budget(), bytes, bytes)?;
        Ok(Self {
            writer,
            next: 0,
            stripe,
            dirty: [false; MAX_COLUMNS],
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
        let width = self.writer.width as usize;
        self.row_spans(y, rgba16be, 8, std::iter::once(0..width), check)
    }

    /// Accept exact RGBA8 from a decoder or display guide.
    pub(crate) fn row8(
        &mut self,
        y: u32,
        rgba8: &[u8],
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        let width = self.writer.width as usize;
        self.row_spans(y, rgba8, 4, std::iter::once(0..width), check)
    }

    /// Columns outside the supplied spans are zero. Each span uses row-local x coordinates.
    pub(crate) fn row_spans(
        &mut self,
        y: u32,
        row: &[u8],
        input_stride: usize,
        spans: impl IntoIterator<Item = Range<usize>>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        let stride = usize::from(self.writer.depth / 8) * 4;
        let invalid = self.failed
            || y != self.next
            || y >= self.writer.height
            || !matches!(input_stride, 4 | 8)
            || input_stride < stride
            || row.len() != self.writer.width as usize * input_stride;
        self.failed = true;
        if invalid {
            return Err(Error::Invalid(
                "Decoded native rows differ from their admitted source",
            ));
        }
        check()?;
        let flushes = self.row_flushes(y);
        let y = self.writer.output_row(y, self.writer.height);
        let tile_bytes = (TILE * TILE) as usize * stride;
        let zero = [0; TILE as usize * 8];
        let tile = TILE as usize;
        let mut previous = 0;
        for Range { mut start, end } in spans {
            if start < previous || start >= end || end > self.writer.width as usize {
                return Err(Error::Invalid("Native row spans are outside their admitted source"));
            }
            previous = end;
            while start < end {
                let column = start / tile;
                let right = end.min((column + 1) * tile);
                if column * tile % 4096 == 0 {
                    check()?;
                }
                let samples = &row[start * input_stride..right * input_stride];
                if samples != &zero[..samples.len()] {
                    self.dirty[column] = true;
                    let at = column * tile_bytes + ((y % TILE) as usize * tile + start % tile) * stride;
                    if input_stride == stride {
                        self.stripe[at..at + samples.len()].copy_from_slice(samples);
                    } else {
                        for (channel, pair) in samples.chunks_exact(2).enumerate() {
                            let word = u16::from_be_bytes([pair[0], pair[1]]);
                            if word % 257 != 0 {
                                return Err(Error::Invalid("Decoded8bit row contains nonnative channel bits"));
                            }
                            self.stripe[at + channel] = (word / 257) as u8;
                        }
                    }
                }
                start = right;
            }
        }
        if flushes {
            self.flush(y, check)?;
        }
        check()?;
        self.next += 1;
        self.failed = false;
        Ok(())
    }

    /// Whether the next valid scanline will write staged tiles.
    pub(crate) fn row_flushes(&self, y: u32) -> bool {
        y < self.writer.height
            && (y + 1 == self.writer.height
                || self.stripe_remaining(self.writer.output_row(y, self.writer.height)) == 1)
    }
    /// Advance rows proven zero without materializing or sampling their pixels.
    pub fn zero_rows(&mut self, count: u32, check: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        let end = self.next.checked_add(count);
        let invalid = self.failed || count == 0 || end.is_none_or(|end| end > self.writer.height);
        self.failed = true;
        if invalid {
            return Err(Error::Invalid("Zero native rows exceed their admitted source"));
        }
        let end = end.unwrap();
        while self.next < end {
            check()?;
            let y = self.writer.output_row(self.next, self.writer.height);
            let stripe_end = self.next + self.stripe_remaining(y);
            let next = end.min(stripe_end);
            if next == stripe_end || next == self.writer.height {
                self.flush(y, check)?;
            }
            self.next = next;
        }
        check()?;
        self.failed = false;
        Ok(())
    }

    /// Advance over scanlines already proven zero by the decoder.
    pub(crate) fn zero_until(&mut self, end: u32, check: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        if end == self.next && !self.failed {
            return Ok(());
        }
        self.zero_rows(end.saturating_sub(self.next), check)
    }

    fn stripe_remaining(&self, y: u32) -> u32 {
        let offset = y % TILE;
        if self.writer.bottom_up {
            offset + 1
        } else {
            TILE - offset
        }
    }
    fn flush(&mut self, y: u32, check: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        let tile_bytes = (TILE * TILE * 4) as usize * usize::from(self.writer.depth / 8);
        let mut blank = 0;
        for (x, dirty) in self.dirty[..self.writer.columns as usize].iter_mut().enumerate() {
            if !*dirty {
                blank += 1;
                continue;
            }
            if blank != 0 {
                self.writer.zero_tiles(blank, check)?;
                blank = 0;
            }
            let at = x * tile_bytes;
            let tile = &mut self.stripe[at..at + tile_bytes];
            self.writer.tile(x as u32, y / TILE, tile, check)?;
            tile.fill(0);
            *dirty = false;
        }
        if blank != 0 {
            self.writer.zero_tiles(blank, check)?;
        }
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
            {
                let mut wide = Rows::new(16000, 2, depth, store.clone()).unwrap();
                let mut values = vec![0; 16000 * 8];
                for y in 0..2 {
                    for x in 0..16000 {
                        for (channel, word) in pixel(x % TILE, y).iter().enumerate() {
                            let at = x as usize * 8 + channel * 2;
                            values[at..at + 2].copy_from_slice(&word.to_be_bytes());
                        }
                    }
                    wide.row(y, &values, &mut || Ok(())).unwrap();
                }
                let wide = wide.finish(&mut || Ok(())).unwrap();
                let view = wide.prepare_region(Region::new([0, 0, 16000, 2]).unwrap(), &mut || Ok(())).unwrap();
                for y in 0..2 {
                    for x in 0..16000 {
                        assert_eq!(view.pixel16(x, y).unwrap(), pixel(x as u32 % TILE, y as u32));
                    }
                }
                let mut cancelled = Rows::new(16000, 1, depth, store.clone()).unwrap();
                let mut calls = 0;
                assert!(cancelled.row(0, &values, &mut || {
                    calls += 1;
                    if calls == 3 { Err(Error::Cancelled) } else { Ok(()) }
                }).is_err());
                assert!(cancelled.finish(&mut || Ok(())).is_err());
            }
            let stride = usize::from(depth / 8) * 4;
            for (x, y, offset) in [
                (1, 0, stride),
                (0, 1, 3 * TILE as usize * stride),
                (1, 1, (2 * TILE as usize + 1) * stride),
                (1, 1, 3 * TILE as usize * stride),
            ] {
                let mut edge = Writer::new(w, h, depth, store.clone()).unwrap();
                edge.zero_tiles(y * 2 + x, &mut || Ok(())).unwrap();
                let mut bad = vec![0; (TILE * TILE) as usize * stride];
                bad[offset] = 1;
                let error = edge.tile(x, y, &bad, &mut || Ok(())).unwrap_err();
                assert!(error.to_string().contains("edge padding"));
                assert!(edge.finish(&mut || Ok(())).is_err());
            }
            let mut empty = Rows::new(w, h, depth, store.clone()).unwrap();
            empty.zero_rows(h, &mut || Ok(())).unwrap();
            assert_eq!(empty.finish(&mut || Ok(())).unwrap().physical_tiles().count(), 0);
            let sparse_x = |y| if y == 512 { w - 1 } else { 0 };
            let sparse_pixel = |x, y| {
                let mut rgba = pixel(x, y);
                if y == 512 {
                    rgba[3] = 0;
                }
                rgba
            };
            let mut display_row = vec![0; w as usize * 4];
            for bottom_up in [false, true] {
                let mut sparse = Rows::from_scanlines(w, 1027, depth, store.clone(), bottom_up).unwrap();
                let mut next = 0;
                for y in [1, 512, 1026] {
                    sparse.zero_rows(y - next, &mut || Ok(())).unwrap();
                    // The staging buffer can retain bytes outside the current span.
                    row.fill(255);
                    let x = sparse_x(y);
                    for (channel, word) in sparse_pixel(x, y).iter().enumerate() {
                        let at = x as usize * 8 + channel * 2;
                        row[at..at + 2].copy_from_slice(&word.to_be_bytes());
                    }
                    let start = x as usize;
                    if depth == 8 {
                        for (value, word) in display_row.iter_mut().zip(row.chunks_exact(2)) {
                            *value = word[0];
                        }
                        sparse.row_spans(y, &display_row, 4, [start..start + 1], &mut || Ok(())).unwrap();
                    } else {
                        sparse.row_spans(y, &row, 8, [start..start + 1], &mut || Ok(())).unwrap();
                    }
                    next = y + 1;
                }
                let sparse = sparse.finish(&mut || Ok(())).unwrap();
                assert_eq!(sparse.physical_tiles().count(), 3);
                for top in (0..1027).step_by(32) {
                    let view = sparse.prepare_region(
                        Region::new([0, top, w as i32, (top + 32).min(1027)]).unwrap(),
                        &mut || Ok(()),
                    ).unwrap();
                    for y in top..(top + 32).min(1027) {
                        let scan_y = if bottom_up { 1026 - y } else { y } as u32;
                        for x in 0..w {
                            let expected = if [1, 512, 1026].contains(&scan_y) && x == sparse_x(scan_y) {
                                sparse_pixel(x, scan_y)
                            } else {
                                [0; 4]
                            };
                            assert_eq!(view.pixel16(x as i32, y).unwrap(), expected);
                        }
                    }
                }
            }
            let mut stopped = Rows::new(w, 1027, depth, store.clone()).unwrap();
            let mut calls = 0;
            assert!(stopped.zero_rows(1027, &mut || {
                calls += 1;
                if calls > 2 { Err(Error::Cancelled) } else { Ok(()) }
            }).is_err());
            assert!(stopped.row(256, &row, &mut || Ok(())).is_err());
            assert!(stopped.finish(&mut || Ok(())).is_err());
            let mut overflow = Rows::new(w, 1027, depth, store.clone()).unwrap();
            assert!(overflow.zero_rows(1028, &mut || Ok(())).is_err());
            assert!(overflow.finish(&mut || Ok(())).is_err());
            if depth == 8 {
                for offset in [1, TILE as usize * 8 + 1] {
                    let mut invalid = Rows::new(w, h, depth, store.clone()).unwrap();
                    row.fill(0);
                    row[offset] = 1;
                    assert!(invalid.row(0, &row, &mut || Ok(())).is_err());
                    assert!(invalid
                        .row(0, &vec![0; w as usize * 8], &mut || Ok(()))
                        .is_err());
                    assert!(invalid.finish(&mut || Ok(())).is_err());
                }
            } else {
                let mut wrong_depth = Rows::new(w, h, depth, store.clone()).unwrap();
                assert!(wrong_depth.row8(0, &display_row, &mut || Ok(())).is_err());
                assert!(wrong_depth.finish(&mut || Ok(())).is_err());
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
            bottom_up: false,
            blocks: BTreeMap::new(),
            black: [None; 4],
            store,
            _index: index,
            failed: false,
        })
    }

    fn output_row(&self, scan_row: u32, rows: u32) -> u32 {
        if self.bottom_up {
            rows - 1 - scan_row
        } else {
            scan_row
        }
    }

    pub(super) fn zero_tiles(&mut self, count: u32, check: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        let invalid = self.failed || count == 0 || count > self.columns * self.rows - self.next;
        self.failed = true;
        if invalid {
            return Err(Error::Invalid("Zero native tiles exceed their admitted source"));
        }
        check()?;
        self.next += count;
        self.failed = false;
        Ok(())
    }

    pub(super) fn opaque_black_tile(
        &mut self, x: u32, y: u32, scratch: &mut [u8], check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        if self.failed || self.next >= self.columns * self.rows
            || x != self.next % self.columns || y != self.output_row(self.next / self.columns, self.rows)
        {
            self.failed = true;
            return Err(Error::Invalid("Native selection tiles are incomplete or out of order"));
        }
        let width = (self.width - x * TILE).min(TILE) as usize;
        let height = (self.height - y * TILE).min(TILE) as usize;
        // A frame has at most four distinct combinations of edge padding.
        let edge = usize::from(width < TILE as usize) + 2 * usize::from(height < TILE as usize);
        if let Some(reference) = self.black[edge] {
            self.failed = true;
            check()?;
            self.blocks.insert((x, y), reference);
            self.next += 1;
            self.failed = false;
            return Ok(());
        }
        let sample = usize::from(self.depth / 8);
        let stride = 4 * sample;
        if scratch.len() != (TILE * TILE) as usize * stride {
            self.failed = true;
            return Err(Error::Invalid("Opaque mask tile differs from its admitted depth"));
        }
        scratch.fill(0);
        for row in scratch.chunks_exact_mut(TILE as usize * stride).take(height) {
            for pixel in row.chunks_exact_mut(stride).take(width) {
                pixel[3 * sample..].fill(255);
            }
        }
        self.tile(x, y, scratch, check)?;
        self.black[edge] = self.blocks.get(&(x, y)).copied();
        Ok(())
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
            || y != self.output_row(self.next / self.columns, self.rows)
            || bytes.len() != spec.bytes();
        self.failed = true;
        if invalid {
            return Err(Error::Invalid(
                "Native selection tiles are incomplete or out of order",
            ));
        }
        check()?;
        let stride = usize::from(self.depth / 8) * 4;
        let width = (self.width - x * TILE).min(TILE) as usize;
        let height = (self.height - y * TILE).min(TILE) as usize;
        // Complete tiles have no padding. Edge tiles validate only the row
        // suffixes and bottom rows outside their logical image rectangle.
        let first = if width == TILE as usize { height } else { 0 };
        for yy in first..TILE as usize {
            if yy % 32 == 0 {
                check()?;
            }
            let start = yy * TILE as usize * stride + if yy < height { width * stride } else { 0 };
            let end = (yy + 1) * TILE as usize * stride;
            if bytes[start..end].iter().any(|v| *v != 0) {
                return Err(Error::Invalid("Native selection edge padding is nonzero"));
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
        let mut verified_black = [false; 4];
        for block in self.blocks.values() {
            check()?;
            if let Some(index) = self.black.iter().position(|reference| reference.as_ref() == Some(block)) {
                if verified_black[index] {
                    continue;
                }
                verified_black[index] = true;
            }
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
