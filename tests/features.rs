use peerbrush::{
    effects,
    engine::{Document, Engine},
    psd,
};
use serde_json::{json, Value};
use std::sync::Arc;
fn fixture() -> (Engine, String) {
    let mut e = Engine::new();
    e.doc = Document::new(64, 48).unwrap();
    let id = e.doc.layers[0].id.clone();
    (e, id)
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], None, None, "test").unwrap();
}
fn preview(e: &Engine) -> Vec<u8> {
    e.doc.preview(None, 64, None, false).unwrap().2
}
#[test]
fn brush_opacity_caps_a_stroke_flow_builds_and_input_density_does_not_change_spacing() {
    let (mut e, id) = fixture();
    let c = json!({"op":"paint","layer":id,"points":[[10,20],[50,20]],"radius":6,"opacity":0.4,"flow":0.3,"hardness":1.0,"spacing":0.1,"color":[230,90,20,255]});
    edit(&mut e, c.clone());
    let p = e.doc.layers[0].pixels.get(30, 20);
    assert!(p[3] > 80 && p[3] <= 102);
    let expected = e.doc.layers[0].pixels.rgba();
    let (mut other, other_id) = fixture();
    let mut c = c;
    c["layer"] = json!(other_id);
    c["points"] = json!([[10, 20], [20, 20], [30, 20], [40, 20], [50, 20]]);
    edit(&mut other, c);
    assert_eq!(expected, other.doc.layers[0].pixels.rgba());
    edit(
        &mut e,
        json!({"op":"paint","layer":id,"points":[[30,20]],"radius":3,"opacity":0.5,"erase":true}),
    );
    assert!(e.doc.layers[0].pixels.get(30, 20)[3] < p[3]);
}
#[test]
fn elliptical_tip_angle_and_softness_are_real_shared_brush_parameters() {
    let (mut e, id) = fixture();
    edit(
        &mut e,
        json!({"op":"paint","layer":id,"points":[[30,24]],"radius":10,"roundness":0.2,"angle":90,"hardness":1,"color":[255,255,255,255]}),
    );
    assert!(e.doc.layers[0].pixels.get(30, 31)[3] > 200);
    assert_eq!(e.doc.layers[0].pixels.get(37, 24)[3], 0);
    let before = e.doc.revision;
    assert!(e
        .edit(
            "ai",
            &[json!({"op":"paint","layer":id,"points":[[1,1]],"hardness":1e300})],
            None,
            None,
            "invalid"
        )
        .is_err());
    assert_eq!(before, e.doc.revision);
}
#[test]
fn fill_shape_and_gradient_target_mask_without_changing_color_and_respect_selection() {
    for op in ["fill", "shape", "gradient"] {
        let (mut e, id) = fixture();
        edit(
            &mut e,
            json!({"op":"fill","layer":id,"color":[240,80,30,255]}),
        );
        edit(&mut e, json!({"op":"mask.add","layer":id,"value":255}));
        e.doc.selection = Some([10, 10, 20, 20]);
        let source = e.doc.layers[0].pixels.rgba();
        edit(
            &mut e,
            json!({"op":op,"layer":id,"mask":true,"rect":[0,0,30,30],"color":[0,0,0,255]}),
        );
        assert_eq!(source, e.doc.layers[0].pixels.rgba());
        assert!(e.doc.layers[0].mask_value(12, 12) < 0.6);
        assert_eq!(e.doc.layers[0].mask_value(2, 2), 1.0);
    }
}
#[test]
fn mask_feather_cache_is_reused_invalidated_and_undoable_and_source_psd_roundtrips() {
    let (mut e, id) = fixture();
    edit(&mut e, json!({"op":"mask.add","layer":id,"value":0}));
    edit(
        &mut e,
        json!({"op":"shape","layer":id,"mask":true,"rect":[15,8,40,40],"color":[255,255,255,255]}),
    );
    let source = e.doc.layers[0].mask.as_ref().unwrap().steps[1]
        .pixels
        .rgba();
    edit(
        &mut e,
        json!({"op":"mask.step.add","layer":id,"kind":"blur","value":9}),
    );
    let m = e.doc.layers[0].mask.as_ref().unwrap();
    let a = m.prepare(64, 48).unwrap();
    let b = m.prepare(64, 48).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert!(a.value(14, 24) > 0.1 && a.value(14, 24) < 0.5);
    assert_eq!(source, m.steps[1].pixels.rgba());
    let step = m.steps.last().unwrap().id.clone();
    edit(
        &mut e,
        json!({"op":"mask.step.update","layer":id,"step":step,"value":2}),
    );
    let changed = e.doc.layers[0]
        .mask
        .as_ref()
        .unwrap()
        .prepare(64, 48)
        .unwrap();
    assert!(!Arc::ptr_eq(&a, &changed));
    e.undo("human").unwrap();
    assert!(Arc::ptr_eq(
        &a,
        &e.doc.layers[0]
            .mask
            .as_ref()
            .unwrap()
            .prepare(64, 48)
            .unwrap()
    ));
    e.redo("human").unwrap();
    let saved = psd::encode(&e.doc).unwrap();
    let loaded = psd::decode(&saved).unwrap();
    assert!(!loaded.read_only);
    assert_eq!(
        loaded.layers[0]
            .mask
            .as_ref()
            .unwrap()
            .steps
            .last()
            .unwrap()
            .value,
        2.0
    );
    assert_eq!(
        loaded.preview(None, 64, Some(&id), true).unwrap().2,
        e.doc.preview(None, 64, Some(&id), true).unwrap().2
    );
}
#[test]
fn effect_budgets_and_invalid_settings_roll_back_atomically() {
    let (mut e, id) = fixture();
    assert!(e
        .edit(
            "human",
            &[
                json!({"op":"mask.add","layer":id}),
                json!({"op":"mask.step.add","layer":id,"kind":"blur","value":1e300})
            ],
            None,
            None,
            "invalid"
        )
        .is_err());
    assert!(e.doc.layers[0].mask.is_none());
    assert!(e.undo.is_empty());
    assert!(effects::validate("curves", &json!({"points":[[0.5,0.2],[0.1,0.7]]})).is_err());
    e.doc = Document::new(4096, 4096).unwrap();
    let commands = (0..9)
        .flat_map(|_| [json!({"op":"layer.add","kind":"paint"})])
        .collect::<Vec<_>>();
    e.edit("human", &commands, None, None, "layers").unwrap();
    let before = e.doc.revision;
    let commands = e
        .doc
        .layers
        .iter()
        .take(9)
        .flat_map(|l| {
            [
                json!({"op":"mask.add","layer":l.id}),
                json!({"op":"mask.step.add","layer":l.id,"kind":"blur","value":8}),
            ]
        })
        .collect::<Vec<_>>();
    assert!(e
        .edit("human", &commands, None, None, "over budget")
        .is_err());
    assert_eq!(before, e.doc.revision);
    assert!(e.doc.layers.iter().all(|l| l.mask.is_none()));
}
#[test]
fn slider_gesture_is_one_undo_step_but_interleaved_agent_edits_break_coalescing() {
    let (mut e, id) = fixture();
    for value in [0.9, 0.8, 0.7] {
        e.edit_with_gesture(
            "human",
            &[json!({"op":"layer.update","layer":id,"opacity":value})],
            None,
            None,
            "opacity",
            Some("drag"),
        )
        .unwrap();
    }
    assert_eq!(e.undo.len(), 1);
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers[0].opacity, 1.0);
    e.redo("human").unwrap();
    edit(
        &mut e,
        json!({"op":"layer.add","kind":"paint","name":"AI layer"}),
    );
    let other = e.doc.layers[0].id.clone();
    e.edit(
        "ai",
        &[json!({"op":"layer.update","layer":other,"name":"AI work"})],
        None,
        None,
        "ai",
    )
    .unwrap();
    e.edit_with_gesture(
        "human",
        &[json!({"op":"layer.update","layer":id,"opacity":0.5})],
        None,
        None,
        "opacity",
        Some("drag"),
    )
    .unwrap();
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers[0].name, "AI work");
    assert_eq!(e.doc.layers[1].opacity, 0.7);
}
#[test]
fn editable_color_effects_on_groups_and_masks_preserve_sources_and_revisit_parameters() {
    let (mut e, id) = fixture();
    edit(
        &mut e,
        json!({"op":"fill","layer":id,"color":[100,120,160,255]}),
    );
    let raw = e.doc.layers[0].pixels.rgba();
    edit(
        &mut e,
        json!({"op":"layer.add","kind":"group","name":"Folder"}),
    );
    let group = e.doc.layers[0].id.clone();
    edit(
        &mut e,
        json!({"op":"layer.parent","layer":id,"parent":group}),
    );
    edit(
        &mut e,
        json!({"op":"effect.add","layer":group,"kind":"curves","settings":{"points":[[0,0],[0.5,0.8],[1,1]]}}),
    );
    let first = preview(&e);
    assert!(first[0] > 100);
    let effect = e.doc.layers[0].effects[0].id.clone();
    edit(
        &mut e,
        json!({"op":"effect.add","layer":id,"kind":"blur","settings":{"radius":3}}),
    );
    edit(
        &mut e,
        json!({"op":"effect.update","layer":group,"effect":effect,"settings":{"points":[[0,0],[0.5,0.2],[1,1]]}}),
    );
    assert!(preview(&e)[0] < first[0]);
    edit(&mut e, json!({"op":"mask.add","layer":group,"value":128}));
    edit(
        &mut e,
        json!({"op":"mask.step.add","layer":group,"kind":"curves","settings":{"points":[[0,0],[0.5,0.8],[1,1]]}}),
    );
    assert!(e.doc.layers[0].mask_value(10, 10) > 0.7);
    assert_eq!(e.doc.layers[1].pixels.rgba(), raw);
    let saved = psd::encode(&e.doc).unwrap();
    let loaded = psd::decode(&saved).unwrap();
    assert!(!loaded.read_only);
    assert_eq!(
        loaded.layers[0].effects[0].settings,
        e.doc.layers[0].effects[0].settings
    );
    assert_eq!(
        loaded.preview(None, 64, None, false).unwrap().2,
        preview(&e)
    );
}
#[test]
fn live_preview_matches_committed_brush_transform_blend_and_reorder_without_history() {
    let (mut e, id) = fixture();
    edit(
        &mut e,
        json!({"op":"fill","layer":id,"color":[30,80,180,255]}),
    );
    edit(&mut e, json!({"op":"layer.add","kind":"paint"}));
    let top = e.doc.layers[0].id.clone();
    let paint = json!({"op":"paint","layer":top,"points":[[10,10],[40,30]],"radius":8,"hardness":0.2,"opacity":0.6,"flow":0.4,"roundness":0.4,"angle":35,"color":[250,100,30,255]});
    for command in [
        paint,
        json!({"op":"move","layer":top,"dx":7,"dy":4}),
        json!({"op":"layer.update","layer":top,"blend":"multiply"}),
        json!({"op":"layer.reorder","layer":top,"index":1}),
    ] {
        let revision = e.doc.revision;
        let history = e.undo.len();
        let draft = Engine::preview_edits(e.doc.clone(), &[command.clone()]).unwrap();
        let pixels = draft.preview(None, 64, None, false).unwrap().2;
        assert_eq!(e.doc.revision, revision);
        assert_eq!(e.undo.len(), history);
        edit(&mut e, command);
        assert_eq!(pixels, preview(&e));
    }
}
#[test]
fn selection_transforms_move_actual_selected_pixels_and_preserve_the_remainder() {
    let (mut e, id) = fixture();
    edit(
        &mut e,
        json!({"op":"shape","layer":id,"rect":[4,4,12,12],"color":[250,80,30,255]}),
    );
    edit(
        &mut e,
        json!({"op":"shape","layer":id,"rect":[40,30,45,35],"color":[30,80,250,255]}),
    );
    e.doc.selection = Some([4, 4, 12, 12]);
    edit(&mut e, json!({"op":"move","layer":id,"dx":10,"dy":0}));
    assert_eq!(e.doc.layers[0].pixels.get(6, 6)[3], 0);
    assert_eq!(e.doc.layers[0].pixels.get(16, 6), [250, 80, 30, 255]);
    assert_eq!(e.doc.layers[0].pixels.get(41, 31), [30, 80, 250, 255]);
    assert_eq!(e.doc.selection, Some([14, 4, 22, 12]));
    edit(
        &mut e,
        json!({"op":"transform","layer":id,"angle":90,"scale_x":1,"scale_y":1,"pivot":[18,8]}),
    );
    assert_eq!(e.doc.layers[0].pixels.get(41, 31), [30, 80, 250, 255]);
    e.undo("human").unwrap();
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers[0].pixels.get(6, 6), [250, 80, 30, 255]);
}
#[test]
fn smart_masks_distinguish_contiguous_and_global_regions_and_support_refinement_undo() {
    let (mut e, id) = fixture();
    edit(
        &mut e,
        json!({"op":"fill","layer":id,"color":[20,30,40,255]}),
    );
    for area in [[3, 3, 12, 12], [30, 3, 40, 12]] {
        edit(
            &mut e,
            json!({"op":"shape","layer":id,"rect":area,"color":[240,100,30,255]}),
        );
    }
    let raw = e.doc.layers[0].pixels.rgba();
    edit(
        &mut e,
        json!({"op":"mask.from_color","layer":id,"point":[5,5],"tolerance":0.02,"contiguous":true}),
    );
    assert_eq!(e.doc.layers[0].mask_value(5, 5), 1.0);
    assert_eq!(e.doc.layers[0].mask_value(32, 5), 0.0);
    edit(
        &mut e,
        json!({"op":"mask.from_color","layer":id,"point":[32,5],"tolerance":0.02,"contiguous":true,"mode":"add"}),
    );
    assert_eq!(e.doc.layers[0].mask_value(32, 5), 1.0);
    assert_eq!(e.doc.layers[0].mask_value(5, 5), 1.0);
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers[0].mask_value(32, 5), 0.0);
    edit(
        &mut e,
        json!({"op":"mask.from_color","layer":id,"point":[5,5],"tolerance":0.02,"contiguous":false}),
    );
    assert_eq!(e.doc.layers[0].mask_value(32, 5), 1.0);
    assert_eq!(raw, e.doc.layers[0].pixels.rgba());
}
