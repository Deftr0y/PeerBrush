//! Guarded Source36 save result. Fields are private; generic callbacks, checksums
//! or individual chunk completions cannot manufacture publication authority.
use super::{
    bulk36::{self, BulkLimits, BulkVerifier, Digest, TileIndexEntry},
    canonical36::{Canonical, Limits as MetadataLimits, Stats},
    container36::{self, Profile},
    manifest36::{self, Manifest, StagedCore},
    memory36::{Admitted, MetadataCharge, WorkingSet},
    row_stream::Source,
    scene_save36::{CapturedRows, ScenePlan},
    source_bulk36::SourceIndex,
    standard36::ComparedStandard,
    structural::{self, Format, Span},
};
use crate::{
    engine::{
        native::Metadata as DocumentMetadata,
        scene::{Document36, SceneSnapshot},
    },
    large_document::{ByteRole, CompletedBytes, Error, LargeSourceTemporary, ScratchBytes},
};
use ring::digest::{Context, SHA256};
use std::{
    cell::Cell,
    io::{Read, Write},
    path::Path,
    sync::Arc,
};

pub(crate) struct SaveLimits {
    pub metadata: MetadataLimits,
    pub bulk: BulkLimits,
    pub file_bytes: u64,
}
/// Each path is a fresh namespace under the caller's already owned profile or
/// session operation. Constructor checks never grant arbitrary path authority.
pub(crate) struct OwnedRoots<'a> {
    pub layers: &'a Path,
    pub composite: &'a Path,
    pub manifest: &'a Path,
    pub container: &'a Path,
}

