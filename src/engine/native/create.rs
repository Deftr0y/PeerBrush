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
        crate::raster::CanvasLimits::Native.check(width, height)?;
        if ![8, 16].contains(&depth) {
            return Err("Native canvas requires RGB8 or RGB16".into());
        }
        let surface = Surface::zero(width, height, depth).map_err(|e| e.to_string())?;
        Self::single_layer(surface, None, None, None, Default::default(), None, pool, disk, check)
    }

    pub(crate) fn new_image(
        surface: Surface,
        name: &str,
        layer: &str,
        definition: Option<Admitted<super::super::scene::Source36>>,
        pool: &ResidentBudget,
        disk: &DiskBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        crate::raster::CanvasLimits::Native.check(surface.width(), surface.height())?;
        let names = Some((name, layer, true));
        if let Some(definition) = definition {
            if !definition.owner().uses_budget(pool)
                || (definition.width, definition.height) != (surface.width(), surface.height())
            {
                return Err(
                    "Native image definition differs from its source frame or admission".into(),
                );
            }
            let document = definition.map_unique(|definition, _| {
                Self::single_layer(surface, names, Some(definition), None, Default::default(), None, pool, disk, check)
            })?;
            return Ok((*document).clone());
        }
        Self::single_layer(surface, names, None, None, Default::default(), None, pool, disk, check)
    }

    pub(crate) fn new_protected_preview(
        surface: Surface,
        name: &str,
        reason: &str,
        profile: Option<Vec<u8>>,
        srgb_tagged: bool,
        resources: crate::psd::DocumentResources,
        file_info: Option<super::super::scene::MetadataPayload<Vec<u8>>>,
        pool: &ResidentBudget,
        disk: &DiskBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        crate::raster::CanvasLimits::Native.check(surface.width(), surface.height())?;
        Self::single_layer(
            surface,
            Some((name, "Saved PSD composite", srgb_tagged)),
            None,
            Some((reason, profile)),
            resources,
            file_info,
            pool,
            disk,
            check,
        )
    }

    pub(crate) fn new_compatible_copy(
        surface: Surface,
        name: &str,
        layer: &str,
        srgb_tagged: bool,
        resources: crate::psd::DocumentResources,
        file_info: Option<super::super::scene::MetadataPayload<Vec<u8>>>,
        pool: &ResidentBudget,
        disk: &DiskBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        crate::raster::CanvasLimits::Native.check(surface.width(), surface.height())?;
        Self::single_layer(
            surface,
            Some((name, layer, srgb_tagged)),
            None,
            None,
            resources,
            file_info,
            pool,
            disk,
            check,
        )
    }

    pub(crate) fn with_protected_structure(self, structure: Option<crate::psd::ProtectedStructure>) -> Result<Self> {
        let Some(structure) = structure else { return Ok(self); };
        self.protected_composite()?;
        if !structure.owner().uses_budget(self.budget()) {
            return Err("Photoshop source diagnostics belong to a different resident pool".into());
        }
        let core = self.core.map_unique(|mut core, _| {
            core.protected_structure = Some(structure);
            Ok(core)
        })?;
        Ok(Self { core })
    }

    fn single_layer(
        surface: Surface,
        image: Option<(&str, &str, bool)>,
        definition: Option<super::super::scene::Source36>,
        protection: Option<(&str, Option<Vec<u8>>)>,
        resources: crate::psd::DocumentResources,
        file_info: Option<super::super::scene::MetadataPayload<Vec<u8>>>,
        pool: &ResidentBudget,
        disk: &DiskBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        let (width, height, depth) = (surface.width(), surface.height(), surface.depth());
        resources.validate()?;
        // Includes bounded UUID/name strings, one layer and one source handle.
        // The roster separately retains its ordinary descriptor admission.
        let bytes = image
            .map_or(Some(HEADER_BYTES), |(name, layer, _)| {
                HEADER_BYTES
                    .checked_add(name.len())?
                    .checked_add(layer.len())
            })
            .and_then(|bytes| {
                if let Some((reason, profile)) = &protection {
                    bytes.checked_add(reason.len())?
                        .checked_add(profile.as_ref().map_or(0, Vec::len))
                } else {
                    Some(bytes)
                }
            })
            .ok_or("Native image name admission overflow")?;
        let owner = MetadataCharge::reserve(pool, bytes, check)?;
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
            name: image.map_or("Paint 1", |(_, layer, _)| layer).into(),
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
            source: definition,
        });
        sources.push(surface);
        // One standard full-canvas layer plus the mandatory current composite
        // already determines a raw lower bound, even for a known-zero source.
        let needs_psb =
            u64::from(width) * u64::from(height) * u64::from(depth / 8) * 8 + 78 > 2_000_000_000;
        let read_only = protection.is_some();
        let (icc_profile, warnings) = protection.map_or((None, Vec::new()), |(reason, profile)| {
            (profile.map(super::super::scene::MetadataPayload::from), vec![reason.to_owned()])
        });
        let metadata = Document36 {
            id: super::super::id(),
            name: image
                .map_or(
                    if needs_psb {
                        "Untitled.psb"
                    } else {
                        "Untitled.psd"
                    },
                    |(name, _, _)| name,
                )
                .into(),
            width,
            height,
            revision: 0,
            bit_depth: depth,
            icc_profile,
            srgb_tagged: image.is_some_and(|(_, _, tagged)| tagged),
            psd_intentionally_untagged: resources.intentionally_untagged,
            psd_rgb8_multiply: false,
            psd_resolution: resources.resolution,
            psd_print_scale: resources.scale,
            psd_pixel_aspect: resources.pixel_aspect,
            psd_grid: resources.grid,
            psd_layer_id_seed: resources.layer_id_seed,
            psd_print_flags: resources.print_flags,
            psd_print_flags_info: resources.print_flags_info,
            psd_copyright: resources.copyright,
            psd_file_info: file_info,
            psd_global_angle: resources.global_angle,
            psd_global_altitude: resources.global_altitude,
            psd_global_mask: None,
            psd_global_metadata: None,
            layers,
            filters: Vec::new(),
            selection: None,
            selection_polygon: None,
            selection_coverage: None,
            selection_previous: None,
            read_only,
            warnings: warnings.into(),
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
