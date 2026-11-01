//! Provisional native-source import. Neither typed JSON nor valid tile hashes
//! alone grant editable authority. Root owns actual semantic admission, source
//! identity/lease/revision publication and runtime ID/lineage decisions.
use super::{
    bulk36::{self, BulkLimits, Digest, Record, TileIndexEntry},
    canonical36::Limits as MetadataLimits,
    manifest36::{self, SourceBinding},
    memory36::{Admitted, MetadataCharge, WorkingSet},
    read36::{self, OwnedSource},
    row_stream::Source,
    scene_save36::{CapturedRows, ScenePlan},
    standard36::ComparedStandard,
    structural::{Index, Span},
};
use crate::{
    engine::scene::{Document36, PendingScene, RasterRoster, SceneIdentity, SceneSnapshot},
    large_document::{BlockRef, Error, ScratchBytes, Store, TileSpec},
    raster::{backed::Plane, Surface},
};
use std::{
    collections::BTreeMap,
    io::{Read, Seek},
    sync::Arc,
};

/// Implemented by the root's authenticated fresh-fragment/session owner. The
/// codec cannot recover ownership from a serialized path, UUID or directory.
pub(crate) trait ImportSession {
    fn store(&self) -> &Store;
    fn check(&self) -> Result<(), String>;
}
pub(crate) struct ImportLimits {
    pub metadata: MetadataLimits,
    pub bulk: BulkLimits,
    pub working: WorkingSet,
}
struct StoredTile {
    depth: u16,
    digest: Digest,
    reference: BlockRef,
}

pub(crate) struct ProvisionalImport<'a, R, S> {
    scene: SceneSnapshot,
    binding: Admitted<SourceBinding>,
    source: OwnedSource<'a, R>,
    session: &'a S,
    sizes: Vec<u32>,
    nonce: String,
    // The exact input index allocation remains charged until its Arc drops.
    _index_owner: Arc<MetadataCharge>,
}
pub(crate) struct ComparedImport {
    nonce: String,
    fidelity: ComparedStandard,
}
pub(crate) struct QualifiedImport<'a, R, S> {
    pending: PendingScene,
    binding: Admitted<SourceBinding>,
    source: OwnedSource<'a, R>,
    session: &'a S,
    _index_owner: Arc<MetadataCharge>,
}