/// Stage, seal, reopen and compare an entire captured native source save before
/// granting the closed publication result. The final publication transaction is
/// still responsible for destination lease/backup/identity/final revision guards.
pub(crate) fn prepare<'a>(
    scene: &'a SceneSnapshot,
    format: Format,
    roots: OwnedRoots<'_>,
    provider: &mut impl CapturedRows,
    limits: SaveLimits,
    check: &dyn Fn() -> Result<(), String>,
    cleanup: &dyn Fn() -> Result<(), String>,
) -> Result<CompletedSave36<'a>, String> {
    check()?;
    limits.metadata.validate()?;
    if !std::ptr::eq(scene, provider.scene())
        || limits.file_bytes == 0
        || limits.file_bytes > 64 * 1024 * 1024 * 1024
    {
        return Err("Source36 save provider or file admission differs from captured scene".into());
    }
    let sources = SourceIndex::new(scene, limits.bulk, check)?;
    let plan = ScenePlan::new(scene, format, sources.sizes(), check)?;
    if plan
        .standard()
        .layer_bytes()
        .checked_add(plan.standard().composite_bytes())
        .and_then(|n| n.checked_add(78))
        .is_none_or(|n| n > limits.file_bytes)
    {
        return Err("Source36 standard data exceeds declared final file admission".into());
    }
    let (core, mut working) = stage_core(scene, &sources, limits.metadata, check)?;
    let core_sha = core.sha256();
    sources.check_core(&core)?;
    let mut scratch = ScratchBytes::new(
        scene.budget(),
        bulk36::MAX_TILE_BYTES,
        bulk36::MAX_TILE_BYTES,
    )
    .map_err(|e| e.to_string())?;
    let mut staging = Vec::new();
    staging
        .try_reserve_exact(4)
        .map_err(|_| "Source36 staging receipt allocation refused")?;
    let result: Result<ComparedStandard, String> = (|| {
        let (layers, layer_written) = stage_file(
            roots.layers,
            plan.standard().layer_bytes(),
            ByteRole::LayerData,
            scene,
            core_sha,
            check,
            cleanup,
            |output| {
                plan.standard().write_layers(
                    output,
                    provider,
                    &mut scratch,
                    check,
                    |ordinal, output| sources.write(ordinal, core_sha, output, check),
                )
            },
        )?;
        staging.push(layers);
        if staging[0].payload_sha256() != layer_written.sha256
            || staging[0].payload_length() != layer_written.bytes
        {
            return Err("Source36 owned layer seal differs from emitted native data".into());
        }
        let (composite, composite_written) = stage_file(
            roots.composite,
            plan.standard().composite_bytes(),
            ByteRole::CompositeData,
            scene,
            core_sha,
            check,
            cleanup,
            |output| {
                plan.standard()
                    .write_composite(output, provider, &mut scratch, check)
            },
        )?;
        staging.push(composite);
        if staging[1].payload_sha256() != composite_written.sha256
            || staging[1].payload_length() != composite_written.bytes
        {
            return Err(
                "Source36 owned composite seal differs from emitted current composite".into(),
            );
        }
        let standard_sha = {
            let mut layer_check = || io_check(check);
            let mut composite_check = || io_check(check);
            let mut layer = staging[0]
                .cursor(0, staging[0].payload_length(), &mut layer_check)
                .map_err(|e| e.to_string())?;
            let mut composite = staging[1]
                .cursor(0, staging[1].payload_length(), &mut composite_check)
                .map_err(|e| e.to_string())?;
            bulk36::standard_digest(
                &mut layer,
                staging[0].payload_length(),
                &mut composite,
                staging[1].payload_length(),
                check,
            )?
        };
        let manifest = core.map_unique(|core, _| {
            Manifest::bind_core(
                core,
                plan.standard().layer_bytes(),
                plan.standard().composite_bytes(),
                standard_sha,
                layer_written.descriptors,
                limits.bulk,
                check,
            )
        })?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(manifest.plan().tiles as usize)
            .map_err(|_| "Source36 verification index allocation refused")?;
        entries.resize(manifest.plan().tiles as usize, TileIndexEntry::vacant());
        let mut counts = Vec::new();
        counts
            .try_reserve_exact(manifest.plan().rasters.len())
            .map_err(|_| "Source36 raster counter allocation refused")?;
        counts.resize(manifest.plan().rasters.len(), 0u64);
        let bulk = {
            let mut verifier = BulkVerifier::new(
                manifest.plan(),
                limits.bulk,
                &mut entries,
                &mut counts,
                check,
            )?;
            for ordinal in 0..sources.sizes().len() {
                let span = plan.standard().chunk_span(ordinal)?;
                let mut guarded = || io_check(check);
                let mut input = staging[0]
                    .cursor(span.offset, span.bytes, &mut guarded)
                    .map_err(|e| e.to_string())?;
                let mut prefix = [0; 24];
                input.read_exact(&mut prefix).map_err(|e| e.to_string())?;
                use std::io::{Seek, SeekFrom};
                input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
                let header = bulk36::Header {
                    ordinal: ordinal as u32,
                    total: sources.sizes().len() as u32,
                    records: u32::from_be_bytes(prefix[20..24].try_into().unwrap()),
                    core: core_sha,
                };
                verifier.chunk(&mut input, header, &mut scratch, check, |_| Ok(()))?;
            }
            verifier.finish(check)?
        };
        let standard = {
            let mut layer_check = || io_check(check);
            let mut composite_check = || io_check(check);
            manifest.verify_standard(
                &mut staging[0]
                    .cursor(0, staging[0].payload_length(), &mut layer_check)
                    .map_err(|e| e.to_string())?,
                staging[0].payload_length(),
                &mut staging[1]
                    .cursor(0, staging[1].payload_length(), &mut composite_check)
                    .map_err(|e| e.to_string())?,
                staging[1].payload_length(),
                check,
            )?
        };
        // Deterministic two-pass compression admits an exact owned append sink.
        let compressed = container36::compress_manifest(
            &manifest,
            limits.metadata,
            &mut std::io::sink(),
            check,
        )?;
        working.raw_bytes = compressed
            .decoded_bytes()
            .checked_next_power_of_two()
            .unwrap_or(limits.metadata.bytes)
            .min(limits.metadata.bytes);
        let (manifest_file, _) = stage_file(
            roots.manifest,
            compressed.bytes(),
            ByteRole::SourceBulk,
            scene,
            core_sha,
            check,
            cleanup,
            |output| container36::compress_manifest(&manifest, limits.metadata, output, check),
        )?;
        staging.push(manifest_file);
        let profile = scene.metadata().srgb_tagged().then(|| Profile {
            bytes: crate::color_profile::srgb_profile(),
        });
        let layout = container36::LayoutPlan::new(
            plan.standard().header(),
            staging[0].payload_length(),
            staging[1].payload_length(),
            compressed.bytes(),
            profile.as_ref(),
        )?;
        if layout.output_bytes() > limits.file_bytes {
            return Err("Source36 final output exceeds declared file admission".into());
        }
        let (candidate, written) = stage_file(
            roots.container,
            layout.output_bytes(),
            ByteRole::PhotoshopContainer,
            scene,
            core_sha,
            check,
            cleanup,
            |output| {
                let mut layer_check = || io_check(check);
                let mut composite_check = || io_check(check);
                let mut manifest_check = || io_check(check);
                container36::write(
                    output,
                    plan.standard().header(),
                    &manifest,
                    limits.metadata,
                    &compressed,
                    &mut staging[2]
                        .cursor(0, staging[2].payload_length(), &mut manifest_check)
                        .map_err(|e| e.to_string())?,
                    &standard,
                    &bulk,
                    &mut staging[0]
                        .cursor(0, staging[0].payload_length(), &mut layer_check)
                        .map_err(|e| e.to_string())?,
                    &mut staging[1]
                        .cursor(0, staging[1].payload_length(), &mut composite_check)
                        .map_err(|e| e.to_string())?,
                    profile.as_ref(),
                    &mut scratch[..65536],
                    check,
                )
            },
        )?;
        staging.push(candidate);
        if staging[3].payload_sha256() != written.sha256()
            || staging[3].payload_length() != written.bytes()
        {
            return Err("Source36 assembled Photoshop container seal differs".into());
        }
        // Every large writer-side DOM/raw/canonical envelope is dead before a
        // separately admitted readback graph is built. Native scene owners stay.
        drop(manifest);
        drop(entries);
        drop(counts);
        let candidate = &staging[3];
        let mut guarded = || io_check(check);
        let source = Source::new(
            candidate
                .cursor(0, candidate.payload_length(), &mut guarded)
                .map_err(|e| e.to_string())?,
            candidate.payload_length(),
        )?;
        let whole = Span {
            offset: 0,
            bytes: candidate.payload_length(),
        };
        let mut index_limits = structural::Limits::current();
        index_limits.file_bytes = limits.file_bytes;
        index_limits.max_edge = 16000;
        index_limits.pixels = 16000 * 16000;
        index_limits.layers = super::standard36::MAX_STANDARD_RECORDS;
        index_limits.row_bytes = 32000;
        index_limits.coordinate = 116000;
        index_limits.mask_pixels8 = 16000 * 16000;
        index_limits.mask_pixels16 = 16000 * 16000;
        index_limits.tags = limits
            .bulk
            .chunks
            .checked_add(4096)
            .ok_or("Source36 index tag admission overflow")?;
        index_limits.resources = 2;
        let _parsed_index_owner = MetadataCharge::reserve(
            scene.budget(),
            4 * 1024 * 1024 + index_limits.tags * 128,
            check,
        )?;
        let index = structural::index(&mut source.range(whole, check)?, index_limits, || check())?;
        let resource = index
            .resources
            .iter()
            .find(|r| r.peerbrush_source)
            .ok_or("Saved Source36 manifest resource is missing")?;
        let encoded = Span {
            offset: resource.data.offset + 4,
            bytes: resource
                .data
                .bytes
                .checked_sub(4)
                .ok_or("Saved Source36 manifest marker is truncated")?,
        };
        let parsed = manifest36::inflate_admitted(
            &mut source.range(encoded, check)?,
            encoded.bytes,
            limits.metadata,
            limits.bulk,
            working,
            scene.budget(),
            check,
        )?;
        compressed.check_origin(&parsed)?;
        let parsed = parsed.map_unique(|manifest, _| {
            manifest.into_typed::<Document36>(
                limits.metadata,
                check,
                |doc, roster, id, revision| {
                    if id != scene.metadata().id()
                        || revision != scene.metadata().revision()
                        || doc.id != id
                        || doc.revision != revision
                        || doc.width != scene.metadata().width()
                        || doc.height != scene.metadata().height()
                        || doc.bit_depth != scene.metadata().depth()
                    {
                        return Err(
                            "Reopened Source36 typed scene identity/geometry differs".into()
                        );
                    }
                    let visited = doc.visit_rasters(|r| {
                        let entry = roster
                            .get(r.id as usize - 1)
                            .ok_or("Reopened Source36 raster reference is missing")?;
                        if entry.id != r.id
                            || entry.width != r.width
                            || entry.height != r.height
                            || entry.depth != r.depth
                        {
                            return Err("Reopened Source36 typed raster roster differs".into());
                        }
                        Ok(())
                    })?;
                    if visited != roster.len() {
                        return Err(
                            "Reopened Source36 typed raster roster has unused entries".into()
                        );
                    }
                    Ok(())
                },
            )
        })?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(parsed.plan().tiles as usize)
            .map_err(|_| "Source36 readback tile index allocation refused")?;
        entries.resize(parsed.plan().tiles as usize, TileIndexEntry::vacant());
        let mut counts = Vec::new();
        counts
            .try_reserve_exact(parsed.plan().rasters.len())
            .map_err(|_| "Source36 readback raster index allocation refused")?;
        counts.resize(parsed.plan().rasters.len(), 0u64);
        let complete = super::read36::verify(
            &source,
            &index,
            candidate.payload_sha256(),
            &parsed,
            super::read36::VerificationLimits {
                bulk: limits.bulk,
                native_streams: super::standard36::MAX_STANDARD_RECORDS * 5 + 1,
            },
            &mut entries,
            &mut counts,
            &mut scratch,
            check,
            |_| Ok(()),
        )?;
        let mut actual =
            ScratchBytes::new(scene.budget(), 32000, 32000).map_err(|e| e.to_string())?;
        let fidelity =
            plan.standard()
                .compare(&source, &index, provider, &mut scratch, &mut actual, check)?;
        fidelity.check(plan.standard(), check)?;
        complete.recheck(
            &source,
            &index,
            &parsed,
            candidate.payload_sha256(),
            &mut scratch,
            check,
        )?;
        check()?;
        Ok(fidelity)
    })();
    match result {
        Ok(fidelity) => {
            let candidate = staging
                .pop()
                .ok_or("Source36 completed candidate receipt is missing")?;
            Ok(CompletedSave36 {
                scene,
                bytes: candidate.payload_length(),
                sha256: candidate.payload_sha256(),
                candidate,
                staging,
                provenance: super::scene_save36::provenance(scene),
                core_sha256: core_sha,
                _standard_fidelity: fidelity,
                failed: Cell::new(false),
            })
        }
        Err(error) => {
            let mut retirement = None;
            for file in staging.into_iter().rev() {
                let mut guarded = || io_check(cleanup);
                if let Err(e) = file.retire(&mut guarded) {
                    if retirement.is_none() {
                        retirement = Some(e.to_string());
                    }
                }
            }
            Err(retirement.map_or(error.clone(), |e| {
                format!("{error}; owned staging retirement also failed: {e}")
            }))
        }
    }
}

