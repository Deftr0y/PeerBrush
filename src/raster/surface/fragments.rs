//! Resolved immutable physical references. Flattening copies metadata, not pixels.
use super::*;
use crate::large_document::{BlockRef, PinnedBytes, Reservation};

#[derive(Clone)]
pub struct PhysicalTile {
    coordinate: (u32, u32),
    owner: Store,
    reference: BlockRef,
}
impl PhysicalTile {
    pub fn coordinate(&self) -> (u32, u32) {
        self.coordinate
    }
    pub fn owner(&self) -> &Store {
        &self.owner
    }
    pub fn reference(&self) -> BlockRef {
        self.reference
    }
}

/// Only complete canonical zero bytes can request an explicit zero patch.
pub struct TilePatch {
    coordinate: (u32, u32),
    spec: TileSpec,
    replacement: Option<PhysicalTile>,
}
impl TilePatch {
    pub(crate) fn stored(coordinate: (u32, u32), owner: Store, reference: BlockRef) -> Self {
        Self {
            coordinate,
            spec: reference.spec(),
            replacement: Some(PhysicalTile {
                coordinate,
                owner,
                reference,
            }),
        }
    }
    pub(crate) fn zero(
        coordinate: (u32, u32),
        spec: TileSpec,
        bytes: &PinnedBytes,
        budget: &ResidentBudget,
    ) -> Result<Self> {
        if bytes.len() != spec.bytes()
            || !bytes.uses_budget(budget)
            || bytes.iter().any(|&b| b != 0)
        {
            return Err(Error::Invalid(
                "Native zero patch requires complete charged zero tile bytes",
            ));
        }
        Ok(Self {
            coordinate,
            spec,
            replacement: None,
        })
    }
}

pub(super) struct Index {
    // Actual reference allocation and its owners drop before the charge.
    pub(super) entries: Vec<PhysicalTile>,
    _charge: Reservation,
    pub(super) budget: ResidentBudget,
    pub(super) disk: DiskBudget,
}
impl Surface {
    /// Fill caller-owned scratch slots in row order. The caller retires the
    /// row on failure, including pins from previous batches.
    pub(crate) fn pin_row(
        &self,
        x: u32,
        y: u32,
        output: &mut [Option<PinnedTile>],
        active: &mut dyn FnMut() -> Result<()>,
        owner: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        let columns = self.width.div_ceil(super::super::TILE);
        if y >= self.height.div_ceil(super::super::TILE)
            || x > columns
            || output.len() > (columns - x) as usize
            || output.iter().any(Option::is_some)
        {
            return Err(Error::Invalid("Invalid native tile row footprint"));
        }
        let coordinate = |index: usize| (x + index as u32, y);
        let mut first = 0;
        while first < output.len() {
            active()?;
            let Some(tile) = self.physical_tile(coordinate(first)) else {
                first += 1;
                continue;
            };
            let store = tile.owner();
            let mut references = [None; Store::PIN_BATCH_LIMIT];
            references[0] = Some(tile.reference());
            let mut end = first + 1;
            while end < output.len() && end - first < references.len() {
                if let Some(tile) = self.physical_tile(coordinate(end)) {
                    if !store.same_owner(tile.owner()) {
                        break;
                    }
                    references[end - first] = Some(tile.reference());
                }
                end += 1;
            }
            store.pin_batch(
                &references[..end - first],
                &mut output[first..end],
                active,
                owner,
            )?;
            first = end;
        }
        Ok(())
    }

    pub(crate) fn physical_tile(&self, coordinate: (u32, u32)) -> Option<PhysicalTile> {
        match &*self.storage {
            SurfaceStorage::Zero => None,
            SurfaceStorage::Stored(plane, _) => {
                plane
                    .blocks()
                    .get(&coordinate)
                    .map(|&reference| PhysicalTile {
                        coordinate,
                        owner: plane.store().clone(),
                        reference,
                    })
            }
            SurfaceStorage::Resolved(index) => index
                .entries
                .binary_search_by_key(&coordinate, PhysicalTile::coordinate)
                .ok()
                .map(|at| index.entries[at].clone()),
        }
    }

