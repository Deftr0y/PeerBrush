//! Native PNG transport over captured, fallibly prepared straight-alpha rows.
//! This produces encoding observations, never destination publication authority.
use super::{Format, Output, Resampling, Settings};
use crate::{engine::scene::SceneSnapshot, large_document::ScratchBytes};
use ring::digest::{Context, SHA256};
use std::{
    borrow::Cow,
    cell::Cell,
    io::{self, Write},
};

type Digest = [u8; 32];
const STRIP_PIXELS: u32 = 128 * 1024;
const MAX_ENCODED: u64 = 64 * 1024 * 1024 * 1024;
const CHUNK_BYTES: usize = 64 * 1024;
pub(super) fn stripe_rows(width: u32, height: u32) -> u32 {
    (STRIP_PIXELS / width.max(1)).max(1).min(256).min(height)
}

pub(crate) struct RowRequest<'a> {
    pub output: &'a Output,
    /// Precision of the requested samples, independently of destination format.
    pub sample_depth: u16,
    pub resampling: Resampling,
    pub first_row: u32,
    pub rows: u32,
    pub provenance: Digest,
}
/// Fill the entire supplied stripe with interleaved straight RGBA, with native16
/// words in big-endian order. Preparation and sampling must preserve the planned
/// crop/padding/resampling/target and complete current composite. Do not supply
/// Photoshop's white-matted composite or substitute zero samples on an error.
pub(crate) trait CapturedExportRows {
    fn scene(&self) -> &SceneSnapshot;
    fn fill(
        &mut self,
        request: RowRequest<'_>,
        output: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String>;
}
pub(crate) struct EncodedPng<'scene, 'output> {
    scene: &'scene SceneSnapshot,
    output: &'output Output,
    resampling: Resampling,
    provenance: Digest,
    bytes: u64,
    sha256: Digest,
    raw_bytes: u64,
    raw_sha256: Digest,
    failed: Cell<bool>,
}
impl EncodedPng<'_, '_> {
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
            return Err("Native PNG encoding receipt is failed".into());
        }
        let result = (|| {
            check()?;
            if binding(self.scene, self.output, self.resampling, check)? != self.provenance {
                return Err(
                    "Native PNG encoding belongs to a changed captured source or output".into(),
                );
            }
            check()
        })();
        if result.is_ok() {
            self.failed.set(false);
        }
        result
    }
    /// The caller independently owns both outputs and all source/session checks.
    /// Equality of complete passes is not a lease, file identity or publish gate.
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
            || self.raw_bytes != other.raw_bytes
            || self.raw_sha256 != other.raw_sha256
        {
            self.failed.set(true);
            other.failed.set(true);
            return Err(
                "Native PNG encoding passes differ in source, rows or encoded bytes".into(),
            );
        }
        check().inspect_err(|_| {
            self.failed.set(true);
            other.failed.set(true);
        })
    }
}

