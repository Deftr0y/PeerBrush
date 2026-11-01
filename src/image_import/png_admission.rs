//! Genuine session-owned PNG source staging. Every read/write runs outside
//! engine/UI locks; publication is a separate final engine decision.
use super::{
    png_surface::{PassWriter, TileIndex},
    prepared_png::{
        self, ColorPolicy, Complete, EncodedPng, FilePng, Header, Limits, PassRow, SourceBinding,
    },
};
use crate::{
    large_document::{BlockRef, FreshFragment, Reservation, ScratchBytes, Snapshot},
    raster::Surface,
    workspace::source_runtime::SourceSession,
};
use std::cell::RefCell;

/// Fully sealed, still unpublished source. Dropping it never deletes files or
/// refunds disk inventory. Call abort explicitly after abandoning a candidate.
pub(crate) struct StagedPng {
    surface: Surface,
    complete: Complete,
    seal: Snapshot,
    fragment: FreshFragment,
}
impl StagedPng {
    /// Provisional worker input only. A clone must not escape into live document,
    /// history, clipboard or recovery authority before the publication mark.
    pub(crate) fn surface(&self) -> &Surface {
        &self.surface
    }
    pub(crate) fn complete(&self) -> &Complete {
        &self.complete
    }
    pub(crate) fn seal(&self) -> &Snapshot {
        &self.seal
    }

    /// Infallible, memory-only final mark. The engine must recheck the captured
    /// revision, actor/lease, source, selection, readonly state and cancellation
    /// immediately before calling this and atomically adopting the result.
    pub(crate) fn publish(self) -> (Surface, Complete, Snapshot) {
        let Self {
            surface,
            complete,
            seal,
            fragment,
        } = self;
        drop(fragment.publish());
        (surface, complete, seal)
    }
    pub(crate) fn abort(self, session: &SourceSession) -> Result<(), String> {
        let Self {
            surface,
            complete,
            seal,
            fragment,
        } = self;
        drop((surface, complete, seal));
        session.abort_fragment(fragment, &mut || Ok(()))
    }
}

