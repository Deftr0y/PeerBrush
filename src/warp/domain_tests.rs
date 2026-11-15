use super::*;
use crate::{
    engine::{
        native::NativeDocument,
        scene::{
            Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, Retained36,
            Warp36,
        },
        Document, Mask, MaskStep,
    },
    large_document::{DiskBudget, Error, ResidentBudget},
    psd::memory36::{Admitted, MetadataCharge},
    raster::{Raster, Surface},
};
use serde_json::json;
use std::sync::Arc;

// Geometry-only fixtures retain all placement records; zero surfaces require no IO.
struct Zeros(Vec<Surface>);
impl RasterCollector for Zeros {
    fn resident(&mut self, raster: &Raster) -> Result<RasterRef36, String> {
        assert!(raster.tiles.is_empty() && raster.samples16.is_empty());
        self.0
            .push(Surface::zero(raster.width, raster.height, raster.depth).unwrap());
        let id = self.0.len() as u32;
        let retained = raster
            .retained
            .as_ref()
            .map(|original| {
                Ok::<_, String>(Box::new(Retained36 {
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
    fn surface(&mut self, _: &Surface) -> Result<RasterRef36, String> {
        unreachable!()
    }
}
fn native(doc: &Document, pool: &ResidentBudget) -> NativeDocument {
    let owner = MetadataCharge::reserve(pool, 1024 * 1024, &|| Ok(())).unwrap();
    let mut collector = Zeros(vec![]);
    let metadata = Document36::capture(doc, &mut collector).unwrap();
    NativeDocument::from_pending(
        PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), owner),
            RasterRoster::new(collector.0).unwrap(),
            pool.clone(),
            DiskBudget::new(1024 * 1024, 4).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}
fn fixture(depth: u16, linked: bool, selected: bool, warped: bool) -> Document {
    let mut doc = Document::new_depth(32, 24, depth).unwrap();
    let mut group = Layer::new("Folder", "group", 32, 24);
    group.pixels = Raster::new_depth(32, 24, depth);
    let layer = &mut doc.layers[0];
    layer.parent = Some(group.id.clone());
    layer.visible = false;
    layer.x = -3;
    layer.y = 5;
    layer.pixels = Raster::new_depth(8, 6, depth);
    let mut original = crate::retained::Original::from(&Raster::new_depth(10, 9, depth));
    original.matrix = [1.1, 0.2, -0.1, 0.9, 20., -7.];
    if warped {
        original.warps.push(crate::warp::Stage {
            before: Map::IDENTITY.matrix(),
            grid: Grid::preset([-2., -2., 12., 11.], "arc", 0.15).unwrap(),
        });
    } else {
        original.projective = Some([1., 0., 0., 0., 1., 0., 0.003, 0.002, 1.]);
    }
    layer.pixels.retained = Some(Box::new(original));
    let mut mask_pixels = Raster::new_depth(8, 6, depth);
    let mut mask_original = crate::retained::Original::from(&Raster::new_depth(12, 7, depth));
    mask_original.matrix = [1., 0., 0., 1., -24., 13.];
    mask_pixels.retained = Some(Box::new(mask_original));
    layer.mask = Some(Mask {
        enabled: true,
        linked,
        cache_key: crate::engine::id(),
        steps: vec![
            MaskStep {
                id: crate::engine::id(),
                kind: "paint".into(),
                enabled: false,
                weight: 0.3,
                value: 255.,
                pixels: mask_pixels,
                settings: json!({}),
            },
            MaskStep {
                id: crate::engine::id(),
                kind: "fill".into(),
                enabled: true,
                weight: 1.,
                value: 255.,
                pixels: Raster::new_depth(8, 6, depth),
                settings: json!({}),
            },
        ],
    });
    doc.layers.push(group);
    if selected {
        doc.selection = Some([2, 2, 6, 5]);
    }
    doc
}

#[test]
fn native_domains_preserve_hidden_originals_linked_masks_and_selected_targets_without_io() {
    for depth in [8, 16] {
        for linked in [false, true] {
            for selected in [false, true] {
                for warped in [false, true] {
                    let doc = fixture(depth, linked, selected, warped);
                    let layer = &doc.layers[0];
                    let step = &layer.mask.as_ref().unwrap().steps[0].id;
                    let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
                    let native = native(&doc, &pool);
                    let before = serde_json::to_value(&doc).unwrap();
                    let key = native.key();
                    let baseline = pool.used();
                    for commands in [
                        vec![json!({"layer":layer.id})],
                        vec![json!({"layer":layer.id,"selection_only":false})],
                        vec![json!({"layer":doc.layers[1].id})],
                        vec![json!({"layer":doc.layers[1].id}), json!({"layer":layer.id})],
                        vec![json!({"layer":layer.id,"mask":true,"step":step})],
                        vec![
                            json!({"layer":layer.id,"mask":true,"step":step,"selection_only":false}),
                        ],
                        vec![json!({"layer":layer.id,"mask":true,"step":"missing"})],
                        vec![json!({"layer":"missing"})],
                        vec![],
                    ] {
                        let expected = domain(&doc, &commands, &|| Ok(()));
                        let actual = domain(&native, &commands, &|| Ok(()));
                        assert_eq!(actual, expected, "depth {depth}, linked {linked}, selected {selected}, warped {warped}: {commands:?}");
                        assert_eq!(pool.used(), baseline);
                    }
                    let target = json!({"layer":layer.id,"selection_only":false});
                    let expected = domain(&native, &[target.clone()], &|| Ok(())).unwrap();
                    let mut pending = target;
                    pending["angle"] = json!(91.);
                    pending["warp"] = json!({"preset":"arc","rect":expected,"amount":0.25});
                    assert_eq!(domain(&native, &[pending], &|| Ok(())).unwrap(), expected);
                    assert_eq!(native.key(), key);
                    assert_eq!(serde_json::to_value(&doc).unwrap(), before);
                    drop(native);
                    assert_eq!(pool.used(), 0);
                }
            }
        }
    }
}

#[test]
fn unsupported_warp_targets_report_the_same_errors_for_both_storage_modes() {
    for kind in ["adjustment", "source", "mask"] {
        let mut doc = fixture(16, false, false, false);
        let layer = &mut doc.layers[0];
        let command = match kind {
            "adjustment" => {
                layer.kind = "adjustment".into();
                json!({"layer":doc.layers[1].id})
            }
            "source" => {
                layer.source = Some(Arc::new(crate::source::Source {
                    width: 8,
                    height: 6,
                    matrix: [1., 0., 0., 1., 0., 0.],
                    content: crate::source::Content::Bezier {
                        nodes: vec![
                            crate::path::Anchor {
                                point: [1., 1.],
                                in_handle: None,
                                out_handle: None,
                                smooth: false,
                            },
                            crate::path::Anchor {
                                point: [6., 4.],
                                in_handle: None,
                                out_handle: None,
                                smooth: false,
                            },
                        ],
                        closed: false,
                        fill: [0; 4],
                        stroke: [65535; 4],
                        stroke_width: 1.,
                    },
                }));
                json!({"layer":layer.id})
            }
            _ => {
                layer.mask = None;
                json!({"layer":layer.id,"mask":true})
            }
        };
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let native = native(&doc, &pool);
        let baseline = pool.used();
        let expected = domain(&doc, &[command.clone()], &|| Ok(())).unwrap_err();
        assert!(expected.contains(match kind {
            "adjustment" => "adjustment children",
            "source" => "editable sources",
            _ => "paint mask step",
        }));
        assert_eq!(
            domain(&native, &[command], &|| Ok(())).unwrap_err(),
            expected
        );
        assert_eq!(pool.used(), baseline);
        drop(native);
        assert_eq!(pool.used(), 0);
    }
}

#[test]
fn domain_cancellation_and_admission_refusal_leave_native_authority_unchanged() {
    let doc = fixture(16, true, false, true);
    let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
    let native = native(&doc, &pool);
    let key = native.key();
    let commands = [json!({"layer":doc.layers[1].id})];
    let baseline = pool.used();
    for stop in 1..=9 {
        let calls = std::cell::Cell::new(0);
        let result = domain(&native, &commands, &|| {
            calls.set(calls.get() + 1);
            if calls.get() == stop {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(result.unwrap_err().contains("cancelled"));
        assert_eq!(pool.used(), baseline);
    }
    let full = pool
        .reserve_bytes(pool.limit() - pool.used(), pool.limit())
        .unwrap();
    assert!(domain(&native, &commands, &|| Ok(())).is_err());
    drop(full);
    assert_eq!(pool.used(), baseline);
    assert_eq!(native.key(), key);
    assert!(domain(&native, &commands, &|| Ok(())).is_ok());
    drop(native);
    assert_eq!(pool.used(), 0);
}