fn stage_file<T>(
    root: &Path,
    length: u64,
    role: ByteRole,
    scene: &SceneSnapshot,
    binding: Digest,
    check: &dyn Fn() -> Result<(), String>,
    cleanup: &dyn Fn() -> Result<(), String>,
    write: impl FnOnce(&mut dyn Write) -> Result<T, String>,
) -> Result<(CompletedBytes, T), String> {
    check()?;
    let mut temporary = LargeSourceTemporary::create(
        root,
        length,
        role,
        scene.budget().clone(),
        scene.disk_budget().clone(),
    )
    .map_err(|e| e.to_string())?;
    let result = {
        let mut guarded = || io_check(check);
        write(&mut temporary.writer(&mut guarded))
    };
    match result {
        Ok(value) => {
            let mut guarded = || io_check(check);
            Ok((
                temporary
                    .finish(binding, &mut guarded)
                    .map_err(|e| e.to_string())?,
                value,
            ))
        }
        Err(error) => {
            let mut guarded = || io_check(cleanup);
            match temporary.abort(&mut guarded) {
                Ok(()) => Err(error),
                Err(e) => Err(format!(
                    "{error}; owned staging retirement also failed: {e}"
                )),
            }
        }
    }
}
fn io_check(check: &dyn Fn() -> Result<(), String>) -> crate::large_document::Result<()> {
    check().map_err(|e| Error::Io(std::io::Error::other(e)))
}

