//! Provisional native-source import. Neither typed JSON nor valid tile hashes
//! alone grant editable authority. The caller owns semantic admission, source
//! identity/lease/revision publication and runtime ID/lineage decisions.
use super::{
    bulk36::{self, BulkLimits, Digest, Record, TileIndexEntry},
    canonical36::Limits as MetadataLimits,
    manifest36::{self, SourceBinding},
    memory36::{Admitted, MetadataCharge, WorkingSet},
    mib_progress,
    read36::{self, OwnedSource, VerificationPhase},
    row_stream::Source,
    scene_save36::{CapturedRows, ScenePlan},
    standard36::ComparedStandard,
    structural::{Index, Span},
};
use crate::{
    engine::{
        native::NativeDocument,
        scene::{Document36, PendingScene, RasterRoster, SceneIdentity, SceneSnapshot},
    },
    large_document::{
        verified_transfer_buffer, BlockRef, Error, FreshFragment, ScratchBytes, Store, TileSpec,
    },
    loading::Control,
    raster::{backed::Plane, Surface},
    workspace::source_runtime::SourceSession,
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek},
    sync::Arc,
};

/// Implemented by the caller's authenticated fresh-fragment/session owner. The
/// codec cannot recover ownership from a serialized path, UUID or directory.
pub(crate) trait ImportSession {
    fn store(&self) -> &Store;
    fn check(&self) -> Result<(), String>;
    fn check_pinned_work(&self) -> Result<(), String> {
        self.check()
    }
}
pub(crate) struct ImportLimits {
    pub metadata: MetadataLimits,
    pub bulk: BulkLimits,
    pub working: WorkingSet,
}

struct OpenFragment<'a> {
    session: &'a SourceSession,
    fragment: &'a FreshFragment,
    control: &'a Control,
}
impl ImportSession for OpenFragment<'_> {
    fn store(&self) -> &Store {
        self.fragment.store()
    }
    fn check(&self) -> Result<(), String> {
        self.session.check()
    }
    fn check_pinned_work(&self) -> Result<(), String> {
        self.control.check()?;
        self.session.check_pinned_work()
    }
}

/// Own a qualified document and its unpublished source fragment together until
/// the workspace installs the document or explicitly retires its staging.
pub(crate) struct PreparedOpen {
    pub(crate) document: NativeDocument,
    pub(crate) fragment: FreshFragment,
}

