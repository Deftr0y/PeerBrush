//! Small data-only fixtures; these do not create a SourceRuntime/read permit.
use crate::{
    engine::{
        native::NativeDocument,
        scene::{
            Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, Retained36,
            SceneIdentity, SceneSnapshot, Warp36,
        },
        Document,
    },
    large_document::{DiskBudget, Limits, ResidentBudget, Store},
    raster::{Raster, Surface},
};
use std::path::PathBuf;
pub(super) fn capture(doc: &Document) -> (NativeDocument, SceneSnapshot, PathBuf) {
    let pool = ResidentBudget::new(96 * 1024 * 1024).unwrap();
    let disk = DiskBudget::new(64 * 1024 * 1024, 4).unwrap();
    let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let path = parent.join(format!("pb-native-observation-{}", crate::engine::id()));
    let store = Store::create_with_disk_budget(
        &path,
        pool.clone(),
        disk.clone(),
        Limits {
            disk_bytes: 64 * 1024 * 1024,
            max_blocks: 256,
            max_resident_blocks: 32,
            manifest_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    struct Collector {
        store: Store,
        sources: Vec<Surface>,
    }
    impl RasterCollector for Collector {
        fn resident(&mut self, r: &Raster) -> Result<RasterRef36, String> {
            let surface = Surface::stage_current_pixels(r, self.store.clone(), &mut || Ok(()))
                .map_err(|e| e.to_string())?;
            self.sources.push(surface);
            let id = self.sources.len() as u32;
            let retained = r
                .retained
                .as_ref()
                .map(|original| -> Result<_, String> {
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
                width: r.width,
                height: r.height,
                depth: r.depth,
                retained,
            })
        }
        fn surface(&mut self, _: &Surface) -> Result<RasterRef36, String> {
            Err("Resident-only data fixture".into())
        }
        fn embedded_coverage(
            &mut self,
            value: &serde_json::Value,
        ) -> Result<crate::engine::scene::Coverage36, String> {
            let coverage: crate::selection::Coverage =
                serde::Deserialize::deserialize(value).map_err(|e| e.to_string())?;
            coverage.validate()?;
            crate::engine::scene::Coverage36::from_capture(&coverage, self)
        }
    }
    let metadata_charge =
        crate::psd::memory36::MetadataCharge::reserve(&pool, 8 * 1024 * 1024, &|| Ok(())).unwrap();
    let provisional = pool.reserve_bytes(64 * 1024, 64 * 1024).unwrap();
    let mut collector = Collector {
        store,
        sources: Vec::with_capacity(128),
    };
    let metadata = Document36::capture(doc, &mut collector).unwrap();
    let metadata =
        crate::psd::memory36::Admitted::from_parts(std::sync::Arc::new(metadata), metadata_charge);
    let pending = PendingScene::new(
        metadata,
        RasterRoster::new(collector.sources).unwrap(),
        pool,
        disk,
    )
    .unwrap();
    let native = NativeDocument::from_pending(pending).unwrap();
    drop(collector.store);
    drop(provisional);
    let scene = SceneSnapshot::from_native(
        SceneIdentity {
            project_id: "observation-fixture".into(),
            document_id: doc.id.clone(),
            revision: doc.revision,
        },
        native.clone(),
        vec![],
        None,
        None,
    )
    .unwrap();
    (native, scene, path)
}
pub(super) fn cleanup(path: &std::path::Path) {
    let path = std::fs::canonicalize(path).unwrap();
    assert_eq!(
        path.parent().unwrap(),
        std::fs::canonicalize(std::env::temp_dir()).unwrap()
    );
    assert!(path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("pb-native-observation-"));
    std::fs::remove_dir_all(path).unwrap();
}
pub(super) fn document(depth: u16, width: u32, height: u32) -> Document {
    let mut doc = Document::new_depth(width, height, depth).unwrap();
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let value = [
                ((x * 173 + y * 59 + 1) % 65536) as u16,
                ((x * 419 + y * 97 + 3) % 65536) as u16,
                ((x * 73 + y * 311 + 7) % 65536) as u16,
                match (x + y) % 7 {
                    0 => 0,
                    1 => 128,
                    2 => 32767,
                    _ => 65535,
                },
            ];
            doc.layers[0].pixels.set16(x, y, value);
        }
    }
    doc
}
