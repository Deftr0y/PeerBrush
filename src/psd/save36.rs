//! Guarded Source36 save result. Fields are private; generic callbacks, checksums
//! or individual chunk completions cannot manufacture publication authority.
use super::{
    bulk36::{self, BulkLimits, BulkVerifier, Digest, TileIndexEntry},
    canonical36::{Canonical, Limits as MetadataLimits, Stats},
    container36::{self, ImageResources},
    manifest36::{self, CompletedStandard, Manifest, StagedCore},
    memory36::{Admitted, MetadataCharge, WorkingSet},
    mib_progress,
    read36::{self, VerificationPhase},
    row_stream::Source,
    scene_save36::{CapturedRows, ScenePlan},
    source_bulk36::SourceIndex,
    standard36::ComparedStandard,
    structural::{self, Format, Span},
};
#[cfg(test)]
use crate::large_document::CompletedBytes;
use crate::{
    engine::{
        native::Metadata as DocumentMetadata,
        scene::{Document36, SceneSnapshot},
    },
    large_document::{
        verified_transfer_buffer, BufferedWrite, ByteRole, Error, ScratchBytes, VERIFIED_BATCH_BYTES,
    },
    workspace::source_runtime::SourceSession,
};
#[path = "save36_ownership.rs"]
mod ownership;
#[cfg(test)]
use ownership::SaveCursor;
use ownership::{StagedBytes, StagingFactory};
use ring::digest::{Context, SHA256};
#[cfg(test)]
use std::path::Path;
use std::{
    cell::Cell,
    io::Write,
    sync::Arc,
};

