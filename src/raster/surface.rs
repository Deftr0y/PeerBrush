//! Explicit immutable pixel authority for the coordinated document migration.
//! No live Layer field uses this type until its metadata/source DTO is migrated.
use super::{
    backed::{PinnedRegion, Plane, Region},
    Pixel, Pixel16, Raster,
};
use crate::large_document::{Error, ResidentBudget, Result, Store};
use ring::digest::{Context, SHA256};
use std::sync::Arc;

/// Native pixel-footprint identity, including the owned backing-store identity.
/// Retained placement, layer style and document revisions are separate scene keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfaceKey([u8; 32]);

impl SurfaceKey {
    pub fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Clone)]
pub struct Surface {
    key: SurfaceKey,
    width: u32,
    height: u32,
    depth: u16,
    storage: Arc<SurfaceStorage>,
}

enum SurfaceStorage {
    /// Complete known absence, never a replacement for failed backing reads.
    Zero,
    Stored(Plane),
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Surface")
            .field("key", &self.key)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

impl Surface {
    /// Pure metadata. Live document admission remains a separate guarded operation.
    pub fn zero(width: u32, height: u32, depth: u16) -> Result<Self> {
        shape(width, height, depth)?;
        let mut hash = key_prefix(b"zero", width, height, depth);
        hash.update(b"known-transparent-plane");
        Ok(Self {
            key: finish(hash),
            width,
            height,
            depth,
            storage: Arc::new(SurfaceStorage::Zero),
        })
    }

    /// Does not grant imported-file or editable-document admission. The codec's
    /// complete source proofs and engine publication guards remain mandatory.
    pub(crate) fn from_plane(plane: Plane) -> Result<Self> {
        let (width, height, depth) = (plane.width(), plane.height(), plane.depth());
        shape(width, height, depth)?;
        let mut hash = key_prefix(b"owned-store", width, height, depth);
        hash.update(&plane.store().identity());
        for (&(x, y), block) in plane.blocks() {
            hash.update(&x.to_be_bytes());
            hash.update(&y.to_be_bytes());
            hash.update(&block.spec().width().to_be_bytes());
            hash.update(&block.spec().height().to_be_bytes());
            hash.update(&block.spec().depth().to_be_bytes());
            hash.update(&block.fingerprint());
        }
        Ok(Self {
            key: finish(hash),
            width,
            height,
            depth,
            storage: Arc::new(SurfaceStorage::Stored(plane)),
        })
    }

    /// Explicit I/O outside engine/root/registry locks. Retained originals cannot
    /// be silently dropped by this whole-raster convenience conversion.
    pub fn stage_raster(
        raster: &Raster,
        store: Store,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        if raster.retained.is_some() {
            return Err(Error::Invalid(
                "Stage retained sources and placement explicitly",
            ));
        }
        Self::stage_current_pixels(raster, store, check)
    }

    /// Pixel plane only: the caller must collect retained original pixels and
    /// placement separately in its scene/source descriptor before adoption.
    pub fn stage_current_pixels(
        raster: &Raster,
        store: Store,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        Self::from_plane(Plane::from_resident(raster, store, check)?)
    }

