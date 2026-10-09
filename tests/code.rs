use peerbrush::{
    engine::{Document, Engine, Scope},
    server,
};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
static SERIAL: Mutex<()> = Mutex::new(());
fn fixture(depth: u16) -> (server::Shared, String, String, Value) {
    let mut e = Engine::new();
    e.doc = Document::new_depth(40, 32, depth).unwrap();
    let layer = e.doc.layers[0].id.clone();
    if depth == 16 {
        e.doc.layers[0]
            .pixels
            .set16(2, 3, [12347, 23459, 34561, 65535]);
    } else {
        e.doc.layers[0].pixels.set(2, 3, [48, 91, 134, 255]);
    }
    let task = e
        .reserve(
            "code-agent",
            "Procedural palette",
            vec![Scope::layer(&layer)],
        )
        .unwrap()
        .id;
    let params = json!({"action":"start","actor":"code-agent","project_id":e.project_id,"document_id":e.doc.id,"expected_revision":e.doc.revision,"task":task,"description":"Drawing a native palette","scopes":[{"target":layer,"rect":null}]});
    (Arc::new(Mutex::new(e)), layer, task, params)
}
fn start(shared: &server::Shared, p: &Value, script: &str) -> String {
    let mut p = p.clone();
    p["script"] = json!(script);
    server::dispatch(shared, "code", &p).unwrap()["run"]
        .as_str()
        .unwrap()
        .into()
}
fn query(shared: &server::Shared, p: &Value, run: &str, action: &str) -> Value {
    server::dispatch(shared,"code",&json!({"action":action,"actor":"code-agent","project_id":p["project_id"],"document_id":p["document_id"],"run":run})).unwrap()
}
fn done(shared: &server::Shared, p: &Value, run: &str) -> Value {
    let start = Instant::now();
    loop {
        let state = query(shared, p, run, "status");
        if state["status"] != "running" {
            return state;
        }
        assert!(start.elapsed() < Duration::from_secs(35));
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn palette(layer: &str) -> String {
    format!(
        r#"
    let patch=begin_pixels("{layer}",[1,1,9,9]);
    for y in 1..9 {{for x in 1..9 {{
        let p=read_pixel("{layer}",x,y);
        write_pixel(patch,x,y,[document.channel_max-x, p[1], x+y, document.channel_max]);
    }}}}
    commit_pixels(patch);
    edit(#{{op:"effect.add",layer:"{layer}",kind:"posterize",settings:#{{levels:5}},weight:0.6}});
    "#
    )
}
#[test]
fn native_code_pixels_atomic_undo_psd_feedback_and_human_task_compensation() {
    let _serial = SERIAL.lock().unwrap();
    for depth in [8, 16] {
        let (shared, layer, task, p) = fixture(depth);
        let before = shared.lock().unwrap().doc.clone();
        let run = start(&shared, &p, &palette(&layer));
        let state = done(&shared, &p, &run);
        assert_eq!(state["status"], "committed", "{state}");
        let result = &state["result"];
        assert_eq!(result["applied"], 2);
        assert_eq!(state["images"][0]["document_rect"], json!([0, 0, 40, 32]));
        let e = shared.lock().unwrap();
        assert_eq!(e.undo.len(), 1);
        assert_eq!(e.doc.bit_depth, depth);
        let pixel = e.doc.layers[0].pixels.get16(2, 3);
        if depth == 16 {
            assert_eq!(pixel, [65533, 23459, 5, 65535]);
        } else {
            assert_eq!(e.doc.layers[0].pixels.get(2, 3), [253, 91, 5, 255]);
        }
        assert_eq!(
            e.doc.layers[0].pixels.get16(20, 20),
            before.layers[0].pixels.get16(20, 20)
        );
        let saved = peerbrush::psd::encode(&e.doc).unwrap();
        let reopened = peerbrush::psd::decode(&saved).unwrap();
        assert!(!reopened.read_only);
        assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
        assert_eq!(e.ai_change.as_ref().unwrap().actor, "code-agent");
        assert_eq!(
            e.ai_change.as_ref().unwrap().scopes[0],
            Scope::layer(&layer)
        );
        let after = e.doc.export_png().unwrap();
        drop(e);
        let revision = shared.lock().unwrap().doc.revision;
        server::dispatch(&shared,"task",&json!({"action":"end","actor":"code-agent","project_id":p["project_id"],"document_id":p["document_id"],"expected_revision":revision,"task":task})).unwrap();
        let mut e = shared.lock().unwrap();
        e.undo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        e.redo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), after);
        e.edit("human",&[json!({"op":"paint","layer":layer,"points":[[28,24]],"radius":2,"color":[230,80,36,255]})],None,None,"Human detail").unwrap();
        let human = e.doc.layers[0].pixels.get16(28, 24);
        e.undo_task("human", "code-agent", &task).unwrap();
        assert_eq!(
            e.doc.layers[0].pixels.get16(2, 3),
            before.layers[0].pixels.get16(2, 3)
        );
        assert_eq!(e.doc.layers[0].pixels.get16(28, 24), human);
    }
}
#[test]
fn code_scope_escape_failure_and_native_limits_never_commit_partial_work() {
    let _serial = SERIAL.lock().unwrap();
    let scripts = [
        "loop {}".to_owned(),
        "eval(\"1\");".into(),
        "import \"file\" as x;".into(),
        "edit(#{op:\"image.patch\",path:\"private\"});".into(),
        "let a=[1]; loop {a.push(a);}".into(),
    ];
    for script in scripts {
        let (shared, _, _, p) = fixture(16);
        let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
        let run = start(&shared, &p, &script);
        assert_eq!(done(&shared, &p, &run)["status"], "discarded");
        assert_eq!(
            serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
            before
        );
        assert!(shared.lock().unwrap().undo.is_empty());
    }
    let (shared, layer, _, mut p) = fixture(16);
    p["scopes"] = json!([{"target":layer,"rect":[1,1,4,4]}]);
    let script=format!("let b=begin_pixels(\"{layer}\",[1,1,4,4]);write_pixel(b,2,2,[12347,23459,34561,65535]);commit_pixels(b);edit(#{{op:\"paint\",layer:\"{layer}\",points:[[30,24]],radius:2}});");
    let before = shared.lock().unwrap().doc.export_png().unwrap();
    let run = start(&shared, &p, &script);
    let state = done(&shared, &p, &run);
    assert_eq!(state["status"], "discarded");
    assert!(state["error"].as_str().unwrap().contains("scopes"));
    assert_eq!(shared.lock().unwrap().doc.export_png().unwrap(), before);
    for code in [
        format!("let b=begin_pixels(\"{layer}\",[1,1,4,4]);write_pixel(b,4,2,[1,2,3,4]);"),
        format!("let b=begin_pixels(\"{layer}\",[1,1,4,4]);write_pixel(b,2,2,[65536,2,3,4]);"),
        format!("let b=begin_pixels(\"{layer}\",[1,1,4,4]);"),
    ] {
        let run = start(&shared, &p, &code);
        assert_eq!(done(&shared, &p, &run)["status"], "discarded");
        assert_eq!(shared.lock().unwrap().doc.export_png().unwrap(), before);
    }
}
#[test]
fn code_takeover_cancel_and_interleaved_human_changes_preserve_source() {
    let _serial = SERIAL.lock().unwrap();
    for action in ["takeover", "cancel", "human"] {
        let (shared, layer, _, p) = fixture(16);
        let run = start(&shared, &p, "loop {};");
        if action == "cancel" {
            query(&shared, &p, &run, "cancel");
        } else {
            let mut e = shared.lock().unwrap();
            if action == "takeover" {
                e.take_over_tasks();
            } else {
                e.edit(
                    "human",
                    &[json!({"op":"layer.add","name":"Human drawing"})],
                    None,
                    None,
                    "Human work",
                )
                .unwrap();
            }
        }
        let state = done(&shared, &p, &run);
        assert_eq!(state["status"], "discarded");
        let e = shared.lock().unwrap();
        assert_eq!(
            e.doc
                .layers
                .iter()
                .find(|l| l.id == layer)
                .unwrap()
                .pixels
                .get16(2, 3),
            [12347, 23459, 34561, 65535]
        );
        assert_eq!(e.undo.len(), usize::from(action == "human"));
    }
}
#[test]
fn code_start_requires_exact_id_active_owned_scopes_and_all_locks() {
    let _serial = SERIAL.lock().unwrap();
    let (shared, layer, _, p) = fixture(16);
    for (key, value) in [
        ("project_id", Value::Null),
        ("document_id", json!("wrong")),
        ("expected_revision", json!(1)),
        ("task", json!("wrong")),
        ("actor", json!("human")),
        ("description", json!("")),
        ("scopes", json!([])),
        ("max_edge", json!(0)),
        ("unexpected", json!(true)),
    ] {
        let mut request = p.clone();
        request["script"] = json!(palette(&layer));
        request[key] = value;
        assert!(
            server::dispatch(&shared, "code", &request).is_err(),
            "{key}"
        );
    }
    shared.lock().unwrap().doc.layers[0].locked = true;
    let mut request = p.clone();
    request["script"] = json!(palette(&layer));
    assert!(server::dispatch(&shared, "code", &request).is_err());
    shared.lock().unwrap().doc.layers[0].locked = false;
    let mut request = p.clone();
    request["scopes"] = json!([{"target":null,"rect":null}]);
    request["script"] = json!(palette(&layer));
    assert!(server::dispatch(&shared, "code", &request).is_err());
    assert!(shared.lock().unwrap().undo.is_empty());
}
#[test]
fn native_pixel_command_retains_selection_lock_and_protocol_guards() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let mut e = Engine::new();
    e.doc = Document::new_depth(8, 8, 16).unwrap();
    let layer = e.doc.layers[0].id.clone();
    e.doc.selection = Some([1, 1, 4, 4]);
    e.doc.selection_polygon = Some(vec![[1., 1.], [4., 1.], [1., 4.]]);
    let data: Vec<u8> = (0..64)
        .flat_map(|_| {
            [12347u16, 23459, 34561, 65535]
                .into_iter()
                .flat_map(u16::to_le_bytes)
        })
        .collect();
    let command = json!({"op":"pixels.replace","layer":layer,"rect":[0,0,8,8],"bit_depth":16,"rgba":STANDARD.encode(data)});
    e.edit("human", &[command.clone()], None, None, "Native patch")
        .unwrap();
    assert_eq!(
        e.doc.layers[0].pixels.get16(1, 1),
        [12347, 23459, 34561, 65535]
    );
    assert_eq!(e.doc.layers[0].pixels.get16(3, 3), [0; 4]);
    let before = e.doc.export_png().unwrap();
    let rev = e.doc.revision;
    for key in ["rgba", "bit_depth", "rect"] {
        let mut wrong = command.clone();
        wrong[key] = json!(0);
        assert!(e
            .edit("human", &[wrong], Some(rev), None, "Invalid")
            .is_err());
        assert_eq!(e.doc.export_png().unwrap(), before);
    }
    e.doc.layers[0].locked = true;
    assert!(e
        .edit("agent", &[command.clone()], Some(rev), None, "Locked")
        .is_err());
    e.doc.layers[0].locked = false;
    e.reserve(
        "other",
        "Protected pixel",
        vec![Scope {
            target: Some(layer),
            rect: Some([1, 1, 2, 2]),
        }],
    )
    .unwrap();
    assert!(e
        .edit("agent", &[command], Some(rev), None, "Reserved")
        .is_err());
    assert_eq!(e.doc.export_png().unwrap(), before);
}
