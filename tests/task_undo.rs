use peerbrush::{
    engine::{Document, Engine, Scope},
    psd, raster, server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn fixture(depth: u16) -> (Engine, String, String) {
    let mut e = Engine::new();
    e.doc = Document::new_depth(16, 12, depth).unwrap();
    let layer = e.doc.layers[0].id.clone();
    e.doc.layers[0].pixels.set16(14, 10, [123, 456, 789, 0]);
    let task = e.reserve("agent", "Retouch", vec![]).unwrap().id;
    (e, layer, task)
}
fn edit(e: &mut Engine, actor: &str, task: Option<&str>, commands: Vec<Value>) {
    e.edit(actor, &commands, Some(e.doc.revision), task, "Retouch")
        .unwrap();
}
fn patch(
    e: &mut Engine,
    actor: &str,
    task: Option<&str>,
    layer: &str,
    x: i32,
    y: i32,
    words: [u16; 4],
) {
    let path = std::env::temp_dir().join(format!("pb-task-{}.png", peerbrush::engine::id()));
    std::fs::write(&path, raster::png16(1, 1, &words).unwrap()).unwrap();
    edit(
        e,
        actor,
        task,
        vec![
            json!({"op":"image.patch","layer":layer,"path":path,"x":x,"y":y,"rect":[x,y,x+1,y+1]}),
        ],
    );
    std::fs::remove_file(path).unwrap();
}
fn source(e: &Engine) -> Value {
    let mut v = serde_json::to_value(&e.doc).unwrap();
    v.as_object_mut().unwrap().remove("revision");
    v
}

#[test]
fn interleaved_pixels_and_metadata_survive_native_task_undo_and_compensation_history() {
    for depth in [8, 16] {
        let (mut e, layer, task) = fixture(depth);
        let baseline = e.doc.layers[0].pixels.clone();
        patch(
            &mut e,
            "agent",
            Some(&task),
            &layer,
            2,
            2,
            [4567, 13245, 27789, 65535],
        );
        patch(
            &mut e,
            "human",
            None,
            &layer,
            3,
            2,
            [1911, 21387, 47813, 65535],
        );
        let human = e.doc.layers[0].pixels.get16(3, 2);
        edit(
            &mut e,
            "human",
            None,
            vec![json!({"op":"layer.update","layer":layer,"opacity":0.61,"name":"Human name"})],
        );
        patch(
            &mut e,
            "agent",
            Some(&task),
            &layer,
            5,
            2,
            [19871, 42137, 63341, 65535],
        );
        let edited = source(&e);
        let history = e.undo.len();
        let review = e.inspect_task_undo("human", "agent", &task).unwrap();
        assert_eq!(review["can_undo"], true);
        assert_eq!(review["batches"].as_array().unwrap().len(), 2);
        assert!(review["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["rect"] != Value::Null));
        e.undo_task("human", "agent", &task).unwrap();
        assert_eq!(e.undo.len(), history + 1);
        assert_eq!(e.doc.layers[0].pixels.get16(2, 2), baseline.get16(2, 2));
        assert_eq!(e.doc.layers[0].pixels.get16(5, 2), baseline.get16(5, 2));
        assert_eq!(e.doc.layers[0].pixels.get16(3, 2), human);
        assert_eq!(e.doc.layers[0].pixels.get16(14, 10), baseline.get16(14, 10));
        assert_eq!(e.doc.layers[0].name, "Human name");
        assert_eq!(e.doc.layers[0].opacity, 0.61);
        let reverted = source(&e);
        let png = e.doc.export_png().unwrap();
        let saved = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        assert!(!saved.read_only);
        assert_eq!(saved.export_png().unwrap(), png);
        assert_eq!(e.task_history()[0]["active_batches"], 0);
        assert!(e.undo_task("agent", "agent", &task).is_err());
        e.undo("human").unwrap();
        assert_eq!(source(&e), edited);
        assert_eq!(e.task_history()[0]["active_batches"], 2);
        e.redo("human").unwrap();
        assert_eq!(source(&e), reverted);
        assert_eq!(e.task_history()[0]["active_batches"], 0);
        e.undo("human").unwrap();
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers[0].pixels.get16(3, 2), human);
        assert_eq!(e.doc.layers[0].opacity, 0.61);
    }
}

#[test]
fn overlapping_pixel_and_setting_conflicts_are_precise_atomic_and_owner_checked() {
    let (mut e, layer, task) = fixture(16);
    patch(
        &mut e,
        "agent",
        Some(&task),
        &layer,
        2,
        2,
        [4567, 13245, 27789, 65535],
    );
    patch(
        &mut e,
        "human",
        None,
        &layer,
        2,
        2,
        [1911, 21387, 47813, 65535],
    );
    patch(
        &mut e,
        "agent",
        Some(&task),
        &layer,
        5,
        2,
        [19871, 42137, 63341, 65535],
    );
    let before = source(&e);
    let rev = e.doc.revision;
    let h = e.undo.len();
    let review = e.inspect_task_undo("human", "agent", &task).unwrap();
    assert_eq!(review["can_undo"], false);
    assert!(review["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["field"] == "pixels"
            && v["target"] == layer
            && v["rect"] == json!([2, 2, 3, 3])));
    assert!(e.undo_task("human", "agent", &task).is_err());
    assert_eq!(source(&e), before);
    assert_eq!(e.doc.revision, rev);
    assert_eq!(e.undo.len(), h);
    assert!(e
        .undo_task("other-agent", "agent", &task)
        .unwrap_err()
        .contains("own tasks"));
}

#[test]
fn separate_effect_settings_merge_but_later_edits_to_created_sources_conflict() {
    let (mut e, layer, task) = fixture(8);
    edit(
        &mut e,
        "human",
        None,
        vec![json!({"op":"effect.add","layer":layer,"kind":"levels"})],
    );
    let effect = e.doc.layers[0].effects[0].id.clone();
    edit(
        &mut e,
        "agent",
        Some(&task),
        vec![json!({"op":"effect.update","layer":layer,"effect":effect,"settings":{"gamma":1.7}})],
    );
    edit(
        &mut e,
        "human",
        None,
        vec![
            json!({"op":"effect.update","layer":layer,"effect":effect,"settings":{"black":0,"white":0.8,"gamma":1.7}}),
        ],
    );
    e.undo_task("human", "agent", &task).unwrap();
    assert_eq!(e.doc.layers[0].effects[0].settings["gamma"], 1.0);
    assert_eq!(e.doc.layers[0].effects[0].settings["white"], 0.8);
    let task2 = e.reserve("agent", "New blur", vec![]).unwrap().id;
    edit(
        &mut e,
        "agent",
        Some(&task2),
        vec![json!({"op":"effect.add","layer":layer,"kind":"blur"})],
    );
    let effect = e.doc.layers[0].effects.last().unwrap().id.clone();
    edit(
        &mut e,
        "human",
        None,
        vec![json!({"op":"effect.update","layer":layer,"effect":effect,"settings":{"radius":12}})],
    );
    let before = source(&e);
    assert!(e.undo_task("human", "agent", &task2).is_err());
    assert_eq!(source(&e), before);
}

#[test]
fn masks_selection_and_removed_layers_keep_coupled_sources_and_later_work() {
    let (mut e, layer, task) = fixture(16);
    edit(
        &mut e,
        "human",
        None,
        vec![json!({"op":"mask.add","layer":layer})],
    );
    let baseline = e.doc.layers[0].mask.clone();
    edit(
        &mut e,
        "agent",
        Some(&task),
        vec![
            json!({"op":"paint","layer":layer,"mask":true,"points":[[2,2],[2.1,2]],"radius":1,"color":[0,0,0,255]}),
        ],
    );
    edit(
        &mut e,
        "human",
        None,
        vec![
            json!({"op":"paint","layer":layer,"mask":true,"points":[[9,8],[9.1,8]],"radius":1,"color":[0,0,0,255]}),
            json!({"op":"selection","rect":[8,7,12,11]}),
        ],
    );
    let human = e.doc.layers[0].mask_value(9, 8);
    assert_eq!(
        baseline.as_ref().unwrap().steps.len(),
        e.doc.layers[0].mask.as_ref().unwrap().steps.len()
    );
    e.undo_task("human", "agent", &task).unwrap();
    assert_eq!(e.doc.layers[0].mask_value(9, 8), human);
    assert_eq!(e.doc.layers[0].mask_value(2, 2), 1.0);
    assert_eq!(e.doc.selection, Some([8, 7, 12, 11]));
    let task2 = e.reserve("agent", "Delete layer", vec![]).unwrap().id;
    edit(
        &mut e,
        "human",
        None,
        vec![json!({"op":"layer.add","name":"Keep","kind":"paint"})],
    );
    let old = e.doc.layers.iter().find(|l| l.id == layer).unwrap().clone();
    edit(
        &mut e,
        "agent",
        Some(&task2),
        vec![json!({"op":"layer.delete","layer":layer})],
    );
    let keep = e.doc.layers[0].id.clone();
    edit(
        &mut e,
        "human",
        None,
        vec![json!({"op":"layer.update","layer":keep,"name":"Later work"})],
    );
    e.undo_task("human", "agent", &task2).unwrap();
    assert!(e.doc.layers.iter().any(|l| l.name == "Later work"));
    let restored = e.doc.layers.iter().find(|l| l.id == layer).unwrap();
    assert_eq!(restored.pixels, old.pixels);
    assert_eq!(restored.mask_value(9, 8), human);
    psd::validate(&e.doc).unwrap();
}

#[test]
fn structural_conflicts_reservations_and_truncated_history_refuse_partial_task_undo() {
    let (mut e, layer, task) = fixture(8);
    patch(
        &mut e,
        "agent",
        Some(&task),
        &layer,
        2,
        2,
        [4567, 13245, 27789, 65535],
    );
    let lease = e
        .reserve(
            "human",
            "Protect other pixel",
            vec![Scope {
                target: Some(layer.clone()),
                rect: Some([8, 8, 9, 9]),
            }],
        )
        .unwrap();
    assert_eq!(
        e.inspect_task_undo("agent", "agent", &task).unwrap()["can_undo"],
        true
    );
    e.leases.retain(|l| l.id != lease.id);
    e.reserve(
        "human",
        "Protect edited pixel",
        vec![Scope {
            target: Some(layer.clone()),
            rect: Some([2, 2, 3, 3]),
        }],
    )
    .unwrap();
    assert_eq!(
        e.inspect_task_undo("agent", "agent", &task).unwrap()["can_undo"],
        false
    );
    assert!(e.undo_task("agent", "agent", &task).is_err());
    e.leases.clear();
    for i in 0..41 {
        edit(
            &mut e,
            "human",
            None,
            vec![json!({"op":"layer.update","layer":layer,"name":format!("{i}")})],
        );
    }
    assert!(e
        .undo_task("human", "agent", &task)
        .unwrap_err()
        .contains("partial"));
    let (mut e, _, task) = fixture(8);
    edit(
        &mut e,
        "agent",
        Some(&task),
        vec![json!({"op":"layer.add","name":"AI folder","kind":"group"})],
    );
    let folder = e
        .doc
        .layers
        .iter()
        .find(|l| l.name == "AI folder")
        .unwrap()
        .id
        .clone();
    edit(
        &mut e,
        "human",
        None,
        vec![json!({"op":"layer.add","name":"Human child","kind":"paint","parent":folder})],
    );
    let before = source(&e);
    assert!(e.undo_task("human", "agent", &task).is_err());
    assert_eq!(source(&e), before);
}

#[test]
fn protocol_inspection_revision_guards_feedback_and_actor_isolation() {
    let (mut e, layer, task) = fixture(16);
    patch(
        &mut e,
        "agent",
        Some(&task),
        &layer,
        2,
        2,
        [4567, 13245, 27789, 65535],
    );
    patch(
        &mut e,
        "human",
        None,
        &layer,
        9,
        8,
        [1337, 45577, 21491, 65535],
    );
    let shared = Arc::new(Mutex::new(e));
    let review = server::dispatch(
        &shared,
        "history",
        &json!({"action":"inspect_task","actor":"agent","task":task}),
    )
    .unwrap();
    assert_eq!(review["can_undo"], true);
    assert!(server::dispatch(
        &shared,
        "history",
        &json!({"action":"undo_task","actor":"agent","task":task})
    )
    .is_err());
    assert!(server::dispatch(
        &shared,
        "history",
        &json!({"action":"undo_task","actor":"agent","task":task,"expected_revision":1})
    )
    .is_err());
    let result=server::dispatch(&shared,"history",&json!({"action":"undo_task","actor":"agent","task":task,"expected_revision":2,"max_edge":32})).unwrap();
    assert_eq!(result["revision"], 3);
    assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 16, 12]));
    assert_eq!(result["tasks"][0]["active_batches"], 0);
    assert!(shared.lock().unwrap().ai_change.as_ref().unwrap().tool == "history");
    server::dispatch(
        &shared,
        "history",
        &json!({"action":"undo","actor":"agent","expected_revision":3,"feedback":"request"}),
    )
    .unwrap();
    assert_eq!(
        shared.lock().unwrap().doc.layers[0].pixels.get16(9, 8),
        [1337, 45577, 21491, 65535]
    );
}

