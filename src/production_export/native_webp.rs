//! Bounded native-row lossless WebP transport. No publication authority.
#[path = "native_webp/vp8l.rs"]
mod vp8l;
use super::{
    native_png::{CapturedExportRows, RowRequest},
    Format, Output, Resampling, Settings,
};
use crate::{engine::scene::SceneSnapshot, large_document::ScratchBytes};
use ring::digest::{Context, SHA256};
use std::{
    cell::Cell,
    io::{self, Write},
};
type Digest = [u8; 32];
const STRIP_PIXELS: u32 = 128 * 1024;
const MAX_RIFF: u64 = u32::MAX as u64 + 8;

pub(crate) struct EncodedWebp<'scene, 'output> {
    scene: &'scene SceneSnapshot,
    output: &'output Output,
    resampling: Resampling,
    provenance: Digest,
    bytes: u64,
    sha256: Digest,
    raw_sha256: Digest,
    failed: Cell<bool>,
}
impl EncodedWebp<'_, '_> {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn sha256(&self) -> Digest {
        self.sha256
    }
    pub(crate) fn raw_sha256(&self) -> Digest {
        self.raw_sha256
    }
    pub(crate) fn provenance(&self) -> Digest {
        self.provenance
    }
    pub(crate) fn recheck(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        if self.failed.replace(true) {
            return Err("Native WebP encoding receipt is failed".into());
        }
        let result = (|| {
            check()?;
            if binding(self.scene, self.output, self.resampling, check)? != self.provenance {
                return Err("Native WebP source or output changed".into());
            }
            check()
        })();
        if result.is_ok() {
            self.failed.set(false);
        }
        result
    }
    /// Equal complete passes remain encoding observations, never a file lease.
    pub(crate) fn same_encoding(
        &self,
        other: &Self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.recheck(check)?;
        other.recheck(check)?;
        if !std::ptr::eq(self.scene, other.scene)
            || !std::ptr::eq(self.output, other.output)
            || self.provenance != other.provenance
            || self.bytes != other.bytes
            || self.sha256 != other.sha256
            || self.raw_sha256 != other.raw_sha256
        {
            self.failed.set(true);
            other.failed.set(true);
            return Err(
                "Native WebP encoding passes differ in source, rows or encoded bytes".into(),
            );
        }
        check().inspect_err(|_| {
            self.failed.set(true);
            other.failed.set(true);
        })
    }
}
fn binding(
    scene: &SceneSnapshot,
    output: &Output,
    resampling: Resampling,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Digest, String> {
    let common = super::native_png::binding(scene, output, resampling, check)?;
    let mut hash = Context::new(&SHA256);
    hash.update(b"PeerBrushNativeWebpRows\0");
    hash.update(&common);
    Ok(hash.finish().as_ref().try_into().unwrap())
}
#[allow(clippy::too_many_arguments)]
fn visit_rows(
    scene: &SceneSnapshot,
    output: &Output,
    resampling: Resampling,
    provenance: Digest,
    provider: &mut impl CapturedExportRows,
    rows: &mut ScratchBytes,
    emit: &mut dyn FnMut([u8; 4]) -> io::Result<()>,
    check: &dyn Fn() -> Result<(), String>,
) -> io::Result<Digest> {
    let native = usize::from(output.native_source_depth / 8);
    let row_bytes = output.width as usize * 4 * native;
    let stripe = STRIP_PIXELS
        .div_ceil(output.width)
        .min(256)
        .min(output.height);
    let mut raw = Context::new(&SHA256);
    let mut first = 0;
    while first < output.height {
        check().map_err(io::Error::other)?;
        if !std::ptr::eq(scene, provider.scene()) {
            return Err(io::Error::other("Native WebP row source changed"));
        }
        let count = stripe.min(output.height - first);
        let bytes = &mut rows[..count as usize * row_bytes];
        provider
            .fill(
                RowRequest {
                    output,
                    sample_depth: output.native_source_depth,
                    resampling,
                    first_row: first,
                    rows: count,
                    provenance,
                },
                bytes,
                check,
            )
            .map_err(io::Error::other)?;
        check().map_err(io::Error::other)?;
        raw.update(bytes);
        for row in bytes.chunks_exact(row_bytes) {
            check().map_err(io::Error::other)?;
            for pixel in row.chunks_exact(4 * native) {
                let p = if native == 1 {
                    pixel.try_into().unwrap()
                } else {
                    std::array::from_fn(|c| {
                        crate::raster::project16(u16::from_be_bytes([
                            pixel[c * 2],
                            pixel[c * 2 + 1],
                        ]))
                    })
                };
                emit(p)?;
            }
        }
        first += count;
    }
    check().map_err(io::Error::other)?;
    Ok(raw.finish().as_ref().try_into().unwrap())
}
struct HashOutput<'a> {
    writer: &'a mut dyn Write,
    check: &'a dyn Fn() -> Result<(), String>,
    failed: &'a Cell<bool>,
    limit: u64,
    bytes: u64,
    hash: Context,
}
impl Write for HashOutput<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        (self.check)().map_err(io::Error::other)?;
        if self
            .bytes
            .checked_add(data.len() as u64)
            .is_none_or(|n| n > self.limit)
        {
            self.failed.set(true);
            return Err(io::Error::other("Native WebP exceeds admitted output"));
        }
        let n = self
            .writer
            .write(data)
            .inspect_err(|_| self.failed.set(true))?;
        if n > data.len() || (n == 0 && !data.is_empty()) {
            self.failed.set(true);
            return Err(io::Error::other("Invalid native WebP output write"));
        }
        self.hash.update(&data[..n]);
        self.bytes += n as u64;
        (self.check)().map_err(io::Error::other)?;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        (self.check)().map_err(io::Error::other)?;
        self.writer.flush().inspect_err(|_| self.failed.set(true))?;
        (self.check)().map_err(io::Error::other)
    }
}
fn chunk_bytes(payload: u64) -> Result<u64, String> {
    payload
        .checked_add(payload % 2)
        .and_then(|n| n.checked_add(8))
        .ok_or_else(|| "Native WebP chunk length overflow".into())
}
fn chunk(writer: &mut dyn Write, key: &[u8; 4], data: &[u8]) -> io::Result<()> {
    let length = u32::try_from(data.len())
        .map_err(|_| io::Error::other("Native WebP chunk is too large"))?;
    writer.write_all(key)?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(data)?;
    if length % 2 != 0 {
        writer.write_all(&[0])?;
    }
    Ok(())
}

