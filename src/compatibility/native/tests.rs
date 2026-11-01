use super::*;
use crate::{
    engine::{
        scene::{Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster},
        view::DocumentSnapshot,
    },
    large_document::{DiskBudget, ResidentBudget},
    psd::memory36::{Admitted, MetadataCharge},
    raster::{Raster, Surface},
};
use std::{cell::Cell, sync::Arc};

fn blank(pool: &ResidentBudget, depth: u16) -> NativeDocument {
    NativeDocument::new_blank(
        7,
        5,
        depth,
        pool,
        &DiskBudget::new(4 * 1024 * 1024, 32).unwrap(),
        &|| Ok(()),
    )
    .unwrap()
}
struct Collector(Vec<Surface>);
impl RasterCollector for Collector {
    fn resident(&mut self, raster: &Raster) -> Result<RasterRef36, String> {
        // These metadata fixtures contain only actual transparent rasters.
        for y in 0..raster.height {
            for x in 0..raster.width {
                assert_eq!(raster.get16(x as i32, y as i32), [0; 4]);
            }
        }
        self.0.push(
            Surface::zero(raster.width, raster.height, raster.depth).map_err(|e| e.to_string())?,
        );
        Ok(RasterRef36 {
            id: self.0.len() as u32,
            width: raster.width,
            height: raster.height,
            depth: raster.depth,
            retained: None,
        })
    }
    fn surface(&mut self, _: &Surface) -> Result<RasterRef36, String> {
        Err("Unexpected surface in transparent metadata fixture".into())
    }
}
fn capture(source: &Document, pool: &ResidentBudget) -> NativeDocument {
    let mut collector = Collector(Vec::new());
    let metadata = Document36::capture(source, &mut collector).unwrap();
    let owner = MetadataCharge::reserve(pool, 128 * 1024, &|| Ok(())).unwrap();
    NativeDocument::from_pending(
        PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), owner),
            RasterRoster::new(collector.0).unwrap(),
            pool.clone(),
            DiskBudget::new(4 * 1024 * 1024, 32).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn native8_native16_report_uses_real_standard_records_and_retains_its_admission() {
    for depth in [8, 16] {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let document = blank(&pool, depth);
        let baseline = pool.used();
        let key = document.key();
        let report = super::report(&document, &|| Ok(())).unwrap();
        assert_eq!(document.key(), key);
        assert!(pool.used() > baseline);
        let data = report.data();
        assert_eq!(data.document_id, document.metadata().id());
        assert_eq!(data.bit_depth, depth);
        assert!(data.save_allowed && data.current_merged_composite);
        assert!(!data.has_editability_losses);
        assert_eq!(data.standard_layout, StandardLayout::LayerTree);
        assert_eq!((data.standard_records, data.standard_layer_count), (1, 1));
        assert_eq!(data.layers[0].standard, Representation::Raster);
        assert_eq!(data.layers[0].standard_bounds, Some([0, 0, 7, 5]));
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::to_value(data).unwrap()
        );
        drop(document);
        assert!(pool.used() >= 1024 * 1024);
        drop(report);
        assert_eq!(pool.used(), 0);
    }
}

#[test]
fn insufficient_admission_and_late_cancel_preserve_document_and_release_report_charge() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let document = blank(&pool, 16);
    let baseline = pool.used();
    let key = document.key();
    let held = pool
        .reserve_bytes(pool.limit() - baseline - 128 * 1024, pool.limit())
        .unwrap();
    let before = pool.used();
    assert!(super::report(&document, &|| Ok(())).is_err());
    assert_eq!(pool.used(), before);
    drop(held);
    let late = Cell::new(false);
    let error = super::report(&document, &|| {
        if pool.used() > baseline {
            late.set(true);
            Err("specific late report cancellation".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert_eq!(error, "specific late report cancellation");
    assert!(late.get());
    assert_eq!(pool.used(), baseline);
    assert_eq!(document.key(), key);
    drop(document);
    assert_eq!(pool.used(), 0);
}

#[test]
fn protected_embedded_profile_never_executes_cms_or_claims_save_authority() {
    let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
    let mut source = Document::new_depth(7, 5, 16).unwrap();
    source.icc_profile = Some(Arc::new(vec![0, 1, 2, 3]));
    source.read_only = true;
    source.warnings = (0..66).map(|_| "א".repeat(1300)).collect();
    let document = capture(&source, &pool);
    let snapshot = DocumentSnapshot::Native(document.clone());
    let report = report_snapshot(&snapshot).unwrap();
    assert!(!report.data().save_allowed && !report.data().current_merged_composite);
    assert_eq!(report.data().standard_layout, StandardLayout::Protected);
    assert_eq!(report.data().output_profile, "source_protected");
    assert!(report.data().details_truncated);
    assert_eq!(report.data().protected_reasons.len(), 64);
    assert!(report
        .data()
        .protected_reasons
        .iter()
        .all(|s| s.chars().count() == 1024));
    assert!(report
        .data()
        .layers
        .iter()
        .all(|l| l.standard == Representation::ProtectedPreview && l.standard_bounds.is_none()));
    drop((snapshot, report, document));
    assert_eq!(pool.used(), 0);
}

#[test]
fn actual_fallback_plan_is_reported_as_hidden_originals_with_current_composite() {
    let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
    let mut source = Document::new_depth(7, 5, 8).unwrap();
    source.layers[0].kind = "adjustment".into();
    let document = capture(&source, &pool);
    let report = super::report(&document, &|| Ok(())).unwrap();
    assert_eq!(
        report.data().standard_layout,
        StandardLayout::CompositeAndHiddenSources
    );
    assert_eq!(report.data().standard_records, 4);
    assert_eq!(report.data().standard_layer_count, 3);
    assert!(report.data().layers[0]
        .peerbrush_sources
        .contains(&"original_layer"));
    assert!(report
        .data()
        .losses
        .iter()
        .any(|l| l.code == LossCode::DocumentFlattened));
    drop((report, document));
    assert_eq!(pool.used(), 0);
}

#[test]
fn resident_snapshot_preserves_existing_report_wire_fields() {
    let source = Document::new_depth(7, 5, 16).unwrap();
    let expected = serde_json::to_value(crate::compatibility::report(&source).unwrap()).unwrap();
    let actual = report_snapshot(&DocumentSnapshot::from(source)).unwrap();
    assert_eq!(serde_json::to_value(actual).unwrap(), expected);
}
