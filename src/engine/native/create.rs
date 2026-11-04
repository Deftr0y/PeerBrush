//! New transparent native documents, before guarded workspace registration.
use super::*;

const HEADER_BYTES: usize = 32 * 1024;

impl NativeDocument {
    /// Construct genuine known-transparent source authority without creating a
    /// legacy Raster or materializing document-sized pixels. Workspace/source
    /// admission and final registry publication remain the caller's obligation.
    pub(crate) fn new_blank(
        width: u32,
        height: u32,
        depth: u16,
        pool: &ResidentBudget,
        disk: &DiskBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        if !(1..=16000).contains(&width)
            || !(1..=16000).contains(&height)
            || ![8, 16].contains(&depth)
        {
            return Err("Native canvas requires 1-16000 pixels per edge and RGB8 or RGB16".into());
        }
        // Includes bounded UUID/name strings, one layer and one source handle.
        // The roster separately retains its ordinary descriptor admission.
        let owner = MetadataCharge::reserve(pool, HEADER_BYTES, check)?;
        let mut layers = Vec::new();
        layers
            .try_reserve_exact(1)
            .map_err(|_| "New native layer allocation refused")?;
        let mut sources = Vec::new();
        sources
            .try_reserve_exact(1)
            .map_err(|_| "New native source allocation refused")?;
        layers.push(Layer36 {
            id: super::super::id(),
            name: "Paint 1".into(),
            kind: "paint".into(),
            parent: None,
            clip_to: None,
            visible: true,
            locked: false,
            opacity: 1.0,
            blend: "normal".into(),
            x: 0,
            y: 0,
            pixels: RasterRef36 {
                id: 1,
                width,
                height,
                depth,
                retained: None,
            },
            color: [233, 84, 32, 255],
            mask: None,
            effects: Vec::new(),
            psd_metadata: Vec::new().into(),
            source: None,
        });
        sources.push(Surface::zero(width, height, depth).map_err(|e| e.to_string())?);
        // One standard full-canvas layer plus the mandatory current composite
        // already determines a raw lower bound, even for a known-zero source.
        let needs_psb =
            u64::from(width) * u64::from(height) * u64::from(depth / 8) * 8 + 78 > 2_000_000_000;
        let metadata = Document36 {
            id: super::super::id(),
            name: if needs_psb {
                "Untitled.psb"
            } else {
                "Untitled.psd"
            }
            .into(),
            width,
            height,
            revision: 0,
            bit_depth: depth,
            icc_profile: None,
            srgb_tagged: false,
            psd_global_mask: None,
            layers,
            filters: Vec::new(),
            selection: None,
            selection_polygon: None,
            selection_coverage: None,
            selection_previous: None,
            read_only: false,
            warnings: Vec::new().into(),
        };
        check()?;
        let pending = PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), owner),
            RasterRoster::new(sources)?,
            pool.clone(),
            disk.clone(),
        )?;
        let document = Self::from_pending(pending)?;
        check()?;
        Ok(document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{scene::RasterCollector, Document},
        raster::Raster,
    };

    struct EmptyCollector;
    impl RasterCollector for EmptyCollector {
        fn resident(&mut self, raster: &Raster) -> Result<RasterRef36> {
            for y in 0..raster.height {
                for x in 0..raster.width {
                    assert_eq!(raster.get16(x as i32, y as i32), [0; 4]);
                }
            }
            Ok(RasterRef36 {
                id: 1,
                width: raster.width,
                height: raster.height,
                depth: raster.depth,
                retained: None,
            })
        }
        fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
            unreachable!()
        }
    }

    #[test]
    fn blank_native_headers_and_sources_match_actual_legacy_new_defaults() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(256 * 1024).unwrap();
            let disk = DiskBudget::new(1024, 8).unwrap();
            let native = NativeDocument::new_blank(7, 5, depth, &pool, &disk, &|| Ok(())).unwrap();
            let mut legacy = Document::new_depth(7, 5, depth).unwrap();
            legacy.id = native.metadata().id().into();
            legacy.layers[0].id = native.metadata().layer(0).unwrap().id().into();
            let expected = Document36::capture(&legacy, &mut EmptyCollector).unwrap();
            assert_eq!(
                serde_json::to_value(native.metadata()).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            let slot = native
                .layer_slot(native.metadata().layer(0).unwrap().id())
                .unwrap();
            let source = native.source(slot).unwrap();
            assert_eq!([source.width(), source.height()], [7, 5]);
            assert_eq!(source.depth(), depth);
            assert!(source.is_known_zero());
            assert!(source.uses_budget(&pool) && source.uses_disk_budget(&disk));
            assert_eq!(disk.used().unwrap(), 0);
            drop(native);
            assert_eq!(pool.used(), 0);
        }
    }

    #[test]
    fn native_edge_creation_is_metadata_only_and_is_not_dense_canvas_qualification() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(256 * 1024).unwrap();
            let disk = DiskBudget::new(1, 1).unwrap();
            let native =
                NativeDocument::new_blank(16000, 16000, depth, &pool, &disk, &|| Ok(())).unwrap();
            let metadata = native.metadata();
            assert_eq!([metadata.width(), metadata.height()], [16000, 16000]);
            assert_eq!(metadata.name(), "Untitled.psb");
            assert_eq!(metadata.layers().len(), 1);
            let slot = native.layer_slot(metadata.layer(0).unwrap().id()).unwrap();
            assert!(native.source(slot).unwrap().is_known_zero());
            assert!(pool.used() > 0 && pool.used() < 64 * 1024);
            assert_eq!(disk.used().unwrap(), 0);
            drop(native);
            assert_eq!(pool.used(), 0);
        }
    }

    #[test]
    fn refusal_and_late_cancellation_leave_no_source_files_or_retained_charge() {
        use std::cell::Cell;
        let pool = ResidentBudget::new(256 * 1024).unwrap();
        let disk = DiskBudget::new(1, 1).unwrap();
        for (w, h, d) in [
            (0, 1, 8),
            (1, 0, 16),
            (16001, 1, 8),
            (1, 16001, 16),
            (1, 1, 32),
        ] {
            assert!(NativeDocument::new_blank(w, h, d, &pool, &disk, &|| Ok(())).is_err());
            assert_eq!(pool.used(), 0);
        }
        assert!(NativeDocument::new_blank(
            1,
            1,
            8,
            &ResidentBudget::new(1).unwrap(),
            &disk,
            &|| Ok(())
        )
        .is_err());
        // Locate the terminal user check through a successful identical build,
        // and prove it observes the completed runtime ownership, not the old
        // broad construction ticket that rehome correctly releases.
        let observed_checks = Cell::new(0usize);
        let completed = NativeDocument::new_blank(7, 5, 16, &pool, &disk, &|| {
            observed_checks.set(observed_checks.get() + 1);
            Ok(())
        })
        .unwrap();
        let terminal_check = observed_checks.get();
        assert!(terminal_check >= 2);
        let completed_bytes = pool.used();
        assert!(completed_bytes > 0);
        drop(completed);
        assert_eq!(pool.used(), 0);
        let checks = Cell::new(0usize);
        let reached = Cell::new(false);
        let result = NativeDocument::new_blank(7, 5, 16, &pool, &disk, &|| {
            checks.set(checks.get() + 1);
            if checks.get() == terminal_check {
                assert_eq!(pool.used(), completed_bytes);
                reached.set(true);
                Err("cancel after native source/header construction".into())
            } else {
                Ok(())
            }
        });
        assert!(reached.get());
        assert_eq!(checks.get(), terminal_check);
        assert!(result.is_err());
        assert_eq!(pool.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }
}