/// Input index/header allocations must have been charged before indexing. Store
/// payloads/maps are provisional until the whole source and later scene render
/// comparisons finish. A refusal never creates an editable Document.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare<'a, R: Read + Seek, S: ImportSession>(
    source: &'a Source<R>,
    index: Arc<Index>,
    index_owner: Arc<MetadataCharge>,
    source_sha256: Digest,
    session: &'a S,
    project_id: String,
    limits: ImportLimits,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Admitted<ProvisionalImport<'a, R, S>>, String> {
    check()?;
    session.check()?;
    let store = session.store();
    let resident = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or("Source36 import owner has no persistent shared disk admission")?;
    if project_id.is_empty() {
        return Err("Source36 provisional import project scope is missing".into());
    }
    if !index_owner.uses_budget(&resident) || index_owner.bytes() < index_bytes(&index)? {
        return Err("Source36 import index is outside retained pre-admission".into());
    }
    let mut resources = index.resources.iter().filter(|r| r.peerbrush_source);
    let resource = resources
        .next()
        .ok_or("Source36 private manifest resource is missing")?;
    if resources.next().is_some() || resource.data.bytes < 10 {
        return Err("Source36 private manifest resource is ambiguous or truncated".into());
    }
    let encoded = Span {
        offset: resource.data.offset + 4,
        bytes: resource.data.bytes - 4,
    };
    let parsed = manifest36::inflate_admitted(
        &mut source.range(encoded, check)?,
        encoded.bytes,
        limits.metadata,
        limits.bulk,
        limits.working,
        &resident,
        check,
    )?;
    let parsed = parsed.map_unique(|manifest, _| {
        manifest.into_typed::<Document36>(limits.metadata, check, |doc, rasters, id, revision| {
            if doc.id != id
                || doc.revision != revision
                || doc.width != index.header.width
                || doc.height != index.header.height
                || doc.bit_depth != index.header.depth
            {
                return Err(
                    "Source36 typed import identity/geometry differs from original source".into(),
                );
            }
            let count = doc.visit_rasters(|r| {
                let entry = rasters
                    .get(r.id.checked_sub(1).ok_or("Invalid native raster ID")? as usize)
                    .ok_or("Missing native raster descriptor")?;
                if entry.id != r.id
                    || entry.width != r.width
                    || entry.height != r.height
                    || entry.depth != r.depth
                {
                    return Err("Source36 typed native raster descriptor differs".into());
                }
                Ok(())
            })?;
            if count != rasters.len() {
                return Err(
                    "Source36 typed import contains unused native raster descriptors".into(),
                );
            }
            Ok(())
        })
    })?;
    let plan = parsed.plan();
    plan.admit(limits.bulk, check)?;
    let bytes = (plan.references as usize)
        .checked_mul(256)
        .and_then(|n| n.checked_add(plan.tiles as usize * std::mem::size_of::<StoredTile>()))
        .and_then(|n| {
            n.checked_add(
                plan.rasters.len()
                    * (std::mem::size_of::<BTreeMap<(u32, u32), BlockRef>>()
                        + std::mem::size_of::<Surface>()),
            )
        })
        .and_then(|n| {
            n.checked_add(
                bulk36::BulkVerifier::index_bytes(plan.tiles as usize, plan.rasters.len()).ok()?,
            )
        })
        .and_then(|n| n.checked_add(plan.chunks.len() * 4 + 4096))
        .ok_or("Source36 import map footprint overflow")?;
    let map_owner = MetadataCharge::reserve(&resident, bytes, check)?;
    let mut tiles = Vec::new();
    tiles
        .try_reserve_exact(plan.tiles as usize)
        .map_err(|_| "Source36 import tile metadata allocation refused")?;
    let mut maps = Vec::new();
    maps.try_reserve_exact(plan.rasters.len())
        .map_err(|_| "Source36 import raster-map allocation refused")?;
    maps.resize_with(plan.rasters.len(), BTreeMap::new);
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(plan.tiles as usize)
        .map_err(|_| "Source36 import verification index refused")?;
    entries.resize(plan.tiles as usize, TileIndexEntry::vacant());
    let mut counts = Vec::new();
    counts
        .try_reserve_exact(plan.rasters.len())
        .map_err(|_| "Source36 import counter allocation refused")?;
    counts.resize(plan.rasters.len(), 0);
    let mut scratch = ScratchBytes::new(&resident, bulk36::MAX_TILE_BYTES, bulk36::MAX_TILE_BYTES)
        .map_err(|e| e.to_string())?;
    let complete = read36::verify(
        source,
        &index,
        source_sha256,
        &parsed,
        read36::VerificationLimits {
            bulk: limits.bulk,
            native_streams: super::standard36::MAX_STANDARD_RECORDS * 5 + 1,
        },
        &mut entries,
        &mut counts,
        &mut scratch,
        check,
        |record| {
            session.check()?;
            check()?;
            match record {
                Record::Tile {
                    spec,
                    digest,
                    payload,
                } => {
                    let mut guarded = || {
                        check()
                            .and_then(|_| session.check())
                            .map_err(|e| Error::Io(std::io::Error::other(e)))
                    };
                    let reference = store
                        .put(
                            TileSpec::new(256, 256, spec.depth()).map_err(|e| e.to_string())?,
                            payload,
                            &mut guarded,
                        )
                        .map_err(|e| e.to_string())?;
                    if reference.digest() != digest || reference.bytes() != payload.len() {
                        return Err("Source36 imported native block readback differs".into());
                    }
                    tiles.push(StoredTile {
                        depth: spec.depth(),
                        digest,
                        reference,
                    });
                }
                Record::Reference(reference) => {
                    let ordinal = reference
                        .raster
                        .checked_sub(1)
                        .ok_or("Invalid imported native raster ID")?
                        as usize;
                    let raster = plan
                        .rasters
                        .get(ordinal)
                        .ok_or("Missing imported native raster descriptor")?;
                    let at = tiles
                        .binary_search_by_key(&(raster.depth, reference.tile), |t| {
                            (t.depth, t.digest)
                        })
                        .map_err(|_| "Imported native reference has no complete block")?;
                    if maps[ordinal]
                        .insert((reference.x, reference.y), tiles[at].reference)
                        .is_some()
                    {
                        return Err("Duplicate imported native tile location".into());
                    }
                }
            }
            Ok(())
        },
    )?
    .own_index(index.clone())?;
    session.check()?;
    check()?;
    let mut surfaces = Vec::new();
    surfaces
        .try_reserve_exact(plan.rasters.len())
        .map_err(|_| "Source36 imported surface roster allocation refused")?;
    for (raster, map) in plan.rasters.iter().zip(maps) {
        check()?;
        session.check()?;
        let plane = Plane::from_blocks(
            raster.width,
            raster.height,
            raster.depth,
            store.clone(),
            map,
        )
        .map_err(|e| e.to_string())?;
        surfaces.push(Surface::from_plane(plane).map_err(|e| e.to_string())?);
    }
    let mut sizes = Vec::new();
    sizes
        .try_reserve_exact(plan.chunks.len())
        .map_err(|_| "Source36 imported chunk-size index refused")?;
    sizes.extend(plan.chunks.iter().map(|c| c.bytes));
    drop(tiles);
    drop(entries);
    drop(counts);
    let roster = RasterRoster::new(surfaces)?;
    let resident = resident.clone();
    // The existing metadata owner covers BOTH moved typed payload and binding.
    // Each surface also retains its own permanent physical-descriptor charge.
    let provisional = parsed
        .retain_owner(&map_owner, check)?
        .map_unique(|manifest, owner| {
            let (metadata, binding) = manifest.into_source_parts();
            let identity = SceneIdentity {
                project_id,
                document_id: metadata.id.clone(),
                revision: metadata.revision,
            };
            let pending = PendingScene::new(
                Admitted::from_parts(Arc::new(metadata), owner.clone()),
                roster,
                resident,
                disk,
            )?;
            let scene = SceneSnapshot::new(identity, pending, vec![], None, None)?;
            Ok(ProvisionalImport {
                scene,
                binding: Admitted::from_parts(Arc::new(binding), owner.clone()),
                source: complete,
                session,
                sizes,
                nonce: crate::engine::id(),
                _index_owner: index_owner,
            })
        })?;
    drop(map_owner);
    Ok(provisional)
}

