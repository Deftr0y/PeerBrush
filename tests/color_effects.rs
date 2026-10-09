use peerbrush::{
    effects::{self, Image},
    engine::{Document, Engine, Layer},
    psd,
};
use serde_json::{json, Value};
use std::sync::Arc;

fn image(pixels: &[[u8; 4]]) -> Image {
    Image {
        width: pixels.len() as u32,
        height: 1,
        bytes: pixels.iter().flatten().copied().collect(),
    }
}
fn fixture() -> (Engine, String) {
    let mut e = Engine::new();
    e.doc = Document::new(32, 16).unwrap();
    let l = &mut e.doc.layers[0];
    for y in 0..16 {
        for x in 0..32 {
            l.pixels.set(
                x,
                y,
                if x < 16 {
                    [32, 64, 96, 200]
                } else {
                    [255, 240, 210, 255]
                },
            );
        }
    }
    let id = l.id.clone();
    (e, id)
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], None, None, "effect").unwrap();
}
fn render(e: &Engine) -> Vec<u8> {
    e.doc.preview(None, 32, None, false).unwrap().2
}
fn cached(e: &Engine) -> Arc<Image> {
    effects::prepare(&e.doc, &vec![None; e.doc.layers.len()]).unwrap()[0]
        .as_ref()
        .unwrap()
        .clone()
}

#[test]
fn hsl_rotates_primary_hues_desaturates_and_keeps_alpha() {
    let mut im = image(&[[255, 0, 0, 77], [0, 255, 0, 255], [10, 20, 30, 0]]);
    effects::apply(&mut im, "hsl", &json!({"hue":120})).unwrap();
    assert_eq!(im.get(0, 0), [0, 255, 0, 77]);
    assert_eq!(im.get(1, 0), [0, 0, 255, 255]);
    assert_eq!(im.get(2, 0), [10, 20, 30, 0]);
    effects::apply(&mut im, "hsl", &json!({"saturation":-1})).unwrap();
    assert_eq!(im.get(0, 0), [128, 128, 128, 77]);
    effects::apply(&mut im, "hsl", &json!({"lightness":1})).unwrap();
    assert_eq!(im.get(0, 0), [255, 255, 255, 77]);
    effects::apply(&mut im, "hsl", &json!({"lightness":-1})).unwrap();
    assert_eq!(im.get(0, 0), [0, 0, 0, 77]);
}

#[test]
fn color_balance_can_cool_shadows_and_warm_highlights_without_recoloring_midtones() {
    let mut im = image(&[[32, 32, 32, 64], [128, 128, 128, 200], [224, 224, 224, 255]]);
    effects::apply(
        &mut im,
        "color_balance",
        &json!({"shadows":[-0.2,0,0.3],"highlights":[0.25,0,-0.2]}),
    )
    .unwrap();
    let shadow = im.get(0, 0);
    let mid = im.get(1, 0);
    let high = im.get(2, 0);
    assert!(shadow[2] > shadow[0]);
    assert!(high[0] > high[2]);
    assert_eq!(mid, [128, 128, 128, 200]);
    for (p, gray, alpha) in [(shadow, 32.0, 64), (high, 224.0, 255)] {
        assert_eq!(p[3], alpha);
        let luma = p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722;
        assert!((luma - gray).abs() < 1.0, "{p:?} {luma}");
    }
    let mut unpreserved = image(&[[224, 224, 224, 255]]);
    effects::apply(
        &mut unpreserved,
        "color_balance",
        &json!({"highlights":[-0.5,-0.5,-0.5],"preserve_luminosity":false}),
    )
    .unwrap();
    assert!(unpreserved.get(0, 0)[0] < 200);
}

