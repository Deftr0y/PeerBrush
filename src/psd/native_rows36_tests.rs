use super::*;
use crate::{
    engine::{
        scene::{
            Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, Retained36,
            SceneIdentity, Warp36,
        },
        Document, Layer,
    },
    large_document::{DiskBudget, Limits, ResidentBudget, Store},
    psd::memory36::{Admitted, MetadataCharge},
    raster::{backed::Plane, Raster, Surface},
};
use std::sync::Arc;

struct Fixture {
    scene: Option<SceneSnapshot>,
    stores: Vec<Store>,
    paths: Vec<std::path::PathBuf>,
    pool: ResidentBudget,
}
impl Fixture {
    fn new(doc: &Document) -> Self {
        Self::build(doc, false)
    }
    fn build(doc: &Document, foreign: bool) -> Self {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(32 * 1024 * 1024, 4).unwrap();
        let mut paths = Vec::new();
        let mut stores = Vec::new();
        for _ in 0..if foreign { 2 } else { 1 } {
            let path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("pb-standard-rows-{}", crate::engine::id()));
            stores.push(
                Store::create_with_disk_budget(
                    &path,
                    pool.clone(),
                    disk.clone(),
                    Limits {
                        disk_bytes: 32 * 1024 * 1024,
                        max_blocks: 64,
                        max_resident_blocks: 16,
                        manifest_bytes: 65536,
                    },
                )
                .unwrap(),
            );
            paths.push(path);
        }
        struct Capture<'a> {
            store: &'a Store,
            entries: Vec<Surface>,
        }
        impl RasterCollector for Capture<'_> {
            fn resident(&mut self, raster: &Raster) -> Result<RasterRef36> {
                let mut current = raster.clone();
                current.retained = None;
                let surface = Surface::stage_raster(&current, self.store.clone(), &mut || Ok(()))
                    .map_err(|e| e.to_string())?;
                self.entries.push(surface);
                let id = self.entries.len() as u32;
                let retained = raster
                    .retained
                    .as_ref()
                    .map(|original| -> Result<_> {
                        Ok(Box::new(Retained36 {
                            pixels: self.resident(&original.pixels)?,
                            matrix: original.matrix,
                            projective: original.projective,
                            warps: original.warps.iter().map(Warp36::capture).collect(),
                        }))
                    })
                    .transpose()?;
                Ok(RasterRef36 {
                    id,
                    width: raster.width,
                    height: raster.height,
                    depth: raster.depth,
                    retained,
                })
            }
            fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
                Err("Fixture accepts resident oracle capture only".into())
            }
        }
        let mut capture = Capture {
            store: &stores[0],
            entries: vec![],
        };
        let metadata = Document36::capture(doc, &mut capture).unwrap();
        if foreign {
            let source =
                Surface::stage_raster(&doc.layers[0].pixels, stores[1].clone(), &mut || Ok(()))
                    .unwrap();
            let blocks = source
                .physical_tiles()
                .map(|t| (t.coordinate(), t.reference()))
                .collect();
            capture.entries[0] = Surface::from_plane(
                Plane::from_blocks(
                    doc.layers[0].pixels.width,
                    doc.layers[0].pixels.height,
                    doc.bit_depth,
                    stores[0].clone(),
                    blocks,
                )
                .unwrap(),
            )
            .unwrap();
        }
        let owner = MetadataCharge::reserve(&pool, 1024 * 1024, &|| Ok(())).unwrap();
        let pending = PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), owner),
            RasterRoster::new(capture.entries).unwrap(),
            pool.clone(),
            disk,
        )
        .unwrap();
        let identity = SceneIdentity {
            project_id: "standard-row-fixture".into(),
            document_id: doc.id.clone(),
            revision: doc.revision,
        };
        let scene = SceneSnapshot::from_native(
            identity,
            crate::engine::native::NativeDocument::from_pending(pending).unwrap(),
            vec![],
            None,
            None,
        )
        .unwrap();
        Self {
            scene: Some(scene),
            stores,
            paths,
            pool,
        }
    }
    fn scene(&self) -> &SceneSnapshot {
        self.scene.as_ref().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.scene.take());
        self.stores.clear();
        assert_eq!(self.pool.used(), 0);
        let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        for path in &self.paths {
            let target = std::fs::canonicalize(path).unwrap();
            assert_eq!(target.parent(), Some(parent.as_path()));
            assert!(target
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pb-standard-rows-"));
            std::fs::remove_dir_all(target).unwrap();
        }
    }
}
fn doc(depth: u16) -> Document {
    let mut doc = Document::new_depth(4, 3, depth).unwrap();
    if depth == 16 {
        doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 0]);
        doc.layers[0]
            .pixels
            .set16(1, 0, [50001, 30003, 10001, 32769]);
    } else {
        doc.layers[0].pixels.set(0, 0, [17, 99, 201, 0]);
        doc.layers[0].pixels.set(1, 0, [222, 144, 33, 127]);
    }
    doc
}
fn request(
    rows: &Rows36<'_>,
    source: RowSource,
    channel: i16,
    bounds: [i32; 4],
    first_row: u32,
    count: u32,
) -> RowRequest {
    RowRequest {
        provenance: rows.provenance(),
        source,
        channel,
        bounds,
        first_row,
        rows: count,
        depth: rows.scene.metadata().depth(),
    }
}
fn values(bytes: &[u8], depth: u16) -> Vec<u16> {
    if depth == 16 {
        bytes
            .chunks_exact(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .collect()
    } else {
        bytes.iter().map(|v| u16::from(*v)).collect()
    }
}
fn sample(raster: &Raster, x: i32, y: i32, depth: u16) -> [u16; 4] {
    if depth == 16 {
        raster.get16(x, y)
    } else {
        raster.get(x, y).map(u16::from)
    }
}
#[test]
fn actual_raw_current_and_original_channels_preserve_hidden_rgb_offsets_and_native_low_bits() {
    for depth in [8, 16] {
        let mut doc = doc(depth);
        doc.layers[0].x = -2;
        doc.layers[0].y = -1;
        let mut original = Raster::new_depth(2, 2, depth);
        original.set16(0, 0, [65535; 4]);
        doc.layers[0].pixels.retained = Some(Box::new(crate::retained::Original::from(&original)));
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        assert!(std::ptr::eq(rows.scene(), fixture.scene()));
        for source in [RowSource::Layer(0), RowSource::OriginalLayer(0)] {
            for channel in [-1, 0, 1, 2] {
                let r = request(&rows, source, channel, [-2, -1, 2, 2], 0, 3);
                let mut output = vec![0xcd; 12 * usize::from(depth / 8)];
                rows.fill(r, &mut output, &|| Ok(())).unwrap();
                let expected = (0..3)
                    .flat_map(|y| (0..4).map(move |x| (x, y)))
                    .map(|(x, y)| {
                        sample(&doc.layers[0].pixels, x, y, depth)
                            [if channel == -1 { 3 } else { channel as usize }]
                    })
                    .collect::<Vec<_>>();
                assert_eq!(values(&output, depth), expected);
            }
        }
    }
}
#[test]
fn procedural_fill_channels_use_colour_and_preserve_transparent_colour_at_both_depths() {
    for depth in [8, 16] {
        let mut doc = doc(depth);
        doc.layers[0].kind = "fill".into();
        doc.layers[0].color = [17, 99, 201, 0];
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        for source in [RowSource::Layer(0), RowSource::OriginalLayer(0)] {
            for channel in [-1, 0, 1, 2] {
                let r = request(&rows, source, channel, [0, 0, 4, 3], 1, 2);
                let mut bytes = vec![0xcd; 8 * usize::from(depth / 8)];
                rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
                let expected = u16::from(
                    doc.layers[0].color[if channel == -1 { 3 } else { channel as usize }],
                ) * if depth == 16 { 257 } else { 1 };
                assert!(values(&bytes, depth).iter().all(|v| *v == expected));
            }
        }
    }
}
#[test]
fn real_composite_and_standard_white_matte_match_legacy_native_channels() {
    for depth in [8, 16] {
        let mut doc = doc(depth);
        let mut fill = Layer::new("backdrop", "fill", 4, 3);
        fill.pixels = Raster::new_depth(4, 3, depth);
        fill.color = [23, 77, 141, 113];
        fill.opacity = 0.47;
        doc.layers.push(fill);
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let high = if depth == 16 {
            Some(crate::depth16::Plan16::new(&doc).unwrap())
        } else {
            None
        };
        let masks = vec![None; doc.layers.len()];
        let colours = vec![None; doc.layers.len()];
        let low = crate::compositor::Plan::new(&doc, &masks, &colours);
        for source in [RowSource::BakedComposite, RowSource::MergedWhiteMatte] {
            let alpha = if source == RowSource::MergedWhiteMatte {
                3
            } else {
                -1
            };
            for channel in [alpha, 0, 1, 2] {
                for (first, count) in [(0, 1), (1, 2)] {
                    let r = request(&rows, source, channel, [0, 0, 4, 3], first, count);
                    let mut bytes = vec![0xcd; 4 * count as usize * usize::from(depth / 8)];
                    rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
                    let mut expected = Vec::new();
                    for y in first..first + count {
                        for x in 0..4 {
                            let p = if let Some(high) = &high {
                                high.pixel(x, y as i32)
                            } else {
                                low.sample(low.group(None), x, y as i32).map(u16::from)
                            };
                            let c = if channel == -1 { 3 } else { channel as usize };
                            let mut value = p[c];
                            if source == RowSource::MergedWhiteMatte && c < 3 {
                                let maximum = if depth == 16 { 65535u64 } else { 255 };
                                value = ((u64::from(value) * u64::from(p[3])
                                    + maximum * (maximum - u64::from(p[3]))
                                    + maximum / 2)
                                    / maximum) as u16;
                            }
                            expected.push(value);
                        }
                    }
                    assert_eq!(
                        values(&bytes, depth),
                        expected,
                        "{depth} {source:?} {channel}"
                    );
                }
            }
        }
    }
}
#[test]
fn raw_adjustment_canvas_padding_reads_current_physical_source_without_a_derived_substitute() {
    let mut doc = doc(16);
    doc.layers[0].kind = "adjustment".into();
    doc.layers[0].pixels = Raster::new_depth(2, 1, 16);
    doc.layers[0]
        .pixels
        .set16(0, 0, [12347, 23459, 34571, 45679]);
    doc.layers[0].x = 1;
    doc.layers[0].y = 1;
    let fixture = Fixture::new(&doc);
    let mut rows = Rows36::new(fixture.scene()).unwrap();
    let r = request(&rows, RowSource::OriginalLayer(0), 0, [0, 0, 4, 3], 0, 3);
    let mut bytes = vec![0xcd; 24];
    rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
    let expected = (0..3)
        .flat_map(|y| (0..4).map(move |x| (x, y)))
        .map(|(x, y)| doc.layers[0].pixels.get16(x - 1, y - 1)[0])
        .collect::<Vec<_>>();
    assert_eq!(values(&bytes, 16), expected);
    let r = request(&rows, RowSource::Layer(0), 0, [1, 1, 3, 2], 0, 1);
    let mut bytes = vec![0xcd; 4];
    assert!(rows.fill(r, &mut bytes, &|| Ok(())).is_err());
    assert_eq!(bytes, [0xcd; 4]);
}
#[test]
fn disabled_raw_masks_current_effects_and_editable_sources_refuse_without_fabricated_samples() {
    for depth in [8, 16] {
        let mut doc = doc(depth);
        doc.layers[0].mask = Some(crate::engine::Mask {
            enabled: false,
            linked: true,
            cache_key: crate::engine::id(),
            steps: vec![],
        });
        doc.layers[0].effects.push(crate::effects::Effect {
            id: "effect".into(),
            kind: "invert".into(),
            enabled: true,
            weight: 1.,
            settings: serde_json::json!({}).into(),
        });
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let mut bytes = vec![0xcd; 4 * usize::from(depth / 8)];
        let r = request(&rows, RowSource::OriginalLayer(0), -2, [0, 0, 4, 3], 0, 1);
        assert!(rows
            .fill(r, &mut bytes, &|| Ok(()))
            .unwrap_err()
            .contains("prepared mask"));
        assert!(bytes.iter().all(|v| *v == 0xcd));
        let r = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
        assert!(rows
            .fill(r, &mut bytes, &|| Ok(()))
            .unwrap_err()
            .contains("regional"));
        assert!(bytes.iter().all(|v| *v == 0xcd));
        let r = request(&rows, RowSource::OriginalLayer(0), 0, [0, 0, 4, 3], 0, 1);
        rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
        let r = request(&rows, RowSource::BakedComposite, 0, [0, 0, 4, 3], 0, 1);
        assert!(rows.fill(r, &mut bytes, &|| Ok(())).is_err());
        drop(rows);
        drop(fixture);
        let mut doc = doc;
        doc.layers[0].effects.clear();
        doc.layers[0].source = Some(
            (crate::source::Source {
                width: 4,
                height: 3,
                matrix: [1., 0., 0., 1., 0., 0.],
                content: crate::source::Content::Svg {
                    svg: "<svg/>".into(),
                },
            })
            .into(),
        );
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let r = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
        assert!(rows.fill(r, &mut bytes, &|| Ok(())).is_err());
    }
}
#[test]
fn malformed_foreign_target_and_cancelled_requests_preserve_source_and_output() {
    let doc = doc(16);
    let fixture = Fixture::new(&doc);
    let other = Fixture::new(&doc);
    let mut rows = Rows36::new(fixture.scene()).unwrap();
    let foreign = Rows36::new(other.scene()).unwrap();
    let good = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
    let mut requests = vec![
        RowRequest {
            provenance: foreign.provenance(),
            ..good
        },
        RowRequest { channel: 4, ..good },
        RowRequest {
            source: RowSource::Layer(9),
            ..good
        },
        RowRequest {
            bounds: [1, 0, 5, 3],
            ..good
        },
        RowRequest { depth: 8, ..good },
        RowRequest {
            first_row: u32::MAX,
            ..good
        },
        RowRequest { rows: 0, ..good },
        RowRequest {
            source: RowSource::MergedWhiteMatte,
            bounds: [-1, 0, 3, 3],
            ..good
        },
    ];
    for request in requests.drain(..) {
        let mut bytes = [0xcd; 8];
        assert!(rows.fill(request, &mut bytes, &|| Ok(())).is_err());
        assert_eq!(bytes, [0xcd; 8]);
    }
    let mut bytes = [0xcd; 8];
    assert_eq!(
        rows.fill(good, &mut bytes, &|| Err("cancelled source guard".into()))
            .unwrap_err(),
        "cancelled source guard"
    );
    assert_eq!(bytes, [0xcd; 8]);
    assert!(rows.fill(good, &mut bytes[..7], &|| Ok(())).is_err());
    assert_eq!(doc.layers[0].pixels.get16(0, 0), [12347, 23459, 34571, 0]);
}
#[test]
fn genuine_foreign_block_and_resident_refusal_never_become_zero_pixels() {
    let doc = doc(16);
    let fixture = Fixture::build(&doc, true);
    let mut rows = Rows36::new(fixture.scene()).unwrap();
    let r = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
    let mut bytes = [0xcd; 8];
    assert!(rows.fill(r, &mut bytes, &|| Ok(())).is_err());
    assert_eq!(bytes, [0xcd; 8]);
    drop(rows);
    drop(fixture);
    let fixture = Fixture::new(&doc);
    let mut rows = Rows36::new(fixture.scene()).unwrap();
    let r = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
    let free = fixture.pool.limit() - fixture.pool.used();
    let held = fixture.pool.reserve_bytes(free, free).unwrap();
    assert!(rows.fill(r, &mut bytes, &|| Ok(())).is_err());
    assert_eq!(bytes, [0xcd; 8]);
    drop(held);
    rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
    assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 12347);
}