    /// A missing physical reference proves transparent samples in this region.
    /// Present references still require their ordinary pin/ownership checks.
    pub(crate) fn has_physical_tiles(&self, rect: [i32; 4]) -> bool {
        use super::super::TILE;
        let [l, t, r, b] = rect;
        let (l, t) = (l.max(0) as u32, t.max(0) as u32);
        let (r, b) = (r.max(0) as u32, b.max(0) as u32);
        let (r, b) = (r.min(self.width), b.min(self.height));
        if l >= r || t >= b {
            return false;
        }
        let mut columns = l / TILE..r.div_ceil(TILE);
        let rows = t / TILE..b.div_ceil(TILE);
        let mut physical = self.physical_tiles();
        if columns.len() * rows.len() < physical.len() {
            columns.any(|x| rows.clone().any(|y| self.physical_tile((x, y)).is_some()))
        } else {
            physical.any(|tile| {
                let (x, y) = tile.coordinate();
                columns.contains(&x) && rows.contains(&y)
            })
        }
    }

    /// Native descriptor metadata is charged for its complete owner lifetime.
    /// The caller retains its provisional index admission during this conversion.
    pub(crate) fn with_tile_patches(
        &self,
        patches: &[TilePatch],
        budget: &ResidentBudget,
        disk: &DiskBudget,
    ) -> Result<Self> {
        if !self.uses_budget(budget) || !self.uses_disk_budget(disk) {
            return Err(Error::Invalid(
                "Native source patches require the shared resident and disk pools",
            ));
        }
        if patches.len() > 4096 {
            return Err(Error::Budget);
        }
        let spec = TileSpec::new(
            super::super::TILE as u16,
            super::super::TILE as u16,
            self.depth,
        )?;
        let mut previous = None;
        for patch in patches {
            if patch.coordinate.0 >= self.width.div_ceil(super::super::TILE)
                || patch.coordinate.1 >= self.height.div_ceil(super::super::TILE)
                || patch.spec != spec
                || previous.is_some_and(|p| p >= patch.coordinate)
                || patch.replacement.as_ref().is_some_and(|tile| {
                    !tile.owner.resident_budget().same_pool(budget)
                        || !tile.owner.uses_disk_budget(disk)
                })
            {
                return Err(Error::Invalid(
                    "Invalid, duplicate or foreign-budget native tile patch",
                ));
            }
            previous = Some(patch.coordinate);
        }
        if patches.is_empty() {
            return Ok(self.clone());
        }
        let mut existing = self.physical_tiles().peekable();
        let count = existing.len();
        let maximum = count.checked_add(patches.len()).ok_or(Error::Budget)?.min(
            self.width.div_ceil(super::super::TILE) as usize
                * self.height.div_ceil(super::super::TILE) as usize,
        );
        let bytes = maximum
            .checked_mul(std::mem::size_of::<PhysicalTile>())
            .and_then(|n| n.checked_add(128))
            .ok_or(Error::Budget)?;
        let charge = budget.reserve_bytes(bytes, bytes)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(maximum)
            .map_err(|_| Error::Budget)?;
        if entries.capacity() > maximum {
            return Err(Error::Budget);
        }
        for patch in patches {
            while let Some(tile) = existing.next_if(|tile| tile.coordinate < patch.coordinate) {
                entries.push(tile);
            }
            let _ = existing.next_if(|tile| tile.coordinate == patch.coordinate);
            entries.extend(patch.replacement.iter().cloned());
        }
        entries.extend(existing);
        if entries.is_empty() {
            return Surface::zero(self.width, self.height, self.depth);
        }
        let mut hash = key_prefix(b"physical-tiles", self.width, self.height, self.depth);
        for tile in &entries {
            hash_tile(&mut hash, tile.coordinate, &tile.owner, tile.reference);
        }
        Ok(Self {
            key: finish(hash),
            width: self.width,
            height: self.height,
            depth: self.depth,
            storage: Arc::new(SurfaceStorage::Resolved(Index {
                entries,
                _charge: charge,
                budget: budget.clone(),
                // Base and replacement owners were all admitted to this pool above.
                disk: disk.clone(),
            })),
        })
    }

