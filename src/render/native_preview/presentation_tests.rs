use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    native::{worker, NativeDocument},
    scene::{CapturedScene, LegacyTileCache, SceneIdentity},
    Document, Layer,
};
use serde_json::json;

#[test]
fn scalar_animation_renders_exact_native_sources_without_history_or_edit_authority() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(256 * 1024 * 1024, 32 * 1024 * 1024);
        let pool = runtime.resident();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let mut before = Document::new_depth(21, 17, depth).unwrap();
        before.layers[0]
            .pixels
            .set16(4, 5, [12347, 23459, 34571, 61379]);
        let mut fill = Layer::new("Fill", "fill", 21, 17);
        fill.pixels = crate::raster::Raster::new_depth(21, 17, depth);
        fill.color = [191, 73, 29, 255];
        fill.opacity = 0.25;
        before.layers.push(fill);
        before.icc_profile = Some(Arc::new(
            moxcms::ColorProfile::new_display_p3().encode().unwrap(),
        ));
        let mut after = before.clone();
        after.revision += 1;
        after.layers[0].x = 3;
        after.layers[0].y = -2;
        after.layers[0].opacity = 0.4;
        after.layers[1].color = [31, 109, 223, 199];
        after.layers[1].opacity = 0.8;
        let original = serde_json::to_value(&after).unwrap();
        let old = CapturedScene::from_parts(before.clone(), "animation".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
        let old_native =
            NativeDocument::from_pending(old.scene.into_provisional_pending().unwrap()).unwrap();
        let mut staged =
            CapturedScene::from_parts(after.clone(), "animation".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let document =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
        let key = document.key();
        let saved = serde_json::to_value(document.metadata()).unwrap();
        let session = Arc::new(staged.session.take().unwrap());
        let weak_session = Arc::downgrade(&session);
        let scene = Arc::new(
            crate::engine::scene::SceneSnapshot::from_native(
                SceneIdentity {
                    project_id: "animation".into(),
                    document_id: after.id.clone(),
                    revision: after.revision,
                },
                document.clone(),
                vec![],
                None,
                None,
            )
            .unwrap(),
        );
        let weak_scene = Arc::downgrade(&scene);
        let mut final_frame = None;
        for t in [0., 0.25, 0.5, 0.75, 1.] {
            let poses = Presentation::capture(&old_native, &document, t, &|| Ok(())).unwrap();
            let display = poses.scene(&scene, &|| Ok(())).unwrap();
            assert!(display.native_document().is_err());
            assert_eq!(display.read_source_key(), key);
            assert_ne!(display.key(), key);
            assert_eq!(display.identity(), scene.identity());
            assert_eq!(
                display
                    .rasters()
                    .iter()
                    .map(|(_, r)| r.key())
                    .collect::<Vec<_>>(),
                scene
                    .rasters()
                    .iter()
                    .map(|(_, r)| r.key())
                    .collect::<Vec<_>>()
            );
            assert!(CapturedScene::from_native(display).is_err());
            assert!(poses
                .scene(&scene, &|| Ok(()))
                .unwrap()
                .into_provisional_pending()
                .is_err());
            let (frame, origin) = render(
                scene.clone(),
                session.clone().into(),
                &poses,
                None,
                21,
                None,
                false,
                &|| Ok(()),
            )
            .unwrap();
            assert!(origin.matches_source("animation", key, after.revision));
            assert!(origin.matches_frame(&frame));
            let mut expected = after.clone();
            for (layer, old) in expected.layers.iter_mut().zip(&before.layers) {
                let lerp = |a: f32, b: f32| a + (b - a) * t;
                layer.x = lerp(old.x as f32, layer.x as f32).round() as i32;
                layer.y = lerp(old.y as f32, layer.y as f32).round() as i32;
                layer.opacity = lerp(old.opacity, layer.opacity);
                layer.color = std::array::from_fn(|i| {
                    lerp(old.color[i] as f32, layer.color[i] as f32).round() as u8
                });
            }
            assert_eq!(
                &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                expected
                    .preview(None, 21, None, false)
                    .unwrap()
                    .2
                    .as_slice()
            );
            final_frame = Some((frame, origin));
        }
        assert_eq!(document.key(), key);
        assert_eq!(serde_json::to_value(document.metadata()).unwrap(), saved);
        assert_eq!(authority.history_lengths(), [0, 0]);
        assert_eq!(serde_json::to_value(&after).unwrap(), original);
        let poses = Presentation::capture(&old_native, &document, 0.5, &|| Ok(())).unwrap();
        let used = pool.used();
        let full = pool
            .reserve_bytes(pool.limit() - used, pool.limit())
            .unwrap();
        assert!(poses.scene(&scene, &|| Ok(())).is_err());
        assert!(Presentation::capture(&old_native, &document, 0.5, &|| Ok(())).is_err());
        drop(full);
        assert_eq!(pool.used(), used);
        assert!(render(
            scene.clone(),
            session.clone().into(),
            &poses,
            None,
            21,
            None,
            false,
            &|| Err("cancel".into())
        )
        .is_err());
        assert_eq!(pool.used(), used);
        drop((
            poses,
            scene,
            session,
            old_native,
            document,
            authority,
            old.session,
        ));
        assert!(weak_scene.upgrade().is_none());
        assert!(weak_session.upgrade().is_none());
        assert!(final_frame
            .as_ref()
            .unwrap()
            .1
            .matches_frame(&final_frame.as_ref().unwrap().0));
        drop((
            final_frame,
            old.seals,
            staged.seals,
            cache,
            runtime,
            connection,
        ));
    }
}