pub(crate) fn open(
    file: &mut File,
    version: crate::file_safety::Version,
    session: &SourceSession,
    project: String,
    filename: &str,
    control: &Control,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<PreparedOpen, String> {
    check()?;
    session.check()?;
    let pool = session.resident();
    let source = Source::with_held_buffer(
        file,
        version,
        verified_transfer_buffer(&pool, version.bytes).map_err(|e| e.to_string())?,
    )?;
    let metadata = MetadataLimits {
        bytes: super::canonical36::MAX_BYTES,
        nodes: 8_388_608,
        depth: 128,
    };
    let bulk = BulkLimits {
        rasters: 65536,
        tiles: 1_048_576,
        references: 1_048_576,
        chunks: 16384,
        encoded_bytes: 64 * 1024 * 1024 * 1024,
    };
    let structure = super::structural::Limits::source36(bulk.encoded_bytes, bulk.chunks)?;
    let index_owner =
        MetadataCharge::reserve(&pool, 4 * 1024 * 1024 + structure.tags * 128, check)?;
    let whole = Span {
        offset: 0,
        bytes: version.bytes,
    };
    let index = Arc::new(super::structural::index(
        &mut source.range(whole, check)?,
        structure,
        check,
    )?);
    if !index
        .resources
        .iter()
        .any(|resource| resource.peerbrush_source)
    {
        return Err("Photoshop file has no PeerBrush native source manifest".into());
    }
    super::standard36::check_layer_info(&source, &index, check)?;
    let fragment = session.fresh_fragment()?;
    let prepared = (|| {
        let owner = OpenFragment {
            session,
            fragment: &fragment,
            control,
        };
        let progress = |stage: &str, completed, total| control.progress(stage, completed, total);
        let mut initial_checksum = mib_progress(
            &progress,
            "Verifying the Photoshop file checksum",
            version.bytes,
        );
        let mut streams_checksum = mib_progress(
            &progress,
            "Rechecking the Photoshop checksum after stream verification",
            version.bytes,
        );
        let candidate = prepare(
            &source,
            index,
            index_owner,
            version.sha256(),
            &owner,
            project,
            ImportLimits {
                metadata,
                bulk,
                working: WorkingSet {
                    raw_bytes: metadata.bytes,
                    canonical_core_bytes: metadata.bytes,
                    canonical_manifest_bytes: metadata.bytes,
                    typed_metadata_bytes: metadata.bytes,
                    tile_scratch_bytes: bulk36::MAX_TILE_BYTES,
                    index_scratch_bytes: 2 * 1024 * 1024,
                },
            },
            check,
            |phase, completed| match phase {
                VerificationPhase::InitialChecksum => initial_checksum(completed),
                VerificationPhase::EditableSources { total_bytes } => progress(
                    "Checking Photoshop editable sources",
                    (if completed == total_bytes {
                        completed.div_ceil(1024 * 1024)
                    } else {
                        completed / (1024 * 1024)
                    }) as usize,
                    total_bytes.div_ceil(1024 * 1024) as usize,
                ),
                VerificationPhase::Streams { total } => progress(
                    "Verifying native channels and merged composite",
                    completed as usize,
                    total,
                ),
                VerificationPhase::AfterStreamsChecksum => streams_checksum(completed),
            },
        )?;
        control.progress("Validating editable Photoshop artwork", 0, 0)?;
        candidate.scene().validate_provisional_editable(check)?;
        let compared = super::native_rows36::with_styles(
            candidate.scene(),
            session,
            check,
            |rows| candidate.compare_rows(rows, check, &progress),
            |_| Ok(()),
        )?;
        let qualified = ProvisionalImport::qualify(candidate, compared, check, &progress)?;
        let document =
            QualifiedImport::map_pending(qualified, check, &progress, |pending, _| {
                NativeDocument::from_pending(pending)
            })?;
        document.for_runtime_registration(Some(filename), check)
    })();
    match prepared {
        Ok(document) => Ok(PreparedOpen { document, fragment }),
        Err(reason) => match session.abort_fragment(fragment, &mut || session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!("{reason}; native open staging retained: {cleanup}")),
        },
    }
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
    progress: impl FnMut(VerificationPhase, u64) -> Result<(), String>,
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
    if let Some(reason) = super::indexed_document_resource_warnings(source, &index, check)?
        .into_iter().next()
    {
        return Err(reason);
    }
    if !super::indexed_saved_composite(source, &index, check)? {
        return Err("Source36 requires a current saved Photoshop composite".into());
    }
    let working_srgb = super::indexed_working_srgb(source, &index, check)?;
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
            if doc.srgb_tagged != working_srgb {
                return Err(super::SOURCE_WORKING_PROFILE_MISMATCH.into());
            }
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
        (check, &|| session.check_pinned_work()),
        progress,
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
                        .put_guarded(
                            TileSpec::new(256, 256, spec.depth()).map_err(|e| e.to_string())?,
                            payload,
                            &mut || session.check_pinned_work().map_err(|error| {
                                Error::Io(std::io::Error::other(error))
                            }),
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
    #[cfg(test)]
    pub(crate) fn compare(
        &self,
        provider: &mut impl CapturedRows,
        validate_semantics: &dyn Fn(&SceneSnapshot) -> Result<(), String>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<ComparedImport, String> {
        check()?;
        self.session.check()?;
        validate_semantics(&self.scene)?;
        self.compare_rows(provider, check, &|_, _, _| Ok(()))
    }
    fn compare_rows(
        &self,
        provider: &mut impl CapturedRows,
        check: &dyn Fn() -> Result<(), String>,
        progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
    ) -> Result<ComparedImport, String> {
        check()?;
        self.session.check()?;
        if !std::ptr::eq(provider.scene(), &self.scene) {
            return Err("Imported native row provider belongs to another provisional scene".into());
        }
        let mut scratch = ScratchBytes::new(
            self.scene.budget(),
            bulk36::MAX_TILE_BYTES,
            bulk36::MAX_TILE_BYTES,
        )
        .map_err(|e| e.to_string())?;
        let file_bytes = self.source.index().file_bytes;
        self.source.recheck(
            &self.binding,
            &mut scratch,
            check,
            mib_progress(
                progress,
                "Verifying the Photoshop file before pixel comparison",
                file_bytes,
            ),
        )?;
        if super::indexed_resources(self.source.source(), self.source.index(), check)?
            != self.scene.metadata().psd_resources()
        {
            return Err("Source36 document resources differ from standard Photoshop data".into());
        }
        if !super::file_info::matches_indexed(
            self.source.source(),
            self.source.index(),
            self.scene.metadata().psd_file_info(),
            check,
        )? {
            return Err(super::file_info::SOURCE_MISMATCH.into());
        }
        let plan = ScenePlan::for_import(&self.scene, self.source.source(), self.source.index(), &self.sizes, check)?;
        let (expected, actual) = scratch.split_at_mut(bulk36::MAX_TILE_BYTES / 2);
        let fidelity = plan.standard().compare(
            self.source.source(),
            self.source.index(),
            provider,
            expected,
            actual,
            check,
            |completed, total| progress("Comparing saved Photoshop pixels", completed, total),
        )?;
        fidelity.check(plan.standard(), check)?;
        self.source.recheck(
            &self.binding,
            &mut scratch,
            check,
            mib_progress(
                progress,
                "Verifying the Photoshop file after pixel comparison",
                file_bytes,
            ),
        )?;
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
        progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
    ) -> Result<Admitted<QualifiedImport<'a, R, S>>, String> {
        this.map_unique(|candidate, _| {
            if candidate.nonce != compared.nonce {
                return Err("Imported native comparison belongs to another candidate".into());
            }
            candidate.session.check()?;
            let plan = ScenePlan::for_import(
                &candidate.scene,
                candidate.source.source(),
                candidate.source.index(),
                &candidate.sizes,
                check,
            )?;
            compared.fidelity.check(plan.standard(), check)?;
            drop(plan);
            let file_bytes = candidate.source.index().file_bytes;
            let mut scratch = verified_transfer_buffer(
                candidate.scene.budget(),
                file_bytes.min(bulk36::MAX_TILE_BYTES as u64),
            )
            .map_err(|e| e.to_string())?;
            candidate.source.recheck(
                &candidate.binding,
                &mut scratch,
                check,
                mib_progress(
                    progress,
                    "Checking the Photoshop file before preparing layers",
                    file_bytes,
                ),
            )?;
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
    /// Invoke inside the caller's final source identity/lease/revision guard. The
    /// exact source/session owners remain borrowed through the adoption callback.
    pub(crate) fn map_pending<U>(
        this: Admitted<Self>,
        check: &dyn Fn() -> Result<(), String>,
        progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
        adopt: impl FnOnce(PendingScene, &Arc<MetadataCharge>) -> Result<U, String>,
    ) -> Result<Admitted<U>, String> {
        this.map_unique(|candidate, owner| {
            candidate.session.check()?;
            let file_bytes = candidate.source.index().file_bytes;
            let mut scratch = verified_transfer_buffer(
                candidate.pending.budget(),
                file_bytes.min(bulk36::MAX_TILE_BYTES as u64),
            )
            .map_err(|e| e.to_string())?;
            candidate.source.recheck(
                &candidate.binding,
                &mut scratch,
                check,
                mib_progress(
                    progress,
                    "Checking the Photoshop file before opening layers",
                    file_bytes,
                ),
            )?;
            check()?;
            candidate.session.check()?;
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
