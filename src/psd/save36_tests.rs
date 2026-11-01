//! Small owned codec fixtures, not installed-profile/runtime qualification.
use super::*;
use crate::{
    engine::{
        scene::{
            Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, SceneIdentity,
        },
        Document, Mask, MaskStep,
    },
    large_document::{DiskBudget, Limits as StoreLimits, ResidentBudget, Store},
    psd::standard36::{NativeRows, RowRequest, RowSource},
    raster::{backed::Region, Raster, Surface},
};
use std::{cell::Cell, fs, path::PathBuf};

struct Fixture {
    root: PathBuf,
    store: Store,
    pool: ResidentBudget,
    disk: DiskBudget,
}
impl Fixture {
    fn new() -> Self {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = parent.join(format!("pb-source36-save-{}", crate::engine::id()));
        fs::create_dir(&root).unwrap();
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(64 * 1024 * 1024, 32).unwrap();
        let store = Store::create_with_disk_budget(
            &root.join("source"),
            pool.clone(),
            disk.clone(),
            StoreLimits {
                disk_bytes: 16 * 1024 * 1024,
                max_blocks: 64,
                max_resident_blocks: 8,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        Self {
            root,
            store,
            pool,
            disk,
        }
    }
    fn roots(&self) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        (
            self.root.join("layers"),
            self.root.join("composite"),
            self.root.join("manifest"),
            self.root.join("container"),
        )
    }
    fn finish(self) {
        let Self {
            root,
            store,
            pool,
            disk,
        } = self;
        drop(store);
        drop(pool);
        drop(disk);
        let owned = fs::canonicalize(&root).unwrap();
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        assert_eq!(owned.parent(), Some(parent.as_path()));
        assert!(owned
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pb-source36-save-"));
        fs::remove_dir_all(owned).unwrap();
    }
}
struct Collect {
    store: Store,
    sources: Vec<Surface>,
}
impl RasterCollector for Collect {
    fn resident(&mut self, raster: &Raster) -> Result<RasterRef36, String> {
        let source = Surface::stage_raster(raster, self.store.clone(), &mut || Ok(()))
            .map_err(|e| e.to_string())?;
        self.sources.push(source);
        Ok(RasterRef36 {
            id: self.sources.len() as u32,
            width: raster.width,
            height: raster.height,
            depth: raster.depth,
            retained: None,
        })
    }
    fn surface(&mut self, source: &Surface) -> Result<RasterRef36, String> {
        self.sources.push(source.clone());
        Ok(RasterRef36 {
            id: self.sources.len() as u32,
            width: source.width(),
            height: source.height(),
            depth: source.depth(),
            retained: None,
        })
    }
}
fn document(depth: u16) -> Document {
    let mut doc = Document::new_depth(5, 259, depth).unwrap();
    let layer = &mut doc.layers[0];
    layer.x = -2;
    layer.y = -1;
    layer.pixels = Raster::new_depth(5, 259, depth);
    let mut mask = Raster::new_depth(5, 259, depth);
    for y in 0..259 {
        for x in 0..5 {
            let p = [
                (10001 + x * 137 + y * 173) as u16,
                (37003 + x * 71 + y * 53) as u16,
                55123,
                if (x + y) % 7 == 0 {
                    0
                } else {
                    (19001 + x * 113 + y * 59) as u16
                },
            ];
            let g = (21001 + x * 19 + y * 127) as u16;
            if depth == 16 {
                layer.pixels.set16(x, y, p);
                mask.set16(x, y, [g, g, g, 65535]);
            } else {
                layer.pixels.set(x, y, p.map(crate::raster::project16));
                mask.set(
                    x,
                    y,
                    [(g / 257) as u8, (g / 257) as u8, (g / 257) as u8, 255],
                );
            }
        }
    }
    // Preserve physical edge padding too, although it is outside logical pixels.
    if depth == 16 {
        let tile = std::sync::Arc::make_mut(layer.pixels.samples16.get_mut(&(0, 0)).unwrap());
        let at = (255 * 256 + 255) * 4;
        tile[at..at + 4].copy_from_slice(&[44445, 55551, 12345, 56789]);
    } else {
        let tile = std::sync::Arc::make_mut(layer.pixels.tiles.get_mut(&(0, 0)).unwrap());
        let at = (255 * 256 + 255) * 4;
        tile[at..at + 4].copy_from_slice(&[171, 219, 43, 231]);
    }
    layer.mask = Some(Mask {
        enabled: false,
        linked: true,
        cache_key: crate::engine::id(),
        steps: vec![MaskStep {
            id: crate::engine::id(),
            kind: "paint".into(),
            enabled: true,
            weight: 1.,
            value: 255.,
            pixels: mask,
            settings: serde_json::Value::Null,
        }],
    });
    doc
}
fn capture(f: &Fixture, doc: &Document) -> SceneSnapshot {
    let mut collect = Collect {
        store: f.store.clone(),
        sources: vec![],
    };
    let metadata = Document36::capture(doc, &mut collect).unwrap();
    let owner = MetadataCharge::reserve(&f.pool, 65536, &|| Ok(())).unwrap();
    let pending = PendingScene::new(
        Admitted::from_parts(Arc::new(metadata), owner),
        RasterRoster::new(collect.sources).unwrap(),
        f.pool.clone(),
        f.disk.clone(),
    )
    .unwrap();
    SceneSnapshot::new(
        SceneIdentity {
            project_id: "source-codec-fixture".into(),
            document_id: doc.id.clone(),
            revision: doc.revision,
        },
        pending,
        vec![],
        None,
        None,
    )
    .unwrap()
}
fn native_capture(f: &Fixture, doc: &Document) -> SceneSnapshot {
    let pending = capture(f, doc).into_provisional_pending().unwrap();
    let native = crate::engine::native::NativeDocument::from_pending(pending).unwrap();
    SceneSnapshot::from_native(
        SceneIdentity {
            project_id: "source-codec-fixture".into(),
            document_id: doc.id.clone(),
            revision: doc.revision,
        },
        native,
        vec![],
        None,
        None,
    )
    .unwrap()
}
fn limits() -> SaveLimits {
    SaveLimits {
        metadata: MetadataLimits {
            bytes: 65536,
            nodes: 32768,
            depth: 64,
        },
        bulk: BulkLimits {
            rasters: 16,
            tiles: 32,
            references: 32,
            chunks: 16,
            encoded_bytes: 16 * 1024 * 1024,
        },
        file_bytes: 16 * 1024 * 1024,
    }
}
struct PreparedRows<'a> {
    scene: &'a SceneSnapshot,
    calls: usize,
    stale: Option<&'a Cell<bool>>,
    fail_at: Option<usize>,
    replace: Option<(PathBuf, PathBuf)>,
    replacement_denied: Option<std::io::ErrorKind>,
}
impl CapturedRows for PreparedRows<'_> {
    fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
}
impl NativeRows for PreparedRows<'_> {
    fn provenance(&self) -> Digest {
        super::super::scene_save36::provenance(self.scene)
    }
    fn fill(
        &mut self,
        r: RowRequest,
        output: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        self.calls += 1;
        if let Some((named, original)) = self.replace.take() {
            if let Err(error) = fs::rename(&named, &original) {
                self.replacement_denied = Some(error.kind());
                return Err(error.to_string());
            }
            fs::write(&named, b"foreign stage replacement").map_err(|e| e.to_string())?;
        }
        let layer = self.scene.metadata().layers().next().unwrap();
        let merged =
            r.source == RowSource::MergedWhiteMatte || r.source == RowSource::BakedComposite;
        assert!(matches!(
            r.source,
            RowSource::Layer(0)
                | RowSource::OriginalLayer(0)
                | RowSource::MergedWhiteMatte
                | RowSource::BakedComposite
        ));
        let reference = if r.channel == -2 {
            layer.mask().unwrap().steps().next().unwrap().pixels()
        } else {
            layer.pixels()
        };
        let source = self.scene.raster(reference.slot())?;
        let left = r.bounds[0] - layer.origin()[0];
        let top = r.bounds[1] + r.first_row as i32 - layer.origin()[1];
        let width = (r.bounds[2] - r.bounds[0]) as usize;
        let region = Region::new([left, top, left + width as i32, top + r.rows as i32])
            .map_err(|e| e.to_string())?;
        let mut guarded = || io_check(check);
        let prepared = source
            .prepare_region(region, &mut guarded)
            .map_err(|e| e.to_string())?;
        assert_eq!(prepared.key(), source.key());
        assert_eq!(
            output.len(),
            width * r.rows as usize * usize::from(r.depth / 8)
        );
        for row in 0..r.rows {
            check()?;
            for x in 0..width {
                let p = prepared
                    .pixel16(left + x as i32, top + row as i32)
                    .map_err(|e| e.to_string())?;
                let channel = if r.channel == -2 {
                    0
                } else if r.channel == -1 {
                    3
                } else {
                    r.channel as usize
                };
                let mut sample = p[channel];
                // This fixture is a single normal/opaque-opacity layer with its mask
                // disabled. The expected merged source is that actual pinned plane.
                if merged && r.source == RowSource::MergedWhiteMatte && channel < 3 {
                    if r.depth == 16 {
                        sample = ((u64::from(sample) * u64::from(p[3])
                            + 65535 * (65535 - u64::from(p[3]))
                            + 32767)
                            / 65535) as u16;
                    } else {
                        let c = u32::from(sample / 257);
                        let a = u32::from(p[3] / 257);
                        sample = (((c * a + 255 * (255 - a) + 127) / 255) as u16) * 257;
                    }
                }
                let at = (row as usize * width + x) * usize::from(r.depth / 8);
                if r.depth == 16 {
                    output[at..at + 2].copy_from_slice(&sample.to_be_bytes());
                } else {
                    output[at] = (sample / 257) as u8;
                }
            }
        }
        if self.fail_at == Some(self.calls) {
            if let Some(stale) = self.stale {
                stale.set(true);
            } else {
                return Err("Injected prepared source failure".into());
            }
        }
        Ok(())
    }
}
fn provider(scene: &SceneSnapshot) -> PreparedRows<'_> {
    PreparedRows {
        scene,
        calls: 0,
        stale: None,
        fail_at: None,
        replace: None,
        replacement_denied: None,
    }
}