impl<'a, R: Read + Seek, S: ImportSession> ProvisionalImport<'a, R, S> {
    /// Read-only rendering is permitted; this object has no editable Document.
    pub(crate) fn scene(&self) -> &SceneSnapshot {
        &self.scene
    }
    pub(crate) fn compare(
        &self,
        provider: &mut impl CapturedRows,
        validate_semantics: &dyn Fn(&SceneSnapshot) -> Result<(), String>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<ComparedImport, String> {
        check()?;
        self.session.check()?;
        validate_semantics(&self.scene)?;
        if !std::ptr::eq(provider.scene(), &self.scene) {
            return Err("Imported native row provider belongs to another provisional scene".into());
        }
        let mut expected = ScratchBytes::new(
            self.scene.budget(),
            bulk36::MAX_TILE_BYTES,
            bulk36::MAX_TILE_BYTES,
        )
        .map_err(|e| e.to_string())?;
        let mut actual =
            ScratchBytes::new(self.scene.budget(), 32000, 32000).map_err(|e| e.to_string())?;
        self.source.recheck(&self.binding, &mut expected, check)?;
        let plan = ScenePlan::new(
            &self.scene,
            self.source.index().header.format,
            &self.sizes,
            check,
        )?;
        let fidelity = plan.standard().compare(
            self.source.source(),
            self.source.index(),
            provider,
            &mut expected,
            &mut actual,
            check,
        )?;
        fidelity.check(plan.standard(), check)?;
        self.source.recheck(&self.binding, &mut expected, check)?;
        self.session.check()?;
        check()?;
        Ok(ComparedImport {
            nonce: self.nonce.clone(),
            fidelity,
        })
    }
    pub(crate) fn qualify(
        this: Admitted<Self>,
        compared: ComparedImport,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Admitted<QualifiedImport<'a, R, S>>, String> {
        this.map_unique(|candidate, _| {
            if candidate.nonce != compared.nonce {
                return Err("Imported native comparison belongs to another candidate".into());
            }
            candidate.session.check()?;
            let plan = ScenePlan::new(
                &candidate.scene,
                candidate.source.index().header.format,
                &candidate.sizes,
                check,
            )?;
            compared.fidelity.check(plan.standard(), check)?;
            drop(plan);
            let mut scratch = ScratchBytes::new(candidate.scene.budget(), 65536, 65536)
                .map_err(|e| e.to_string())?;
            candidate
                .source
                .recheck(&candidate.binding, &mut scratch, check)?;
            candidate.session.check()?;
            check()?;
            let pending = candidate.scene.into_provisional_pending()?;
            Ok(QualifiedImport {
                pending,
                binding: candidate.binding,
                source: candidate.source,
                session: candidate.session,
                _index_owner: candidate._index_owner,
            })
        })
    }
}
impl<R: Read + Seek, S: ImportSession> QualifiedImport<'_, R, S> {
    /// Invoke inside the root's final source identity/lease/revision guard. The
    /// exact source/session owners remain borrowed through the adoption callback.
    pub(crate) fn map_pending<U>(
        this: Admitted<Self>,
        check: &dyn Fn() -> Result<(), String>,
        adopt: impl FnOnce(PendingScene, &Arc<MetadataCharge>) -> Result<U, String>,
    ) -> Result<Admitted<U>, String> {
        this.map_unique(|candidate, owner| {
            candidate.session.check()?;
            let mut scratch = ScratchBytes::new(candidate.pending.budget(), 65536, 65536)
                .map_err(|e| e.to_string())?;
            candidate
                .source
                .recheck(&candidate.binding, &mut scratch, check)?;
            check()?;
            let value = adopt(candidate.pending, owner)?;
            // Bindings/source/session stay in this frame until adopt returns.
            Ok(value)
        })
    }
}
fn index_bytes(index: &Index) -> Result<usize, String> {
    let mut bytes = std::mem::size_of::<Index>() + 4096;
    for amount in [
        index.layers.capacity() * std::mem::size_of::<super::structural::Layer>(),
        index.resources.capacity() * std::mem::size_of::<super::structural::Resource>(),
        index.global_tags.capacity() * std::mem::size_of::<super::structural::Tag>(),
        index.ambiguities.capacity() * std::mem::size_of::<super::structural::Ambiguity>(),
    ] {
        bytes = bytes
            .checked_add(amount)
            .ok_or("Source36 source-index footprint overflow")?;
    }
    for layer in &index.layers {
        bytes = bytes
            .checked_add(
                layer.channels.capacity() * std::mem::size_of::<super::structural::Channel>(),
            )
            .and_then(|n| {
                n.checked_add(layer.tags.capacity() * std::mem::size_of::<super::structural::Tag>())
            })
            .ok_or("Source36 layer-index footprint overflow")?;
    }
    Ok(bytes)
}