#[test]
fn later_moves_reserve_the_actual_inverse_region_and_visibility_stays_independent() {
    let (mut e, layer, task) = fixture(16);
    patch(
        &mut e,
        "agent",
        Some(&task),
        &layer,
        2,
        2,
        [4567, 13245, 27789, 65535],
    );
    edit(
        &mut e,
        "human",
        None,
        vec![json!({"op":"move","layer":layer,"dx":20,"dy":10})],
    );
    let review = e.inspect_task_undo("agent", "agent", &task).unwrap();
    assert_eq!(review["scopes"][0]["rect"], json!([22, 12, 23, 13]));
    e.reserve(
        "human",
        "Moved source",
        vec![Scope {
            target: Some(layer.clone()),
            rect: Some([22, 12, 23, 13]),
        }],
    )
    .unwrap();
    assert!(e.undo_task("agent", "agent", &task).is_err());
    e.leases.retain(|l| l.owner != "human");
    e.undo_task("agent", "agent", &task).unwrap();
    assert_eq!((e.doc.layers[0].x, e.doc.layers[0].y), (20, 10));
    assert_eq!(e.doc.layers[0].pixels.get16(2, 2), [0; 4]);
    let task2 = e.reserve("agent", "Visibility", vec![]).unwrap().id;
    edit(
        &mut e,
        "agent",
        Some(&task2),
        vec![json!({"op":"layer.update","layer":layer,"visible":false})],
    );
    e.reserve("human", "Pixels", vec![Scope::layer(&layer)])
        .unwrap();
    assert_eq!(
        e.inspect_task_undo("agent", "agent", &task2).unwrap()["can_undo"],
        true
    );
    e.undo_task("agent", "agent", &task2).unwrap();
    assert!(e.doc.layers[0].visible);
}