pub(crate) struct SaveLimits {
    pub metadata: MetadataLimits,
    pub bulk: BulkLimits,
    pub file_bytes: u64,
}
/// Private fixture namespaces. Production construction requires SourceSession.
#[cfg(test)]
struct OwnedRoots<'a> {
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
    session: &'a SourceSession,
    provider: &mut impl CapturedRows,
    limits: SaveLimits,
    progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
    check: &dyn Fn() -> Result<(), String>,
    cleanup: &dyn Fn() -> Result<(), String>,
) -> Result<CompletedSave36<'a>, String> {
    // The factory supplies the source-session and retirement checks.
    prepare_owned(
        scene,
        format,
        &session,
        provider,
        limits,
        progress,
        check,
        cleanup,
    )
}
#[cfg(test)]
fn prepare_fixture<'a>(
    scene: &'a SceneSnapshot,
    format: Format,
    roots: OwnedRoots<'_>,
    provider: &mut impl CapturedRows,
    limits: SaveLimits,
    check: &dyn Fn() -> Result<(), String>,
    cleanup: &dyn Fn() -> Result<(), String>,
) -> Result<CompletedSave36<'a>, String> {
    prepare_owned(
        scene,
        format,
        &roots,
        provider,
        limits,
        &|_, _, _| Ok(()),
        check,
        cleanup,
    )
}
fn prepare_owned<'a>(
    scene: &'a SceneSnapshot,
    format: Format,
    factory: &impl StagingFactory<'a>,
    provider: &mut impl CapturedRows,
    limits: SaveLimits,
    progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
    check: &dyn Fn() -> Result<(), String>,
    cleanup: &dyn Fn() -> Result<(), String>,
) -> Result<CompletedSave36<'a>, String> {
    let pinned = || {
        check()?;
        factory.check_pinned_work()
    };
    let active = || {
        check()?;
        factory.check()
    };
    let retiring = || {
        cleanup()?;
        factory.cleanup_check()
    };
    let check = &active;
    let cleanup = &retiring;
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
        let layer_bytes = plan.standard().layer_bytes();
        let composite_bytes = plan.standard().composite_bytes();
        let source_bytes = sources.sizes().iter().map(|&bytes| u64::from(bytes)).sum();
        let mut standard_hash = bulk36::standard_hasher(layer_bytes, composite_bytes);
        let (layers, layer_written) = stage_file(
            factory,
            layer_bytes,
            ByteRole::LayerData,
            scene,
            core_sha,
            check,
            cleanup,
            |output| {
                let mut output = StandardWriter {
                    output,
                    hash: &mut standard_hash,
                };
                let mut report_sources = mib_progress(
                    progress,
                    "Writing Photoshop editable sources",
                    source_bytes,
                );
                let mut completed_source_bytes = 0;
                let written = plan.standard().write_layers_with_checks(
                    &mut output,
                    provider,
                    &mut scratch,
                    (check, &pinned),
                    |completed, total| {
                        progress("Encoding native Photoshop layers", completed, total)
                    },
                    |ordinal, output| {
                        if ordinal == 0 {
                            report_sources(0)?;
                        }
                        let descriptor = sources.write(ordinal, core_sha, output, &pinned, check)?;
                        completed_source_bytes += u64::from(descriptor.bytes);
                        report_sources(completed_source_bytes)?;
                        Ok(descriptor)
                    },
                )?;
                progress("Sealing native Photoshop layers", 0, 0)?;
                Ok(written)
            },
        )?;
        staging.push(layers);
        if staging[0].payload_sha256() != layer_written.sha256
            || staging[0].payload_length() != layer_written.bytes
        {
            return Err("Source36 owned layer seal differs from emitted native data".into());
        }
        progress("Preparing the native Photoshop merged composite", 0, 0)?;
        let (composite, composite_written) = stage_file(
            factory,
            composite_bytes,
            ByteRole::CompositeData,
            scene,
            core_sha,
            check,
            cleanup,
            |output| {
                let mut output = StandardWriter {
                    output,
                    hash: &mut standard_hash,
                };
                let written = plan.standard().write_composite(
                    &mut output,
                    provider,
                    &mut scratch,
                    check,
                    |completed, total| {
                        progress("Encoding the native Photoshop merged composite", completed, total)
                    },
                )?;
                progress("Sealing the native Photoshop merged composite", 0, 0)?;
                Ok(written)
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
        // Both emitted ranges matched their sealed spools. Assembly checks the
        // same aggregate digest independently while replaying those spools.
        let standard = CompletedStandard::from_digest(
            core_sha,
            layer_bytes,
            composite_bytes,
            standard_hash.finish().as_ref().try_into().unwrap(),
        );
        let manifest = core.map_unique(|core, _| {
            Manifest::bind_core(
                core,
                plan.standard().layer_bytes(),
                plan.standard().composite_bytes(),
                standard.sha256(),
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
            let mut report_bulk = mib_progress(
                progress,
                "Checking staged Photoshop editable sources",
                source_bytes,
            );
            if source_bytes != 0 {
                report_bulk(0)?;
            }
            let mut verified = 0;
            let charge = scene
                .budget()
                .reserve_bytes(VERIFIED_BATCH_BYTES, VERIFIED_BATCH_BYTES)
                .ok();
            for ordinal in 0..sources.sizes().len() {
                let span = plan.standard().chunk_span(ordinal)?;
                let mut guarded = || io_check(check);
                let mut input = staging[0]
                    .cursor(span.offset, span.bytes, &mut guarded)
                    .map_err(|e| e.to_string())?;
                let header = sources.header(ordinal, core_sha)?;
                if charge.is_some() {
                    let mut input = std::io::BufReader::with_capacity(VERIFIED_BATCH_BYTES, input);
                    verifier.chunk(&mut input, header, &mut scratch, &pinned, |_| Ok(()))?;
                } else {
                    verifier.chunk(&mut input, header, &mut scratch, &pinned, |_| Ok(()))?;
                }
                check()?;
                verified += span.bytes;
                report_bulk(verified)?;
            }
            verifier.finish(check)?
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
            factory,
            compressed.bytes(),
            ByteRole::SourceBulk,
            scene,
            core_sha,
            check,
            cleanup,
            |output| container36::compress_manifest(&manifest, limits.metadata, output, check),
        )?;
        staging.push(manifest_file);
        let image = ImageResources {
            profile: scene.metadata().srgb_tagged().then(crate::color_profile::srgb_profile),
            document: scene.metadata().psd_resources(),
            file_info: scene.metadata().psd_file_info(),
        };
        let working_bytes = super::file_info::validation_working_bytes(image.file_info)?;
        let _file_info_charge = super::memory36::MetadataCharge::reserve(scene.budget(), working_bytes, check)?;
        let layout = container36::LayoutPlan::new(
            plan.standard().header(),
            staging[0].payload_length(),
            staging[1].payload_length(),
            compressed.bytes(),
            image,
        )?;
        if layout.output_bytes() > limits.file_bytes {
            return Err("Source36 final output exceeds declared file admission".into());
        }
        let assembling = "Assembling the native Photoshop file";
        let output_bytes = layout.output_bytes();
        let mut assembly_progress = mib_progress(progress, assembling, output_bytes);
        assembly_progress(0)?;
        let (candidate, written) = stage_file(
            factory,
            output_bytes,
            ByteRole::PhotoshopContainer,
            scene,
            core_sha,
            check,
            cleanup,
            |output| {
                let mut transfer = match verified_transfer_buffer(scene.budget(), output_bytes) {
                    Ok(transfer) => Some(transfer),
                    Err(Error::Budget) => None,
                    Err(error) => return Err(error.to_string()),
                };
                let mut layer_check = || io_check(check);
                let mut composite_check = || io_check(check);
                let mut manifest_check = || io_check(check);
                let written = container36::write(
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
                    image,
                    transfer.as_deref_mut().unwrap_or(&mut scratch),
                    assembly_progress,
                    check,
                )?;
                progress("Sealing the assembled Photoshop file", 0, 0)?;
                Ok(written)
            },
        )?;
        staging.push(candidate);
        if staging[3].payload_sha256() != written.sha256()
            || staging[3].payload_length() != written.bytes()
        {
            return Err("Source36 assembled Photoshop container seal differs".into());
        }
        progress("Checking the assembled Photoshop file structure", 0, 0)?;
        // Every large writer-side DOM/raw/canonical envelope is dead before a
        // separately admitted readback graph is built. Native scene owners stay.
        drop(manifest);
        drop(entries);
        drop(counts);
        let candidate = &staging[3];
        let mut guarded = || io_check(check);
        let source = Source::with_sealed_buffer(
            candidate
                .cursor(0, candidate.payload_length(), &mut guarded)
                .map_err(|e| e.to_string())?,
            candidate.payload_length(),
            verified_transfer_buffer(scene.budget(), candidate.payload_length())
                .map_err(|e| e.to_string())?,
        )?;
        let whole = Span {
            offset: 0,
            bytes: candidate.payload_length(),
        };
        let index_limits = structural::Limits::source36(limits.file_bytes, limits.bulk.chunks)?;
        let _parsed_index_owner = MetadataCharge::reserve(
            scene.budget(),
            4 * 1024 * 1024 + index_limits.tags * 128,
            check,
        )?;
        let index = structural::index(&mut source.range(whole, check)?, index_limits, || check())?;
        if super::indexed_working_srgb(&source, &index, check)? != scene.metadata().srgb_tagged() {
            return Err(super::SOURCE_WORKING_PROFILE_MISMATCH.into());
        }
        if super::indexed_resources(&source, &index, check)? != scene.metadata().psd_resources()
            || !super::file_info::matches_indexed(&source, &index, scene.metadata().psd_file_info(), check)?
        {
            return Err("Saved Source36 document resources differ from the captured document".into());
        }
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
        let mut initial_checksum = mib_progress(
            progress,
            "Verifying the assembled Photoshop file checksum",
            candidate.payload_length(),
        );
        let mut streams_checksum = mib_progress(
            progress,
            "Rechecking the Photoshop checksum after stream verification",
            candidate.payload_length(),
        );
        let complete = read36::verify(
            &source,
            &index,
            candidate.payload_sha256(),
            &parsed,
            read36::VerificationLimits {
                bulk: limits.bulk,
                native_streams: super::standard36::MAX_STANDARD_RECORDS * 5 + 1,
            },
            &mut entries,
            &mut counts,
            &mut scratch,
            (check, &pinned),
            |phase, bytes| match phase {
                VerificationPhase::InitialChecksum => initial_checksum(bytes),
                VerificationPhase::EditableSources { total_bytes } => progress(
                    "Checking assembled Photoshop editable sources",
                    (if bytes == total_bytes {
                        bytes.div_ceil(1024 * 1024)
                    } else {
                        bytes / (1024 * 1024)
                    }) as usize,
                    total_bytes.div_ceil(1024 * 1024) as usize,
                ),
                VerificationPhase::Streams { total } => {
                    progress("Checking the assembled Photoshop file streams", bytes as usize, total)
                }
                VerificationPhase::AfterStreamsChecksum => streams_checksum(bytes),
            },
            |_| Ok(()),
        )?;
        // Each half fits the complete 128 KiPixel stripe at native16.
        let (expected, actual) = scratch.split_at_mut(bulk36::MAX_TILE_BYTES / 2);
        let fidelity = plan.standard().compare(
            &source,
            &index,
            provider,
            expected,
            actual,
            check,
            |done, total| progress("Comparing saved Photoshop pixels", done, total),
        )?;
        fidelity.check(plan.standard(), check)?;
        complete.recheck(
            &source,
            &index,
            &parsed,
            candidate.payload_sha256(),
            &mut scratch,
            check,
            mib_progress(
                progress,
                "Rechecking the assembled Photoshop file checksum",
                candidate.payload_length(),
            ),
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
                if let Err(e) = file.retire(cleanup) {
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


struct StandardWriter<'a> {
    output: &'a mut dyn Write,
    hash: &'a mut Context,
}

impl Write for StandardWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.output.write(bytes)?;
        self.hash.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}

fn stage_file<'a, T>(
    factory: &impl StagingFactory<'a>,
    length: u64,
    role: ByteRole,
    scene: &SceneSnapshot,
    binding: Digest,
    check: &dyn Fn() -> Result<(), String>,
    cleanup: &dyn Fn() -> Result<(), String>,
    write: impl FnOnce(&mut dyn Write) -> Result<T, String>,
) -> Result<(StagedBytes<'a>, T), String> {
    check()?;
    let mut temporary = factory.create(scene, length, role)?;
    let result = (|| {
        let mut guarded = || io_check(check);
        let mut output = temporary.temporary()?.writer(&mut guarded);
        if matches!(role, ByteRole::LayerData | ByteRole::CompositeData | ByteRole::PhotoshopContainer) {
            match verified_transfer_buffer(scene.budget(), length) {
                Ok(mut scratch) => {
                    let mut buffered = BufferedWrite::new(&mut output, &mut scratch);
                    let result = write(&mut buffered)?;
                    buffered.flush().map_err(|e| e.to_string())?;
                    return Ok(result);
                }
                Err(Error::Budget) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        write(&mut output)
    })();
    match result {
        Ok(value) => Ok((temporary.finish(binding, check)?, value)),
        Err(error) => match temporary.abort(cleanup) {
            Ok(()) => Err(error),
            Err(retirement) => Err(format!(
                "{error}; owned staging retirement also failed: {retirement}"
            )),
        },
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
    candidate: StagedBytes<'a>,
    staging: Vec<StagedBytes<'a>>,
    bytes: u64,
    sha256: Digest,
    provenance: Digest,
    core_sha256: Digest,
    _standard_fidelity: ComparedStandard,
    failed: Cell<bool>,
}
impl<'source> CompletedSave36<'source> {
    /// Namespace/owner proof only. Complete source/native fidelity and the final
    /// full candidate SHA recheck remain mandatory publication requirements.
    pub(crate) fn check_session(&self, session: &SourceSession) -> Result<(), String> {
        self.check_failed()?;
        if !self.scene.budget().same_pool(&session.resident())
            || !self.scene.disk_budget().same_pool(&session.disk())
        {
            return Err("Completed native save uses another resource admission".into());
        }
        self.candidate.check_session(session)?;
        for staged in &self.staging {
            staged.check_session(session)?;
        }
        Ok(())
    }
    pub(crate) fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
    #[cfg(test)]
    fn candidate(&self) -> &CompletedBytes {
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
    fn check_guards(&self) -> Result<(), String> {
        self.candidate.check()?;
        for file in &self.staging {
            file.check()?;
        }
        Ok(())
    }
    fn check_failed(&self) -> Result<(), String> {
        if self.failed.get() {
            return Err("Source36 completed save is failed".into());
        }
        Ok(())
    }
    #[cfg(test)]
    fn check_cursor_guard(&self) -> Result<(), String> {
        self.check_failed()?;
        self.candidate.check()
    }
    #[cfg(test)]
    fn cursor<'read>(
        &'read self,
        start: u64,
        length: u64,
        check: &'read mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> Result<SaveCursor<'read, 'source>, String> {
        self.check_failed()?;
        self.check_guards()?;
        let input = self
            .candidate
            .cursor(start, length, check)
            .map_err(|e| e.to_string())?;
        Ok(SaveCursor::new(self, input))
    }
    pub(crate) fn publication_buffer(&self) -> Result<ScratchBytes, String> {
        verified_transfer_buffer(self.scene.budget(), self.bytes).map_err(|e| e.to_string())
    }

    pub(crate) fn read_publication_batch(
        &self,
        session: &SourceSession,
        start: u64,
        count: usize,
        output: &mut ScratchBytes,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.read_verified_batch(start, count, output, &|| {
            self.check_session(session)?;
            check()
        })
    }

    fn read_verified_batch(
        &self,
        start: u64,
        count: usize,
        output: &mut ScratchBytes,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let mut failure = None;
        self.candidate
            .read_complete_chunks_into(start, count, output, &mut || {
                check().map_err(|error| {
                    failure = Some(error);
                    Error::Cancelled
                })
            })
            .map_err(|error| failure.unwrap_or_else(|| error.to_string()))
    }

    /// Reobserve the entire owned candidate immediately before destination copy.
    /// Source file identity/lease and engine revision are the caller's guard;
    /// each supplied error is preserved through the owned I/O callback.
    pub(crate) fn recheck(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        self.recheck_with_progress(check, &mut |_| Ok(()))
    }
    pub(crate) fn recheck_with_progress(
        &self,
        check: &dyn Fn() -> Result<(), String>,
        progress: &mut dyn FnMut(u64) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.failed.replace(true) {
            return Err("Source36 completed save is failed".into());
        }
        let result = self.recheck_inner(check, progress);
        if result.is_ok() {
            self.failed.set(false);
        }
        result
    }
    fn recheck_inner(
        &self,
        check: &dyn Fn() -> Result<(), String>,
        progress: &mut dyn FnMut(u64) -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        self.check_guards()?;
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
        let mut transfer = self.publication_buffer()?;
        let active = || {
            check()?;
            self.candidate.check()
        };
        let mut offset = 0;
        let mut hash = Context::new(&SHA256);
        progress(0)?;
        while offset < self.bytes {
            let n = (self.bytes - offset).min(transfer.len() as u64) as usize;
            self.read_verified_batch(offset, n, &mut transfer, &active)?;
            hash.update(&transfer[..n]);
            offset += n as u64;
            if offset < self.bytes {
                progress(offset)?;
            }
        }
        if hash.finish().as_ref() != &self.sha256[..] {
            return Err("Source36 save candidate bytes changed after qualification".into());
        }
        check()?;
        self.check_guards()?;
        progress(offset)
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
            if let Err(error) = file.retire(check) {
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
#[cfg(test)]
pub(crate) use tests::{with_session_candidate, with_session_candidate_join};
