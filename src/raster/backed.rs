//! Explicit native pixel-plane backing. This is not a serialized document or
//! permission to evict the resident raster used by existing infallible getters.
use super::{check_size, project16, Pixel, Pixel16, Raster, TILE};
use crate::large_document::{BlockRef, Error, PinnedTile, Result, ScratchBytes, Store, TileSpec};
use std::collections::BTreeMap;

pub const MAX_REGION_PIXELS: usize = 128 * 1024;
const MAX_REGION_TILES: usize = 128;
const MAX_PLANE_TILES: usize = 4096;

/// A bounded sampling window, including explicitly requested transparent border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region([i32; 4]);

impl Region {
    pub fn new(bounds: [i32; 4]) -> Result<Self> {
        let [left, top, right, bottom] = bounds.map(i64::from);
        let pixels = (right - left).checked_mul(bottom - top);
        if left >= right
            || top >= bottom
            || bounds.iter().any(|v| v.unsigned_abs() > 100_000)
            || pixels.is_none_or(|n| n <= 0 || n as u64 > MAX_REGION_PIXELS as u64)
        {
            return Err(Error::Invalid(
                "Native region exceeds its explicit sampling budget",
            ));
        }
        Ok(Self(bounds))
    }

    pub fn bounds(self) -> [i32; 4] {
        self.0
    }
    pub fn pixels(self) -> usize {
        (self.0[2] - self.0[0]) as usize * (self.0[3] - self.0[1]) as usize
    }
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.0[0] && y >= self.0[1] && x < self.0[2] && y < self.0[3]
    }
}

/// Immutable plane descriptors are cheap to share across snapshots. Every
/// present tile is read fallibly before a PinnedRegion can be constructed.
#[derive(Clone)]
pub struct Plane {
    width: u32,
    height: u32,
    depth: u16,
    store: Store,
    blocks: BTreeMap<(u32, u32), BlockRef>,
}

impl Plane {
    /// Existing resident sources retain their current admission and complete
    /// padded tiles. Retained originals remain the caller's separate source;
    /// this descriptor represents only the raster's current pixel plane.
    pub fn from_resident(
        raster: &Raster,
        store: Store,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        check_size(raster.width, raster.height)
            .map_err(|_| Error::Invalid("Resident raster admission differs"))?;
        raster
            .validate_layout()
            .map_err(|_| Error::Invalid("Resident tile layout differs"))?;
        let spec = TileSpec::new(TILE as u16, TILE as u16, raster.depth)?;
        let mut blocks = BTreeMap::new();
        if raster.depth == 16 {
            for (&coordinate, words) in &raster.samples16 {
                check()?;
                let mut bytes =
                    ScratchBytes::new(&store.resident_budget(), spec.bytes(), spec.bytes())?;
                for (index, (&word, output)) in
                    words.iter().zip(bytes.chunks_exact_mut(2)).enumerate()
                {
                    if index % 32768 == 0 {
                        check()?;
                    }
                    output.copy_from_slice(&word.to_be_bytes());
                }
                blocks.insert(coordinate, store.put(spec, &bytes, check)?);
            }
        } else {
            for (&coordinate, bytes) in &raster.tiles {
                check()?;
                blocks.insert(coordinate, store.put(spec, bytes, check)?);
            }
        }
        check()?;
        Self::from_blocks(raster.width, raster.height, raster.depth, store, blocks)
    }