#[test]
fn actual_owned_small_native8_native16_psd_psb_driver_reopens_and_retires_complete_source() {
    for (format, native) in [
        (Format::Psd, false),
        (Format::Psb, false),
        (Format::Psd, true),
        (Format::Psb, true),
    ] {
        for depth in [8, 16] {
            let f = Fixture::new();
            let doc = document(depth);
            let scene = if native {
                native_capture(&f, &doc)
            } else {
                capture(&f, &doc)
            };
            let baseline = f.disk.used().unwrap();
            let (layers, composite, manifest, container) = f.roots();
            let mut rows = provider(&scene);
            let done = prepare(
                &scene,
                format,
                OwnedRoots {
                    layers: &layers,
                    composite: &composite,
                    manifest: &manifest,
                    container: &container,
                },
                &mut rows,
                limits(),
                &|| Ok(()),
                &|| Ok(()),
            )
            .unwrap();
            assert!(std::ptr::eq(done.scene(), &scene));
            assert_eq!(
                done.provenance(),
                super::super::scene_save36::provenance(&scene)
            );
            assert_eq!(done.core_sha256(), done.candidate().binding());
            assert_eq!(done.bytes(), done.candidate().payload_length());
            assert_eq!(done.sha256(), done.candidate().payload_sha256());
            done.recheck(&|| Ok(())).unwrap();
            let mut guarded = || Ok(());
            let input = done
                .candidate()
                .cursor(0, done.bytes(), &mut guarded)
                .unwrap();
            let source = Source::with_buffer(
                input,
                done.bytes(),
                ScratchBytes::new(&f.pool, 65536, 65536).unwrap(),
            )
            .unwrap();
            let index = structural::index(
                &mut source
                    .range(
                        Span {
                            offset: 0,
                            bytes: done.bytes(),
                        },
                        &|| Ok(()),
                    )
                    .unwrap(),
                structural::Limits::current(),
                || Ok(()),
            )
            .unwrap();
            assert_eq!(index.header.depth, depth);
            assert_eq!(index.header.height, 259);
            assert_eq!(index.layers[0].bounds, [-1, -2, 258, 3]);
            let mask = index.layers[0]
                .channels
                .iter()
                .find(|c| c.id == -2)
                .unwrap();
            let mut decoded = source
                .rows(mask.encoding.as_ref().unwrap(), &|| Ok(()))
                .unwrap();
            let mut row = vec![0; 5 * usize::from(depth / 8)];
            assert!(decoded.read_row(&mut row).unwrap());
            assert!(
                row.iter().any(|&v| v != 255),
                "disabled native mask data is retained"
            );
            drop(decoded);
            drop(source);
            assert!(
                rows.calls > 20,
                "more than one stripe and separate readback fidelity"
            );
            done.retire(&|| Ok(())).unwrap();
            assert_eq!(f.disk.used().unwrap(), baseline);
            drop(rows);
            drop(scene);
            drop(doc);
            f.finish();
        }
    }
}