#[test]
fn zero_source_read_permits_bind_to_the_original_roster_and_cannot_adopt_display_metadata() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let before =
        NativeDocument::new_blank(7, 5, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let authority = DocumentAuthority::from_native(before.clone(), 4).unwrap();
    let layer = before.metadata().layers().next().unwrap().id().to_owned();
    let prepared = worker::prepare(
        worker::tests::capture(
            &authority,
            &[json!({"op":"layer.update","layer":layer,"opacity":0.25})],
        ),
        &session,
        &|| Ok(()),
    )
    .unwrap();
    {
        let after = prepared.preview();
        let scene = Arc::new(
            crate::engine::scene::SceneSnapshot::from_native(
                SceneIdentity {
                    project_id: "zero".into(),
                    document_id: after.metadata().id().into(),
                    revision: after.metadata().revision(),
                },
                after.clone(),
                vec![],
                None,
                None,
            )
            .unwrap(),
        );
        let earlier = crate::engine::scene::SceneSnapshot::from_native(
            SceneIdentity {
                project_id: "zero".into(),
                document_id: before.metadata().id().into(),
                revision: before.metadata().revision(),
            },
            before.clone(),
            vec![],
            None,
            None,
        )
        .unwrap();
        let poses = Presentation::capture(&before, after, 0.5, &|| Ok(())).unwrap();
        let display = poses.scene(&scene, &|| Ok(())).unwrap();
        assert!(runtime
            .begin_read(&earlier)
            .unwrap()
            .check_scene(&display)
            .is_err());
        let read = runtime.begin_read(&scene).unwrap();
        read.check_scene(&display).unwrap();
        let (frame, origin) = render(scene, read, &poses, None, 7, None, false, &|| Ok(())).unwrap();
        assert!(frame
            .copy_rgba8(&mut || Ok(()))
            .unwrap()
            .iter()
            .all(|byte| *byte == 0));
        assert!(origin.matches_frame(&frame));
        assert!(Presentation::capture(&before, after, f32::NAN, &|| Ok(())).is_err());
        assert!(Presentation::capture(after, &before, 0.5, &|| Ok(())).is_err());
    }
    prepared
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(authority.history_lengths(), [0, 0]);
    drop((authority, before, session, runtime, connection));
}
