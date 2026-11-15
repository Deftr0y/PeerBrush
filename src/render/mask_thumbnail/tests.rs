use super::*;
use crate::engine::{
    native::{worker::tests::Fixture, NativeDocument},
    scene::{CapturedScene, LegacyTileCache, SceneIdentity},
    Engine,
};
use serde_json::json;
use std::sync::Arc;

#[test]
fn thumbnail_keys_follow_mask_prefix_and_canvas_placement() {
    for depth in [8, 16] {
        let pool = crate::large_document::ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut engine = Engine::new();
        engine.resident_state_mut().unwrap().document =
            crate::engine::Document::new_depth(32, 24, depth).unwrap();
        let id = engine.resident_state().unwrap().document.layers[0].id.clone();
        engine.edit("human", &[
            json!({"op":"mask.add","layer":id,"value":41}),
            json!({"op":"mask.step.add","layer":id,"kind":"invert","weight":0.3}),
        ], None, None, "Mask").unwrap();
        let mut doc = engine.resident_state().unwrap().document.clone();
        let first = doc.layers[0].mask.as_ref().unwrap().steps[0].id.clone();
        let keys = |document: &crate::engine::Document| {
            let native = crate::tree::presentation_tests::native(document, &pool);
            let layer = native.metadata().layer(0).unwrap();
            [None, Some(first.as_str())].map(|step| cache_key(&native, layer, step).unwrap())
        };
        let original = keys(&doc);
        doc.layers[0].name = "Renamed".into();
        doc.layers[0].mask.as_mut().unwrap().enabled = false;
        assert_eq!(keys(&doc), original);
        doc.layers[0].x += 1;
        let moved = keys(&doc);
        assert_ne!(moved[0], original[0]);
        assert_eq!(moved[1], original[1]);
        doc.layers[0].mask.as_mut().unwrap().steps[1].weight = 0.8;
        let edited = keys(&doc);
        assert_ne!(edited[0], moved[0]);
        assert_eq!(edited[1], moved[1]);
        doc.layers[0].mask.as_mut().unwrap().steps[0].value = 90.;
        let changed_prefix = keys(&doc);
        assert_ne!(changed_prefix[0], edited[0]);
        assert_ne!(changed_prefix[1], edited[1]);
    }
}

#[test]
fn raw_color_thumbnail_keys_follow_placement_fill_and_profile() {
    use crate::render::native_preview::layer_thumbnail_key;

    for depth in [8, 16] {
        let pool = crate::large_document::ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut doc = crate::engine::Document::new_depth(32, 24, depth).unwrap();
        let key = |doc: &crate::engine::Document, profile| {
            let native = crate::tree::presentation_tests::native(doc, &pool);
            layer_thumbnail_key(&native, native.metadata().layer(0).unwrap(), profile).unwrap()
        };
        let original = key(&doc, None);
        doc.layers[0].name = "Renamed".into();
        doc.layers[0].visible = false;
        assert_eq!(key(&doc, None), original);
        assert_ne!(key(&doc, Some([1; 32])), original);
        doc.layers[0].x += 1;
        assert_ne!(key(&doc, None), original);
        doc.layers[0].kind = "fill".into();
        let fill = key(&doc, None);
        doc.layers[0].color[0] ^= 1;
        assert_ne!(key(&doc, None), fill);
    }
}