/// Preserves raw native channels and original orientation/ICC facts. Selection
/// explicitly chooses RawSelection's legacy default-IDAT/no-CMS policy. No live
/// canvas limit is changed by a caller selecting larger component limits.
pub(crate) fn stage(
    session: &SourceSession,
    encoded: &EncodedPng,
    limits: Limits,
    color: ColorPolicy,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<StagedPng, String> {
    let binding = encoded.binding(&session.resident())?;
    stage_with(session, binding, color, check, |guard, row| {
        prepared_png::decode_pass_rows(encoded, limits, color, &session.resident(), guard, row)
    })
}

pub(crate) fn stage_file(
    session: &SourceSession,
    file: &mut FilePng<'_>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<StagedPng, String> {
    let binding = file.binding(session)?;
    let color = file.color_policy();
    stage_with(session, binding, color, check, |guard, row| {
        file.decode_pass_rows(guard, row)
    })
}

type RowSink<'a> = dyn FnMut(Header, PassRow, &[u8]) -> Result<(), String> + 'a;
fn stage_with(
    session: &SourceSession,
    binding: SourceBinding,
    color: ColorPolicy,
    check: &mut dyn FnMut() -> Result<(), String>,
    decode: impl FnOnce(
        &mut dyn FnMut() -> Result<(), String>,
        &mut RowSink<'_>,
    ) -> Result<Complete, String>,
) -> Result<StagedPng, String> {
    session.check()?;
    check()?;
    if !binding.uses_budget(&session.resident()) || color == ColorPolicy::ToSrgb {
        return Err("Native PNG staging requires its actual pool and raw color policy".into());
    }
    let mut partial = None;
    let mut final_fragment = None;
    let result: Result<StagedPng, String> = (|| {
        let caller = RefCell::new(check);
        let mut guarded = || {
            session.check()?;
            (caller.borrow_mut())()
        };
        let mut writer = None;
        let complete = decode(&mut guarded, &mut |header, row, bytes| {
            if writer.is_none() {
                let fragment = session.fresh_fragment()?;
                partial = Some(fragment);
                writer = Some(PassWriter::new_binding(
                    header,
                    row,
                    partial.as_ref().unwrap().store(),
                    &binding,
                    color,
                )?);
            }
            writer.as_mut().unwrap().push(header, row, bytes, &mut || {
                session.check()?;
                (caller.borrow_mut())()
            })
        })?;
        let passes = writer
            .ok_or("PNG source completed without native rows")?
            .finish(complete)?;
        let index = if passes.is_plain() {
            let index = passes.into_plain()?;
            final_fragment = partial.take();
            index
        } else {
            final_fragment = Some(session.fresh_fragment()?);
            let index = passes.merge(final_fragment.as_ref().unwrap().store(), &mut guarded)?;
            // The merge consumes all pass maps/pins/store clones. Ignore only
            // obsolete gesture cancellation for cleanup; the session still
            // checks the real profile/instance/filesystem ownership itself.
            session
                .abort_fragment(partial.take().unwrap(), &mut || Ok(()))
                .map_err(|e| format!("PNG pass cleanup retained inventory: {e}"))?;
            index
        };
        guarded()?;
        let metadata = metadata(&index, &session.resident(), &mut guarded)?;
        let references = References::new(&index, &session.resident())?;
        guarded()?;
        let seal = session.seal_store(index.store(), &references.values, &metadata)?;
        guarded()?;
        drop((references, metadata));
        let (surface, complete) = index.into_surface()?;
        guarded()?;
        Ok(StagedPng {
            surface,
            complete,
            seal,
            fragment: final_fragment
                .take()
                .ok_or("PNG final fragment is missing")?,
        })
    })();
    match result {
        Ok(staged) => Ok(staged),
        Err(mut error) => {
            // Every closure-local index, decoder, Surface, seal and pin has
            // dropped. Busy/foreign/uncertain retirement deliberately retains
            // inventory and disk; never replace it with generic deletion.
            for fragment in [final_fragment.take(), partial.take()]
                .into_iter()
                .flatten()
            {
                if let Err(cleanup) = session.abort_fragment(fragment, &mut || Ok(())) {
                    error.push_str("; PNG source cleanup retained inventory: ");
                    error.push_str(&cleanup);
                }
            }
            Err(error)
        }
    }
}

struct References {
    values: Vec<BlockRef>,
    // The actual vector allocation drops before its admission.
    _charge: Reservation,
}
impl References {
    fn new(
        index: &TileIndex,
        pool: &crate::large_document::ResidentBudget,
    ) -> Result<Self, String> {
        let count = index.blocks().len();
        let admitted = count
            .checked_mul(std::mem::size_of::<BlockRef>())
            .and_then(|v| v.checked_mul(2))
            .ok_or("PNG reference allocation overflow")?;
        let charge = pool
            .reserve_bytes(admitted, admitted)
            .map_err(|e| e.to_string())?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| "PNG reference allocation failed")?;
        if values
            .capacity()
            .checked_mul(std::mem::size_of::<BlockRef>())
            .is_none_or(|v| v > admitted)
        {
            return Err("PNG reference allocator exceeded admission".into());
        }
        // Repeated references are permitted by Store.commit; the sealed
        // coordinate map below distinguishes their semantic placements.
        values.extend(index.blocks().values().copied());
        Ok(Self {
            values,
            _charge: charge,
        })
    }
}

