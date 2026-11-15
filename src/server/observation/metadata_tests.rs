use super::{fixture, metadata};
use crate::{
    engine::{Engine, Layer, Mask, MaskStep},
    raster::Raster,
    source::{Content, Source, TextLayout},
};
use serde_json::json;
use std::sync::Arc;

#[test]
fn typed_historic_liquify_coverage_remains_observable_without_inline_raster_json() {
    let mut doc = fixture::document(16, 9, 7);
    let mut mask = Raster::new(4, 4);
    mask.set(0, 0, [73, 0, 0, 255]);
    mask.set(2, 2, [191, 0, 0, 255]);
    mask.retained = Some(Box::new(crate::retained::Original {
        pixels: Box::new(mask.clone()),
        matrix: [1., 0., 0., 1., 0.25, -0.5],
        projective: None,
        warps: vec![],
    }));
    let coverage = crate::selection::Coverage {
        mask,
        bounds: [1, 1, 3, 3],
        origin: [0, 0],
        contours: vec![vec![[1., 1.], [3., 1.], [3., 3.], [1., 3.]]],
    };
    let settings = json!({"radius":4.,"strength":0.7,"opaque_extension":{"kept":true},"strokes":[{"mode":"push","points":[[1.,1.],[2.,2.]],"radius":3.,"strength":0.4,"coverage":coverage,"selection":[1,1,3,3]}]});
    doc.layers[0].effects.push(crate::effects::Effect {
        id: "historic-clip".into(),
        kind: "liquify".into(),
        enabled: false,
        weight: 0.9,
        settings: Arc::new(settings.clone()),
    });
    let original = serde_json::to_value(&doc).unwrap();
    let (native, scene, path) = fixture::capture(&doc);
    let pool = native.budget().clone();
    let layers = serde_json::to_value(metadata::Layers(&scene)).unwrap();
    let effect = &layers[0]["effects"][0];
    let mut expected = settings;
    expected["strokes"][0]
        .as_object_mut()
        .unwrap()
        .remove("coverage");
    assert_eq!(effect["settings"], expected);
    let clip = &effect["native_coverage_clips"][0];
    assert_eq!(clip["stroke"], 0);
    assert_eq!(clip["coverage"]["bounds"], json!([1, 1, 3, 3]));
    assert_eq!(clip["coverage"]["origin"], json!([0, 0]));
    assert_eq!(clip["coverage"]["contours"], json!(coverage.contours));
    assert_eq!(clip["coverage"]["bit_depth"], 8);
    assert_eq!(clip["coverage"]["storage_kind"], "source-backed");
    assert_eq!(
        clip["coverage"]["transform_source"]["matrix"],
        json!([1., 0., 0., 1., 0.25, -0.5])
    );
    let coverage = native
        .metadata()
        .layer(0)
        .unwrap()
        .effects()
        .next()
        .unwrap()
        .clips()
        .next()
        .unwrap()
        .1;
    let view = native
        .source(coverage.mask().slot())
        .unwrap()
        .prepare_region(
            crate::raster::backed::Region::new([0, 0, 1, 1]).unwrap(),
            &mut || Ok(()),
        )
        .unwrap();
    assert_eq!(view.pixel(0, 0).unwrap(), [73, 0, 0, 255]);
    drop(view);
    assert_eq!(serde_json::to_value(&doc).unwrap(), original);
    drop(scene);
    drop(native);
    assert_eq!(pool.used(), 0);
    fixture::cleanup(&path);
}
#[test]
fn native_metadata_preserves_legacy_fields_definitions_paths_masks_and_honest_retained_storage() {
    for depth in [8, 16] {
        let mut doc = fixture::document(depth, 17, 13);
        doc.name = "Observation source".into();
        doc.warnings.push("Actual source warning".into());
        doc.selection = Some([2, 1, 7, 6]);
        doc.selection_polygon = Some(vec![[2., 1.], [7., 2.], [6., 6.], [2., 5.]]);
        let original = crate::retained::Original {
            pixels: Box::new(doc.layers[0].pixels.clone()),
            matrix: [1., 0., 0., 1., 2., 3.],
            projective: None,
            warps: vec![],
        };
        doc.layers[0].pixels.retained = Some(Box::new(original));
        doc.layers[0].effects.push(crate::effects::Effect {
            id: "metadata-effect".into(),
            kind: "drop_shadow".into(),
            enabled: false,
            weight: 0.375,
            settings: Arc::new(json!({"unknown_preserved":"source definition","offset_x":3})),
        });
        let mut mask = Raster::new(7, 5);
        if depth == 16 {
            mask.promote16();
        }
        mask.set16(2, 3, [31999, 31999, 31999, 65535]);
        doc.layers[0].mask = Some(Mask {
            enabled: false,
            linked: false,
            cache_key: crate::engine::id(),
            steps: vec![MaskStep {
                id: "metadata-mask".into(),
                kind: "paint".into(),
                enabled: true,
                weight: 0.7,
                value: 0.4,
                pixels: mask,
                settings: json!({"unknown":"retained"}),
            }],
        });
        let mut path = Layer::new("Actual path", "paint", 8, 8);
        if depth == 16 {
            path.pixels.promote16();
        }
        path.x = 3;
        path.y = -2;
        path.source = Some(Arc::new(Source {
            width: 8,
            height: 8,
            matrix: [1., 0.25, -0.5, 1., 2., -1.],
            content: Content::Bezier {
                nodes: vec![
                    crate::path::Anchor {
                        point: [1., 2.],
                        in_handle: Some([0.25, 1.]),
                        out_handle: Some([2.25, 3.]),
                        smooth: true,
                    },
                    crate::path::Anchor {
                        point: [5., 6.],
                        in_handle: None,
                        out_handle: None,
                        smooth: false,
                    },
                ],
                closed: false,
                fill: [0; 4],
                stroke: [12347, 33559, 51237, 65535],
                stroke_width: 1.25,
            },
        }));
        doc.layers.push(path);
        let mut text = Layer::new("Default text omissions", "paint", 8, 8);
        if depth == 16 {
            text.pixels.promote16();
        }
        text.source = Some(Arc::new(Source {
            width: 8,
            height: 8,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content::Text {
                text: "Native metadata".into(),
                layout: TextLayout::Legacy,
                font: crate::fonts::TextFont::Bundled("regular".into()),
                size: 12.,
                line_height: 1.2,
                letter_spacing: 0.,
                align: "left".into(),
                color: [12347, 33559, 51237, 65535],
            },
        }));
        doc.layers.push(text);
        let mut legacy = Engine::new();
        legacy.resident_state_mut().expect("resident fixture").document = doc.clone();
        let expected = legacy.capture_state().unwrap().project().unwrap();
        let (native, scene, path) = fixture::capture(&doc);
        let pool = native.budget().clone();
        let profile = crate::color_profile::summary(None);
        let document = serde_json::to_value(metadata::Document {
            scene: &scene,
            profile: &profile,
        })
        .unwrap();
        assert_eq!(document, expected["document"]);
        let mut layers = serde_json::to_value(metadata::Layers(&scene)).unwrap();
        let stored = &mut layers[0]["transform_source"];
        assert!(stored["bytes"].is_null());
        assert_eq!(stored["storage_kind"], "source-backed");
        assert_eq!(
            stored["native_logical_bytes"],
            17 * 13 * 4 * (depth / 8) as u64
        );
        // Only the explicit Native storage observation differs from the legacy
        // resident allocation count; every other field uses its real oracle.
        stored.as_object_mut().unwrap().remove("storage_kind");
        stored
            .as_object_mut()
            .unwrap()
            .remove("native_logical_bytes");
        stored["bytes"] = expected["layers"][0]["transform_source"]["bytes"].clone();
        assert_eq!(layers, expected["layers"]);
        drop(scene);
        drop(native);
        assert_eq!(pool.used(), 0);
        fixture::cleanup(&path);
    }
}