#[test]
fn closed_driver_stale_cancel_and_foreign_scratch_fail_without_source_or_destination_mutation() {
    for fault in 0..3 {
        let f = Fixture::new();
        let doc = document(16);
        let scene = capture(&f, &doc);
        let (layers, composite, manifest, container) = f.roots();
        let stale = Cell::new(false);
        let mut rows = provider(&scene);
        rows.stale = Some(&stale);
        rows.fail_at = if fault == 0 { Some(3) } else { None };
        if fault == 2 {
            rows.replace = Some((
                layers.join("bytes.pbb"),
                f.root.join("retained-owned-stage.pbb"),
            ));
        }
        let source_key = scene
            .raster(scene.metadata().layers().next().unwrap().pixels().slot())
            .unwrap()
            .key();
        let check = || {
            if fault == 1 {
                Err("Cancelled source save".into())
            } else if stale.get() {
                Err("Source revision changed".into())
            } else {
                Ok(())
            }
        };
        let baseline = f.disk.used().unwrap();
        let result = prepare(
            &scene,
            Format::Psb,
            OwnedRoots {
                layers: &layers,
                composite: &composite,
                manifest: &manifest,
                container: &container,
            },
            &mut rows,
            limits(),
            &check,
            &|| Ok(()),
        );
        assert!(result.is_err());
        assert!(!container.exists());
        assert_eq!(
            scene
                .raster(scene.metadata().layers().next().unwrap().pixels().slot())
                .unwrap()
                .key(),
            source_key
        );
        if fault == 2 && rows.replacement_denied.is_some() {
            assert!(cfg!(windows));
            assert_eq!(
                rows.replacement_denied,
                Some(std::io::ErrorKind::PermissionDenied)
            );
            assert!(!layers.exists());
            assert_eq!(f.disk.used().unwrap(), baseline);
        } else if fault == 2 {
            assert_eq!(rows.replacement_denied, None);
            assert_eq!(
                fs::read(layers.join("bytes.pbb")).unwrap(),
                b"foreign stage replacement"
            );
            assert!(f.disk.used().unwrap() > baseline);
        } else {
            assert_eq!(f.disk.used().unwrap(), baseline);
        }
        drop(rows);
        drop(scene);
        drop(doc);
        f.finish();
    }
}