/// Analyze, count and emit the pinned lossless transform without retaining a
/// complete RGBA/VP8L frame. Each pass validates the native rows before any
/// 16-to-8 projection. The caller must stage/replay exact outputs independently.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_to<'scene, 'output>(
    scene: &'scene SceneSnapshot,
    output: &'output Output,
    settings: &Settings,
    provider: &mut impl CapturedExportRows,
    writer: &mut dyn Write,
    max_bytes: u64,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<EncodedWebp<'scene, 'output>, String> {
    check()?;
    let meta = scene.metadata();
    if !std::ptr::eq(scene, provider.scene())
        || output.format != Format::Webp
        || settings.format != Format::Webp
        || output.bit_depth != 8
        || ![8, 16].contains(&output.native_source_depth)
        || output.native_source_depth != meta.depth()
        || output.width == 0
        || output.height == 0
        || output.width > 16000
        || output.height > 16000
        || max_bytes == 0
        || max_bytes > MAX_RIFF
    {
        return Err("Native WebP source, dimensions, precision or output admission differs".into());
    }
    let profile = super::native_png::profile(scene);
    if profile.is_some_and(|p| p.is_empty() || p.len() > crate::color_profile::MAX_PROFILE_BYTES) {
        return Err("Native WebP profile exceeds its exact source envelope".into());
    }
    // Covers bounded Huffman heaps/stacks/indexes and all fixed table arrays
    // before allocation. Source stripe/previous-row allocations have own tickets.
    let _backend = scene
        .budget()
        .reserve_bytes(256 * 1024, 256 * 1024)
        .map_err(|e| e.to_string())?;
    settings.validate()?;
    let rect = output.document_rect;
    if i64::from(rect[2]) - i64::from(rect[0]) <= 0
        || i64::from(rect[3]) - i64::from(rect[1]) <= 0
        || rect.iter().any(|n| n.unsigned_abs() > 116000)
        || output.scale_xy.iter().any(|n| !n.is_finite() || *n <= 0.)
        || (meta.read_only() && output.target.is_some())
        || output
            .target
            .as_deref()
            .is_some_and(|id| !meta.layers().any(|l| l.id() == id))
    {
        return Err("Native WebP rectangle, target or scale differs".into());
    }
    let provenance = binding(scene, output, settings.resampling, check)?;
    let stripe = STRIP_PIXELS
        .div_ceil(output.width)
        .min(256)
        .min(output.height);
    let row_bytes = output.width as usize * 4 * usize::from(output.native_source_depth / 8);
    let mut rows = ScratchBytes::new(
        scene.budget(),
        row_bytes * stripe as usize,
        (STRIP_PIXELS as usize + 15999) * 8,
    )
    .map_err(|e| e.to_string())?;
    let mut previous = ScratchBytes::new(scene.budget(), output.width as usize * 4, 16000 * 4)
        .map_err(|e| e.to_string())?;
    let failed = Cell::new(false);
    let active = || {
        if failed.get() {
            return Err("Native WebP encoding is failed".into());
        }
        check().inspect_err(|_| failed.set(true))
    };
    let mut raw = None;
    let frequencies = {
        let mut visit = |emit: &mut dyn FnMut([u8; 4]) -> io::Result<()>| {
            let hash = visit_rows(
                scene,
                output,
                settings.resampling,
                provenance,
                provider,
                &mut rows,
                emit,
                &active,
            )?;
            raw = Some(hash);
            Ok(())
        };
        vp8l::analyze(output.width, output.height, &mut previous, &mut visit)
            .map_err(|e| e.to_string())?
    };
    let raw = raw.ok_or("Native WebP analysis did not finish all native rows")?;
    let mut counted_raw = None;
    let mut counted = HashOutput {
        writer: &mut io::sink(),
        check: &active,
        failed: &failed,
        limit: max_bytes,
        bytes: 0,
        hash: Context::new(&SHA256),
    };
    {
        let mut visit = |emit: &mut dyn FnMut([u8; 4]) -> io::Result<()>| {
            let hash = visit_rows(
                scene,
                output,
                settings.resampling,
                provenance,
                provider,
                &mut rows,
                emit,
                &active,
            )?;
            counted_raw = Some(hash);
            Ok(())
        };
        vp8l::write_frame(
            output.width,
            output.height,
            &frequencies,
            &mut previous,
            &mut visit,
            &mut counted,
        )
        .map_err(|e| e.to_string())?;
    }
    let frame_bytes = counted.bytes;
    let frame_sha: Digest = counted.hash.finish().as_ref().try_into().unwrap();
    if counted_raw != Some(raw) {
        return Err("Native WebP native rows changed during frame counting".into());
    }
    let riff = 4u64
        .checked_add(chunk_bytes(frame_bytes)?)
        .and_then(|n| {
            profile.map_or(Some(n), |p| {
                chunk_bytes(p.len() as u64)
                    .ok()
                    .and_then(|size| n.checked_add(18 + size))
            })
        })
        .ok_or("Native WebP container length overflow")?;
    if frame_bytes > u64::from(u32::MAX) || riff > u64::from(u32::MAX) || riff + 8 > max_bytes {
        return Err("Native WebP RIFF32 length exceeds admitted output".into());
    }
    active()?;
    if binding(scene, output, settings.resampling, &active)? != provenance {
        return Err("Native WebP captured source changed before emission".into());
    }
    let mut encoded = HashOutput {
        writer,
        check: &active,
        failed: &failed,
        limit: max_bytes,
        bytes: 0,
        hash: Context::new(&SHA256),
    };
    let emitted = (|| -> Result<(), String> {
        encoded.write_all(b"RIFF").map_err(|e| e.to_string())?;
        encoded
            .write_all(&(riff as u32).to_le_bytes())
            .map_err(|e| e.to_string())?;
        encoded.write_all(b"WEBP").map_err(|e| e.to_string())?;
        if let Some(profile) = profile {
            let mut extended = [0u8; 10];
            extended[0] = 0x30;
            extended[4..7].copy_from_slice(&(output.width - 1).to_le_bytes()[..3]);
            extended[7..10].copy_from_slice(&(output.height - 1).to_le_bytes()[..3]);
            chunk(&mut encoded, b"VP8X", &extended).map_err(|e| e.to_string())?;
            chunk(&mut encoded, b"ICCP", profile).map_err(|e| e.to_string())?;
        }
        encoded.write_all(b"VP8L").map_err(|e| e.to_string())?;
        encoded
            .write_all(&(frame_bytes as u32).to_le_bytes())
            .map_err(|e| e.to_string())?;
        let mut emitted_raw = None;
        let mut frame = HashOutput {
            writer: &mut encoded,
            check: &active,
            failed: &failed,
            limit: frame_bytes,
            bytes: 0,
            hash: Context::new(&SHA256),
        };
        {
            let mut visit = |emit: &mut dyn FnMut([u8; 4]) -> io::Result<()>| {
                let hash = visit_rows(
                    scene,
                    output,
                    settings.resampling,
                    provenance,
                    provider,
                    &mut rows,
                    emit,
                    &active,
                )?;
                emitted_raw = Some(hash);
                Ok(())
            };
            vp8l::write_frame(
                output.width,
                output.height,
                &frequencies,
                &mut previous,
                &mut visit,
                &mut frame,
            )
            .map_err(|e| e.to_string())?;
        }
        let size = frame.bytes;
        let digest: Digest = frame.hash.finish().as_ref().try_into().unwrap();
        if emitted_raw != Some(raw) || size != frame_bytes || digest != frame_sha {
            return Err("Native WebP emitted native rows or encoded frame differs".into());
        }
        if frame_bytes % 2 != 0 {
            encoded.write_all(&[0]).map_err(|e| e.to_string())?;
        }
        active()?;
        if encoded.bytes != riff + 8
            || binding(scene, output, settings.resampling, &active)? != provenance
            || !std::ptr::eq(scene, provider.scene())
        {
            return Err("Native WebP completed source or container differs".into());
        }
        Ok(())
    })();
    if let Err(error) = emitted {
        failed.set(true);
        return Err(error);
    }
    active()?;
    Ok(EncodedWebp {
        scene,
        output,
        resampling: settings.resampling,
        provenance,
        bytes: encoded.bytes,
        sha256: encoded.hash.finish().as_ref().try_into().unwrap(),
        raw_sha256: raw,
        failed: Cell::new(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{
            scene::{
                Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, SceneIdentity,
            },
            Document,
        },
        large_document::{DiskBudget, ResidentBudget},
        psd::memory36::{Admitted, MetadataCharge},
        raster::{Raster, Surface},
    };
    use image::ImageEncoder;
    use std::sync::Arc;
    struct Collect(Vec<Surface>);
    impl RasterCollector for Collect {
        fn resident(&mut self, r: &Raster) -> Result<RasterRef36, String> {
            self.0
                .push(Surface::zero(r.width, r.height, r.depth).map_err(|e| e.to_string())?);
            Ok(RasterRef36 {
                id: self.0.len() as u32,
                width: r.width,
                height: r.height,
                depth: r.depth,
                retained: None,
            })
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
    fn fixture(depth: u16, profile: bool) -> SceneSnapshot {
        let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(8 * 1024 * 1024, 16).unwrap();
        let mut doc = Document::new_depth(19, 515, depth).unwrap();
        if profile {
            doc.icc_profile = Some(Arc::new((0..1023).map(|n| (n * 73) as u8).collect()));
            doc.read_only = true;
        }
        let mut collect = Collect(vec![]);
        let doc = Document36::capture(&doc, &mut collect).unwrap();
        let identity = SceneIdentity {
            project_id: "webp-transport".into(),
            document_id: doc.id.clone(),
            revision: doc.revision,
        };
        let owner = MetadataCharge::reserve(&pool, 65536, &|| Ok(())).unwrap();
        let pending = PendingScene::new(
            Admitted::from_parts(Arc::new(doc), owner),
            RasterRoster::new(collect.0).unwrap(),
            pool,
            disk,
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
            filename: "transport.webp".into(),
            document_rect: [0, 0, m.width() as i32, m.height() as i32],
            width: m.width(),
            height: m.height(),
            format: Format::Webp,
            native_source_depth: m.depth(),
            bit_depth: 8,
            scale_xy: [1., 1.],
        }
    }
    fn settings() -> Settings {
        Settings {
            format: Format::Webp,
            ..Settings::default()
        }
    }
    fn sample(x: u32, y: u32, c: usize, depth: u16, uniform: bool) -> u16 {
        if uniform {
            return [10001, 27003, 52007, 0][c];
        }
        if c == 3 && (x + y) % 7 == 0 {
            return 0;
        }
        if depth == 8 {
            ((x * 47 + y * 83 + c as u32 * 31) % 256) as u16
        } else {
            (10001 + x * 137 + y * 173 + c as u32 * 71) as u16
        }
    }
    // Transport fixture only; real current composite/crop/resampling is supplied
    // and qualified by the captured renderer adapter, independently of codecs.
    struct Rows<'a> {
        scene: &'a SceneSnapshot,
        calls: usize,
        uniform: bool,
        mutate_at: Option<usize>,
        fail_at: Option<usize>,
    }
    fn rows(scene: &SceneSnapshot) -> Rows<'_> {
        Rows {
            scene,
            calls: 0,
            uniform: false,
            mutate_at: None,
            fail_at: None,
        }
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
            assert_eq!(r.sample_depth, r.output.native_source_depth);
            assert_eq!(
                r.provenance,
                binding(self.scene, r.output, r.resampling, check)?
            );
            assert!(r.rows > 0 && r.rows <= 256);
            let native = usize::from(r.sample_depth / 8);
            assert_eq!(
                bytes.len(),
                r.output.width as usize * r.rows as usize * 4 * native
            );
            for y in 0..r.rows {
                for x in 0..r.output.width {
                    for c in 0..4 {
                        let mut p = sample(x, r.first_row + y, c, r.sample_depth, self.uniform);
                        if self.mutate_at.is_some_and(|at| self.calls >= at)
                            && x == 0
                            && y == 0
                            && c == 0
                        {
                            p ^= 1;
                        }
                        let at = ((y * r.output.width + x) as usize * 4 + c) * native;
                        if native == 1 {
                            bytes[at] = p as u8;
                        } else {
                            bytes[at..at + 2].copy_from_slice(&p.to_be_bytes());
                        }
                    }
                }
            }
            if self.fail_at == Some(self.calls) {
                return Err("Injected WebP prepared row refusal".into());
            }
            check()
        }
    }
    fn expected(scene: &SceneSnapshot, uniform: bool) -> Vec<u8> {
        let m = scene.metadata();
        (0..m.height())
            .flat_map(|y| {
                (0..m.width()).flat_map(move |x| {
                    (0..4).map(move |c| {
                        let p = sample(x, y, c, m.depth(), uniform);
                        if m.depth() == 8 {
                            p as u8
                        } else {
                            crate::raster::project16(p)
                        }
                    })
                })
            })
            .collect()
    }
    fn reference(scene: &SceneSnapshot, source: &[u8]) -> Vec<u8> {
        let m = scene.metadata();
        let mut bytes = vec![];
        let mut encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut bytes);
        if let Some(profile) = super::super::native_png::profile(scene) {
            encoder.set_icc_profile(profile.to_vec()).unwrap();
        }
        encoder
            .write_image(
                source,
                m.width(),
                m.height(),
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        bytes
    }
    #[test]
    fn native8_native16_rows_match_existing_lossless_webp_bytes_pixels_hidden_rgb_and_icc() {
        for depth in [8, 16] {
            for icc in [false, true] {
                for uniform in [false, true] {
                    let scene = fixture(depth, icc);
                    let output = output(&scene);
                    let mut source = rows(&scene);
                    source.uniform = uniform;
                    let before = scene.render_key().unwrap();
                    let mut bytes = vec![];
                    let receipt = write_to(
                        &scene,
                        &output,
                        &settings(),
                        &mut source,
                        &mut bytes,
                        8 * 1024 * 1024,
                        &|| Ok(()),
                    )
                    .unwrap();
                    assert_eq!(source.calls, 9);
                    assert_eq!(receipt.bytes(), bytes.len() as u64);
                    assert_eq!(
                        ring::digest::digest(&SHA256, &bytes).as_ref(),
                        &receipt.sha256()[..]
                    );
                    let expected = expected(&scene, uniform);
                    assert_eq!(bytes, reference(&scene, &expected));
                    let decoded =
                        image::load_from_memory_with_format(&bytes, image::ImageFormat::WebP)
                            .unwrap()
                            .to_rgba8();
                    assert_eq!(decoded.as_raw(), &expected);
                    assert_eq!(scene.render_key().unwrap(), before);
                    receipt.recheck(&|| Ok(())).unwrap();
                }
            }
        }
    }
    #[test]
    fn lowbit_change_between_analysis_count_or_emission_refuses_even_if_projection_is_identical() {
        for at in [4, 7] {
            let scene = fixture(16, false);
            let output = output(&scene);
            let mut source = rows(&scene);
            source.mutate_at = Some(at);
            let mut bytes = vec![];
            assert!(write_to(
                &scene,
                &output,
                &settings(),
                &mut source,
                &mut bytes,
                8 * 1024 * 1024,
                &|| Ok(())
            )
            .is_err());
            if at == 4 {
                assert!(bytes.is_empty());
            } else {
                assert!(!bytes.is_empty());
            }
        }
    }
    #[test]
    fn independent_complete_passes_compare_hashes_and_failed_receipts_stay_failed() {
        let scene = fixture(16, true);
        let output = output(&scene);
        let first = write_to(
            &scene,
            &output,
            &settings(),
            &mut rows(&scene),
            &mut io::sink(),
            8 * 1024 * 1024,
            &|| Ok(()),
        )
        .unwrap();
        let second = write_to(
            &scene,
            &output,
            &settings(),
            &mut rows(&scene),
            &mut io::sink(),
            first.bytes(),
            &|| Ok(()),
        )
        .unwrap();
        first.same_encoding(&second, &|| Ok(())).unwrap();
        let mut changed = rows(&scene);
        changed.uniform = true;
        let changed = write_to(
            &scene,
            &output,
            &settings(),
            &mut changed,
            &mut io::sink(),
            8 * 1024 * 1024,
            &|| Ok(()),
        )
        .unwrap();
        assert!(first.same_encoding(&changed, &|| Ok(())).is_err());
        assert!(first.recheck(&|| Ok(())).is_err());
        assert!(second.recheck(&|| Err("cancelled receipt".into())).is_err());
        assert!(second.recheck(&|| Ok(())).is_err());
    }
    #[test]
    fn resident_limits_cancel_source_mismatch_prepared_row_and_transport_failures_refuse() {
        let scene = fixture(16, false);
        let other = fixture(16, false);
        let output = output(&scene);
        let mut bytes = vec![];
        assert!(write_to(
            &scene,
            &output,
            &settings(),
            &mut rows(&other),
            &mut bytes,
            8 * 1024 * 1024,
            &|| Ok(())
        )
        .is_err());
        assert!(write_to(
            &scene,
            &output,
            &settings(),
            &mut rows(&scene),
            &mut bytes,
            8 * 1024 * 1024,
            &|| Err("cancel".into())
        )
        .is_err());
        let mut source = rows(&scene);
        source.fail_at = Some(5);
        assert!(write_to(
            &scene,
            &output,
            &settings(),
            &mut source,
            &mut bytes,
            8 * 1024 * 1024,
            &|| Ok(())
        )
        .is_err());
        assert!(bytes.is_empty());
        assert!(write_to(
            &scene,
            &output,
            &settings(),
            &mut rows(&scene),
            &mut bytes,
            8,
            &|| Ok(())
        )
        .is_err());
        assert!(bytes.is_empty());
        let pressure = scene
            .budget()
            .reserve_bytes(
                scene.budget().limit() - scene.budget().used() - 1,
                32 * 1024 * 1024,
            )
            .unwrap();
        let mut source = rows(&scene);
        assert!(write_to(
            &scene,
            &output,
            &settings(),
            &mut source,
            &mut bytes,
            8 * 1024 * 1024,
            &|| Ok(())
        )
        .is_err());
        assert_eq!(source.calls, 0);
        drop(pressure);
        struct Refuse(usize);
        impl Write for Refuse {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.0 += 1;
                Err(io::Error::other("injected WebP output refusal"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Refuse(0);
        assert!(write_to(
            &scene,
            &output,
            &settings(),
            &mut rows(&scene),
            &mut writer,
            8 * 1024 * 1024,
            &|| Ok(())
        )
        .is_err());
        assert_eq!(writer.0, 1);
    }
}
