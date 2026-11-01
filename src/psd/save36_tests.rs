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
            fs::rename(&named, &original).map_err(|e| e.to_string())?;
            fs::write(&named, b"foreign stage replacement").map_err(|e| e.to_string())?;
        }
        let layer = &self.scene.metadata().layers[0];
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
            &layer.mask.as_ref().unwrap().steps[0].pixels
        } else {
            &layer.pixels
        };
        let source = self.scene.raster(reference)?;
        let left = r.bounds[0] - layer.x;
        let top = r.bounds[1] + r.first_row as i32 - layer.y;
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
    }
}

#[test]
fn actual_owned_small_native8_native16_psd_psb_driver_reopens_and_retires_complete_source() {
    for format in [Format::Psd, Format::Psb] {
        for depth in [8, 16] {
            let f = Fixture::new();
            let doc = document(depth);
            let scene = capture(&f, &doc);
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
            let mut input = done
                .candidate()
                .cursor(0, done.bytes(), &mut guarded)
                .unwrap();
            let index =
                structural::index(&mut input, structural::Limits::current(), || Ok(())).unwrap();
            assert_eq!(index.header.depth, depth);
            assert_eq!(index.header.height, 259);
            assert_eq!(index.layers[0].bounds, [-1, -2, 258, 3]);
            let source = Source::new(input, done.bytes()).unwrap();
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
            .raster(&scene.metadata().layers[0].pixels)
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
                .raster(&scene.metadata().layers[0].pixels)
                .unwrap()
                .key(),
            source_key
        );
        if fault == 2 {
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
    fs::rename(&named, &original).unwrap();
    fs::write(&named, b"foreign completed candidate").unwrap();
    assert!(done.recheck(&|| Ok(())).is_err());
    assert_eq!(fs::read(&named).unwrap(), b"foreign completed candidate");
    fs::remove_file(&named).unwrap();
    fs::rename(&original, &named).unwrap();
    assert!(done.recheck(&|| Ok(())).is_err());
    done.retire(&|| Ok(())).unwrap();
    drop(rows);
    drop(scene);
    drop(doc);
    f.finish();
}