#[test]
fn native_prefixes_match_native_depth_masks_and_retire_source_owners() {
    const WIDTH: u32 = 513;
    const HEIGHT: u32 = 269;
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let pool = runtime.resident();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let mut engine = Engine::new();
        engine.resident_state_mut().expect("resident fixture").document =
            crate::engine::Document::new_depth(WIDTH, HEIGHT, depth).unwrap();
        let id = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[
                    json!({"op":"mask.add","layer":id,"value":41}),
                    json!({"op":"mask.step.add","layer":id,"kind":"paint"}),
                    json!({"op":"mask.step.add","layer":id,"kind":"blur","value":2.}),
                    json!({"op":"mask.step.add","layer":id,"kind":"invert","weight":0.3}),
                    json!({"op":"mask.toggle","layer":id,"enabled":false}),
                ],
                None,
                None,
                "Mask",
            )
            .unwrap();
        let layer = &mut engine.resident_state_mut().expect("resident fixture").document.layers[0];
        layer.x = -17;
        layer.y = 43;
        let paint = &mut layer.mask.as_mut().unwrap().steps[1].pixels;
        paint.set16(10, 10, [12347, 12347, 12347, 61379]);
        paint.set16(267, 256, [23459, 23459, 23459, 45713]);
        let original = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
        let history = engine.history_lengths();
        let mut staged = CapturedScene::from_parts(
            engine.resident_state().expect("resident fixture").document.clone(),
            "mask-thumbnail".into(),
            vec![],
            None,
            None,
        )
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let scene = Arc::new(
            SceneSnapshot::from_native(
                SceneIdentity {
                    project_id: "mask-thumbnail".into(),
                    document_id: engine.resident_state().expect("resident fixture").document.id.clone(),
                    revision: engine.resident_state().expect("resident fixture").document.revision,
                },
                native,
                vec![],
                None,
                None,
            )
            .unwrap(),
        );
        let session = Arc::new(staged.session.take().unwrap());
        let read = SourceReadSession::from(session.clone());
        let weak_scene = Arc::downgrade(&scene);
        let weak_session = Arc::downgrade(&session);
        let mut completed = None;
        for index in 0..engine.resident_state().expect("resident fixture").document.layers[0].mask.as_ref().unwrap().steps.len() {
            let mut oracle = engine.resident_state().expect("resident fixture").document.clone();
            let mask = oracle.layers[0].mask.as_mut().unwrap();
            mask.steps.truncate(index + 1);
            mask.cache_key = crate::engine::id();
            mask.enabled = true;
            let step = &engine.resident_state().expect("resident fixture").document.layers[0].mask.as_ref().unwrap().steps[index].id;
            let image = render(&scene, &read, &id, step, &|| Ok(())).unwrap();
            let scale = EDGE as f64 / f64::from(WIDTH);
            let masks16 = (depth == 16).then(|| crate::depth16::prepare_masks(&oracle).unwrap());
            let mask8 = oracle.layers[0].mask.as_ref().unwrap().prepare(WIDTH, HEIGHT);
            for y in 0..image.size[1] {
                for x in 0..image.size[0] {
                    let (sx, sy) = ((x as f64 / scale) as i32, (y as f64 / scale) as i32);
                    let factor = if let Some(masks) = &masks16 {
                        crate::depth16::mask_value_prepared(
                            &oracle.layers[0],
                            sx,
                            sy,
                            masks[0].as_deref(),
                            true,
                        )
                    } else {
                        oracle.layers[0].mask_value_prepared(sx, sy, mask8.as_deref(), true) as f64
                    };
                    let gray = (factor * 255.).round() as u8;
                    let at = (y * image.size[0] + x) as usize * 4;
                    assert_eq!(&image.pixels[at..at + 4], &[gray, gray, gray, 255]);
                }
            }
            completed = Some(image);
        }
        let used = pool.used();
        let native = scene.native_document().unwrap();
        let layer = native.metadata().layer(0).unwrap();
        let [x, y] = layer.origin();
        let columns = [10, 267, 512];
        let rows = [10, 256, 268];
        for prefix in [2, layer.mask().unwrap().steps().len()] {
            let grid = crate::mask::region::prepare_bake_grid(
                native, layer, prefix, &columns, &rows,
                &mut ReadChecks::caller_only(&mut || Ok(())),
            ).unwrap();
            for (row, &sy) in rows.iter().enumerate() {
                let dense = crate::mask::region::prepare_bake(
                    native, layer, [x, y + sy, x + WIDTH as i32, y + sy + 1], prefix, false,
                    &mut ReadChecks::caller_only(&mut || Ok(())),
                ).unwrap();
                for (column, &sx) in columns.iter().enumerate() {
                    assert_eq!(
                        grid.value16(column as i32, row as i32).unwrap().to_bits(),
                        dense.value16(sx, sy).unwrap().to_bits()
                    );
                }
            }
        }
        let mut checks = 0;
        let interrupted = crate::mask::region::prepare_bake(
            native,
            layer,
            [x, y, x + 1, y + 1],
            2,
            false,
            &mut ReadChecks::caller_only(&mut || {
                checks += 1;
                if checks == 3 {
                    Err(crate::workspace::source_runtime::INVENTORY_CHECK_BUSY.into())
                } else {
                    Ok(())
                }
            }),
        );
        assert_eq!(interrupted.err().as_deref(), Some(crate::workspace::source_runtime::INVENTORY_CHECK_BUSY));
        assert_eq!(pool.used(), used);
        assert!(render(&scene, &read, &id, "missing", &|| Ok(())).is_err());
        let step = &engine.resident_state().expect("resident fixture").document.layers[0].mask.as_ref().unwrap().steps[0].id;
        assert!(render(&scene, &read, &id, step, &|| Err("cancelled".into())).is_err());
        let held = pool
            .reserve_bytes(pool.limit() - pool.used(), pool.limit())
            .unwrap();
        assert!(render(&scene, &read, &id, step, &|| Ok(())).is_err());
        drop(held);
        assert_eq!(pool.used(), used);
        assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), original);
        assert_eq!(engine.history_lengths(), history);
        drop(scene);
        drop(read);
        drop(session);
        assert!(weak_scene.upgrade().is_none());
        assert!(weak_session.upgrade().is_none());
        assert_eq!(completed.unwrap().pixels.len(), EDGE * 25 * 4);
        drop(connection);
    }
}