#[test]
fn explicit_zero_and_exact_128_kipixel_admission_do_not_expand_the_stripe() {
    for depth in [8, 16] {
        let doc = Document::new_depth(1024, 128, depth).unwrap();
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        for channel in [-1, 0, 1, 2] {
            let r = request(&rows, RowSource::Zero, channel, [0, 0, 1024, 128], 0, 128);
            let mut bytes = vec![0xcd; 128 * 1024 * usize::from(depth / 8)];
            rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
            assert!(bytes.iter().all(|v| *v == 0));
        }
        let r = request(&rows, RowSource::Zero, 0, [0, 0, 1024, 129], 0, 129);
        let mut bytes = vec![0xcd; 129 * 1024 * usize::from(depth / 8)];
        assert!(rows
            .fill(r, &mut bytes, &|| Ok(()))
            .unwrap_err()
            .contains("128 KiPixel"));
        assert!(bytes.iter().all(|v| *v == 0xcd));
    }
}

#[test]
fn cancellation_during_actual_source_pin_restores_shared_admission_without_sampling() {
    for depth in [8, 16] {
        let doc = doc(depth);
        let fixture = Fixture::new(&doc);
        let mut rows = Rows36::new(fixture.scene()).unwrap();
        let r = request(&rows, RowSource::Layer(0), 0, [0, 0, 4, 3], 0, 1);
        let mut bytes = vec![0; 4 * usize::from(depth / 8)];
        rows.fill(r, &mut bytes, &|| Ok(())).unwrap();
        let baseline = fixture.pool.used();
        let payload = fixture.pool.payload_used();
        let disk = fixture.scene().disk_budget().used().unwrap();
        let reached = std::cell::Cell::new(false);
        bytes.fill(0xcd);
        let error = rows
            .fill(r, &mut bytes, &|| {
                if fixture.pool.payload_used() > payload {
                    reached.set(true);
                    Err("cancelled while source payload admitted".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(reached.get());
        assert_eq!(error, "cancelled while source payload admitted");
        assert!(bytes.iter().all(|v| *v == 0xcd));
        assert_eq!(fixture.pool.used(), baseline);
        assert_eq!(fixture.scene().disk_budget().used().unwrap(), disk);
    }
}

#[test]
fn actual_psd_psb_standard_emission_and_comparison_use_bounded_captured_rows() {
    use crate::psd::{
        row_stream::Source,
        standard36::{Plan, Record},
        structural::{self, Format as PhotoshopFormat, Header},
    };
    use std::io::Cursor;
    for format in [PhotoshopFormat::Psd, PhotoshopFormat::Psb] {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(1024, 256, depth).unwrap();
            doc.layers[0]
                .pixels
                .set16(1, 0, [12347, 23459, 34571, 32769]);
            let fixture = Fixture::new(&doc);
            let mut rows = Rows36::new(fixture.scene()).unwrap();
            // Bound the coexisting in-memory test container/sections/record indexes
            // before any vector grows. This fixture creates no destination files.
            let _outputs = fixture
                .pool
                .reserve_bytes(16 * 1024 * 1024, 16 * 1024 * 1024)
                .unwrap();
            let header = Header {
                format,
                width: 1024,
                height: 256,
                depth,
                channels: 4,
            };
            let record = Record {
                source: RowSource::Layer(0),
                bounds: [0, 0, 1024, 256],
                mask: None,
                name: "Source",
                blend: *b"norm",
                opacity: 255,
                clipped: false,
                visible: true,
                locked: false,
                has_clipped_layers: false,
                section: 0,
                metadata: vec![],
            };
            let plan = Plan::new(
                header,
                rows.provenance(),
                vec![record],
                &[],
                &[],
                &|| Ok(()),
            )
            .unwrap();
            let mut scratch = ScratchBytes::new(&fixture.pool, 512 * 1024, 512 * 1024).unwrap();
            let mut layers = Vec::new();
            let mut composite = Vec::new();
            let written = plan
                .write_layers(
                    &mut layers,
                    &mut rows,
                    &mut scratch,
                    &|| Ok(()),
                    |_, _| unreachable!(),
                )
                .unwrap();
            let merged = plan
                .write_composite(&mut composite, &mut rows, &mut scratch, &|| Ok(()))
                .unwrap();
            assert_eq!(written.bytes, layers.len() as u64);
            assert_eq!(merged.bytes, composite.len() as u64);
            let mut bytes = b"8BPS".to_vec();
            bytes.extend(
                (if format == PhotoshopFormat::Psb {
                    2u16
                } else {
                    1u16
                })
                .to_be_bytes(),
            );
            bytes.extend([0; 6]);
            bytes.extend(4u16.to_be_bytes());
            bytes.extend(header.height.to_be_bytes());
            bytes.extend(header.width.to_be_bytes());
            bytes.extend(depth.to_be_bytes());
            bytes.extend(3u16.to_be_bytes());
            bytes.extend([0; 8]);
            if format == PhotoshopFormat::Psb {
                bytes.extend((layers.len() as u64).to_be_bytes());
            } else {
                bytes.extend((layers.len() as u32).to_be_bytes());
            }
            bytes.extend(&layers);
            bytes.extend(&composite);
            let index = structural::index(
                &mut Cursor::new(&bytes),
                structural::Limits::current(),
                || Ok(()),
            )
            .unwrap();
            let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
            let mut actual = ScratchBytes::new(&fixture.pool, 512 * 1024, 512 * 1024).unwrap();
            let compared = plan
                .compare(
                    &source,
                    &index,
                    &mut rows,
                    &mut scratch,
                    &mut actual,
                    &|| Ok(()),
                )
                .unwrap();
            assert_eq!(compared.samples(), 8 * 1024 * 256);
        }
    }
}
