//! Append-only native PNG pass staging. These private indices are provisional;
//! session admission, final seal and engine publication remain mandatory.
use super::prepared_png::{ColorPolicy, Complete, EncodedPng, Header, PassRow, SourceBinding};
use crate::{
    large_document::{BlockRef, Error, Reservation, ResidentBudget, ScratchBytes, Store, TileSpec},
    raster::TILE,
};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
struct Geometry {
    pass: u8,
    sx: u32,
    sy: u32,
    dx: u32,
    dy: u32,
    width: u32,
    height: u32,
}
impl Geometry {
    fn at(header: Header, pass: u8) -> Option<Self> {
        let (sx, sy, dx, dy) = match pass {
            0 => (0, 0, 1, 1),
            1 => (0, 0, 8, 8),
            2 => (4, 0, 8, 8),
            3 => (0, 4, 4, 8),
            4 => (2, 0, 4, 4),
            5 => (0, 2, 2, 4),
            6 => (1, 0, 2, 2),
            7 => (0, 1, 1, 2),
            _ => return None,
        };
        if sx >= header.width || sy >= header.height {
            return None;
        }
        Some(Self {
            pass,
            sx,
            sy,
            dx,
            dy,
            width: (header.width - sx).div_ceil(dx),
            height: (header.height - sy).div_ceil(dy),
        })
    }
    fn next(header: Header, current: u8) -> Option<Self> {
        if current == 0 {
            return None;
        }
        (current + 1..=7).find_map(|p| Self::at(header, p))
    }
    fn spec(self, key: (u32, u32), depth: u16, plain: bool) -> Result<TileSpec, String> {
        if key.0 >= self.width.div_ceil(TILE) || key.1 >= self.height.div_ceil(TILE) {
            return Err("PNG pass tile is outside its declared plane".into());
        }
        let width = if plain {
            TILE
        } else {
            (self.width - key.0 * TILE).min(TILE)
        };
        let height = if plain {
            TILE
        } else {
            (self.height - key.1 * TILE).min(TILE)
        };
        TileSpec::new(width as u16, height as u16, depth).map_err(|e| e.to_string())
    }
    fn tiles(self) -> usize {
        (self.width.div_ceil(TILE) * self.height.div_ceil(TILE)) as usize
    }
}
fn matches(a: Header, b: Header) -> bool {
    a.width == b.width
        && a.height == b.height
        && a.depth == b.depth
        && a.has_alpha == b.has_alpha
        && a.grayscale == b.grayscale
        && a.cicp == b.cicp
        && a.orientation == b.orientation
        && a.animated == b.animated
}
fn checked(header: Header) -> Result<(), String> {
    if header.width == 0
        || header.height == 0
        || header.width > 16000
        || header.height > 16000
        || ![8, 16].contains(&header.depth)
    {
        return Err("Invalid native PNG source shape".into());
    }
    Ok(())
}
fn index_charge(pool: &ResidentBudget, count: usize) -> Result<Reservation, String> {
    let bytes = count
        .checked_mul(512)
        .and_then(|v| v.checked_add(512))
        .ok_or("PNG tile index footprint overflow")?;
    pool.reserve_bytes(bytes, bytes).map_err(|e| e.to_string())
}
fn storage_check(check: &mut dyn FnMut() -> Result<(), String>) -> Result<(), Error> {
    check().map_err(|_| Error::Cancelled)
}
fn nonzero(bytes: &[u8], check: &mut dyn FnMut() -> Result<(), String>) -> Result<bool, String> {
    for part in bytes.chunks(64 * 1024) {
        check()?;
        if part.iter().any(|v| *v != 0) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// One native256-sample-row band, charged before allocation. Ordinary rows write
/// final full-stride tiles directly. Adam7 writes compact sample-plane blocks,
/// rather than seven generations of mostly empty physical tiles.
pub(crate) struct PassWriter {
    header: Header,
    geometry: Geometry,
    plain: bool,
    next_line: u32,
    band_top: u32,
    band: Option<ScratchBytes>,
    maps: [BTreeMap<(u32, u32), BlockRef>; 8],
    store: Store,
    pool: ResidentBudget,
    poisoned: bool,
    expected_source: [u8; 32],
    expected_transport: Option<[u8; 32]>,
    expected_policy: ColorPolicy,
    _index_charge: Reservation,
}
impl PassWriter {
    /// `store` must be borrowed from the caller's genuine admitted, unpublished
    /// SourceSession fragment. This kernel neither creates nor publishes files.
    pub(crate) fn new(
        header: Header,
        first: PassRow,
        store: &Store,
        encoded: &EncodedPng,
        policy: ColorPolicy,
    ) -> Result<Self, String> {
        Self::new_binding(
            header,
            first,
            store,
            &encoded.binding(&store.resident_budget())?,
            policy,
        )
    }
    pub(super) fn new_binding(
        header: Header,
        first: PassRow,
        store: &Store,
        source: &SourceBinding,
        policy: ColorPolicy,
    ) -> Result<Self, String> {
        checked(header)?;
        if matches!(policy, ColorPolicy::ToSrgb) || !source.uses_budget(&store.resident_budget()) {
            return Err(
                "PNG pass writer requires its captured native source and actual pool".into(),
            );
        }
        if ![0, 1].contains(&first.pass) {
            return Err("PNG source starts at an invalid pass".into());
        }
        let plain = first.pass == 0;
        let geometry =
            Geometry::at(header, first.pass).ok_or("PNG first pass has no source pixels")?;
        let count = if plain {
            geometry.tiles()
        } else {
            (1..=7)
                .filter_map(|p| Geometry::at(header, p))
                .map(Geometry::tiles)
                .sum()
        };
        let pool = store.resident_budget();
        let charge = index_charge(&pool, count)?;
        Ok(Self {
            header,
            geometry,
            plain,
            next_line: 0,
            band_top: 0,
            band: None,
            maps: std::array::from_fn(|_| BTreeMap::new()),
            store: store.clone(),
            pool,
            poisoned: false,
            expected_source: source.source_sha256(),
            expected_transport: source.base64_sha256(),
            expected_policy: policy,
            _index_charge: charge,
        })
    }
    pub(crate) fn push(
        &mut self,
        header: Header,
        event: PassRow,
        row: &[u8],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.poisoned {
            return Err("PNG provisional writer is poisoned".into());
        }
        self.poisoned = true;
        self.push_checked(header, event, row, check)?;
        self.poisoned = false;
        Ok(())
    }
    fn push_checked(
        &mut self,
        header: Header,
        event: PassRow,
        row: &[u8],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if !matches(self.header, header) {
            return Err("PNG source header changed during pass staging".into());
        }
        if event.pass != self.geometry.pass {
            if self.plain || self.next_line != self.geometry.height || self.band.is_some() {
                return Err("PNG source pass advanced before complete rows".into());
            }
            self.geometry = Geometry::next(self.header, self.geometry.pass)
                .ok_or("PNG source has excess passes")?;
            self.next_line = 0;
            self.band_top = 0;
        }
        let g = self.geometry;
        if event.pass != g.pass
            || self.next_line >= g.height
            || event.y != g.sy + self.next_line * g.dy
            || event.first_x != g.sx
            || event.step_x != g.dx
            || event.samples != g.width
            || row.len() != g.width as usize * 8
        {
            return Err("PNG pass event differs from its exact expected footprint".into());
        }
        let pixel_bytes = if self.header.depth == 16 { 8 } else { 4 };
        let rows = (g.height - self.band_top).min(TILE);
        if self.band.is_none() {
            let length = g.width as usize * rows as usize * pixel_bytes;
            self.band =
                Some(ScratchBytes::new(&self.pool, length, length).map_err(|e| e.to_string())?);
        }
        let offset = (self.next_line - self.band_top) as usize * g.width as usize * pixel_bytes;
        let target =
            &mut self.band.as_mut().unwrap()[offset..offset + g.width as usize * pixel_bytes];
        if self.header.depth == 16 {
            target.copy_from_slice(row);
        } else {
            for (source, target) in row.chunks_exact(8).zip(target.chunks_exact_mut(4)) {
                for c in 0..4 {
                    if source[c * 2] != source[c * 2 + 1] {
                        return Err("Native8 PNG pass contains16-bit residuals".into());
                    }
                    target[c] = source[c * 2];
                }
            }
        }
        self.next_line += 1;
        if self.next_line - self.band_top == rows {
            self.flush(check)?;
        }
        check()
    }
    fn flush(&mut self, check: &mut dyn FnMut() -> Result<(), String>) -> Result<(), String> {
        let band = self.band.take().ok_or("PNG pass band is missing")?;
        let g = self.geometry;
        let bytes_per_pixel = if self.header.depth == 16 { 8 } else { 4 };
        let rows = self.next_line - self.band_top;
        let ty = self.band_top / TILE;
        for tx in 0..g.width.div_ceil(TILE) {
            check()?;
            let key = (tx, ty);
            let spec = g.spec(key, self.header.depth, self.plain)?;
            let mut output = ScratchBytes::new(&self.pool, spec.bytes(), spec.bytes())
                .map_err(|e| e.to_string())?;
            let columns = (g.width - tx * TILE).min(TILE) as usize;
            for y in 0..rows as usize {
                check()?;
                let source = (y * g.width as usize + tx as usize * TILE as usize) * bytes_per_pixel;
                let target = y * spec.width() as usize * bytes_per_pixel;
                output[target..target + columns * bytes_per_pixel]
                    .copy_from_slice(&band[source..source + columns * bytes_per_pixel]);
            }
            if nonzero(&output, check)? {
                let reference = self
                    .store
                    .put(spec, &output, &mut || storage_check(check))
                    .map_err(|e| e.to_string())?;
                if self.maps[g.pass as usize].insert(key, reference).is_some() {
                    return Err("PNG pass tile was staged twice".into());
                }
            }
        }
        self.band_top = self.next_line;
        Ok(())
    }
    /// A complete decoder receipt is mandatory, even when every sample was zero.
    pub(crate) fn finish(self, complete: Complete) -> Result<PassSet, String> {
        if self.poisoned
            || self.band.is_some()
            || self.next_line != self.geometry.height
            || (!self.plain && Geometry::next(self.header, self.geometry.pass).is_some())
            || !matches(self.header, complete.header())
            || complete.source_sha256() != self.expected_source
            || complete.base64_sha256() != self.expected_transport
            || complete.color_policy() != self.expected_policy
        {
            return Err("PNG source cannot finish incomplete or poisoned pass staging".into());
        }
        Ok(PassSet {
            header: self.header,
            plain: self.plain,
            maps: self.maps,
            store: self.store,
            complete,
            _index_charge: self._index_charge,
        })
    }
}

pub(crate) struct PassSet {
    header: Header,
    plain: bool,
    maps: [BTreeMap<(u32, u32), BlockRef>; 8],
    store: Store,
    complete: Complete,
    _index_charge: Reservation,
}
/// Unpublished metadata owner. It is deliberately not a Surface or a document
/// admission token; the registered session must seal it and the engine must
/// validate source/revision/lease/selection before adopting anything.
pub(crate) struct TileIndex {
    blocks: BTreeMap<(u32, u32), BlockRef>,
    store: Store,
    complete: Complete,
    _index_charge: Reservation,
}
impl TileIndex {
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }
    pub(crate) fn complete(&self) -> &Complete {
        &self.complete
    }
    pub(crate) fn blocks(&self) -> &BTreeMap<(u32, u32), BlockRef> {
        &self.blocks
    }
    /// The session adapter seals the complete coordinate map before this
    /// transfer. Provisional admission remains held until Surface has acquired
    /// its permanent index charge; neither conversion publishes the fragment.
    pub(super) fn into_surface(self) -> Result<(crate::raster::Surface, Complete), String> {
        let Self {
            blocks,
            store,
            complete,
            _index_charge,
        } = self;
        let h = complete.header();
        let plane =
            crate::raster::backed::Plane::from_blocks(h.width, h.height, h.depth, store, blocks)
                .map_err(|e| e.to_string())?;
        let surface = crate::raster::Surface::from_plane(plane).map_err(|e| e.to_string())?;
        drop(_index_charge);
        Ok((surface, complete))
    }
}
impl PassSet {
    pub(crate) fn is_plain(&self) -> bool {
        self.plain
    }
    pub(crate) fn into_plain(mut self) -> Result<TileIndex, String> {
        if !self.plain {
            return Err("Adam7 sample blocks require native final-tile assembly".into());
        }
        Ok(TileIndex {
            blocks: std::mem::take(&mut self.maps[0]),
            store: self.store,
            complete: self.complete,
            _index_charge: self._index_charge,
        })
    }
    /// Final fragment is distinct from compact pass storage. No random write or
    /// sparse-resident substitute: each logical sample is copied exactly once
    /// into its complete physical256 tile, including native hidden RGB.
    pub(crate) fn merge(
        self,
        final_store: &Store,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<TileIndex, String> {
        if self.plain {
            return Err("Ordinary PNG rows already contain final physical tiles".into());
        }
        check()?;
        let pool = self.store.resident_budget();
        if !final_store.resident_budget().same_pool(&pool)
            || final_store.identity() == self.store.identity()
        {
            return Err("PNG final fragment must be distinct and share resident admission".into());
        }
        let columns = self.header.width.div_ceil(TILE);
        let rows = self.header.height.div_ceil(TILE);
        let charge = index_charge(&pool, (columns * rows) as usize)?;
        let mut blocks = BTreeMap::new();
        let bpp = if self.header.depth == 16 { 8 } else { 4 };
        let full = TileSpec::new(TILE as u16, TILE as u16, self.header.depth)
            .map_err(|e| e.to_string())?;
        for ty in 0..rows {
            for tx in 0..columns {
                check()?;
                let mut output = ScratchBytes::new(&pool, full.bytes(), full.bytes())
                    .map_err(|e| e.to_string())?;
                let left = tx * TILE;
                let top = ty * TILE;
                let right = (left + TILE).min(self.header.width);
                let bottom = (top + TILE).min(self.header.height);
                for pass in 1..=7 {
                    let Some(g) = Geometry::at(self.header, pass) else {
                        continue;
                    };
                    let x0 = left.saturating_sub(g.sx).div_ceil(g.dx);
                    let y0 = top.saturating_sub(g.sy).div_ceil(g.dy);
                    let x1 = right.saturating_sub(g.sx).div_ceil(g.dx).min(g.width);
                    let y1 = bottom.saturating_sub(g.sy).div_ceil(g.dy).min(g.height);
                    if x0 >= x1 || y0 >= y1 {
                        continue;
                    }
                    let key = (x0 / TILE, y0 / TILE);
                    //256 is divisible by every Adam7 step, so this physical tile
                    // intersects at most one compact block from each pass.
                    if (x1 - 1) / TILE != key.0 || (y1 - 1) / TILE != key.1 {
                        return Err("PNG pass footprint crosses its bounded compact tile".into());
                    }
                    let Some(reference) = self.maps[pass as usize].get(&key) else {
                        continue;
                    };
                    let spec = g.spec(key, self.header.depth, false)?;
                    if reference.spec() != spec {
                        return Err("PNG compact pass tile shape differs".into());
                    }
                    let source = self
                        .store
                        .pin(*reference, &mut || storage_check(check))
                        .map_err(|e| e.to_string())?;
                    for y in y0..y1 {
                        check()?;
                        for x in x0..x1 {
                            let sx = x % TILE;
                            let sy = y % TILE;
                            let source_at =
                                (sy as usize * spec.width() as usize + sx as usize) * bpp;
                            let target_at = ((g.sy + y * g.dy - top) as usize * TILE as usize
                                + (g.sx + x * g.dx - left) as usize)
                                * bpp;
                            output[target_at..target_at + bpp]
                                .copy_from_slice(&source.bytes()[source_at..source_at + bpp]);
                        }
                    }
                }
                if nonzero(&output, check)? {
                    let reference = final_store
                        .put(full, &output, &mut || storage_check(check))
                        .map_err(|e| e.to_string())?;
                    blocks.insert((tx, ty), reference);
                }
            }
        }
        check()?;
        Ok(TileIndex {
            blocks,
            store: final_store.clone(),
            complete: self.complete,
            _index_charge: charge,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_import::prepared_png::{self, ColorPolicy, EncodedPng, Limits};
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use std::{io::Write, path::PathBuf};
    struct Fixture {
        store: Option<Store>,
        path: PathBuf,
    }
    impl Fixture {
        fn new(pool: &ResidentBudget, disk: u64) -> Self {
            let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = parent.join(format!("pb-png-tiles-{}", uuid::Uuid::new_v4()));
            let store = Store::create(
                &path,
                pool.clone(),
                crate::large_document::Limits {
                    disk_bytes: disk,
                    max_blocks: 4096,
                    max_resident_blocks: 16,
                    manifest_bytes: 1024 * 1024,
                },
            )
            .unwrap();
            Self {
                store: Some(store),
                path,
            }
        }
        fn get(&self) -> &Store {
            self.store.as_ref().unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            drop(self.store.take());
            // Only this exclusive fixture's known file and now-empty directory;
            // no recursive cleanup, historical/source-session authority claim.
            let _ = std::fs::remove_file(self.path.join("blocks.pbt"));
            let _ = std::fs::remove_dir(&self.path);
        }
    }
    fn pixel(x: u32, y: u32, depth: u16, zero: bool) -> [u16; 4] {
        if zero {
            return [0; 4];
        }
        let value = [
            12347u16.wrapping_add(x as u16 * 31),
            33559u16.wrapping_add(y as u16 * 17),
            51237,
            if (x + y) % 13 == 0 { 0 } else { 45679 },
        ];
        if depth == 16 {
            value
        } else {
            value.map(|v| u16::from(crate::raster::project16(v)) * 257)
        }
    }
    fn chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        output.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = output.len();
        output.extend_from_slice(kind);
        output.extend_from_slice(data);
        let mut crc = !0u32;
        for &byte in &output[start..] {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ ((0u32.wrapping_sub(crc & 1)) & 0xedb88320);
            }
        }
        output.extend_from_slice(&(!crc).to_be_bytes());
    }
    fn png(width: u32, height: u32, depth: u16, adam: bool, zero: bool) -> Vec<u8> {
        let mut scan = Vec::new();
        let passes = if adam { 1..=7 } else { 0..=0 };
        let header = Header {
            width,
            height,
            depth,
            has_alpha: true,
            grayscale: false,
            cicp: None,
            orientation: image::metadata::Orientation::NoTransforms,
            animated: false,
        };
        for pass in passes {
            let Some(g) = Geometry::at(header, pass) else {
                continue;
            };
            for row in 0..g.height {
                scan.push(0);
                for sample in 0..g.width {
                    for value in pixel(g.sx + sample * g.dx, g.sy + row * g.dy, depth, zero) {
                        if depth == 16 {
                            scan.extend_from_slice(&value.to_be_bytes());
                        } else {
                            scan.push(crate::raster::project16(value));
                        }
                    }
                }
            }
        }
        let mut compressor =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        compressor.write_all(&scan).unwrap();
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = Vec::new();
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[depth as u8, 6, 0, 0, u8::from(adam)]);
        chunk(&mut bytes, b"IHDR", &header);
        chunk(&mut bytes, b"IDAT", &compressor.finish().unwrap());
        chunk(&mut bytes, b"IEND", &[]);
        bytes
    }
    fn stage(
        encoded: &EncodedPng,
        pool: &ResidentBudget,
        store: &Store,
    ) -> Result<PassSet, String> {
        let mut writer = None;
        let complete = prepared_png::decode_pass_rows(
            encoded,
            Limits::SELECTION,
            ColorPolicy::RawSelection,
            pool,
            &mut || Ok(()),
            |h, event, row| {
                if writer.is_none() {
                    writer = Some(PassWriter::new(
                        h,
                        event,
                        store,
                        encoded,
                        ColorPolicy::RawSelection,
                    )?);
                }
                writer.as_mut().unwrap().push(h, event, row, &mut || Ok(()))
            },
        )?;
        writer
            .ok_or("PNG fixture has no source rows")?
            .finish(complete)
    }
    fn assert_pixels(index: &TileIndex, width: u32, height: u32, depth: u16, zero: bool) {
        for ty in 0..height.div_ceil(TILE) {
            for tx in 0..width.div_ceil(TILE) {
                let source = index
                    .blocks()
                    .get(&(tx, ty))
                    .map(|r| index.store().pin(*r, &mut || Ok(())).unwrap());
                for y in 0..TILE {
                    for x in 0..TILE {
                        let gx = tx * TILE + x;
                        let gy = ty * TILE + y;
                        let expected = if gx < width && gy < height {
                            pixel(gx, gy, depth, zero)
                        } else {
                            [0; 4]
                        };
                        let at = (y * TILE + x) as usize * if depth == 16 { 8 } else { 4 };
                        let actual = source.as_ref().map_or([0; 4], |p| {
                            if depth == 16 {
                                std::array::from_fn(|c| {
                                    u16::from_be_bytes([
                                        p.bytes()[at + c * 2],
                                        p.bytes()[at + c * 2 + 1],
                                    ])
                                })
                            } else {
                                std::array::from_fn(|c| u16::from(p.bytes()[at + c]) * 257)
                            }
                        });
                        assert_eq!(
                            actual, expected,
                            "PNG physical tile{tx},{ty} pixel{x},{y} depth{depth}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn compact_passes_merge_exact_native_pixels_and_physical_padding_without_random_writes() {
        for depth in [8, 16] {
            for adam in [false, true] {
                let pool = ResidentBudget::new(96 * 1024 * 1024).unwrap();
                let original = png(273, 265, depth, adam, false);
                let encoded = EncodedPng::base64(
                    &STANDARD.encode(original),
                    Limits::SELECTION,
                    &pool,
                    &mut || Ok(()),
                )
                .unwrap();
                let pass_store = Fixture::new(&pool, 8 * 1024 * 1024);
                let final_store = Fixture::new(&pool, 8 * 1024 * 1024);
                let passes = stage(&encoded, &pool, pass_store.get()).unwrap();
                assert_eq!(passes.is_plain(), !adam);
                let index = if adam {
                    passes.merge(final_store.get(), &mut || Ok(())).unwrap()
                } else {
                    passes.into_plain().unwrap()
                };
                assert_pixels(&index, 273, 265, depth, false);
                let charge = index._index_charge.bytes();
                let used = pool.used();
                drop(index);
                assert_eq!(pool.used(), used - charge);
                drop(final_store);
                drop(pass_store);
                drop(encoded);
                assert_eq!(pool.used(), 0);
            }
        }
    }
    #[test]
    fn completed_zero_passes_are_known_zero_and_bad_rows_poison_the_whole_writer() {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let store = Fixture::new(&pool, 8 * 1024 * 1024);
        let encoded = EncodedPng::base64(
            &STANDARD.encode(png(17, 13, 16, true, true)),
            Limits::SELECTION,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let passes = stage(&encoded, &pool, store.get()).unwrap();
        assert!(passes.maps.iter().all(BTreeMap::is_empty));
        let final_store = Fixture::new(&pool, 8 * 1024 * 1024);
        let index = passes.merge(final_store.get(), &mut || Ok(())).unwrap();
        assert!(index.blocks().is_empty());
        assert_pixels(&index, 17, 13, 16, true);
        drop(index);
        let mut writer = None;
        assert!(prepared_png::decode_pass_rows(
            &encoded,
            Limits::SELECTION,
            ColorPolicy::RawSelection,
            &pool,
            &mut || Ok(()),
            |h, event, row| {
                let mut candidate =
                    PassWriter::new(h, event, store.get(), &encoded, ColorPolicy::RawSelection)?;
                let mut bad = event;
                bad.samples += 1;
                assert!(candidate.push(h, bad, row, &mut || Ok(())).is_err());
                assert!(candidate.push(h, event, row, &mut || Ok(())).is_err());
                writer = Some(candidate);
                Err("Stop poisoned fixture".into())
            }
        )
        .is_err());
        drop(writer);
        drop(encoded);
        drop(final_store);
        drop(store);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn disk_refusal_and_merge_cancel_cannot_return_a_complete_native_index() {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let encoded = EncodedPng::base64(
            &STANDARD.encode(png(17, 13, 16, true, false)),
            Limits::SELECTION,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let refused = Fixture::new(&pool, 24);
        assert!(stage(&encoded, &pool, refused.get()).is_err());
        drop(refused);
        let source = Fixture::new(&pool, 8 * 1024 * 1024);
        let target = Fixture::new(&pool, 8 * 1024 * 1024);
        let passes = stage(&encoded, &pool, source.get()).unwrap();
        assert!(passes
            .merge(target.get(), &mut || Err("Cancelled final assembly".into()))
            .is_err());
        drop(source);
        drop(target);
        drop(encoded);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn source_and_color_policy_receipts_cannot_be_swapped_after_complete_pass_rows() {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let store = Fixture::new(&pool, 8 * 1024 * 1024);
        let first = EncodedPng::base64(
            &STANDARD.encode(png(17, 13, 16, true, false)),
            Limits::SELECTION,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let other = EncodedPng::base64(
            &STANDARD.encode(png(17, 13, 16, true, true)),
            Limits::SELECTION,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        for mismatch_policy in [false, true] {
            let mut writer = None;
            let original = prepared_png::decode_pass_rows(
                &first,
                Limits::SELECTION,
                ColorPolicy::RawSelection,
                &pool,
                &mut || Ok(()),
                |h, event, row| {
                    if writer.is_none() {
                        writer = Some(PassWriter::new(
                            h,
                            event,
                            store.get(),
                            &first,
                            ColorPolicy::RawSelection,
                        )?);
                    }
                    writer.as_mut().unwrap().push(h, event, row, &mut || Ok(()))
                },
            )
            .unwrap();
            let wrong = prepared_png::decode_pass_rows(
                if mismatch_policy { &first } else { &other },
                Limits::SELECTION,
                if mismatch_policy {
                    ColorPolicy::Raw
                } else {
                    ColorPolicy::RawSelection
                },
                &pool,
                &mut || Ok(()),
                |_, _, _| Ok(()),
            )
            .unwrap();
            assert!(writer.unwrap().finish(wrong).is_err());
            drop(original);
        }
        drop(first);
        drop(other);
        drop(store);
        assert_eq!(pool.used(), 0);
    }
}