#[test]
fn completed_save_recheck_refuses_replacement_and_never_regains_authority_after_failure() {
    let f = Fixture::new();
    let doc = document(16);
    let scene = capture(&f, &doc);
    let (layers, composite, manifest, container) = f.roots();
    let mut rows = provider(&scene);
    let done = prepare(
        &scene,
        Format::Psb,
        OwnedRoots {
            layers: &layers,
            composite: &composite,
            manifest: &manifest,
            container: &container,
        },
        &mut rows,
        limits(),
        &|| Ok(()),
        &|| Ok(()),
    )
    .unwrap();
    let named = container.join("bytes.pbb");
    let original = f.root.join("owned-container-original.pbb");
    let renamed = fs::rename(&named, &original);
    if let Err(error) = renamed {
        assert!(cfg!(windows));
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!original.exists());
        done.recheck(&|| Ok(())).unwrap();
        assert!(done
            .recheck(&|| Err("Cancelled final source check".into()))
            .is_err());
    } else {
        fs::write(&named, b"foreign completed candidate").unwrap();
        assert!(done.recheck(&|| Ok(())).is_err());
        assert_eq!(fs::read(&named).unwrap(), b"foreign completed candidate");
        fs::remove_file(&named).unwrap();
        fs::rename(&original, &named).unwrap();
    }
    assert!(done.recheck(&|| Ok(())).is_err());
    done.retire(&|| Ok(())).unwrap();
    drop(rows);
    drop(scene);
    drop(doc);
    f.finish();
}

