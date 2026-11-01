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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        engine::{
            scene::{
                Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, SceneIdentity,
            },
            Document, Engine,
        },
        large_document::{
            root::{Directory, PersistentRootContext},
            Limits as StoreLimits,
        },
        psd::memory36::{Admitted, MetadataCharge},
        raster::{Raster, Surface},
        workspace::source_runtime::{Limits, SourceRuntime},
    };
    use std::{
        fs,
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    // Actual profile/instance/session admission, with a small transport-only row
    // fixture. Synthetic samples do not qualify the compositor's scene fidelity.
    struct Fixture {
        base: PathBuf,
        held: Option<Directory>,
    }
    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir()
                .join(format!("peerbrush-native-rows-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&base).unwrap();
            let base = fs::canonicalize(base).unwrap();
            let held = Some(Directory::open(&base).unwrap());
            fs::create_dir(base.join("legacy")).unwrap();
            Self { base, held }
        }
        fn runtime(
            &self,
        ) -> (
            crate::settings::Profile,
            crate::server::Shared,
            crate::server::Connection,
            Arc<SourceRuntime>,
        ) {
            let profile = crate::settings::Profile::open(
                crate::settings::Locations {
                    config: self.base.join("config"),
                    data: self.base.join("data"),
                    cache: self.base.join("cache"),
                },
                &self.base.join("legacy"),
            )
            .unwrap();
            let shared = Arc::new(Mutex::new(Engine::new()));
            let connection =
                crate::server::start(shared.clone(), self.base.join("runtime")).unwrap();
            let context =
                PersistentRootContext::from_profile(&connection, profile.source_profile().unwrap())
                    .unwrap();
            let runtime = SourceRuntime::new(
                context,
                Limits {
                    resident_bytes: 768 * 1024 * 1024,
                    disk_bytes: 8 * 1024 * 1024,
                    max_files: 32,
                    store: StoreLimits {
                        disk_bytes: 4 * 1024 * 1024,
                        max_blocks: 64,
                        max_resident_blocks: 16,
                        manifest_bytes: 1024 * 1024,
                    },
                },
            )
            .unwrap();
            (profile, shared, connection, runtime)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if fs::canonicalize(std::env::temp_dir()).is_ok_and(|temp| self.base.starts_with(temp))
                && self
                    .base
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("peerbrush-native-rows-"))
                && self.held.as_ref().is_some_and(|held| held.check().is_ok())
                && fs::canonicalize(&self.base).is_ok_and(|p| p == self.base)
            {
                drop(self.held.take());
                let _ = fs::remove_dir_all(&self.base);
            }
        }
    }
    struct Collect(Vec<Surface>);
    impl RasterCollector for Collect {
        fn resident(&mut self, r: &Raster) -> Result<RasterRef36, String> {
            self.surface(&Surface::zero(r.width, r.height, r.depth).map_err(|e| e.to_string())?)
        }
        fn surface(&mut self, s: &Surface) -> Result<RasterRef36, String> {
            self.0.push(s.clone());
            Ok(RasterRef36 {
                id: self.0.len() as u32,
                width: s.width(),
                height: s.height(),
                depth: s.depth(),
                retained: None,
            })
        }
    }
    fn scene(session: &SourceSession, depth: u16) -> SceneSnapshot {
        scene_dimensions(session, depth, 17, 513)
    }
    fn scene_dimensions(
        session: &SourceSession,
        depth: u16,
        width: u32,
        height: u32,
    ) -> SceneSnapshot {
        let mut doc = Document::new_depth(width, height, depth).unwrap();
        doc.srgb_tagged = true;
        let mut collect = Collect(vec![]);
        let metadata = Document36::capture(&doc, &mut collect).unwrap();
        let identity = SceneIdentity {
            project_id: "native-row-transport".into(),
            document_id: metadata.id.clone(),
            revision: metadata.revision,
        };
        let pool = session.resident();
        let owner = MetadataCharge::reserve(&pool, 65536, &|| Ok(())).unwrap();
        let pending = PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), owner),
            RasterRoster::new(collect.0).unwrap(),
            pool,
            session.disk(),
        )
        .unwrap();
        SceneSnapshot::new(identity, pending, vec![], None, None).unwrap()
    }
    fn output(scene: &SceneSnapshot) -> Output {
        let m = scene.metadata();
        Output {
            target: None,
            target_name: "Composite".into(),
            variant: "native".into(),
            filename: "fixture.jxl".into(),
            document_rect: [0, 0, m.width() as i32, m.height() as i32],
            width: m.width(),
            height: m.height(),
            format: Format::Jxl,
            native_source_depth: m.depth(),
            bit_depth: m.depth(),
            scale_xy: [1., 1.],
        }
    }
    pub(crate) fn expected_byte(at: usize, depth: u16) -> u8 {
        let sample = at / usize::from(depth / 8);
        let channel = sample % 4;
        let pixel = sample / 4;
        let value = if channel == 3 && pixel % 7 == 0 {
            0
        } else {
            (10001 + pixel * 137 + channel * 71) as u16
        };
        if depth == 8 {
            value as u8
        } else {
            value.to_be_bytes()[at % 2]
        }
    }
    struct Rows<'a> {
        scene: &'a SceneSnapshot,
        calls: usize,
        fail_at: Option<usize>,
    }
    impl CapturedExportRows for Rows<'_> {
        fn scene(&self) -> &SceneSnapshot {
            self.scene
        }
        fn fill(
            &mut self,
            r: RowRequest<'_>,
            bytes: &mut [u8],
            check: &dyn Fn() -> Result<(), String>,
        ) -> Result<(), String> {
            check()?;
            self.calls += 1;
            if self.fail_at == Some(self.calls) {
                return Err("specific captured row refusal".into());
            }
            assert_eq!(r.sample_depth, r.output.native_source_depth);
            assert_eq!(
                r.provenance,
                native_png::binding(self.scene, r.output, r.resampling, check)?
            );
            assert!(r.rows <= 256 && r.rows * r.output.width <= STRIP_PIXELS);
            let start = r.first_row as usize
                * r.output.width as usize
                * 4
                * usize::from(r.sample_depth / 8);
            for (i, value) in bytes.iter_mut().enumerate() {
                *value = expected_byte(start + i, r.sample_depth);
            }
            check()
        }
    }
    fn settings() -> Settings {
        Settings {
            format: Format::Jxl,
            ..Settings::default()
        }
    }
    pub(crate) fn with_staged(depth: u16, call: impl FnOnce(StagedNativeRows<'_, '_, '_>)) {
        with_staged_options(depth, 17, 513, crate::jpeg_xl::Options::default(), call)
    }
    pub(crate) fn with_staged_options(
        depth: u16,
        width: u32,
        height: u32,
        options: crate::jpeg_xl::Options,
        call: impl FnOnce(StagedNativeRows<'_, '_, '_>),
    ) {
        let f = Fixture::new();
        let (_profile, _shared, _connection, runtime) = f.runtime();
        let session = runtime.begin().unwrap();
        let scene = scene_dimensions(&session, depth, width, height);
        let output = output(&scene);
        let mut provider = Rows {
            scene: &scene,
            calls: 0,
            fail_at: None,
        };
        let raw = u64::from(output.width) * u64::from(output.height) * 4 * u64::from(depth / 8);
        let mut settings = settings();
        settings.jxl = options;
        let input = StagedNativeRows::stage(
            &session,
            &scene,
            &output,
            &settings,
            &mut provider,
            crate::jpeg_xl::stream_protocol::source_read_ceiling(raw).unwrap(),
            &|| Ok(()),
        )
        .unwrap();
        call(input);
        assert_eq!(runtime.disk().used().unwrap(), 56);
    }
    #[test]
    fn real_session_stages_native8_native16_multistrips_and_meters_authenticated_ranges() {
        for depth in [8, 16] {
            let f = Fixture::new();
            let (_profile, _shared, _connection, runtime) = f.runtime();
            let session = runtime.begin().unwrap();
            let scene = scene(&session, depth);
            let output = output(&scene);
            let raw = u64::from(output.width) * u64::from(output.height) * 4 * u64::from(depth / 8);
            let mut provider = Rows {
                scene: &scene,
                calls: 0,
                fail_at: None,
            };
            let input = StagedNativeRows::stage(
                &session,
                &scene,
                &output,
                &settings(),
                &mut provider,
                crate::jpeg_xl::stream_protocol::source_read_ceiling(raw).unwrap(),
                &|| Ok(()),
            )
            .unwrap();
            assert_eq!(provider.calls, 3);
            assert_eq!(
                input.read_used(),
                LargeSourceTemporary::encoded_bytes(raw).unwrap()
            );
            let mut hash = Context::new(&SHA256);
            for at in 0..raw as usize {
                hash.update(&[expected_byte(at, depth)]);
            }
            assert_eq!(
                input.raw_sha256(),
                <Digest>::try_from(hash.finish().as_ref()).unwrap()
            );
            input.recheck(&|| Ok(())).unwrap();
            let start = 65530u64.min(raw / 2);
            let mut range = ScratchBytes::new(scene.budget(), 64, 64).unwrap();
            let before = input.read_used();
            input.copy_range(start, &mut range, &|| Ok(())).unwrap();
            assert_eq!(
                &*range,
                &(start as usize..start as usize + 64)
                    .map(|at| expected_byte(at, depth))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                input.read_used() - before,
                input
                    .input
                    .bytes()
                    .authenticated_read_cost(start, 64)
                    .unwrap()
            );
            drop(range);
            input.retire(&mut || session.cleanup_check()).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), 56);
        }
    }
    #[test]
    fn row_refusal_and_insufficient_read_work_never_mint_completion_and_retire_only_owned_stage() {
        let f = Fixture::new();
        let (_profile, _shared, _connection, runtime) = f.runtime();
        let session = runtime.begin().unwrap();
        let scene = scene(&session, 16);
        let output = output(&scene);
        let mut provider = Rows {
            scene: &scene,
            calls: 0,
            fail_at: Some(2),
        };
        let error = StagedNativeRows::stage(
            &session,
            &scene,
            &output,
            &settings(),
            &mut provider,
            1024 * 1024,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert_eq!(error, "specific captured row refusal");
        assert_eq!(runtime.disk().used().unwrap(), 56);
        let mut provider = Rows {
            scene: &scene,
            calls: 0,
            fail_at: None,
        };
        let raw = u64::from(output.width) * u64::from(output.height) * 8;
        let input = StagedNativeRows::stage(
            &session,
            &scene,
            &output,
            &settings(),
            &mut provider,
            LargeSourceTemporary::encoded_bytes(raw).unwrap(),
            &|| Ok(()),
        )
        .unwrap();
        assert!(input.recheck(&|| Ok(())).unwrap_err().contains("read work"));
        assert!(input.check(&|| Ok(())).unwrap_err().contains("failed"));
        input.retire(&mut || session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 56);
    }
    #[test]
    fn explicit_late_source_cancellation_is_sticky_without_preventing_verified_retirement() {
        let f = Fixture::new();
        let (_profile, _shared, _connection, runtime) = f.runtime();
        let session = runtime.begin().unwrap();
        let scene = scene(&session, 8);
        let output = output(&scene);
        let mut provider = Rows {
            scene: &scene,
            calls: 0,
            fail_at: None,
        };
        let input = StagedNativeRows::stage(
            &session,
            &scene,
            &output,
            &settings(),
            &mut provider,
            1024 * 1024,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(
            input
                .recheck(&|| Err("specific revision changed".into()))
                .unwrap_err(),
            "specific revision changed"
        );
        assert!(input.check(&|| Ok(())).is_err());
        input.retire(&mut || session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 56);
    }
}