/// Fixed-layout source receipt, including exact native coordinate references
/// and retained ICC bytes. It is opaque metadata, not a version35/36 document.
fn metadata(
    index: &TileIndex,
    pool: &crate::large_document::ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<ScratchBytes, String> {
    let complete = index.complete();
    let h = complete.header();
    let profile = complete.profile().unwrap_or(&[]);
    let length = 128usize
        .checked_add(profile.len())
        .and_then(|v| {
            index
                .blocks()
                .len()
                .checked_mul(40)
                .and_then(|n| v.checked_add(n))
        })
        .ok_or("PNG source receipt footprint overflow")?;
    let mut bytes = ScratchBytes::new(pool, length, length).map_err(|e| e.to_string())?;
    bytes[..16].copy_from_slice(b"PBPNG-SOURCE-01\0");
    bytes[16..20].copy_from_slice(&h.width.to_be_bytes());
    bytes[20..24].copy_from_slice(&h.height.to_be_bytes());
    bytes[24..26].copy_from_slice(&h.depth.to_be_bytes());
    bytes[26] = u8::from(h.has_alpha);
    bytes[27] = u8::from(h.grayscale);
    bytes[28] = h.orientation.to_exif();
    bytes[29] = u8::from(h.animated);
    bytes[30] = u8::from(h.cicp.is_some());
    bytes[31] = if complete.color_policy() == ColorPolicy::RawSelection {
        1
    } else {
        0
    };
    bytes[32..64].copy_from_slice(&complete.source_sha256());
    if let Some(hash) = complete.base64_sha256() {
        bytes[64..96].copy_from_slice(&hash);
        bytes[96] = 1;
    }
    if let Some(cicp) = h.cicp {
        bytes[97..101].copy_from_slice(&cicp);
    }
    bytes[101..105].copy_from_slice(&(profile.len() as u32).to_be_bytes());
    bytes[105..109].copy_from_slice(&(index.blocks().len() as u32).to_be_bytes());
    for (target, source) in bytes[128..128 + profile.len()]
        .chunks_mut(64 * 1024)
        .zip(profile.chunks(64 * 1024))
    {
        check()?;
        target.copy_from_slice(source);
    }
    let mut at = 128 + profile.len();
    for (&(x, y), reference) in index.blocks() {
        check()?;
        bytes[at..at + 4].copy_from_slice(&x.to_be_bytes());
        bytes[at + 4..at + 8].copy_from_slice(&y.to_be_bytes());
        bytes[at + 8..at + 40].copy_from_slice(&reference.fingerprint());
        at += 40;
    }
    check()?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_document::root::{Directory, PersistentRootContext};
    use crate::workspace::source_runtime::{Limits as RuntimeLimits, SourceRuntime};
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        sync::{Arc, Mutex},
    };
    struct Fixture {
        path: PathBuf,
        held: Option<Directory>,
    }
    impl Fixture {
        fn new() -> Self {
            let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = parent.join(format!("pb-png-session-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self {
                held: Some(Directory::open(&path).unwrap()),
                path,
            }
        }
        fn runtime(&self, disk: u64) -> (Arc<SourceRuntime>, crate::server::Connection) {
            let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
            let connection = crate::server::start(shared, self.path.join("state")).unwrap();
            let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
            let runtime = SourceRuntime::new(
                context,
                RuntimeLimits {
                    resident_bytes: 128 * 1024 * 1024,
                    disk_bytes: disk,
                    max_files: 16,
                    store: crate::large_document::Limits {
                        disk_bytes: disk,
                        max_blocks: 128,
                        max_resident_blocks: 16,
                        manifest_bytes: 8 * 1024 * 1024,
                    },
                },
            )
            .unwrap();
            (runtime, connection)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
            if self.path.parent() == Some(parent.as_path())
                && self
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("pb-png-session-")
                && self.held.as_ref().is_some_and(|held| held.check().is_ok())
                && fs::canonicalize(&self.path).is_ok_and(|p| p == self.path)
            {
                drop(self.held.take());
                fs::remove_dir_all(&self.path).unwrap();
            }
        }
    }
    fn chunk(bytes: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let first = bytes.len();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        let mut crc = !0u32;
        for byte in &bytes[first..] {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0u32.wrapping_sub(crc & 1) & 0xedb88320);
            }
        }
        bytes.extend_from_slice(&(!crc).to_be_bytes());
    }
    fn png(adam: bool, depth: u16) -> Vec<u8> {
        let (w, h) = (9u32, 7u32);
        let geometries = if adam {
            &[
                (0, 0, 8, 8),
                (4, 0, 8, 8),
                (0, 4, 4, 8),
                (2, 0, 4, 4),
                (0, 2, 2, 4),
                (1, 0, 2, 2),
                (0, 1, 1, 2),
            ][..]
        } else {
            &[(0, 0, 1, 1)][..]
        };
        let mut scan = Vec::new();
        for &(sx, sy, dx, dy) in geometries {
            for y in (sy..h).step_by(dy as usize) {
                scan.push(0);
                for x in (sx..w).step_by(dx as usize) {
                    for word in [x as u16 * 911 + 3, y as u16 * 1111 + 7, 49151, 65535] {
                        if depth == 16 {
                            scan.extend_from_slice(&word.to_be_bytes());
                        } else {
                            scan.push(crate::raster::project16(word));
                        }
                    }
                }
            }
        }
        let mut compressor =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        compressor.write_all(&scan).unwrap();
        let mut result = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = Vec::new();
        header.extend_from_slice(&w.to_be_bytes());
        header.extend_from_slice(&h.to_be_bytes());
        header.extend_from_slice(&[depth as u8, 6, 0, 0, u8::from(adam)]);
        chunk(&mut result, b"IHDR", &header);
        chunk(&mut result, b"IDAT", &compressor.finish().unwrap());
        chunk(&mut result, b"IEND", &[]);
        result
    }
    fn encoded(bytes: &[u8], runtime: &SourceRuntime) -> EncodedPng {
        let mut charged = ScratchBytes::new(&runtime.resident(), bytes.len(), bytes.len()).unwrap();
        charged.copy_from_slice(bytes);
        EncodedPng::from_charged_bytes(
            charged.freeze(),
            bytes.len(),
            Limits::SELECTION,
            &runtime.resident(),
            &mut || Ok(()),
        )
        .unwrap()
    }
    #[test]
    fn actual_session_seals_native_plain_adam_and_reclaims_all_unpublished_fragments() {
        let f = Fixture::new();
        let (runtime, _connection) = f.runtime(16 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let baseline = runtime.disk().used().unwrap();
        for round in 0..20 {
            let depth = if round % 2 == 0 { 8 } else { 16 };
            let source = encoded(&png(round % 3 != 0, depth), &runtime);
            let staged = stage(
                &session,
                &source,
                Limits::SELECTION,
                ColorPolicy::Raw,
                &mut || Ok(()),
            )
            .unwrap();
            assert_eq!(staged.complete().source_sha256(), source.source_sha256());
            let metadata = staged.seal().metadata(&mut || Ok(())).unwrap();
            assert_eq!(&metadata[..16], b"PBPNG-SOURCE-01\0");
            drop(metadata);
            let tile = staged
                .surface()
                .prepare_tile((0, 0), &mut || Ok(()))
                .unwrap();
            let bytes = tile.bytes().unwrap();
            let bpp = if depth == 16 { 8 } else { 4 };
            let at = (6 * 256 + 8) * bpp;
            if depth == 16 {
                assert_eq!(
                    &bytes[at..at + 8],
                    &[0x1c, 0x7b, 0x1a, 0x11, 0xbf, 0xff, 0xff, 0xff]
                );
            } else {
                assert_eq!(&bytes[at..at + 4], &[28, 26, 191, 255]);
            }
            assert!(bytes[9 * bpp..256 * bpp].iter().all(|v| *v == 0));
            drop(tile);
            staged.abort(&session).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), baseline);
        }
    }
    #[test]
    fn actual_session_cancel_crc_and_disk_refusal_keep_no_partial_authority() {
        let f = Fixture::new();
        let (runtime, _connection) = f.runtime(16 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let baseline = runtime.disk().used().unwrap();
        let source = encoded(&png(true, 16), &runtime);
        let result = stage(
            &session,
            &source,
            Limits::SELECTION,
            ColorPolicy::Raw,
            &mut || {
                if runtime.disk().used().unwrap() > baseline + 24 {
                    Err("Cancelled after staged pixels".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        let mut bad = png(false, 8);
        let end = bad.len() - 1;
        bad[end] ^= 1;
        assert!(stage(
            &session,
            &encoded(&bad, &runtime),
            Limits::SELECTION,
            ColorPolicy::Raw,
            &mut || Ok(())
        )
        .is_err());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        let g = Fixture::new();
        let (small, _small_connection) = g.runtime(256);
        let small_session = small.begin().unwrap();
        let before = small.disk().used().unwrap();
        assert!(stage(
            &small_session,
            &encoded(&png(false, 16), &small),
            Limits::SELECTION,
            ColorPolicy::Raw,
            &mut || Ok(())
        )
        .is_err());
        assert_eq!(small.disk().used().unwrap(), before);
    }

    #[test]
    fn held_file_streams_to_the_identical_native_sink_and_preserves_original_limits() {
        use crate::image_import::prepared_png::{FileLimits, FilePng};
        let f = Fixture::new();
        let (runtime, _connection) = f.runtime(16 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let baseline = runtime.disk().used().unwrap();
        for depth in [8, 16] {
            for adam in [false, true] {
                let path = f.path.join("source.png");
                let bytes = png(adam, depth);
                fs::write(&path, &bytes).unwrap();
                let source = encoded(&bytes, &runtime);
                let resident = stage(
                    &session,
                    &source,
                    Limits::SELECTION,
                    ColorPolicy::Raw,
                    &mut || Ok(()),
                )
                .unwrap();
                let mut held = FilePng::open(
                    &session,
                    &path,
                    FileLimits::SELECTION,
                    ColorPolicy::Raw,
                    &mut || Ok(()),
                )
                .unwrap();
                let streamed = stage_file(&session, &mut held, &mut || Ok(())).unwrap();
                assert_eq!(streamed.complete().source_sha256(), source.source_sha256());
                let a = resident
                    .surface()
                    .prepare_tile((0, 0), &mut || Ok(()))
                    .unwrap();
                let b = streamed
                    .surface()
                    .prepare_tile((0, 0), &mut || Ok(()))
                    .unwrap();
                assert_eq!(a.bytes(), b.bytes());
                drop((a, b));
                streamed.abort(&session).unwrap();
                resident.abort(&session).unwrap();
                assert_eq!(runtime.disk().used().unwrap(), baseline);
                drop(held);
                assert!(FilePng::open(
                    &session,
                    &path,
                    FileLimits {
                        encoded_bytes: 8,
                        ..FileLimits::SELECTION
                    },
                    ColorPolicy::Raw,
                    &mut || Ok(())
                )
                .is_err());
            }
        }
    }

    #[test]
    fn held_file_crc_and_resident_footprint_do_not_depend_on_ignored_chunk_length() {
        use crate::image_import::prepared_png::{FileLimits, FilePng};
        let f = Fixture::new();
        let (runtime, _connection) = f.runtime(16 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let path = f.path.join("unused.png");
        let ordinary = png(false, 16);
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&ordinary[..33]).unwrap();
        let length = 2u32 * 1024 * 1024;
        file.write_all(&length.to_be_bytes()).unwrap();
        file.write_all(b"vpAg").unwrap();
        let block = [7u8; 64 * 1024];
        let mut crc = !0u32;
        for byte in b"vpAg"
            .iter()
            .chain(block.iter().cycle().take(length as usize))
        {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0u32.wrapping_sub(crc & 1) & 0xedb88320);
            }
        }
        for _ in 0..length as usize / block.len() {
            file.write_all(&block).unwrap();
        }
        file.write_all(&(!crc).to_be_bytes()).unwrap();
        file.write_all(&ordinary[33..]).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let baseline = runtime.resident().used();
        let mut held = FilePng::open(
            &session,
            &path,
            FileLimits::SELECTION,
            ColorPolicy::Raw,
            &mut || Ok(()),
        )
        .unwrap();
        // Includes a conservative worst-case long-path owner, not encoded bytes.
        assert!(runtime.resident().used() - baseline < 2 * 1024 * 1024);
        let source = stage_file(&session, &mut held, &mut || Ok(())).unwrap();
        source.abort(&session).unwrap();
        drop(held);
        assert_eq!(runtime.resident().used(), baseline);
        let mut writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
        use std::io::{Seek, SeekFrom};
        writer.seek(SeekFrom::Start(41)).unwrap();
        writer.write_all(&[8]).unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        assert!(FilePng::open(
            &session,
            &path,
            FileLimits::SELECTION,
            ColorPolicy::Raw,
            &mut || Ok(())
        )
        .is_err());
    }

    #[test]
    fn held_file_rejects_changed_decoder_bytes_even_with_a_restored_stamp() {
        use crate::image_import::prepared_png::{FileLimits, FilePng};
        let f = Fixture::new();
        let (runtime, _connection) = f.runtime(16 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let baseline = runtime.disk().used().unwrap();
        let path = f.path.join("changed.png");
        let plain = png(true, 16);
        let mut before = plain[..33].to_vec();
        chunk(&mut before, b"vpAg", b"first");
        before.extend_from_slice(&plain[33..]);
        let mut after = plain[..33].to_vec();
        chunk(&mut after, b"vpAg", b"other");
        after.extend_from_slice(&plain[33..]);
        assert_eq!(before.len(), after.len());
        fs::write(&path, &before).unwrap();
        let stamp = fs::metadata(&path).unwrap().modified().unwrap();
        let mut held = FilePng::open(
            &session,
            &path,
            FileLimits::SELECTION,
            ColorPolicy::Raw,
            &mut || Ok(()),
        )
        .unwrap();
        match fs::OpenOptions::new().write(true).open(&path) {
            Ok(mut writer) => {
                writer.write_all(&after).unwrap();
                writer
                    .set_times(fs::FileTimes::new().set_modified(stamp))
                    .unwrap();
                writer.sync_all().unwrap();
                drop(writer);
                held.recheck().unwrap();
                let error = stage_file(&session, &mut held, &mut || Ok(()))
                    .err()
                    .unwrap();
                assert!(error.contains("Bytes consumed"), "{error}");
            }
            Err(error) => {
                // Windows retains a read owner denying write/delete access;
                // no bypass of the actual OS protection is needed or attempted.
                assert!(
                    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32)),
                    "{error}"
                );
                held.recheck().unwrap();
                let source = stage_file(&session, &mut held, &mut || Ok(())).unwrap();
                source.abort(&session).unwrap();
            }
        }
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
}