#[test]
fn closed_driver_resident_admission_refusal_preserves_source_before_any_staging_creation() {
    let f = Fixture::new();
    let doc = document(16);
    let scene = capture(&f, &doc);
    let (layers, composite, manifest, container) = f.roots();
    let mut rows = provider(&scene);
    let baseline = f.pool.used();
    let disk = f.disk.used().unwrap();
    let remaining = f.pool.limit() - baseline - 128;
    let pressure = ScratchBytes::new(&f.pool, remaining, 64 * 1024 * 1024).unwrap();
    assert!(prepare(
        &scene,
        Format::Psb,
        OwnedRoots {
            layers: &layers,
            composite: &composite,
            manifest: &manifest,
            container: &container
        },
        &mut rows,
        limits(),
        &|| Ok(()),
        &|| Ok(())
    )
    .is_err());
    assert_eq!(rows.calls, 0);
    assert!(!layers.exists() && !composite.exists() && !manifest.exists() && !container.exists());
    assert_eq!(f.disk.used().unwrap(), disk);
    drop(pressure);
    assert_eq!(f.pool.used(), baseline);
    drop(rows);
    drop(scene);
    drop(doc);
    f.finish();
}

struct ImportOwner {
    store: Store,
    alive: Cell<bool>,
}
impl crate::psd::import36::ImportSession for ImportOwner {
    fn store(&self) -> &Store {
        &self.store
    }
    fn check(&self) -> Result<(), String> {
        if self.alive.get() {
            Ok(())
        } else {
            Err("Import session closed".into())
        }
    }
}
#[test]
fn actual_owned_native_save_import_keeps_provisional_gate_and_exact_physical_source_payloads() {
    use crate::psd::import36::{self, ProvisionalImport, QualifiedImport};
    for (depth, revoke_during_handoff) in [(8, false), (16, false), (16, true)] {
        let f = Fixture::new();
        let doc = document(depth);
        let scene = capture(&f, &doc);
        let (layers, composite, manifest, container) = f.roots();
        let mut rows = provider(&scene);
        let saved = prepare(
            &scene,
            Format::Psb,
            OwnedRoots {
                layers: &layers,
                composite: &composite,
                manifest: &manifest,
                container: &container,
            },
            &mut rows,
            limits(),
            &|| Ok(()),
            &|| Ok(()),
        )
        .unwrap();
        let fresh = Store::create_with_disk_budget(
            &f.root.join("import"),
            f.pool.clone(),
            f.disk.clone(),
            StoreLimits {
                disk_bytes: 16 * 1024 * 1024,
                max_blocks: 64,
                max_resident_blocks: 8,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        assert_ne!(fresh.identity(), f.store.identity());
        let session = ImportOwner {
            store: fresh,
            alive: Cell::new(true),
        };
        let index_owner = MetadataCharge::reserve(&f.pool, 4 * 1024 * 1024, &|| Ok(())).unwrap();
        let mut guarded = || Ok(());
        let input = saved
            .candidate()
            .cursor(0, saved.bytes(), &mut guarded)
            .unwrap();
        let source = Source::with_buffer(
            input,
            saved.bytes(),
            ScratchBytes::new(&f.pool, 65536, 65536).unwrap(),
        )
        .unwrap();
        let index = Arc::new(
            structural::index(
                &mut source
                    .range(
                        Span {
                            offset: 0,
                            bytes: saved.bytes(),
                        },
                        &|| Ok(()),
                    )
                    .unwrap(),
                structural::Limits::current(),
                || Ok(()),
            )
            .unwrap(),
        );
        let import_limits = import36::ImportLimits {
            metadata: limits().metadata,
            bulk: limits().bulk,
            working: WorkingSet {
                raw_bytes: 65536,
                canonical_core_bytes: 65536,
                canonical_manifest_bytes: 65536,
                typed_metadata_bytes: 2 * 1024 * 1024,
                tile_scratch_bytes: bulk36::MAX_TILE_BYTES,
                index_scratch_bytes: 2 * 1024 * 1024,
            },
        };
        let provisional = import36::prepare(
            &source,
            index,
            index_owner,
            saved.sha256(),
            &session,
            "import-codec-fixture".into(),
            import_limits,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(provisional.scene().metadata().id(), doc.id);
        assert_eq!(
            provisional
                .scene()
                .metadata()
                .layers()
                .next()
                .unwrap()
                .origin()[0],
            -2
        );
        assert!(!provisional
            .scene()
            .metadata()
            .layers()
            .next()
            .unwrap()
            .mask()
            .unwrap()
            .enabled());
        assert!(provisional
            .compare(&mut rows, &|_| Ok(()), &|| Ok(()))
            .is_err());
        let mut imported_rows = provider(provisional.scene());
        assert!(provisional
            .compare(
                &mut imported_rows,
                &|_| Err("Unsupported fixture semantics".into()),
                &|| Ok(())
            )
            .is_err());
        let compared = provisional
            .compare(
                &mut imported_rows,
                &|s| {
                    // Actual fixture semantic subset only, not the application's complete
                    // supported-source validator or installed-profile admission.
                    if s.metadata().read_only()
                        || s.metadata().profile().is_some()
                        || s.metadata().layers().len() != 1
                        || s.metadata().layers().next().unwrap().kind() != "paint"
                        || s.metadata().filters().len() != 0
                        || s.metadata().layers().next().unwrap().effects().len() != 0
                    {
                        return Err("Fixture semantics differ".into());
                    }
                    s.metadata().visit_rasters(|r| {
                        s.raster(r.slot())?;
                        Ok(())
                    })?;
                    Ok(())
                },
                &|| Ok(()),
            )
            .unwrap();
        drop(imported_rows);
        let qualified = ProvisionalImport::qualify(provisional, compared, &|| Ok(())).unwrap();
        let invoked = Cell::new(false);
        let result = QualifiedImport::map_pending(
            qualified,
            &|| {
                if revoke_during_handoff {
                    session.alive.set(false);
                }
                Ok(())
            },
            |pending, _| {
                invoked.set(true);
                Ok(pending)
            },
        );
        assert_eq!(invoked.get(), !revoke_during_handoff);
        if revoke_during_handoff {
            assert!(result.is_err());
        } else {
            let pending = result.unwrap();
            let reference = &pending.metadata().layers[0].pixels;
            let surface = pending.rasters().get(reference).unwrap();
            assert_ne!(
                surface.key(),
                scene
                    .raster(scene.metadata().layers().next().unwrap().pixels().slot())
                    .unwrap()
                    .key()
            );
            let tile = surface.prepare_tile((0, 0), &mut || Ok(())).unwrap();
            let bytes = tile.bytes().unwrap();
            let at = (255 * 256 + 255) * 4;
            if depth == 16 {
                assert_eq!(
                    &bytes[at * 2..at * 2 + 8],
                    &[44445u16, 55551, 12345, 56789]
                        .into_iter()
                        .flat_map(u16::to_be_bytes)
                        .collect::<Vec<_>>()
                );
            } else {
                assert_eq!(&bytes[at..at + 4], &[171, 219, 43, 231]);
            }
            drop(tile);
            drop(pending);
        }
        drop(source);
        drop(session);
        saved.retire(&|| Ok(())).unwrap();
        drop(rows);
        drop(scene);
        drop(doc);
        f.finish();
    }
}