#[test]
fn completed_compensation_allows_new_batches_on_same_task_and_keeps_other_tasks() {
    let (mut e, layer, task) = fixture(16);
    patch(
        &mut e,
        "agent",
        Some(&task),
        &layer,
        2,
        2,
        [4567, 13245, 27789, 65535],
    );
    let other = e.reserve("other", "Other task", vec![]).unwrap().id;
    patch(
        &mut e,
        "other",
        Some(&other),
        &layer,
        9,
        8,
        [1337, 45577, 21491, 65535],
    );
    e.undo_task("human", "agent", &task).unwrap();
    patch(
        &mut e,
        "agent",
        Some(&task),
        &layer,
        5,
        2,
        [19871, 42137, 63341, 65535],
    );
    assert_eq!(
        e.inspect_task_undo("agent", "agent", &task).unwrap()["batches"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    e.undo_task("human", "agent", &task).unwrap();
    assert_eq!(
        e.doc.layers[0].pixels.get16(9, 8),
        [1337, 45577, 21491, 65535]
    );
    e.undo_task("human", "other", &other).unwrap();
    assert_eq!(e.doc.layers[0].pixels.get16(9, 8), [0; 4]);
    for i in 0..41 {
        edit(
            &mut e,
            "human",
            None,
            vec![json!({"op":"layer.update","layer":layer,"name":format!("{i}")})],
        );
    }
    patch(
        &mut e,
        "agent",
        Some(&task),
        &layer,
        4,
        4,
        [19871, 42137, 63341, 65535],
    );
    e.undo_task("human", "agent", &task).unwrap();
    assert_eq!(e.doc.layers[0].pixels.get16(4, 4), [0; 4]);
}

#[test]
fn project_frame_inverse_cannot_crop_or_quantize_later_human_work() {
    let (mut e, layer, task) = fixture(8);
    edit(
        &mut e,
        "agent",
        Some(&task),
        vec![json!({"op":"document.settings","width":32,"height":24,"bit_depth":16})],
    );
    patch(
        &mut e,
        "human",
        None,
        &layer,
        4,
        4,
        [19871, 42137, 63341, 65535],
    );
    let before = source(&e);
    let review = e.inspect_task_undo("human", "agent", &task).unwrap();
    assert_eq!(review["can_undo"], false);
    assert_eq!(review["conflicts"][0]["field"], "document.frame");
    assert!(e.undo_task("human", "agent", &task).is_err());
    assert_eq!(source(&e), before);
    assert_eq!(
        e.doc.layers[0].pixels.get16(4, 4),
        [19871, 42137, 63341, 65535]
    );
}