fn stage_core<'a>(
    scene: &'a SceneSnapshot,
    index: &SourceIndex<'a>,
    limits: MetadataLimits,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(Admitted<StagedCore<DocumentMetadata<'a>>>, WorkingSet), String> {
    let header_bytes = index
        .rasters()
        .len()
        .checked_mul(
            std::mem::size_of::<manifest36::Raster>() + std::mem::size_of::<bulk36::RasterSpec>(),
        )
        .and_then(|n| n.checked_add(4096))
        .ok_or("Source36 writer header memory overflow")?;
    let header_owner = MetadataCharge::reserve(scene.budget(), header_bytes, check)?;
    let core = index.core();
    let length = Canonical::raw_length(&core, limits, check)?;
    let raw_capacity = length
        .checked_next_power_of_two()
        .unwrap_or(limits.bytes)
        .min(limits.bytes);
    let raw_owner = MetadataCharge::reserve(
        scene.budget(),
        raw_capacity
            .checked_mul(3)
            .and_then(|n| n.checked_add(2 * 1024 * 1024))
            .ok_or("Source36 writer preflight workspace overflow")?,
        check,
    )?;
    let raw = Canonical::serialize_raw(&core, limits, check)?;
    if raw.len() != length || raw.capacity() > raw_capacity {
        return Err("Source36 immutable writer metadata changed during admission".into());
    }
    let stats = Stats::scan(&raw, limits, check)?;
    let extra = index
        .sizes()
        .len()
        .checked_mul(4096)
        .and_then(|n| n.checked_add(2 * 1024 * 1024))
        .and_then(|n| {
            BulkVerifier::index_bytes(core.tiles as usize, core.rasters.len())
                .ok()
                .and_then(|b| n.checked_add(b))
        })
        .ok_or("Source36 manifest descriptor workspace overflow")?;
    let working = WorkingSet {
        raw_bytes: raw_capacity,
        canonical_core_bytes: limits.bytes,
        canonical_manifest_bytes: limits.bytes,
        typed_metadata_bytes: stats
            .tree_upper_bytes()?
            .checked_add(header_bytes + extra)
            .ok_or("Source36 typed metadata footprint overflow")?,
        tile_scratch_bytes: bulk36::MAX_TILE_BYTES,
        index_scratch_bytes: extra,
    };
    let owner = MetadataCharge::reserve(scene.budget(), working.total(&stats)?, check)?;
    drop(raw);
    drop(raw_owner);
    let staged = StagedCore::stage(
        core,
        limits,
        BulkLimits {
            rasters: 65536,
            tiles: 1_048_576,
            references: 1_048_576,
            chunks: 16384,
            encoded_bytes: 64 * 1024 * 1024 * 1024,
        },
        check,
    )?;
    drop(header_owner);
    Ok((Admitted::from_parts(Arc::new(staged), owner), working))
}

