//! Closed straight-native row staging for the isolated JPEG XL operation.
//! This source/transport proof is not destination publication authority.
pub(crate) use super::native_png::{CapturedExportRows, RowRequest};
use super::{native_png, Format, Output, Resampling, Settings};
use crate::{
    engine::scene::SceneSnapshot,
    large_document::{ByteRole, Error, LargeSourceTemporary, ScratchBytes},
    workspace::source_runtime::{SessionBytes, SourceSession},
};
use ring::digest::{Context, SHA256};
use std::{
    io::Read,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
type Digest = [u8; 32];
const CHUNK: usize = 64 * 1024;
const STRIP_PIXELS: u32 = 128 * 1024;

pub(crate) struct StagedNativeRows<'scene, 'output, 'session> {
    input: SessionBytes<'session>,
    session: &'session SourceSession,
    scene: &'scene SceneSnapshot,
    output: &'output Output,
    resampling: Resampling,
    options: crate::jpeg_xl::Options,
    binding: Digest,
    raw_sha256: Digest,
    raw_bytes: u64,
    profile_sha256: Digest,
    read_limit: u64,
    read_used: AtomicU64,
    failed: AtomicBool,
}
fn profile(scene: &SceneSnapshot) -> &[u8] {
    // Preserve existing JXL behavior: untagged RGB is assigned sRGB by its
    // encoder, while PNG may remain untagged. No sample conversion occurs here.
    match native_png::profile(scene) {
        Some(profile) => profile,
        None => crate::color_profile::srgb_profile(),
    }
}
fn binding(
    scene: &SceneSnapshot,
    output: &Output,
    resampling: Resampling,
    options: crate::jpeg_xl::Options,
    read_limit: u64,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Digest, String> {
    let common = native_png::binding(scene, output, resampling, check)?;
    let mut hash = Context::new(&SHA256);
    hash.update(b"PeerBrushStraightNativeRowsV1\0");
    hash.update(&common);
    hash.update(&[u8::from(options.lossless)]);
    hash.update(&options.distance.to_bits().to_be_bytes());
    hash.update(&options.effort.to_be_bytes());
    hash.update(&read_limit.to_be_bytes());
    hash.update(ring::digest::digest(&SHA256, profile(scene)).as_ref());
    check()?;
    Ok(hash.finish().as_ref().try_into().unwrap())
}
impl<'scene, 'output, 'session> StagedNativeRows<'scene, 'output, 'session> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn stage(
        session: &'session SourceSession,
        scene: &'scene SceneSnapshot,
        output: &'output Output,
        settings: &Settings,
        provider: &mut impl CapturedExportRows,
        max_source_read_bytes: u64,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        session.check()?;
        if !scene.budget().same_pool(&session.resident())
            || !scene.disk_budget().same_pool(&session.disk())
        {
            return Err("Native row stage belongs to another operation budget".into());
        }
        let metadata = scene.metadata();
        let raw = u64::from(output.width)
            .checked_mul(u64::from(output.height))
            .and_then(|n| n.checked_mul(4 * u64::from(output.native_source_depth / 8)))
            .ok_or("Native row size overflow")?;
        if !std::ptr::eq(scene, provider.scene())
            || settings.format != Format::Jxl
            || output.format != Format::Jxl
            || output.width == 0
            || output.height == 0
            || output.width > 16000
            || output.height > 16000
            || ![8, 16].contains(&output.native_source_depth)
            || output.native_source_depth != metadata.depth()
            || output.bit_depth != metadata.depth()
            || max_source_read_bytes == 0
            || max_source_read_bytes > crate::jpeg_xl::stream_protocol::source_read_ceiling(raw)?
        {
            return Err(
                "Native row stage shape, precision, source or read admission differs".into(),
            );
        }
        let readback = LargeSourceTemporary::encoded_bytes(raw).map_err(|e| e.to_string())?;
        if readback > max_source_read_bytes {
            return Err("Native row readback exceeds operation work admission".into());
        }
        let _metadata = scene
            .budget()
            .reserve_bytes(CHUNK, CHUNK)
            .map_err(|e| e.to_string())?;
        settings.validate()?;
        let rect = output.document_rect;
        if i64::from(rect[2]) - i64::from(rect[0]) <= 0
            || i64::from(rect[3]) - i64::from(rect[1]) <= 0
            || rect.iter().any(|n| n.unsigned_abs() > 116000)
            || output.scale_xy.iter().any(|n| !n.is_finite() || *n <= 0.)
            || (metadata.read_only() && output.target.is_some())
            || output
                .target
                .as_deref()
                .is_some_and(|id| !metadata.layers().any(|l| l.id() == id))
        {
            return Err("Native row rectangle, scale or isolated target differs".into());
        }
        let profile = profile(scene);
        if profile.is_empty() || profile.len() > crate::color_profile::MAX_PROFILE_BYTES {
            return Err("Native row profile exceeds source envelope".into());
        }
        let provenance = binding(
            scene,
            output,
            settings.resampling,
            settings.jxl,
            max_source_read_bytes,
            check,
        )?;
        let row_provenance = native_png::binding(scene, output, settings.resampling, check)?;
        let profile_sha256 = ring::digest::digest(&SHA256, profile)
            .as_ref()
            .try_into()
            .unwrap();
        let stripe = native_png::stripe_rows(output.width, output.height);
        let row_bytes = output.width as usize * 4 * usize::from(output.native_source_depth / 8);
        let mut rows = ScratchBytes::new(
            scene.budget(),
            row_bytes * stripe as usize,
            (STRIP_PIXELS as usize + 15999) * 8,
        )
        .map_err(|e| e.to_string())?;
        let mut temporary = Some(session.byte_temporary(raw, ByteRole::StraightNativeRgba)?);
        let outcome = (|| -> Result<(SessionBytes<'session>, Digest), String> {
            let active = || {
                check()?;
                session.check()
            };
            let mut first = 0;
            let mut hash = Context::new(&SHA256);
            let mut count = 0u64;
            while first < output.height {
                active()?;
                if !std::ptr::eq(scene, provider.scene()) {
                    return Err("Native row provider changed capture".into());
                }
                let height = stripe.min(output.height - first);
                let bytes = &mut rows[..row_bytes * height as usize];
                provider.fill(
                    RowRequest {
                        output,
                        sample_depth: output.native_source_depth,
                        resampling: settings.resampling,
                        first_row: first,
                        rows: height,
                        provenance: row_provenance,
                    },
                    bytes,
                    &active,
                )?;
                active()?;
                let file = temporary
                    .as_mut()
                    .ok_or("Native row temporary was consumed")?;
                file.check()?;
                let mut failure = None;
                file.temporary()
                    .append(bytes, &mut || {
                        active().map_err(|e| {
                            failure = Some(e);
                            Error::Cancelled
                        })
                    })
                    .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
                file.check()?;
                hash.update(bytes);
                count = count
                    .checked_add(bytes.len() as u64)
                    .ok_or("Native row count overflow")?;
                first += height;
            }
            active()?;
            if count != raw
                || !std::ptr::eq(scene, provider.scene())
                || binding(
                    scene,
                    output,
                    settings.resampling,
                    settings.jxl,
                    max_source_read_bytes,
                    &active,
                )? != provenance
            {
                return Err(
                    "Native row stage completed a changed source or incomplete rows".into(),
                );
            }
            let raw_sha256: Digest = hash.finish().as_ref().try_into().unwrap();
            let file = temporary
                .take()
                .ok_or("Native row temporary was consumed")?;
            let input = file.finish(provenance, &mut || active())?;
            Ok((input, raw_sha256))
        })();
        let (input, raw_sha256) = match outcome {
            Ok(value) => value,
            Err(error) => {
                if let Some(file) = temporary.take() {
                    if let Err(cleanup) = file.abort(&mut || session.cleanup_check()) {
                        return Err(format!("{error}; native row cleanup retained: {cleanup}"));
                    }
                }
                return Err(error);
            }
        };
        let staged = Self {
            input,
            session,
            scene,
            output,
            resampling: settings.resampling,
            options: settings.jxl,
            binding: provenance,
            raw_sha256,
            raw_bytes: raw,
            profile_sha256,
            read_limit: max_source_read_bytes,
            read_used: AtomicU64::new(readback),
            failed: AtomicBool::new(false),
        };
        if let Err(error) = staged.check(check) {
            if let Err(cleanup) = staged.retire(&mut || session.cleanup_check()) {
                return Err(format!("{error}; native row cleanup retained: {cleanup}"));
            }
            return Err(error);
        }
        Ok(staged)
    }
    pub(crate) fn session(&self) -> &'session SourceSession {
        self.session
    }
    pub(crate) fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
    pub(crate) fn output(&self) -> &Output {
        self.output
    }
    pub(crate) fn options(&self) -> crate::jpeg_xl::Options {
        self.options
    }
    pub(crate) fn binding(&self) -> Digest {
        self.binding
    }
    pub(crate) fn raw_sha256(&self) -> Digest {
        self.raw_sha256
    }
    pub(crate) fn raw_bytes(&self) -> u64 {
        self.raw_bytes
    }
    pub(crate) fn profile(&self) -> &[u8] {
        profile(self.scene)
    }
    pub(crate) fn profile_sha256(&self) -> Digest {
        self.profile_sha256
    }
    pub(crate) fn read_limit(&self) -> u64 {
        self.read_limit
    }
    pub(crate) fn read_used(&self) -> u64 {
        self.read_used.load(Ordering::Acquire)
    }
    fn charge(&self, count: u64) -> Result<(), String> {
        self.read_used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(count).filter(|n| *n <= self.read_limit)
            })
            .map(|_| ())
            .map_err(|_| {
                self.failed.store(true, Ordering::Release);
                "Native source read work exceeds admitted operation".into()
            })
    }
    pub(crate) fn check(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        let result = (|| {
            if self.failed.load(Ordering::Acquire) {
                return Err("Native row source is failed".into());
            }
            check()?;
            self.session.check()?;
            self.input.check_session(self.session)?;
            let source = self.input.bytes();
            if source.role() != ByteRole::StraightNativeRgba
                || source.payload_length() != self.raw_bytes
                || source.payload_sha256() != self.raw_sha256
                || source.binding() != self.binding
                || !source.uses_budgets(&self.session.resident(), &self.session.disk())
            {
                return Err("Native row source identity, seal or budget differs".into());
            }
            check()
        })();
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    /// Exactly one uncached authenticated range invocation. Its frozen original
    /// chunk index verifies complete covered64KiB payload chunks before copying.
    pub(crate) fn copy_range(
        &self,
        start: u64,
        output: &mut ScratchBytes,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.check(check)?;
        if !output.uses_budget(&self.session.resident()) {
            self.failed.store(true, Ordering::Release);
            return Err("Native row range belongs to another pool".into());
        }
        let result = (|| {
            let bytes = self.input.bytes();
            self.charge(
                bytes
                    .authenticated_read_cost(start, output.len() as u64)
                    .map_err(|e| e.to_string())?,
            )?;
            let mut failure = None;
            bytes
                .read_range_into(start, output, &mut || {
                    self.check(check).map_err(|e| {
                        failure = Some(e);
                        Error::Cancelled
                    })
                })
                .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
            self.check(check)
        })();
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    /// Fresh whole payload hash, with every physical covered-chunk read admitted
    /// before IO. No cached data or mutable on-disk hash index is trusted.
    pub(crate) fn recheck(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        self.check(check)?;
        let result = (|| {
            if binding(
                self.scene,
                self.output,
                self.resampling,
                self.options,
                self.read_limit,
                check,
            )? != self.binding
            {
                return Err("Native row capture binding changed".into());
            }
            let mut transfer = ScratchBytes::new(&self.session.resident(), CHUNK, CHUNK)
                .map_err(|e| e.to_string())?;
            let mut failure = None;
            let mut active = || {
                self.check(check).map_err(|e| {
                    failure = Some(e);
                    Error::Cancelled
                })
            };
            let bytes = self.input.bytes();
            let reading = (|| -> Result<Digest, String> {
                let mut cursor = bytes
                    .cursor(0, self.raw_bytes, &mut active)
                    .map_err(|e| e.to_string())?;
                let mut at = 0u64;
                let mut hash = Context::new(&SHA256);
                while at < self.raw_bytes {
                    let count = (self.raw_bytes - at).min(CHUNK as u64) as usize;
                    self.charge(
                        bytes
                            .authenticated_read_cost(at, count as u64)
                            .map_err(|e| e.to_string())?,
                    )?;
                    let n = cursor
                        .read(&mut transfer[..count])
                        .map_err(|e| e.to_string())?;
                    if n != count {
                        return Err("Native row fresh hash was incomplete".into());
                    }
                    hash.update(&transfer[..count]);
                    at += count as u64;
                }
                let mut extra = [0];
                if cursor.read(&mut extra).map_err(|e| e.to_string())? != 0 {
                    return Err("Native row source has trailing bytes".into());
                }
                Ok(hash.finish().as_ref().try_into().unwrap())
            })();
            drop(active);
            if let Some(error) = failure {
                return Err(error);
            }
            if reading? != self.raw_sha256 {
                return Err("Native row source hash changed".into());
            }
            self.check(check)
        })();
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    pub(crate) fn retire(
        self,
        cleanup: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.input.retire(cleanup)
    }
}
