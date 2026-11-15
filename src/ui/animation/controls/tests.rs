use super::*;
use crate::engine::{
    native::{worker, NativeDocument},
    scene::{CapturedScene, LegacyTileCache},
    Mask, MaskStep,
};

#[test]
fn resident_and_native_numbers_share_metadata_without_changing_sources() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let pool = runtime.resident();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let mut before = Document::new_depth(12, 8, depth).unwrap();
        let layer = &mut before.layers[0];
        layer.pixels.set16(3, 4, [12347, 23459, 34571, 61379]);
        layer.color = [20, 40, 60, 200];
        layer.effects.push(crate::effects::Effect {
            id: "color".into(),
            kind: "color_overlay".into(),
            enabled: true,
            weight: 1.,
            settings: Arc::new(json!({"color":[20,40,60,200],"mode":"normal","enabled":true})),
        });
        layer.mask = Some(Mask {
            enabled: true,
            linked: true,
            cache_key: "mask".into(),
            steps: vec![MaskStep {
                id: "mask-step".into(),
                kind: "curves".into(),
                enabled: true,
                weight: 1.,
                value: 10.,
                pixels: crate::raster::Raster::new_depth(12, 8, depth),
                settings: json!({"points":[[0.,0.],[1.,1.]],"enabled":true}),
            }],
        });
        before.filters.push(crate::effects::Effect {
            id: "filter".into(),
            kind: "posterize".into(),
            enabled: true,
            weight: 1.,
            settings: Arc::new(json!({"levels":4})),
        });
        let mut after = before.clone();
        after.revision += 1;
        after.layers[0].opacity = 0.2;
        after.layers[0].color = [100, 120, 140, 240];
        after.layers[0].effects[0].weight = 0.2;
        after.layers[0].effects[0].settings =
            Arc::new(json!({"color":[100,120,140,240],"mode":"multiply","enabled":false}));
        let mask = after.layers[0].mask.as_mut().unwrap();
        mask.steps[0].weight = 0.2;
        mask.steps[0].value = 30.;
        mask.steps[0].settings = json!({"points":[[0.,0.4],[1.,0.8]],"enabled":false});
        after.filters[0].weight = 0.2;
        after.filters[0].settings = Arc::new(json!({"levels":8}));
        let stage = |doc: &Document| {
            let staged =
                CapturedScene::from_parts(doc.clone(), "controls".into(), vec![], None, None)
                    .stage(&runtime, &cache, &|| Ok(()))
                    .unwrap();
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap()
        };
        let native_before = stage(&before);
        let native_after = stage(&after);
        let replacement = stage(&after);
        let original = serde_json::to_value(native_after.metadata()).unwrap();
        let key = native_after.key();
        let change: RuntimeValue<AiChange> = AiChange {
            serial: 1,
            actor: "agent".into(),
            label: "Controls".into(),
            tool: "effects".into(),
            scopes: vec![Scope::layer(&after.layers[0].id)],
            at: 0,
        }
        .into();
        let observations: [(DocumentSnapshot, DocumentSnapshot); 2] = [
            (before.clone().into(), after.clone().into()),
            (native_before.clone().into(), native_after.clone().into()),
        ];
        for (old, current) in observations {
            let mut animation = Animation::default();
            animation.observe(&old, None, 0.);
            animation.observe(&current, Some(change.clone()), 1.);
            let view = current.view();
            let layer = view.layer(0).unwrap();
            let effect = layer.effect(0).unwrap();
            let step = layer.mask().unwrap().step(0).unwrap();
            let filter = view.filter(0).unwrap();
            let used = pool.used();
            let numbers = animation.controls(view, 1.18);
            assert_eq!(pool.used(), used);
            assert!((numbers.opacity(layer) - 0.6).abs() < 0.0001);
            assert_eq!(numbers.color(layer), [60, 80, 100, 220]);
            assert!((numbers.effect_weight(Some(layer.id()), effect) - 0.6).abs() < 0.0001);
            assert!((numbers.effect_weight(None, filter) - 0.6).abs() < 0.0001);
            assert!((numbers.mask_weight(layer.id(), step) - 0.6).abs() < 0.0001);
            assert!((numbers.mask_value(layer.id(), step) - 20.).abs() < 0.0001);
            let mut settings = effect.settings().clone();
            numbers.effect_settings(Some(layer.id()), effect, &mut settings);
            assert!((settings["color"][0].as_f64().unwrap() - 60.).abs() < 0.0001);
            assert_eq!(settings["mode"], "multiply");
            assert_eq!(settings["enabled"], false);
            let mut settings = filter.settings().clone();
            numbers.effect_settings(None, filter, &mut settings);
            assert!((settings["levels"].as_f64().unwrap() - 6.).abs() < 0.0001);
            let mut settings = step.settings().clone();
            numbers.mask_settings(layer.id(), step, &mut settings);
            assert!((settings["points"][0][1].as_f64().unwrap() - 0.2).abs() < 0.0001);
            assert_eq!(settings["enabled"], false);
            assert_eq!(animation.controls(view, 2.).opacity(layer), 0.2);
            assert_eq!(animation.controls(view, f64::NAN).opacity(layer), 0.2);
            assert_eq!(animation.controls(&replacement, 1.18).opacity(layer), 0.2);
            assert_eq!(animation.controls(old.view(), 1.18).opacity(layer), 0.2);
            assert_eq!(pool.used(), used);
            let (mut app, ctx) = crate::ui::workspace_tests::fixture(depth);
            app.animation = animation;
            let output = ctx.run(
                egui::RawInput {
                    time: Some(1.18),
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(500., 400.))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        app.document_filter_stack(ui, view);
                        app.layer_footer(ui, view, layer);
                    });
                },
            );
            assert_eq!(output.shapes.iter().filter(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "60%")
            }).count(), 2);
            let mut animation = std::mem::take(&mut app.animation);
            animation.cancel();
            assert_eq!(animation.controls(view, 1.18).opacity(layer), 0.2);
        }
        assert_eq!(native_after.key(), key);
        assert_eq!(
            serde_json::to_value(native_after.metadata()).unwrap(),
            original
        );
        assert_eq!(
            after.layers[0].pixels.get16(3, 4),
            before.layers[0].pixels.get16(3, 4)
        );
        drop(connection);
    }
}

#[test]
fn settings_keep_unselected_payloads_and_replaced_effects_out_of_animation() {
    let mut before = Document::new(2, 2).unwrap();
    before.layers[0].effects.push(crate::effects::Effect {
        id: "texture".into(),
        kind: "texture".into(),
        enabled: true,
        weight: 1.,
        settings: Arc::new(json!({"scale":1.,"bitmap_png":"source-payload"})),
    });
    let mut after = before.clone();
    after.layers[0].effects[0].settings =
        Arc::new(json!({"scale":3.,"bitmap_png":"new-source-payload"}));
    let numbers = Controls {
        before: Some(before.into()),
        t: 0.5,
    };
    let layer = DocumentView::from(&after).layer(0).unwrap();
    let effect = layer.effect(0).unwrap();
    let mut display = json!({"scale":3.});
    numbers.effect_settings(Some(layer.id()), effect, &mut display);
    assert_eq!(display, json!({"scale":2.}));
    after.layers[0].effects[0].kind = "blur".into();
    after.layers[0].effects[0].weight = 0.2;
    let effect = DocumentView::from(&after)
        .layer(0)
        .unwrap()
        .effect(0)
        .unwrap();
    assert_eq!(
        numbers.effect_weight(Some(&after.layers[0].id), effect),
        0.2
    );
}
