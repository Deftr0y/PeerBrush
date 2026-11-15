use super::*;
use crate::engine::{
    native::worker::tests::Fixture,
    scene::{CapturedScene, LegacyTileCache},
    Engine,
};
use serde_json::json;

// The established resident scoring/order, independent of the new stripe reader.
fn oracle(doc: &Document, point: [f32; 2], radius: f32) -> [f32; 2] {
    let r = radius.clamp(2., 32.) as i32;
    let (x, y) = (point[0] as i32, point[1] as i32);
    let area = [
        (x - r).max(0),
        (y - r).max(0),
        (x + r + 1).min(doc.width as i32),
        (y + r + 1).min(doc.height as i32),
    ];
    let (w, h, p, a) = doc
        .preview(Some(area), (2 * r + 1) as u32, None, false)
        .unwrap();
    let lum = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        0.2126 * p[i] as f32 + 0.7152 * p[i + 1] as f32 + 0.0722 * p[i + 2] as f32
    };
    let mut best = point;
    let mut score = 8.;
    for yy in 1..h.saturating_sub(1) {
        for xx in 1..w.saturating_sub(1) {
            let dx = a[0] as f32 + xx as f32 - point[0];
            let dy = a[1] as f32 + yy as f32 - point[1];
            let edge = ((lum(xx + 1, yy) - lum(xx - 1, yy)).powi(2)
                + (lum(xx, yy + 1) - lum(xx, yy - 1)).powi(2))
            .sqrt();
            let value = edge / (1. + 0.2 * (dx * dx + dy * dy).sqrt());
            if value > score {
                score = value;
                best = [a[0] as f32 + xx as f32, a[1] as f32 + yy as f32];
            }
        }
    }
    best
}

#[test]
fn native_and_resident_snap_to_the_same_profiled_edges_without_changing_sources() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(256 * 1024 * 1024, 32 * 1024 * 1024);
        let pool = runtime.resident();
        let mut engine = Engine::from_document(Document::new_depth(87, 73, depth).unwrap());
        let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
        engine.edit("human", &[
            json!({"op":"paint.fill","layer":layer,"color":[0,0,0,255]}),
            json!({"op":"paint.fill","layer":layer,"rect":[31,10,70,65],"color":[255,201,130,255]}),
            json!({"op":"effect.add","layer":layer,"kind":"blur","settings":{"radius":1}}),
        ],None,None,"Edges").unwrap();
        engine.resident_state_mut().expect("resident fixture").document.layers[0]
            .pixels
            .set16(31, 25, [61439, 32891, 11987, 65535]);
        engine.resident_state_mut().expect("resident fixture").document.icc_profile = Some(Arc::new(
            moxcms::ColorProfile::new_display_p3().encode().unwrap(),
        ));
        let before = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
        let history = engine.resident_state().expect("resident fixture").undo.len();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(engine.resident_state().expect("resident fixture").document.clone(), "magnetic".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let scene = Arc::new(staged.scene);
        let session = Arc::new(staged.session.take().unwrap());
        let weak_scene = Arc::downgrade(&scene);
        let weak_session = Arc::downgrade(&session);
        for point in [[27.25, 24.75], [35.8, 40.1], [1.1, 1.9], [86.5, 72.5]] {
            for width in [2., 11.75, 32., 64.] {
                let expected = oracle(&engine.resident_state().expect("resident fixture").document, point, width);
                assert_eq!(
                    resident(&engine.resident_state().expect("resident fixture").document, point, width, &mut || Ok(())).unwrap(),
                    expected
                );
                assert_eq!(
                    native(scene.clone(), session.clone(), point, width, &mut || Ok(())).unwrap(),
                    expected
                );
            }
        }
        assert_ne!(
            resident(&engine.resident_state().expect("resident fixture").document, [27.25, 24.75], 12., &mut || Ok(())).unwrap(),
            [27.25, 24.75]
        );
        let used = pool.used();
        let full = pool
            .reserve_bytes(pool.limit() - used, pool.limit())
            .unwrap();
        assert!(
            native(scene.clone(), session.clone(), [30., 25.], 12., &mut || Ok(
                ()
            ))
            .is_err()
        );
        drop(full);
        assert_eq!(pool.used(), used);
        assert!(native(
            scene.clone(),
            session.clone(),
            [30., 25.],
            12.,
            &mut || Err("cancel".into())
        )
        .is_err());
        assert_eq!(pool.used(), used);
        assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history);
        drop((scene, session));
        assert!(weak_scene.upgrade().is_none());
        assert!(weak_session.upgrade().is_none());
        drop((staged.seals, cache, runtime, connection));
    }
}

#[test]
fn flat_tiny_edges_and_invalid_queries_are_explicit() {
    for (w, h) in [(1, 1), (2, 9), (9, 2), (8, 8)] {
        let doc = Document::new_depth(w, h, 16).unwrap();
        assert_eq!(
            resident(&doc, [0.5, 0.5], 12., &mut || Ok(())).unwrap(),
            [0.5, 0.5]
        );
    }
    for (point, width) in [
        ([f32::NAN, 0.], 12.),
        ([-1., 0.], 12.),
        ([8., 0.], 12.),
        ([1., 1.], f32::INFINITY),
    ] {
        assert!(area([8, 8], point, width).is_err());
    }
}