#[test]
fn default_effects_are_identity_and_settings_are_safely_normalized() {
    let original = [[34, 84, 218, 1], [255, 0, 12, 255], [7, 8, 9, 0]];
    for kind in ["color_balance", "hsl", "liquify"] {
        let mut im = image(&original);
        effects::apply(&mut im, kind, &effects::defaults(kind)).unwrap();
        assert_eq!(
            im.bytes,
            original.iter().flatten().copied().collect::<Vec<_>>(),
            "{kind}"
        );
    }
    let settings = effects::normalized("bloom", &json!({"strength":0.7})).unwrap();
    assert_eq!(settings["threshold"], json!(0.75));
    assert_eq!(settings["spread"], json!(12.0));
    for (kind, settings) in [
        ("hsl", json!({"hue":181})),
        ("hsl", json!({"saturation":1e300})),
        ("color_balance", json!({"shadows":[1,2,3]})),
        ("color_balance", json!({"midtones":[0,0]})),
        ("color_balance", json!({"preserve_luminosity":"yes"})),
        ("bloom", json!({"spread":65})),
        ("bloom", json!({"threshold":-0.1})),
        ("bloom", json!({"strength":"high"})),
    ] {
        assert!(
            effects::validate(kind, &settings).is_err(),
            "{kind} {settings}"
        );
    }
}

#[test]
fn blur_preserves_faint_color_and_ignores_hidden_rgb_at_transparent_edges() {
    let mut faint = Image {
        width: 9,
        height: 3,
        bytes: [[231, 41, 11, 1]; 27].iter().flatten().copied().collect(),
    };
    let expected = faint.bytes.clone();
    effects::apply(&mut faint, "blur", &json!({"radius":4})).unwrap();
    assert_eq!(faint.bytes, expected);
    let mut edge = Image {
        width: 17,
        height: 9,
        bytes: [[255, 0, 255, 0]; 153].iter().flatten().copied().collect(),
    };
    edge.bytes[(4 * 17 + 8) * 4..(4 * 17 + 8) * 4 + 4].copy_from_slice(&[0, 255, 0, 128]);
    effects::apply(&mut edge, "blur", &json!({"radius":1})).unwrap();
    assert!(edge.get(7, 4)[3] > 0);
    for p in edge.bytes.chunks_exact(4).filter(|p| p[3] > 0) {
        assert_eq!(p[0], 0);
        assert_eq!(p[2], 0);
        assert!(p[1] > 250, "{p:?}");
    }
}

