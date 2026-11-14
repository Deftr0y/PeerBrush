//! Seal complete physical source sets without changing captured document owners.
use super::{RasterRoster, Result, StageSession};
use crate::{
    large_document::{BlockRef, DiskBudget, Error, Reservation, ResidentBudget, Snapshot, Store},
    raster::{surface::TilePatch, Surface},
};
use ring::digest::{Context, SHA256};
use std::collections::BTreeMap;

pub(crate) struct SealedSources {
    pub(crate) rasters: RasterRoster,
    pub(crate) seals: Vec<Snapshot>,
    pub(crate) charge: Reservation,
}

pub(crate) fn seal_sources(
    surfaces: &[Surface],
    resident: &ResidentBudget,
    disk: &DiskBudget,
    session: &impl StageSession,
    metadata: &[u8],
    limits: (usize, usize),
    extra_bytes: usize,
    check: &dyn Fn() -> Result<()>,
) -> Result<SealedSources> {
    let (maximum_refs, maximum_stores) = limits;
    session.check()?;
    check()?;
    if surfaces.len() > super::MAX_RASTER_LOCATIONS {
        return Err("Native source roster exceeds admission".into());
    }
    let count = surfaces.iter().try_fold(0usize, |count, surface| {
        if !surface.uses_budget(resident) || !surface.uses_disk_budget(disk) {
            return Err("Native source uses foreign admission pools".to_string());
        }
        surface.physical_tiles().try_fold(count, |count, _| {
            check()?;
            count
                .checked_add(1)
                .filter(|count| *count <= maximum_refs)
                .ok_or_else(|| "Native source references exceed admission".into())
        })
    })?;
    // Covers old/new references, maps, patches and the returned strong-owner roster.
    let bytes = count
        .checked_mul(3 * std::mem::size_of::<BlockRef>() + 512)
        .and_then(|n| {
            surfaces
                .len()
                .checked_mul(2 * std::mem::size_of::<Surface>())
                .and_then(|r| n.checked_add(r))
        })
        .and_then(|n| n.checked_add(extra_bytes))
        .and_then(|n| n.checked_add(4096))
        .ok_or("Native source seal footprint overflow")?;
    let charge = resident
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut stores: BTreeMap<[u8; 16], (Store, Vec<BlockRef>)> = BTreeMap::new();
    for surface in surfaces {
        for tile in surface.physical_tiles() {
            session.check()?;
            check()?;
            if stores.len() >= maximum_stores && !stores.contains_key(&tile.owner().identity()) {
                return Err("Native source store count exceeds admission".into());
            }
            let entry = stores
                .entry(tile.owner().identity())
                .or_insert_with(|| (tile.owner().clone(), vec![]));
            if !entry.0.same_owner(tile.owner()) {
                return Err("Native source identity names different physical owners".into());
            }
            entry
                .1
                .try_reserve(1)
                .map_err(|_| "Native source reference allocation refused")?;
            entry.1.push(tile.reference());
        }
    }
    for (_, refs) in stores.values_mut() {
        refs.sort_unstable_by_key(|reference| reference.fingerprint());
        refs.dedup();
    }
    let mut seals = Vec::new();
    seals
        .try_reserve_exact(stores.len())
        .map_err(|_| "Native seal allocation refused")?;
    if seals.capacity() > stores.len() {
        return Err("Native seal capacity exceeds admission".into());
    }
    let mut replacements: BTreeMap<[u8; 16], (Store, Vec<BlockRef>)> = BTreeMap::new();
    for (id, (owner, refs)) in &stores {
        session.check()?;
        check()?;
        if let Some(seal) = session.try_seal_store(owner, refs, metadata)? {
            if !seal.store().same_owner(owner) {
                return Err("Native selected source belongs to another physical owner".into());
            }
            seals.push(seal);
            continue;
        }
        // This key only reuses an owned writable mirror; references still require
        // actual session registration and pin/put validation on both owners.
        let mut hash = Context::new(&SHA256);
        hash.update(b"PeerBrushReadonlyNativeMirror1");
        hash.update(id);
        let lineage: [u8; 16] = hash.finish().as_ref()[..16].try_into().unwrap();
        // shortcut: abandoned persistent mirrors remain charged; add verified
        // shared-source collection before ordinary native document adoption.
        let mirror = session.store_for_lineage(lineage)?;
        if stores.contains_key(&mirror.identity())
            || seals
                .iter()
                .any(|seal| seal.store().identity() == mirror.identity())
            || !mirror.resident_budget().same_pool(resident)
            || !mirror.uses_disk_budget(disk)
        {
            return Err("Native source mirror is foreign or aliases a captured owner".into());
        }
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(refs.len())
            .map_err(|_| "Native copy reference allocation refused")?;
        if copied.capacity() > refs.len() {
            return Err("Native copy references exceed admission".into());
        }
        let mut io_check = || {
            session
                .check()
                .and_then(|_| check())
                .map_err(|_| Error::Cancelled)
        };
        for reference in refs {
            let pinned = owner
                .pin(*reference, &mut io_check)
                .map_err(|e| e.to_string())?;
            let replacement = mirror
                .put(reference.spec(), pinned.bytes(), &mut io_check)
                .map_err(|e| e.to_string())?;
            if replacement.spec() != reference.spec() || replacement.digest() != reference.digest()
            {
                return Err("Native source copy changed tile precision or bytes".into());
            }
            copied.push(replacement);
        }
        let seal = session.seal_store(&mirror, &copied, metadata)?;
        if !seal.store().same_owner(&mirror) {
            return Err("Native mirror selection belongs to another physical owner".into());
        }
        seals.push(seal);
        replacements.insert(*id, (mirror, copied));
    }
    let mut rasters = Vec::new();
    rasters
        .try_reserve_exact(surfaces.len())
        .map_err(|_| "Native sealed roster allocation refused")?;
    if rasters.capacity() > surfaces.len() {
        return Err("Native sealed roster exceeds admission".into());
    }
    for surface in surfaces {
        check()?;
        let count = surface
            .physical_tiles()
            .filter(|tile| replacements.contains_key(&tile.owner().identity()))
            .count();
        if count > 4096 {
            return Err("Native source patches exceed surface admission".into());
        }
        let mut patches = Vec::new();
        patches
            .try_reserve_exact(count)
            .map_err(|_| "Native copy patch allocation refused")?;
        if patches.capacity() > count {
            return Err("Native copy patches exceed admission".into());
        }
        for tile in surface.physical_tiles() {
            check()?;
            if let Some((mirror, copied)) = replacements.get(&tile.owner().identity()) {
                let refs = &stores[&tile.owner().identity()].1;
                let at = refs
                    .binary_search_by_key(&tile.reference().fingerprint(), |reference| {
                        reference.fingerprint()
                    })
                    .map_err(|_| "Native copy lost an admitted reference")?;
                if refs[at] != tile.reference() {
                    return Err("Native copy reference differs from its physical owner".into());
                }
                patches.push(TilePatch::stored(
                    tile.coordinate(),
                    mirror.clone(),
                    copied[at],
                ));
            }
        }
        rasters.push(
            surface
                .with_tile_patches(&patches, resident, disk)
                .map_err(|e| e.to_string())?,
        );
    }
    session.check()?;
    check()?;
    Ok(SealedSources {
        rasters: RasterRoster::new(rasters)?,
        seals,
        charge,
    })
}
