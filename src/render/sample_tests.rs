use super::*;
use crate::engine::{
    native::worker::tests::Fixture, scene::CapturedScene, scene::LegacyTileCache, Engine,
};
use serde_json::json;

fn artwork(depth: u16) -> Document {
    let mut engine = Engine::new();
    engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(19, 13, depth).unwrap();
    let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
    engine
        .edit(
            "human",
            &[
                json!({"op":"paint.fill","layer":layer,"rect":[0,0,19,13],"color":[31,83,149,199]}),
                json!({"op":"mask.add","layer":layer,"value":0}),
                json!({"op":"mask.toggle","layer":layer,"enabled":false}),
                json!({"op":"effect.add","layer":layer,"kind":"blur","settings":{"radius":1}}),
                json!({"op":"filter.add","kind":"blur","settings":{"radius":1}}),
            ],
            None,
            None,
            "Sample fixture",
        )
        .unwrap();
    let layer = &mut engine.resident_state_mut().expect("resident fixture").document.layers[0];
    layer.pixels.set16(3, 4, [12347, 23459, 34571, 61379]);
    let mask = layer.mask.as_mut().unwrap();
    mask.steps[1].weight = 0.5;
    mask.steps[1]
        .pixels
        .set16(3, 4, [32638, 32638, 32638, 65535]);
    engine.resident_state_mut().expect("resident fixture").document.icc_profile = Some(Arc::new(
        moxcms::ColorProfile::new_display_p3().encode().unwrap(),
    ));
    engine.resident_state_mut().expect("resident fixture").document.selection = Some([1, 2, 5, 6]);
    engine.resident_state_mut().expect("resident fixture").document.selection_polygon = Some(vec![[1., 2.], [5., 2.], [5., 6.]]);
    engine.into_resident_document().unwrap()
}

#[test]
fn exact_native_samples_match_profiled_composites_and_disabled_raw_masks_without_source_mutation() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(256 * 1024 * 1024, 32 * 1024 * 1024);
        let pool = runtime.resident();
        let mut doc = artwork(depth);
        doc.layers[0].x = -2;
        doc.layers[0].y = 1;
        let original = serde_json::to_value(&doc).unwrap();
        let layer = doc.layers[0].id.as_str();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(doc.clone(), "sample".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let scene = Arc::new(staged.scene);
        let session = Arc::new(staged.session.take().unwrap());
        let weak_scene = Arc::downgrade(&scene);
        let weak_session = Arc::downgrade(&session);
        let key = scene.key();
        for point in [[0, 0], [1, 5], [3, 4], [18, 12]] {
            for target in [None, Some(layer)] {
                let expected: Pixel = doc
                    .preview(
                        Some([point[0], point[1], point[0] + 1, point[1] + 1]),
                        1,
                        target,
                        target.is_some(),
                    )
                    .unwrap()
                    .2
                    .try_into()
                    .unwrap();
                assert_eq!(resident(&doc, point, target).unwrap(), expected);
                let actual = native(
                    scene.clone(),
                    session.clone(),
                    point,
                    target,
                    &mut || Ok(()),
                )
                .unwrap();
                assert_eq!(
                    actual, expected,
                    "depth {depth} at {point:?}, target {target:?}"
                );
                if depth == 16 {
                    let area = [point[0], point[1], point[0] + 1, point[1] + 1];
                    let (_, _, raw, _) = crate::depth16::preview16(&doc, Some(area), 1, target, target.is_some()).unwrap();
                    let expected = if target.is_none() {
                        let profile = crate::color_profile::transforms(doc.icc_profile.as_deref().unwrap()).unwrap();
                        let mut converted = [0; 4];
                        profile.convert16_into(&raw, &mut converted).unwrap();
                        converted
                    } else {
                        <[u16; 4]>::try_from(raw).unwrap()
                    };
                    assert_eq!(
                        native_color(scene.clone(), session.clone(), point, target, &mut || Ok(())).unwrap().words(),
                        Some(expected),
                    );
                }
                assert_eq!(scene.key(), key);
            }
        }
        assert!(native(
            scene.clone(),
            session.clone(),
            [3, 4],
            Some("missing"),
            &mut || Ok(())
        )
        .is_err());
        for at in [[-1, 0], [19, 0], [0, 13], [i32::MAX, 0]] {
            assert!(resident(&doc, at, None).is_err());
            assert!(native(scene.clone(), session.clone(), at, None, &mut || Ok(())).is_err());
        }
        assert!(
            native(scene.clone(), session.clone(), [3, 4], None, &mut || Err(
                "Cancelled sampling".into()
            ))
            .unwrap_err()
            .contains("Cancelled")
        );
        assert_eq!(serde_json::to_value(&doc).unwrap(), original);
        drop((scene, session));
        assert!(weak_scene.upgrade().is_none());
        assert!(weak_session.upgrade().is_none());
        drop((staged.seals, cache, runtime, connection));
    }
}

#[test]
fn mask_sampling_projects_after_native_precision_evaluation_and_ignores_profile_conversion() {
    let mut doc = artwork(16);
    let id = doc.layers[0].id.clone();
    let source = doc.layers[0].mask.as_ref().unwrap().steps[1]
        .pixels
        .get16(3, 4);
    assert_eq!(source, [32638, 32638, 32638, 65535]);
    assert_eq!(
        resident(&doc, [3, 4], Some(&id)).unwrap(),
        [63, 63, 63, 255]
    );
    assert_eq!(
        (doc.layers[0].mask_value_raw(3, 4) * 255.).round() as u8,
        64
    );
    doc.icc_profile = None;
    assert_eq!(
        resident(&doc, [3, 4], Some(&id)).unwrap(),
        [63, 63, 63, 255]
    );
    assert_eq!(
        doc.layers[0].mask.as_ref().unwrap().steps[1]
            .pixels
            .get16(3, 4),
        source
    );
    doc.layers[0].mask = None;
    assert_eq!(resident(&doc, [3, 4], Some(&id)).unwrap(), [255; 4]);
    assert!(resident(&doc, [3, 4], Some("missing")).is_err());
}

#[test]
fn points_floor_within_the_document_and_never_saturate_invalid_coordinates() {
    assert_eq!(point([19, 13], [3.99, 4.01]).unwrap(), [3, 4]);
    assert_eq!(point([19, 13], [-0., 12.99]).unwrap(), [0, 12]);
    for at in [
        [f32::NAN, 0.],
        [f32::INFINITY, 0.],
        [0., f32::NEG_INFINITY],
        [-0.01, 0.],
        [19., 0.],
        [0., 13.],
        [f32::MAX, 0.],
    ] {
        assert!(point([19, 13], at).is_err());
    }
    assert!(point([0, 0], [0., 0.]).is_err());
    assert!(rect([u32::MAX, 13], [i32::MAX, 0]).is_err());
}