#[test]
fn bloom_threshold_strength_and_alpha_halo_are_real() {
    let mut below = image(&[[128, 128, 128, 255], [255, 255, 255, 0]]);
    let before = below.bytes.clone();
    effects::apply(&mut below, "bloom", &json!({"threshold":0.9})).unwrap();
    assert_eq!(below.bytes, before);
    for settings in [json!({"threshold":1}), json!({"strength":0})] {
        let mut bright = image(&[[255, 255, 255, 200]]);
        let before = bright.bytes.clone();
        effects::apply(&mut bright, "bloom", &settings).unwrap();
        assert_eq!(bright.bytes, before);
    }
    let mut bright = Image {
        width: 17,
        height: 9,
        bytes: vec![0; 17 * 9 * 4],
    };
    bright.bytes[(4 * 17 + 8) * 4..(4 * 17 + 8) * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    effects::apply(
        &mut bright,
        "bloom",
        &json!({"threshold":0.8,"spread":1,"strength":1}),
    )
    .unwrap();
    assert_eq!(bright.get(8, 4), [255, 255, 255, 255]);
    let halo = bright.get(7, 4);
    assert!(halo[3] > 0 && halo[3] < 255);
    assert_eq!(&halo[..3], &[255, 255, 255]);
    let mut color = image(&[[255, 100, 50, 255]]);
    effects::apply(
        &mut color,
        "bloom",
        &json!({"threshold":0,"spread":0,"strength":1}),
    )
    .unwrap();
    assert!(color.get(0, 0)[1] > 100);
    assert_eq!(color.get(0, 0)[3], 255);
}

#[test]
fn effect_preview_equals_commit_revisited_cache_undo_and_editable_psd_sources() {
    for (kind, first, last) in [
        (
            "color_balance",
            json!({"shadows":[0,0,0.3]}),
            json!({"highlights":[0.3,0,-0.3]}),
        ),
        (
            "hsl",
            json!({"hue":45}),
            json!({"hue":-90,"saturation":-0.5}),
        ),
        (
            "bloom",
            json!({"threshold":0.8,"spread":2,"strength":0.5}),
            json!({"threshold":0.5,"spread":4,"strength":1}),
        ),
        (
            "liquify",
            json!({"radius":8,"strength":1,"strokes":[{"mode":"push","points":[[8,8],[12,8]],"radius":8,"strength":0.5}]}),
            json!({"radius":8,"strength":0.3,"strokes":[{"mode":"push","points":[[8,8],[12,8]],"radius":6,"strength":0.8}]}),
        ),
    ] {
        let (mut e, id) = fixture();
        let raw = e.doc.layers[0].pixels.rgba();
        edit(
            &mut e,
            json!({"op":"effect.add","layer":id,"kind":kind,"settings":first}),
        );
        let original = render(&e);
        let first_cache = cached(&e);
        assert!(Arc::ptr_eq(&first_cache, &cached(&e)));
        let effect = e.doc.layers[0].effects[0].id.clone();
        // Another effect/layer edit does not replace this source or prevent revisiting its controls.
        edit(
            &mut e,
            json!({"op":"layer.add","kind":"paint","name":"Another layer"}),
        );
        let command = json!({"op":"effect.update","layer":id,"effect":effect,"settings":last});
        let revision = e.doc.revision;
        let history = e.undo.len();
        let draft = Engine::preview_edits(e.doc.clone(), &[command.clone()]).unwrap();
        let preview = draft.preview(None, 32, None, false).unwrap().2;
        assert_eq!(e.doc.revision, revision);
        assert_eq!(e.undo.len(), history);
        edit(&mut e, command);
        assert_eq!(render(&e), preview, "{kind}");
        let changed = render(&e);
        assert_ne!(original, changed, "{kind}");
        let layer = e.doc.layers.iter().find(|l| l.id == id).unwrap();
        assert_eq!(layer.pixels.rgba(), raw);
        let bytes = psd::encode(&e.doc).unwrap();
        let loaded = psd::decode(&bytes).unwrap();
        assert!(!loaded.read_only);
        assert_eq!(loaded.preview(None, 32, None, false).unwrap().2, changed);
        assert_eq!(
            loaded.layers.iter().find(|l| l.id == id).unwrap().effects[0].settings,
            layer.effects[0].settings
        );
        e.undo("human").unwrap();
        assert_eq!(render(&e), original);
        e.redo("human").unwrap();
        assert_eq!(render(&e), changed);
    }
}

#[test]
fn invalid_effects_and_color_only_mask_requests_roll_back_atomically() {
    let (mut e, id) = fixture();
    let before = render(&e);
    for kind in ["color_balance", "hsl", "bloom"] {
        let revision = e.doc.revision;
        let history = e.undo.len();
        assert!(e
            .edit(
                "human",
                &[
                    json!({"op":"mask.add","layer":id}),
                    json!({"op":"mask.step.add","layer":id,"kind":kind})
                ],
                None,
                None,
                "invalid mask"
            )
            .is_err());
        assert_eq!(e.doc.revision, revision);
        assert_eq!(e.undo.len(), history);
        assert!(e.doc.layers[0].mask.is_none());
    }
    assert!(e
        .edit(
            "human",
            &[
                json!({"op":"effect.add","layer":id,"kind":"hsl","settings":{"hue":90}}),
                json!({"op":"effect.add","layer":id,"kind":"bloom","settings":{"strength":4}})
            ],
            None,
            None,
            "invalid effect"
        )
        .is_err());
    assert!(e.doc.layers[0].effects.is_empty());
    assert!(e.undo.is_empty());
    assert_eq!(render(&e), before);
}

#[test]
fn effects_enforce_derived_and_transient_budgets_before_rendering() {
    let mut doc = Document::new(8192, 4096).unwrap();
    doc.layers[0].effects.push(effects::Effect {
        weight: 1.0,
        id: "large".into(),
        kind: "blur".into(),
        enabled: true,
        settings: json!({"radius":1}),
    });
    assert!(effects::validate_budget(&doc).is_err());
    doc.layers[0].effects[0].kind = "hsl".into();
    doc.layers[0].effects[0].settings = effects::defaults("hsl");
    assert!(effects::validate_budget(&doc).is_ok());
    for _ in 0..2 {
        let mut layer = Layer::new("large", "paint", 8192, 4096);
        layer.effects = doc.layers[0].effects.clone();
        doc.layers.push(layer);
    }
    assert!(effects::validate_budget(&doc).is_err());
    let mut malformed = Image {
        width: 2,
        height: 2,
        bytes: vec![0; 4],
    };
    assert!(effects::apply(&mut malformed, "hsl", &json!({})).is_err());
}