/// PNG0.18.1 keeps three full row buffers, a bounded IDAT buffer and its deflater.
/// ICC writing transiently holds compressed/profile framing buffers. This
/// conservative pinned-backend envelope is held before those allocations and
/// until every encoder/stream destructor finishes; it is not total process RAM.
fn backend_bytes(row: usize, profile: usize) -> Result<usize, String> {
    profile
        .checked_mul(4)
        .and_then(|n| row.checked_mul(8).and_then(|r| n.checked_add(r)))
        .and_then(|n| n.checked_add(4 * 1024 * 1024))
        .ok_or_else(|| "Native PNG backend footprint overflow".into())
}
pub(super) fn profile(scene: &SceneSnapshot) -> Option<&[u8]> {
    let metadata = scene.metadata();
    metadata.profile().or_else(|| {
        metadata
            .srgb_tagged()
            .then(crate::color_profile::srgb_profile)
    })
}
pub(super) fn binding(
    scene: &SceneSnapshot,
    output: &Output,
    resampling: Resampling,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Digest, String> {
    check()?;
    let mut hash = Context::new(&SHA256);
    hash.update(b"PeerBrushNativePngRows\0");
    hash.update(&scene.render_key()?);
    for text in [&scene.identity().project_id, &scene.identity().document_id] {
        hash.update(&(text.len() as u64).to_be_bytes());
        hash.update(text.as_bytes());
    }
    hash.update(&scene.identity().revision.to_be_bytes());
    for value in [
        output.width,
        output.height,
        u32::from(output.native_source_depth),
        u32::from(output.bit_depth),
    ] {
        hash.update(&value.to_be_bytes());
    }
    for value in output.document_rect {
        hash.update(&value.to_be_bytes());
    }
    for value in output.scale_xy {
        hash.update(&value.to_bits().to_be_bytes());
    }
    let target = output.target.as_deref().unwrap_or("");
    hash.update(&(target.len() as u64).to_be_bytes());
    hash.update(target.as_bytes());
    hash.update(&[if resampling == Resampling::Nearest {
        0
    } else {
        1
    }]);
    let icc = profile(scene).unwrap_or(&[]);
    hash.update(&(icc.len() as u64).to_be_bytes());
    hash.update(icc);
    check()?;
    Ok(hash.finish().as_ref().try_into().unwrap())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn write_to<'scene, 'output>(
    scene: &'scene SceneSnapshot,
    output: &'output Output,
    settings: &Settings,
    provider: &mut impl CapturedExportRows,
    writer: &mut dyn Write,
    max_bytes: u64,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<EncodedPng<'scene, 'output>, String> {
    check()?;
    let metadata = scene.metadata();
    if !std::ptr::eq(scene, provider.scene())
        || settings.format != Format::Png
        || output.format != Format::Png
        || output.native_source_depth != metadata.depth()
        || output.bit_depth != metadata.depth()
        || ![8, 16].contains(&output.bit_depth)
        || output.width == 0
        || output.height == 0
        || output.width > 16000
        || output.height > 16000
        || u64::from(output.width) * u64::from(output.height) > 16000 * 16000
        || max_bytes == 0
        || max_bytes > MAX_ENCODED
    {
        return Err("Native PNG source, geometry, precision or output admission differs".into());
    }
    let icc = profile(scene);
    if icc.is_some_and(|p| p.is_empty() || p.len() > crate::color_profile::MAX_PROFILE_BYTES) {
        return Err("Native PNG profile exceeds its exact source envelope".into());
    }
    let row_bytes = (output.width as usize)
        .checked_mul(4 * usize::from(output.bit_depth / 8))
        .ok_or("Native PNG row footprint overflow")?;
    let backend = backend_bytes(row_bytes, icc.map_or(0, <[u8]>::len))?;
    let _backend = scene
        .budget()
        .reserve_bytes(backend, 32 * 1024 * 1024)
        .map_err(|e| e.to_string())?;
    settings.validate()?;
    let rect = output.document_rect;
    let width = i64::from(rect[2]) - i64::from(rect[0]);
    let height = i64::from(rect[3]) - i64::from(rect[1]);
    if width <= 0
        || height <= 0
        || rect.iter().any(|n| n.unsigned_abs() > 116000)
        || output.scale_xy.iter().any(|n| !n.is_finite() || *n <= 0.)
        || (metadata.read_only() && output.target.is_some())
        || output
            .target
            .as_deref()
            .is_some_and(|id| !metadata.layers().any(|layer| layer.id() == id))
    {
        return Err("Native PNG output rectangle, target or scale differs".into());
    }
    let provenance = binding(scene, output, settings.resampling, check)?;
    let stripe = stripe_rows(output.width, output.height);
    let capacity = row_bytes
        .checked_mul(stripe as usize)
        .ok_or("Native PNG stripe footprint overflow")?;
    let mut rows = ScratchBytes::new(
        scene.budget(),
        capacity,
        (STRIP_PIXELS as usize + 15999) * 8,
    )
    .map_err(|e| e.to_string())?;
    let failed = Cell::new(false);
    let active = || {
        if failed.get() {
            return Err("Native PNG encoding is failed".into());
        }
        check().inspect_err(|_| failed.set(true))
    };
    let mut sink = HashOutput {
        writer,
        check: &active,
        failed: &failed,
        limit: max_bytes,
        written: 0,
        hash: Context::new(&SHA256),
    };
    let mut raw = Context::new(&SHA256);
    let mut raw_bytes = 0u64;
    {
        let mut info = png::Info::with_size(output.width, output.height);
        info.icc_profile = icc.map(Cow::Borrowed);
        let mut encoder = png::Encoder::with_info(&mut sink, info).map_err(|e| e.to_string())?;
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(if output.bit_depth == 16 {
            png::BitDepth::Sixteen
        } else {
            png::BitDepth::Eight
        });
        encoder.set_compression(png::Compression::Fast);
        encoder.set_filter(png::Filter::Adaptive);
        let mut png = encoder.write_header().map_err(|e| e.to_string())?;
        {
            let mut stream = png
                .stream_writer_with_size(CHUNK_BYTES)
                .map_err(|e| e.to_string())?;
            let mut first = 0;
            while first < output.height {
                active()?;
                if !std::ptr::eq(scene, provider.scene()) {
                    failed.set(true);
                    return Err("Native PNG provider changed its captured source".into());
                }
                let count = stripe.min(output.height - first);
                let bytes = &mut rows[..row_bytes * count as usize];
                provider
                    .fill(
                        RowRequest {
                            output,
                            sample_depth: output.native_source_depth,
                            resampling: settings.resampling,
                            first_row: first,
                            rows: count,
                            provenance,
                        },
                        bytes,
                        &active,
                    )
                    .inspect_err(|_| failed.set(true))?;
                active()?;
                raw.update(bytes);
                raw_bytes = raw_bytes
                    .checked_add(bytes.len() as u64)
                    .ok_or("Native PNG row count overflow")?;
                // The encoder consumes complete rows internally. HashOutput guards each encoded write.
                stream.write_all(bytes).map_err(|e| {
                    failed.set(true);
                    e.to_string()
                })?;
                first += count;
            }
            active()?;
            stream.finish().map_err(|e| {
                failed.set(true);
                e.to_string()
            })?;
        }
        active()?;
        png.finish().map_err(|e| {
            failed.set(true);
            e.to_string()
        })?;
    }
    active()?;
    if !std::ptr::eq(scene, provider.scene())
        || binding(scene, output, settings.resampling, check)? != provenance
        || raw_bytes
            != u64::from(output.width)
                * u64::from(output.height)
                * 4
                * u64::from(output.bit_depth / 8)
    {
        failed.set(true);
        return Err("Native PNG source or completed row coverage differs".into());
    }
    Ok(EncodedPng {
        scene,
        output,
        resampling: settings.resampling,
        provenance,
        bytes: sink.written,
        sha256: sink.hash.finish().as_ref().try_into().unwrap(),
        raw_bytes,
        raw_sha256: raw.finish().as_ref().try_into().unwrap(),
        failed: Cell::new(false),
    })
}

struct HashOutput<'a> {
    writer: &'a mut dyn Write,
    check: &'a dyn Fn() -> Result<(), String>,
    failed: &'a Cell<bool>,
    limit: u64,
    written: u64,
    hash: Context,
}
impl Write for HashOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        (self.check)().map_err(io::Error::other)?;
        if self
            .written
            .checked_add(bytes.len() as u64)
            .is_none_or(|n| n > self.limit)
        {
            self.failed.set(true);
            return Err(io::Error::other(
                "Native PNG encoded bytes exceed admitted output",
            ));
        }
        let count = self
            .writer
            .write(bytes)
            .inspect_err(|_| self.failed.set(true))?;
        if count > bytes.len() || (count == 0 && !bytes.is_empty()) {
            self.failed.set(true);
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "Invalid native PNG output write",
            ));
        }
        self.hash.update(&bytes[..count]);
        self.written += count as u64;
        (self.check)().map_err(io::Error::other)?;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        (self.check)().map_err(io::Error::other)?;
        self.writer.flush().inspect_err(|_| self.failed.set(true))?;
        (self.check)().map_err(io::Error::other)
    }
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
    use std::{io::Cursor, sync::Arc};
    #[test]
    fn nondivisor_width_is_admitted_before_raw_provider_within_shared_pixel_limit() {
        let scene = fixture_scene(16, 0);
        let mut output = output(&scene);
        output.width = 16000;
        output.height = 17;
        struct Inspect<'a> {
            scene: &'a SceneSnapshot,
            calls: usize,
        }
        impl CapturedExportRows for Inspect<'_> {
            fn scene(&self) -> &SceneSnapshot {
                self.scene
            }
            fn fill(
                &mut self,
                request: RowRequest<'_>,
                _: &mut [u8],
                _: &dyn Fn() -> Result<(), String>,
            ) -> Result<(), String> {
                self.calls += 1;
                assert_eq!(request.rows, 8);
                assert!(request.output.width * request.rows <= STRIP_PIXELS);
                Err("metadata stripe admission observed before samples".into())
            }
        }
        let mut provider = Inspect {
            scene: &scene,
            calls: 0,
        };
        let mut bytes = vec![];
        assert!(write_to(
            &scene,
            &output,
            &Settings::default(),
            &mut provider,
            &mut bytes,
            1024 * 1024,
            &|| Ok(())
        )
        .is_err());
        assert_eq!(provider.calls, 1);
        for width in [1, 3, 513, 8191, 16000] {
            let rows = stripe_rows(width, 10000);
            assert!(rows >= 1 && rows <= 256 && width * rows <= STRIP_PIXELS);
        }
    }
    // Transport-only source. Pixel generation is deliberately independent of
    // the compositor, whose current-scene semantic qualification is separate.
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
    fn fixture_scene(depth: u16, profile_kind: u8) -> SceneSnapshot {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(16 * 1024 * 1024, 16).unwrap();
        let mut doc = Document::new_depth(259, 515, depth).unwrap();
        doc.srgb_tagged = profile_kind == 1;
        if profile_kind == 2 {
            doc.icc_profile = Some(Arc::new((0..1023).map(|n| (n * 73) as u8).collect()));
            doc.read_only = true;
        }
        let mut collect = Collect(vec![]);
        let metadata = Document36::capture(&doc, &mut collect).unwrap();
        let identity = SceneIdentity {
            project_id: "png-transport".into(),
            document_id: metadata.id.clone(),
            revision: metadata.revision,
        };
        let owner = MetadataCharge::reserve(&pool, 65536, &|| Ok(())).unwrap();
        let pending = PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), owner),
            RasterRoster::new(collect.0).unwrap(),
            pool,
            disk,
        )
        .unwrap();
        SceneSnapshot::new(identity, pending, vec![], None, None).unwrap()
    }
    fn output(scene: &SceneSnapshot) -> Output {
        let meta = scene.metadata();
        Output {
            target: None,
            target_name: "Composite".into(),
            variant: "native".into(),
            filename: "transport.png".into(),
            document_rect: [0, 0, meta.width() as i32, meta.height() as i32],
            width: meta.width(),
            height: meta.height(),
            format: Format::Png,
            native_source_depth: meta.depth(),
            bit_depth: meta.depth(),
            scale_xy: [1., 1.],
        }
    }
    fn sample(x: u32, y: u32, c: usize, depth: u16) -> u16 {
        if c == 3 && (x + y) % 7 == 0 {
            return 0;
        }
        if depth == 8 {
            ((x * 47 + y * 83 + c as u32 * 31) % 256) as u16
        } else {
            (10001 + x * 137 + y * 173 + c as u32 * 71) as u16
        }
    }
    struct Rows<'a> {
        scene: &'a SceneSnapshot,
        calls: usize,
        fail_at: Option<usize>,
        flip: bool,
        uniform: bool,
        failed: &'a Cell<bool>,
        peak: usize,
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
            assert!(r.rows > 0 && r.rows <= 256);
            assert_eq!(r.sample_depth, r.output.native_source_depth);
            assert_eq!(r.resampling, Resampling::Bilinear);
            assert_eq!(
                r.provenance,
                binding(self.scene, r.output, r.resampling, check)?
            );
            assert_eq!(
                bytes.len(),
                r.output.width as usize * r.rows as usize * 4 * usize::from(r.output.bit_depth / 8)
            );
            self.peak = self.peak.max(bytes.len());
            for y in 0..r.rows {
                for x in 0..r.output.width {
                    for c in 0..4 {
                        let mut value = if self.uniform {
                            [10001, 27003, 52007, 45679][c]
                        } else {
                            sample(x, r.first_row + y, c, r.output.bit_depth)
                        };
                        if self.flip && x == 0 && y == 0 && c == 0 {
                            value ^= 1;
                        }
                        let at = ((y * r.output.width + x) as usize * 4 + c)
                            * usize::from(r.output.bit_depth / 8);
                        if r.output.bit_depth == 16 {
                            bytes[at..at + 2].copy_from_slice(&value.to_be_bytes());
                        } else {
                            bytes[at] = value as u8;
                        }
                    }
                }
            }
            if self.fail_at == Some(self.calls) {
                self.failed.set(true);
                return Err("Injected prepared row refusal".into());
            }
            Ok(())
        }
    }
    fn rows<'a>(scene: &'a SceneSnapshot, failed: &'a Cell<bool>) -> Rows<'a> {
        Rows {
            scene,
            calls: 0,
            fail_at: None,
            flip: false,
            uniform: false,
            failed,
            peak: 0,
        }
    }
    #[test]
    fn native8_native16_png_transport_keeps_alpha_lowbits_profile_and_complete_multistrip_rows() {
        for depth in [8, 16] {
            for kind in 0..3 {
                let scene = fixture_scene(depth, kind);
                let output = output(&scene);
                let settings = Settings::default();
                let before = scene.render_key().unwrap();
                let failed = Cell::new(false);
                let mut source = rows(&scene, &failed);
                let mut bytes = vec![];
                let proof = write_to(
                    &scene,
                    &output,
                    &settings,
                    &mut source,
                    &mut bytes,
                    8 * 1024 * 1024,
                    &|| Ok(()),
                )
                .unwrap();
                assert_eq!(proof.bytes(), bytes.len() as u64);
                assert_eq!(
                    &proof.sha256()[..],
                    ring::digest::digest(&SHA256, &bytes).as_ref()
                );
                assert_eq!(source.calls, 3);
                assert!(source.peak <= (STRIP_PIXELS as usize + 258) * usize::from(depth / 8) * 4);
                let mut decoder = ::png::Decoder::new(Cursor::new(&bytes));
                decoder.set_transformations(::png::Transformations::IDENTITY);
                let mut decoder = decoder.read_info().unwrap();
                assert_eq!(decoder.info().icc_profile.as_deref(), profile(&scene));
                assert_eq!(
                    decoder.info().bit_depth,
                    if depth == 16 {
                        ::png::BitDepth::Sixteen
                    } else {
                        ::png::BitDepth::Eight
                    }
                );
                let mut raw = Context::new(&SHA256);
                let mut y = 0;
                while let Some(row) = decoder.next_row().unwrap() {
                    let data = row.data();
                    raw.update(data);
                    for x in 0..output.width {
                        for c in 0..4 {
                            let at = (x as usize * 4 + c) * usize::from(depth / 8);
                            let value = sample(x, y, c, depth);
                            if depth == 16 {
                                assert_eq!(&data[at..at + 2], &value.to_be_bytes());
                            } else {
                                assert_eq!(data[at], value as u8);
                            }
                        }
                    }
                    y += 1;
                }
                decoder.finish().unwrap();
                assert_eq!(y, output.height);
                assert_eq!(raw.finish().as_ref(), &proof.raw_sha256()[..]);
                proof.recheck(&|| Ok(())).unwrap();
                assert_eq!(scene.render_key().unwrap(), before);
            }
        }
    }
    #[test]
    fn counted_and_written_native_png_passes_compare_all_rows_binding_and_encoded_bytes() {
        let scene = fixture_scene(16, 1);
        let output = output(&scene);
        let settings = Settings::default();
        let failed = Cell::new(false);
        let count = write_to(
            &scene,
            &output,
            &settings,
            &mut rows(&scene, &failed),
            &mut std::io::sink(),
            8 * 1024 * 1024,
            &|| Ok(()),
        )
        .unwrap();
        let mut bytes = vec![];
        let written = write_to(
            &scene,
            &output,
            &settings,
            &mut rows(&scene, &failed),
            &mut bytes,
            count.bytes(),
            &|| Ok(()),
        )
        .unwrap();
        count.same_encoding(&written, &|| Ok(())).unwrap();
        let mut changed = rows(&scene, &failed);
        changed.flip = true;
        let changed = write_to(
            &scene,
            &output,
            &settings,
            &mut changed,
            &mut std::io::sink(),
            8 * 1024 * 1024,
            &|| Ok(()),
        )
        .unwrap();
        assert!(count.same_encoding(&changed, &|| Ok(())).is_err());
        let mut uniform = rows(&scene, &failed);
        uniform.uniform = true;
        let uniform = write_to(
            &scene,
            &output,
            &settings,
            &mut uniform,
            &mut std::io::sink(),
            8 * 1024 * 1024,
            &|| Ok(()),
        )
        .unwrap();
        assert!(
            uniform.bytes() < uniform.raw_bytes / 16,
            "Known uniform data remains compressed"
        );
        assert!(written
            .recheck(&|| Err("Cancelled completed encoding".into()))
            .is_err());
        assert!(written.recheck(&|| Ok(())).is_err());
    }
    struct Observed<'a> {
        bytes: Vec<u8>,
        stop: &'a Cell<bool>,
        after: usize,
        fail: bool,
    }
    impl Write for Observed<'_> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if self.stop.get() {
                self.after += 1;
            }
            if self.fail {
                self.stop.set(true);
                return Err(io::Error::other("Injected output refusal"));
            }
            self.bytes.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn prepared_row_errors_and_writer_errors_latch_before_encoder_drop_can_write_more() {
        for transport in [false, true] {
            let scene = fixture_scene(16, 0);
            let output = output(&scene);
            let stop = Cell::new(false);
            let mut source = rows(&scene, &stop);
            if !transport {
                source.fail_at = Some(2);
            }
            let mut writer = Observed {
                bytes: vec![],
                stop: &stop,
                after: 0,
                fail: transport,
            };
            assert!(write_to(
                &scene,
                &output,
                &Settings::default(),
                &mut source,
                &mut writer,
                8 * 1024 * 1024,
                &|| Ok(())
            )
            .is_err());
            assert_eq!(writer.after, 0);
        }
    }
    #[test]
    fn output_limits_resident_admission_cancel_and_wrong_scene_refuse_without_complete_receipt() {
        let scene = fixture_scene(16, 0);
        let other = fixture_scene(16, 0);
        let output = output(&scene);
        let stop = Cell::new(false);
        let mut source = rows(&scene, &stop);
        let mut bytes = vec![];
        assert!(write_to(
            &scene,
            &output,
            &Settings::default(),
            &mut source,
            &mut bytes,
            8,
            &|| Ok(())
        )
        .is_err());
        assert!(bytes.len() <= 8);
        assert_eq!(source.calls, 0);
        bytes.clear();
        assert!(write_to(
            &scene,
            &output,
            &Settings::default(),
            &mut rows(&other, &stop),
            &mut bytes,
            8 * 1024 * 1024,
            &|| Ok(())
        )
        .is_err());
        assert!(bytes.is_empty());
        assert!(write_to(
            &scene,
            &output,
            &Settings::default(),
            &mut source,
            &mut bytes,
            8 * 1024 * 1024,
            &|| Err("Canceled native export".into())
        )
        .is_err());
        assert!(bytes.is_empty());
        let pressure = scene
            .budget()
            .reserve_bytes(
                scene.budget().limit() - scene.budget().used() - 1,
                64 * 1024 * 1024,
            )
            .unwrap();
        assert!(write_to(
            &scene,
            &output,
            &Settings::default(),
            &mut source,
            &mut bytes,
            8 * 1024 * 1024,
            &|| Ok(())
        )
        .is_err());
        assert!(bytes.is_empty());
        assert_eq!(source.calls, 0);
        drop(pressure);
    }
}