    /// This is an index, not imported-source authority. Semantic codec checks
    /// and completion tokens must precede document adoption. Present missing or
    /// corrupt blocks always fail pinning; only absent descriptors mean zero.
    pub fn from_blocks(
        width: u32,
        height: u32,
        depth: u16,
        store: Store,
        blocks: BTreeMap<(u32, u32), BlockRef>,
    ) -> Result<Self> {
        // Component storage can represent a future16K plane; live document
        // entry points retain check_size until every consumer is migrated.
        if width == 0
            || height == 0
            || width > 16000
            || height > 16000
            || ![8, 16].contains(&depth)
            || blocks.len() > MAX_PLANE_TILES
            || blocks.iter().any(|(&(x, y), block)| {
                x >= width.div_ceil(TILE)
                    || y >= height.div_ceil(TILE)
                    || block.spec().width() != TILE as u16
                    || block.spec().height() != TILE as u16
                    || block.spec().depth() != depth
            })
        {
            return Err(Error::Invalid("Invalid bounded native plane index"));
        }
        Ok(Self {
            width,
            height,
            depth,
            store,
            blocks,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn depth(&self) -> u16 {
        self.depth
    }
    pub fn store(&self) -> &Store {
        &self.store
    }
    pub fn blocks(&self) -> &BTreeMap<(u32, u32), BlockRef> {
        &self.blocks
    }

    pub fn pin(
        &self,
        region: Region,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<PinnedRegion> {
        check()?;
        let [left, top, right, bottom] = region.0;
        let x0 = left.max(0).min(self.width as i32) as u32 / TILE;
        let y0 = top.max(0).min(self.height as i32) as u32 / TILE;
        let x1 = (right.max(0).min(self.width as i32) as u32).div_ceil(TILE);
        let y1 = (bottom.max(0).min(self.height as i32) as u32).div_ceil(TILE);
        if u64::from(x1.saturating_sub(x0)) * u64::from(y1.saturating_sub(y0))
            > MAX_REGION_TILES as u64
        {
            return Err(Error::Budget);
        }
        let mut tiles = BTreeMap::new();
        for y in y0..y1 {
            for x in x0..x1 {
                check()?;
                if let Some(reference) = self.blocks.get(&(x, y)) {
                    let tile = self.store.pin(*reference, check)?;
                    tiles.insert((x, y), tile);
                }
            }
        }
        check()?;
        Ok(PinnedRegion {
            width: self.width,
            height: self.height,
            depth: self.depth,
            region,
            tiles,
        })
    }
}

/// No disk access occurs while sampling this view. The immutable source pins
/// retain their byte charges until the final sharing view releases them.
pub struct PinnedRegion {
    width: u32,
    height: u32,
    depth: u16,
    region: Region,
    tiles: BTreeMap<(u32, u32), PinnedTile>,
}

impl PinnedRegion {
    pub(crate) fn from_pins(
        width: u32,
        height: u32,
        depth: u16,
        region: Region,
        tiles: BTreeMap<(u32, u32), PinnedTile>,
    ) -> Result<Self> {
        if width == 0
            || height == 0
            || width > 16000
            || height > 16000
            || ![8, 16].contains(&depth)
            || tiles.len() > 128
            || tiles.iter().any(|(&(x, y), tile)| {
                x >= width.div_ceil(TILE)
                    || y >= height.div_ceil(TILE)
                    || tile.spec().width() != TILE as u16
                    || tile.spec().height() != TILE as u16
                    || tile.spec().depth() != depth
            })
        {
            return Err(Error::Invalid("Invalid resolved native region pins"));
        }
        Ok(Self {
            width,
            height,
            depth,
            region,
            tiles,
        })
    }
    pub fn pixel16(&self, x: i32, y: i32) -> Result<Pixel16> {
        if !self.region.contains(x, y) {
            return Err(Error::Invalid(
                "Sample is outside the successfully pinned region",
            ));
        }
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return Ok([0; 4]);
        }
        let (x, y) = (x as u32, y as u32);
        let Some(tile) = self.tiles.get(&(x / TILE, y / TILE)) else {
            return Ok([0; 4]);
        };
        let index = (((y % TILE) * TILE + x % TILE) * 4) as usize;
        if self.depth == 16 {
            Ok(std::array::from_fn(|channel| {
                let at = (index + channel) * 2;
                u16::from_be_bytes([tile.bytes()[at], tile.bytes()[at + 1]])
            }))
        } else {
            Ok(std::array::from_fn(|channel| {
                u16::from(tile.bytes()[index + channel]) * 257
            }))
        }
    }

    pub fn pixel(&self, x: i32, y: i32) -> Result<Pixel> {
        self.pixel16(x, y).map(|p| p.map(project16))
    }

    /// Caller owns the bounded row buffer. Native16 output remains big endian.
    pub fn copy_row(&self, y: i32, output: &mut [u8]) -> Result<()> {
        let width = (self.region.0[2] - self.region.0[0]) as usize;
        let stride = if self.depth == 16 { 8 } else { 4 };
        if output.len() != width * stride || y < self.region.0[1] || y >= self.region.0[3] {
            return Err(Error::Invalid(
                "Native row does not match its pinned window/depth",
            ));
        }
        for (x, out) in (self.region.0[0]..self.region.0[2]).zip(output.chunks_exact_mut(stride)) {
            let pixel = self.pixel16(x, y)?;
            if self.depth == 16 {
                for (word, bytes) in pixel.into_iter().zip(out.chunks_exact_mut(2)) {
                    bytes.copy_from_slice(&word.to_be_bytes());
                }
            } else {
                out.copy_from_slice(&pixel.map(project16));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_document::{Limits, ResidentBudget};

    fn make_store(bytes: usize) -> (Store, std::path::PathBuf) {
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-plane-{}", crate::engine::id()));
        assert_eq!(
            path.parent().unwrap().canonicalize().unwrap(),
            path.parent().unwrap()
        );
        let store = Store::create(
            &path,
            ResidentBudget::new(bytes).unwrap(),
            Limits {
                disk_bytes: 16 * 1024 * 1024,
                max_blocks: 32,
                max_resident_blocks: 16,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        (store, path)
    }
    #[test]
    fn native_planes_preserve_edge_stride_low_bits_and_explicit_window() {
        for depth in [8, 16] {
            let (store, path) = make_store(4 * 1024 * 1024);
            let mut raster = Raster::new(257, 257);
            if depth == 16 {
                raster.promote16();
            }
            raster.set16(256, 256, [1, 12345, 54321, 65535]);
            let plane = Plane::from_resident(&raster, store.clone(), &mut || Ok(())).unwrap();
            let view = plane
                .pin(Region::new([255, 255, 258, 258]).unwrap(), &mut || Ok(()))
                .unwrap();
            assert_eq!(view.pixel16(256, 256).unwrap(), raster.get16(256, 256));
            assert_eq!(view.pixel16(257, 257).unwrap(), [0; 4]);
            assert_eq!(view.pixel16(255, 255).unwrap(), [0; 4]);
            assert!(view.pixel16(254, 255).is_err());
            let mut row = vec![0; 3 * if depth == 16 { 8 } else { 4 }];
            view.copy_row(256, &mut row).unwrap();
            if depth == 16 {
                assert_eq!(&row[8..16], &[0, 1, 48, 57, 212, 49, 255, 255]);
            }
            drop(view);
            assert_eq!(store.resident_budget().used(), 0);
            drop(plane);
            drop(store);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    #[test]
    fn absent_tiles_are_zero_but_present_foreign_blocks_are_errors() {
        let (store, path) = make_store(1024 * 1024);
        let (other, other_path) = make_store(1024 * 1024);
        let spec = TileSpec::new(256, 256, 8).unwrap();
        let block = other
            .put(spec, &vec![17; spec.bytes()], &mut || Ok(()))
            .unwrap();
        let plane = Plane::from_blocks(
            512,
            256,
            8,
            store.clone(),
            BTreeMap::from([((0, 0), block)]),
        )
        .unwrap();
        assert!(plane
            .pin(Region::new([0, 0, 1, 1]).unwrap(), &mut || Ok(()))
            .is_err());
        let zero = plane
            .pin(Region::new([256, 0, 257, 1]).unwrap(), &mut || Ok(()))
            .unwrap();
        assert_eq!(zero.pixel16(256, 0).unwrap(), [0; 4]);
        drop(zero);
        drop(plane);
        drop(store);
        drop(other);
        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(other_path).unwrap();
    }
    #[test]
    fn pin_budget_and_cancellation_return_no_partial_view_or_charge() {
        let (store, path) = make_store(256 * 256 * 4);
        let mut raster = Raster::new(512, 256);
        raster.set(0, 0, [1; 4]);
        raster.set(256, 0, [2; 4]);
        let plane = Plane::from_resident(&raster, store.clone(), &mut || Ok(())).unwrap();
        assert!(plane
            .pin(Region::new([0, 0, 512, 256]).unwrap(), &mut || Ok(()))
            .is_err());
        assert_eq!(store.resident_budget().used(), 0);
        assert!(plane
            .pin(Region::new([0, 0, 1, 1]).unwrap(), &mut || Err(
                Error::Cancelled
            ))
            .is_err());
        assert_eq!(store.resident_budget().used(), 0);
        drop(plane);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
}