/// Only the full scene save driver may construct this after reopened complete
/// source/native stream validation and exact standard metadata/sample fidelity.
/// Retaining the snapshot keeps its metadata, Surface owners and leases alive.
pub(crate) struct CompletedSave36<'a> {
    scene: &'a SceneSnapshot,
    candidate: CompletedBytes,
    staging: Vec<CompletedBytes>,
    bytes: u64,
    sha256: Digest,
    provenance: Digest,
    core_sha256: Digest,
    _standard_fidelity: ComparedStandard,
    failed: Cell<bool>,
}
impl CompletedSave36<'_> {
    pub(crate) fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
    pub(crate) fn candidate(&self) -> &CompletedBytes {
        &self.candidate
    }
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn sha256(&self) -> Digest {
        self.sha256
    }
    pub(crate) fn provenance(&self) -> Digest {
        self.provenance
    }
    pub(crate) fn core_sha256(&self) -> Digest {
        self.core_sha256
    }
    /// Reobserve the entire owned candidate immediately before destination copy.
    /// Source file identity/lease and engine revision are the caller's guard;
    /// each supplied error is preserved through the owned I/O callback.
    pub(crate) fn recheck(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        if self.failed.replace(true) {
            return Err("Source36 completed save is failed".into());
        }
        let result = self.recheck_inner(check);
        if result.is_ok() {
            self.failed.set(false);
        }
        result
    }
    fn recheck_inner(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        check()?;
        if super::scene_save36::provenance(self.scene) != self.provenance
            || self.candidate.role() != ByteRole::PhotoshopContainer
            || self.candidate.binding() != self.core_sha256
            || !self
                .candidate
                .uses_budgets(self.scene.budget(), self.scene.disk_budget())
            || self.candidate.payload_length() != self.bytes
            || self.candidate.payload_sha256() != self.sha256
        {
            return Err(
                "Source36 save candidate differs from its captured source/owned completion".into(),
            );
        }
        let mut transfer =
            ScratchBytes::new(self.scene.budget(), 65536, 65536).map_err(|e| e.to_string())?;
        let mut guarded = || check().map_err(|e| Error::Io(std::io::Error::other(e)));
        let mut input = self
            .candidate
            .cursor(0, self.bytes, &mut guarded)
            .map_err(|e| e.to_string())?;
        let mut left = self.bytes;
        let mut hash = Context::new(&SHA256);
        while left != 0 {
            check()?;
            let n = left.min(65536) as usize;
            input
                .read_exact(&mut transfer[..n])
                .map_err(|e| e.to_string())?;
            hash.update(&transfer[..n]);
            left -= n as u64;
        }
        if input.read(&mut [0; 1]).map_err(|e| e.to_string())? != 0
            || hash.finish().as_ref() != &self.sha256[..]
        {
            return Err("Source36 save candidate bytes changed after qualification".into());
        }
        check()
    }
    /// Call only after destination publication/abort no longer needs staging.
    /// Verified retirement never removes a foreign replacement. Failed remains
    /// charged in the persistent operation inventory for admitted later cleanup.
    pub(crate) fn retire(self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        let mut first = None;
        for file in self
            .staging
            .into_iter()
            .chain(std::iter::once(self.candidate))
        {
            let mut guarded = || check().map_err(|e| Error::Io(std::io::Error::other(e)));
            if let Err(error) = file.retire(&mut guarded) {
                if first.is_none() {
                    first = Some(error.to_string());
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
#[path = "save36_tests.rs"]
mod tests;