    pub fn physical_tiles(&self) -> impl ExactSizeIterator<Item = PhysicalTile> + '_ {
        enum Entries<'a> {
            Zero,
            Stored(
                std::collections::btree_map::Iter<'a, (u32, u32), BlockRef>,
                &'a Store,
            ),
            Resolved(std::slice::Iter<'a, PhysicalTile>),
        }
        impl Iterator for Entries<'_> {
            type Item = PhysicalTile;
            fn next(&mut self) -> Option<PhysicalTile> {
                match self {
                    Self::Zero => None,
                    Self::Stored(entries, owner) => {
                        entries
                            .next()
                            .map(|(&coordinate, &reference)| PhysicalTile {
                                coordinate,
                                reference,
                                owner: (*owner).clone(),
                            })
                    }
                    Self::Resolved(entries) => entries.next().cloned(),
                }
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                let len = match self {
                    Self::Zero => 0,
                    Self::Stored(entries, _) => entries.len(),
                    Self::Resolved(entries) => entries.len(),
                };
                (len, Some(len))
            }
            fn count(self) -> usize {
                self.len()
            }
        }
        impl ExactSizeIterator for Entries<'_> {}
        match &*self.storage {
            SurfaceStorage::Zero => Entries::Zero,
            SurfaceStorage::Stored(plane, _) => {
                Entries::Stored(plane.blocks().iter(), plane.store())
            }
            SurfaceStorage::Resolved(index) => Entries::Resolved(index.entries.iter()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_document::Limits;
    fn owned(budget: &ResidentBudget, disk: &DiskBudget) -> (Store, std::path::PathBuf) {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-fragment-map-{}", crate::engine::id()));
        let store = Store::create_with_disk_budget(
            &root,
            budget.clone(),
            disk.clone(),
            Limits {
                disk_bytes: 8 * 1024 * 1024,
                max_blocks: 32,
                max_resident_blocks: 16,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        (store, root)
    }
    #[test]
    fn resolved_fragments_share_owners_history_and_charged_metadata_without_pixel_copy() {
        for depth in [8, 16] {
            let budget = ResidentBudget::new(8 * 1024 * 1024).unwrap();
            let disk = DiskBudget::new(32 * 1024 * 1024, 16).unwrap();
            let foreign_disk = DiskBudget::new(32 * 1024 * 1024, 16).unwrap();
            let (base_store, base_path) = owned(&budget, &disk);
            let (fragment, fragment_path) = owned(&budget, &disk);
            let (foreign, foreign_path) = owned(&budget, &foreign_disk);
            let mut raster = Raster::new_depth(768, 256, depth);
            raster.set16(0, 0, [12345, 23456, 34567, 65535]);
            raster.set16(512, 0, [34567, 23456, 12345, 65535]);
            let base = Surface::stage_raster(&raster, base_store.clone(), &mut || Ok(())).unwrap();
            let history = base.clone();
            let baseline = budget.used();
            assert!(baseline > 0);
            let spec = TileSpec::new(256, 256, depth).unwrap();
            let mut bytes = ScratchBytes::new(&budget, spec.bytes(), spec.bytes()).unwrap();
            let pixel = [10001u16, 20002, 30003, 65535];
            if depth == 16 {
                for i in 0..4 {
                    bytes[i * 2..i * 2 + 2].copy_from_slice(&pixel[i].to_be_bytes());
                }
            } else {
                bytes[..4].copy_from_slice(&pixel.map(super::super::super::project16));
            }
            let reference = fragment.put(spec, &bytes, &mut || Ok(())).unwrap();
            let foreign_reference = foreign.put(spec, &bytes, &mut || Ok(())).unwrap();
            drop(bytes);
            assert!(base.with_tile_patches(
                &[TilePatch::stored((1, 0), foreign.clone(), foreign_reference)], &budget, &disk,
            ).is_err());
            drop(foreign);
            let patch = TilePatch::stored((1, 0), fragment.clone(), reference);
            let overlay = base.with_tile_patches(&[patch], &budget, &disk).unwrap();
            assert_ne!(base.key(), overlay.key());
            assert!(overlay.uses_disk_budget(&disk));
            assert!(!overlay.uses_disk_budget(&foreign_disk));
            assert!(overlay.with_tile_patches(&[], &budget, &foreign_disk).is_err());
            assert!(budget.used() > baseline);
            drop(fragment);
            let region = Region::new([0, 0, 513, 1]).unwrap();
            let view = overlay
                .prepare_region_guarded(region, &mut || Ok(()), &mut || Ok(()))
                .unwrap();
            let expected = overlay.prepare_region(region, &mut || Ok(())).unwrap();
            for x in [0, 255, 256, 511, 512] {
                assert_eq!(view.pixel16(x, 0).unwrap(), expected.pixel16(x, 0).unwrap());
            }
            drop(expected);
            assert_eq!(
                view.pixel16(256, 0).unwrap(),
                if depth == 16 {
                    pixel
                } else {
                    pixel
                        .map(super::super::super::project16)
                        .map(|n| n as u16 * 257)
                }
            );
            let old = history
                .prepare_region(Region::new([256, 0, 257, 1]).unwrap(), &mut || Ok(()))
                .unwrap();
            assert_eq!(old.pixel16(256, 0).unwrap(), [0; 4]);
            drop(view);
            drop(old);
            let payload = budget.payload_used();
            let mut owner_checks = 0;
            let interrupted = overlay.prepare_region_guarded(region, &mut || Ok(()), &mut || {
                owner_checks += 1;
                if owner_checks == 4 { Err(Error::Changed) } else { Ok(()) }
            });
            assert!(matches!(interrupted, Err(Error::Changed)));
            assert_eq!(budget.payload_used(), payload);
            let zero = ScratchBytes::new(&budget, spec.bytes(), spec.bytes())
                .unwrap()
                .freeze();
            let patch = TilePatch::zero((1, 0), spec, &zero, &budget).unwrap();
            drop(zero);
            let cleared = overlay.with_tile_patches(std::slice::from_ref(&patch), &budget, &disk).unwrap();
            assert_eq!(cleared.key(), base.key());
            let mut tiles = cleared.physical_tiles();
            assert_eq!(tiles.len(), 2);
            assert_eq!(tiles.next().unwrap().coordinate(), (0, 0));
            assert_eq!(tiles.count(), 1);
            let installed = overlay.physical_tile((1, 0)).unwrap();
            let replacement = TilePatch::stored((0, 0), installed.owner().clone(), reference);
            let changed = overlay.with_tile_patches(&[replacement, patch], &budget, &disk).unwrap();
            assert_eq!(changed.physical_tiles().map(|tile| tile.coordinate()).collect::<Vec<_>>(), [(0, 0), (2, 0)]);
            assert_eq!(changed.physical_tile((0, 0)).unwrap().reference(), reference);
            assert_eq!(changed.physical_tile((2, 0)).unwrap().reference(), base.physical_tile((2, 0)).unwrap().reference());
            drop((changed, installed));
            drop(cleared);
            drop(overlay);
            assert_eq!(budget.used(), baseline);
            drop(history);
            drop(base);
            assert_eq!(budget.payload_used(), 0);
            drop(base_store);
            assert_eq!(budget.used(), 0);
            std::fs::remove_dir_all(base_path).unwrap();
            std::fs::remove_dir_all(fragment_path).unwrap();
            std::fs::remove_dir_all(foreign_path).unwrap();
        }
    }
    #[test]
    fn zero_patch_refuses_hidden_rgb_and_padding_and_foreign_pool() {
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let spec = TileSpec::new(256, 256, 16).unwrap();
        let mut bytes = ScratchBytes::new(&budget, spec.bytes(), spec.bytes()).unwrap();
        bytes[spec.bytes() - 1] = 1;
        assert!(TilePatch::zero((0, 0), spec, &bytes.freeze(), &budget).is_err());
        let foreign = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let bytes = ScratchBytes::new(&foreign, spec.bytes(), spec.bytes())
            .unwrap()
            .freeze();
        assert!(TilePatch::zero((0, 0), spec, &bytes, &budget).is_err());
    }
}

pub(super) fn hash_tile(
    hash: &mut Context,
    coordinate: (u32, u32),
    owner: &Store,
    reference: BlockRef,
) {
    hash.update(&coordinate.0.to_be_bytes());
    hash.update(&coordinate.1.to_be_bytes());
    hash.update(&owner.identity());
    hash.update(&reference.fingerprint());
}