    pub fn key(&self) -> SurfaceKey {
        self.key
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
    pub fn is_known_zero(&self) -> bool {
        matches!(&*self.storage, SurfaceStorage::Zero)
    }
    pub fn uses_budget(&self, budget: &ResidentBudget) -> bool {
        match &*self.storage {
            SurfaceStorage::Zero => true,
            SurfaceStorage::Stored(plane) => plane.store().resident_budget().same_pool(budget),
        }
    }

    pub fn prepare_region(
        &self,
        region: Region,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<SurfaceRegion> {
        check()?;
        let pixels = match &*self.storage {
            SurfaceStorage::Zero => RegionPixels::Zero,
            SurfaceStorage::Stored(plane) => RegionPixels::Pinned(plane.pin(region, check)?),
        };
        check()?;
        Ok(SurfaceRegion {
            key: self.key,
            width: self.width,
            height: self.height,
            depth: self.depth,
            region,
            pixels,
        })
    }
}

fn shape(width: u32, height: u32, depth: u16) -> Result<()> {
    if width == 0 || height == 0 || width > 16000 || height > 16000 || ![8, 16].contains(&depth) {
        return Err(Error::Invalid("Invalid native surface shape"));
    }
    Ok(())
}
fn key_prefix(domain: &[u8], width: u32, height: u32, depth: u16) -> Context {
    let mut hash = Context::new(&SHA256);
    hash.update(b"PeerBrush-native-surface-v1\0");
    hash.update(domain);
    hash.update(&width.to_be_bytes());
    hash.update(&height.to_be_bytes());
    hash.update(&depth.to_be_bytes());
    hash
}
fn finish(hash: Context) -> SurfaceKey {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(hash.finish().as_ref());
    SurfaceKey(bytes)
}

/// Fully prepared source window: all sampling is memory-only and fallible on an
/// undeclared footprint. Payload pins keep their original shared-pool charges.
pub struct SurfaceRegion {
    key: SurfaceKey,
    width: u32,
    height: u32,
    depth: u16,
    region: Region,
    pixels: RegionPixels,
}
enum RegionPixels {
    Zero,
    Pinned(PinnedRegion),
}

impl SurfaceRegion {
    pub fn key(&self) -> SurfaceKey {
        self.key
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
    pub fn region(&self) -> Region {
        self.region
    }
    pub fn pixel16(&self, x: i32, y: i32) -> Result<Pixel16> {
        match &self.pixels {
            RegionPixels::Pinned(view) => view.pixel16(x, y),
            RegionPixels::Zero => {
                let [left, top, right, bottom] = self.region.bounds();
                if x < left || y < top || x >= right || y >= bottom {
                    return Err(Error::Invalid(
                        "Sample is outside the successfully prepared surface region",
                    ));
                }
                Ok([0; 4])
            }
        }
    }
    pub fn pixel(&self, x: i32, y: i32) -> Result<Pixel> {
        self.pixel16(x, y).map(|p| p.map(super::project16))
    }
    /// Exact requested row, RGBA8 or canonical RGBA16 big endian.
    pub fn copy_row(&self, y: i32, output: &mut [u8]) -> Result<()> {
        match &self.pixels {
            RegionPixels::Pinned(view) => view.copy_row(y, output),
            RegionPixels::Zero => {
                let [left, top, right, bottom] = self.region.bounds();
                let stride = if self.depth == 16 { 8 } else { 4 };
                if y < top || y >= bottom || output.len() != (right - left) as usize * stride {
                    return Err(Error::Invalid(
                        "Native row does not match its prepared surface window/depth",
                    ));
                }
                output.fill(0);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_document::{Limits, TileSpec};
    use std::collections::BTreeMap;

    fn store(budget: ResidentBudget) -> (Store, std::path::PathBuf) {
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-surface-{}", crate::engine::id()));
        assert_eq!(
            path.parent().unwrap().canonicalize().unwrap(),
            path.parent().unwrap()
        );
        let store = Store::create(
            &path,
            budget,
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
    fn surface_types_are_thread_safe_without_implicit_sampler_traits() {
        fn require<T: Send + Sync>() {}
        require::<Surface>();
        require::<SurfaceRegion>();
    }
    #[test]
    fn explicit_zero_checks_footprints_rows_depth_and_cancellation() {
        for depth in [8, 16] {
            let surface = Surface::zero(16000, 16000, depth).unwrap();
            assert!(surface.is_known_zero());
            let view = surface
                .prepare_region(Region::new([-1, -1, 2, 2]).unwrap(), &mut || Ok(()))
                .unwrap();
            assert_eq!(view.key(), surface.key());
            assert_eq!(view.pixel16(0, 0).unwrap(), [0; 4]);
            assert_eq!(view.pixel16(-1, -1).unwrap(), [0; 4]);
            assert!(view.pixel16(2, 0).is_err());
            let mut row = vec![99; 3 * if depth == 16 { 8 } else { 4 }];
            view.copy_row(0, &mut row).unwrap();
            assert!(row.iter().all(|&v| v == 0));
            assert!(view.copy_row(3, &mut row).is_err());
            assert!(view.copy_row(0, &mut row[..1]).is_err());
            assert!(surface
                .prepare_region(Region::new([0, 0, 1, 1]).unwrap(), &mut || Err(
                    Error::Cancelled
                ))
                .is_err());
        }
        assert_ne!(
            Surface::zero(1, 1, 8).unwrap().key(),
            Surface::zero(1, 1, 16).unwrap().key()
        );
        assert!(Surface::zero(16001, 1, 8).is_err());
    }
    #[test]
    fn immutable_clones_share_native_pins_and_exact_global_budget() {
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let (store, path) = store(budget.clone());
        let mut raster = Raster::new_depth(256, 256, 16);
        raster.set16(2, 3, [1, 12345, 54321, 65535]);
        let surface = Surface::stage_raster(&raster, store.clone(), &mut || Ok(())).unwrap();
        let copy = surface.clone();
        assert_eq!(surface.key(), copy.key());
        assert!(copy.uses_budget(&budget));
        assert!(!copy.uses_budget(&ResidentBudget::new(budget.limit()).unwrap()));
        assert_eq!(budget.used(), 0);
        let a = surface
            .prepare_region(Region::new([2, 3, 3, 4]).unwrap(), &mut || Ok(()))
            .unwrap();
        let charge = budget.used();
        assert_eq!(charge, 256 * 256 * 8);
        let b = copy
            .prepare_region(Region::new([2, 3, 3, 4]).unwrap(), &mut || Ok(()))
            .unwrap();
        assert_eq!(budget.used(), charge);
        assert_eq!(b.pixel16(2, 3).unwrap(), [1, 12345, 54321, 65535]);
        drop(a);
        drop(surface);
        drop(copy);
        assert_eq!(budget.used(), charge);
        drop(b);
        assert_eq!(budget.used(), 0);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn foreign_store_identity_cannot_alias_verified_footprints() {
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let (first, first_path) = store(budget.clone());
        let (second, second_path) = store(budget);
        let spec = TileSpec::new(256, 256, 8).unwrap();
        let bytes = vec![37; spec.bytes()];
        let a = first.put(spec, &bytes, &mut || Ok(())).unwrap();
        let b = second.put(spec, &bytes, &mut || Ok(())).unwrap();
        assert_eq!(a.digest(), b.digest());
        let make = |store: Store, block| {
            Surface::from_plane(
                Plane::from_blocks(256, 256, 8, store, BTreeMap::from([((0, 0), block)])).unwrap(),
            )
            .unwrap()
        };
        let own = make(first.clone(), a);
        let foreign = make(second.clone(), b);
        let invalid = make(first.clone(), b);
        assert_ne!(own.key(), foreign.key());
        assert_ne!(own.key(), invalid.key());
        assert!(invalid
            .prepare_region(Region::new([0, 0, 1, 1]).unwrap(), &mut || Ok(()))
            .is_err());
        drop(own);
        drop(foreign);
        drop(invalid);
        drop(first);
        drop(second);
        std::fs::remove_dir_all(first_path).unwrap();
        std::fs::remove_dir_all(second_path).unwrap();
    }
    #[test]
    fn whole_raster_staging_refuses_to_drop_retained_authority() {
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let (store, path) = store(budget);
        let mut raster = Raster::new_depth(256, 256, 16);
        raster.set16(2, 3, [1, 12345, 54321, 65535]);
        raster.retained = Some(Box::new(crate::retained::Original {
            pixels: Box::new(raster.clone()),
            matrix: crate::retained::IDENTITY,
            projective: None,
            warps: vec![],
        }));
        let mut calls = 0;
        assert!(Surface::stage_raster(&raster, store.clone(), &mut || {
            calls += 1;
            Ok(())
        })
        .is_err());
        assert_eq!(calls, 0);
        let current =
            Surface::stage_current_pixels(&raster, store.clone(), &mut || Ok(())).unwrap();
        let view = current
            .prepare_region(Region::new([2, 3, 3, 4]).unwrap(), &mut || Ok(()))
            .unwrap();
        assert_eq!(view.pixel16(2, 3).unwrap(), raster.get16(2, 3));
        assert!(raster.retained.is_some());
        drop(view);
        drop(current);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
}
