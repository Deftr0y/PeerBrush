use super::*;
use crate::{
    engine::{
        scene::{
            Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, Retained36,
            Warp36,
        },
        Document, Engine, Mask, MaskStep,
    },
    large_document::{DiskBudget, Limits, ResidentBudget, Store},
    psd::memory36::{Admitted, MetadataCharge},
    raster::{Raster, Surface, TILE},
    transform::native::{self, Mapping},
};
use serde_json::json;
use std::sync::Arc;

struct Collector {
    sources: Vec<Surface>,
    store: Store,
    pool: ResidentBudget,
}
impl RasterCollector for Collector {
    fn resident(&mut self, r: &Raster) -> Result<RasterRef36, String> {
        let size = (r.tiles.len() + r.samples16.len())
            .checked_mul(512)
            .unwrap()
            + 4096;
        let guard = self.pool.reserve_bytes(size, size).unwrap();
        let source = if r.tiles.is_empty() && r.samples16.is_empty() {
            Surface::zero(r.width, r.height, r.depth).unwrap()
        } else {
            Surface::stage_current_pixels(r, self.store.clone(), &mut || Ok(())).unwrap()
        };
        drop(guard);
        self.sources.push(source);
        let id = self.sources.len() as u32;
        let retained = r
            .retained
            .as_ref()
            .map(|o| {
                Ok::<_, String>(Box::new(Retained36 {
                    pixels: self.resident(&o.pixels)?,
                    matrix: o.matrix,
                    projective: o.projective,
                    warps: o.warps.iter().map(Warp36::capture).collect(),
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
        Err("Unexpected fixture source".into())
    }
}
fn fixture(depth: u16, kind: &str, linked: Option<bool>) -> Document {
    let mut doc = Document::new_depth(17, 19, depth).unwrap();
    let layer = &mut doc.layers[0];
    layer.x = -7;
    layer.y = 13;
    layer.kind = kind.into();
    layer.color = [17, 83, 219, 127];
    for (x, y, p) in [
        (2, 3, [12345, 23456, 34567, 65535]),
        (5, 7, [12001, 23003, 34007, 21987]),
        (8, 1, [0, 1, 0, 0]),
    ] {
        layer.pixels.set16(x, y, p);
    }
    if kind == "fill" {
        layer.pixels = Raster::new_depth(17, 19, depth);
    }
    if let Some(linked) = linked {
        let mut paint = Raster::new_depth(17, 19, depth);
        paint.set16(16, 18, [20001, 30003, 40007, 50009]);
        layer.mask = Some(Mask {
            enabled: true,
            linked,
            cache_key: crate::engine::id(),
            steps: vec![
                MaskStep {
                    id: "base".into(),
                    kind: "fill".into(),
                    enabled: true,
                    weight: 1.,
                    value: 1.,
                    pixels: Raster::new_depth(17, 19, depth),
                    settings: json!({}),
                },
                MaskStep {
                    id: "paint".into(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 1.,
                    value: 1.,
                    pixels: paint,
                    settings: json!({}),
                },
            ],
        });
    }
    doc
}
fn native_document(
    doc: &Document,
    pool: &ResidentBudget,
    disk: &DiskBudget,
    store: &Store,
) -> NativeDocument {
    let mut collector = Collector {
        sources: Vec::new(),
        store: store.clone(),
        pool: pool.clone(),
    };
    let metadata = Document36::capture(doc, &mut collector).unwrap();
    let charge = MetadataCharge::reserve(pool, 1024 * 1024, &|| Ok(())).unwrap();
    NativeDocument::from_pending(
        PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), charge),
            RasterRoster::new(collector.sources).unwrap(),
            pool.clone(),
            disk.clone(),
        )
        .unwrap(),
    )
    .unwrap()
}
fn assert_pixels(actual: &Surface, expected: &Raster, pool: &ResidentBudget) {
    assert_eq!(
        [actual.width(), actual.height()],
        [expected.width, expected.height]
    );
    assert_eq!(actual.depth(), expected.depth);
    for y in 0..expected.height.div_ceil(TILE) {
        for x in 0..expected.width.div_ceil(TILE) {
            let tile = actual
                .prepare_tile((x, y), &mut || Ok(()))
                .unwrap()
                .copy_to_scratch(pool)
                .unwrap();
            if expected.depth == 16 {
                let words = expected.samples16.get(&(x, y));
                for (i, pair) in tile.chunks_exact(2).enumerate() {
                    assert_eq!(
                        u16::from_be_bytes([pair[0], pair[1]]),
                        words.map_or(0, |v| v[i]),
                        "Native16 tile{x},{y} word{i}"
                    );
                }
            } else {
                let pixels = expected.tiles.get(&(x, y));
                for (i, b) in tile.iter().enumerate() {
                    assert_eq!(
                        *b,
                        pixels.map_or(0, |v| v[i]),
                        "Native8 tile{x},{y} byte{i}"
                    );
                }
            }
        }
    }
}
fn assert_plane(
    plan: &Plane<'_>,
    frame: [u32; 2],
    expected: &Raster,
    store: &Store,
    work: &mut Work,
) {
    let pool = store.resident_budget();
    let (output, mapping) = match &plan.pixels {
        Pixels::Native { input, pose } => {
            let mapping = Mapping::retained(
                pose.frame[0],
                pose.frame[1],
                pose.depth,
                pose.matrix,
                pose.projective,
                pose.warps(),
                &pool,
            )
            .unwrap();
            let output = native::write_plane(
                input.source().unwrap(),
                &mapping,
                frame,
                store,
                work,
                &|| Ok(()),
            )
            .unwrap();
            (output, Some((input, pose, mapping)))
        }
        Pixels::Fill { input, pose, color } => {
            let original =
                native::write_fill(pose.frame, pose.depth, *color, store, work, &|| Ok(()))
                    .unwrap();
            let expected_original = expected.retained.as_ref().unwrap();
            assert_pixels(&original, &expected_original.pixels, &pool);
            let mapping = Mapping::retained(
                pose.frame[0],
                pose.frame[1],
                pose.depth,
                pose.matrix,
                pose.projective,
                pose.warps(),
                &pool,
            )
            .unwrap();
            let output =
                native::write_plane(&original, &mapping, frame, store, work, &|| Ok(())).unwrap();
            (output, Some((input, pose, mapping)))
        }
        Pixels::Zero { depth } => (Surface::zero(frame[0], frame[1], *depth).unwrap(), None),
        Pixels::Editable { .. } => {
            panic!("Editable source rendering requires the separately qualified evaluator")
        }
    };
    assert_pixels(&output, expected, &pool);
    if let Some((input, pose, mapping)) = mapping {
        let original = expected.retained.as_ref().unwrap();
        assert_eq!(mapping.matrix(), original.matrix);
        assert_eq!(pose.projective, original.projective);
        assert_eq!(
            serde_json::to_value(pose.warps()).unwrap(),
            serde_json::to_value(&original.warps).unwrap()
        );
        if matches!(plan.pixels, Pixels::Native { .. }) {
            assert_pixels(input.source().unwrap(), &original.pixels, &pool);
        }
    } else {
        assert!(expected.retained.is_none());
    }
}

#[test]
fn whole_layer_and_mask_policy_match_actual_legacy_operators_at_native_depth() {
    for depth in [8, 16] {
        for (kind, linked) in [
            ("paint", None),
            ("paint", Some(true)),
            ("paint", Some(false)),
            ("fill", Some(true)),
        ] {
            let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
            let disk = DiskBudget::new(64 * 1024 * 1024, 8).unwrap();
            let root = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("peerbrush-native-policy-{}", crate::engine::id()));
            let limits = Limits {
                disk_bytes: 32 * 1024 * 1024,
                max_blocks: 256,
                max_resident_blocks: 16,
                manifest_bytes: 1024 * 1024,
            };
            let input =
                Store::create_with_disk_budget(&root, pool.clone(), disk.clone(), limits).unwrap();
            let doc = fixture(depth, kind, linked);
            let native = native_document(&doc, &pool, &disk, &input);
            let id = doc.layers[0].id.clone();
            for mut command in [
                json!({"op":"move","dx":3,"dy":-2}),
                json!({"op":"transform","angle":17.5,"scale_x":0.83,"scale_y":1.07,"dx":3.25,"dy":-2.75,"fractional_translation":true}),
                json!({"op":"transform","angle":90,"mirror_x":true}),
                json!({"op":"transform","mask":true,"step":"paint","angle":-13.25,"dx":-2}),
                json!({"op":"transform","projective":{"rect":[-7,13,10,32],"quad":[[-6,11],[11,14],[9,34],[-8,31]]}}),
                json!({"op":"transform","warp":{"rect":[-9,11,12,34],"preset":"arc","amount":0.07}}),
            ] {
                if command["mask"] == true && linked.is_none() {
                    continue;
                }
                command["layer"] = json!(id);
                let expected = Engine::preview_edits(doc.clone(), &[command.clone()]);
                let mut work = Work::new(10_000_000).unwrap();
                let planned = layer(&native, &id, &command, &mut work, &|| Ok(()));
                if let Err(reason) = expected {
                    assert!(
                        planned.is_err(),
                        "Expected legacy refusal {reason}: {command}"
                    );
                    continue;
                }
                let expected = expected.unwrap();
                let expected = expected.layers.iter().find(|l| l.id == id).unwrap();
                let plan = planned.unwrap().unwrap();
                assert_eq!(plan.origin, [expected.x, expected.y], "{command}");
                assert_eq!(
                    plan.frame,
                    [expected.pixels.width, expected.pixels.height],
                    "{command}"
                );
                let output_path = root.join(crate::engine::id());
                let output = Store::create_with_disk_budget(
                    &output_path,
                    pool.clone(),
                    disk.clone(),
                    limits,
                )
                .unwrap();
                if let Some(color) = &plan.color {
                    assert_plane(color, plan.frame, &expected.pixels, &output, &mut work);
                } else {
                    assert_pixels(
                        native.source(native.layer_slot(&id).unwrap()).unwrap(),
                        &expected.pixels,
                        &pool,
                    );
                }
                if let Some(mask) = plan.layer.mask() {
                    for step in mask.steps() {
                        let expected = expected
                            .mask
                            .as_ref()
                            .unwrap()
                            .steps
                            .iter()
                            .find(|s| s.id == step.id())
                            .unwrap();
                        if let Some(plane) =
                            plan.masks.iter().find(|p| p.target == step.pixels().slot())
                        {
                            assert_plane(plane, plan.frame, &expected.pixels, &output, &mut work);
                        } else {
                            assert_pixels(
                                native.source(step.pixels().slot()).unwrap(),
                                &expected.pixels,
                                &pool,
                            );
                        }
                    }
                }
                drop(output);
                std::fs::remove_dir_all(output_path).unwrap();
            }
            drop(native);
            drop(input);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
