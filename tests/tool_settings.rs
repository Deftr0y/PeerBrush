use peerbrush::{
    brush::Settings,
    engine::{Document, Engine},
    server,
    tool_settings::{Profile, Store},
    workspace,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn per_tool_preferences_are_shared_by_projects_and_never_enter_document_history_or_psd() {
    let root = Arc::new(Mutex::new(Engine::new()));
    let original = peerbrush::psd::encode(&root.lock().unwrap().doc).unwrap();
    let second = workspace::new_project(&root, 32, 24, 16, None).unwrap();
    let result=server::dispatch(&root,"tool_settings",&json!({"action":"set","tool":"brush","actor":"artist","expected_tool_revision":0,"preset":"brush-pen","settings":{"radius":17.25,"opacity":0.4,"tip":"grain","seed":37}})).unwrap();
    assert_eq!(result["profile"]["settings"]["radius"], 17.25);
    assert_eq!(result["profile"]["preset"], "brush-pen");
    assert_eq!(
        server::dispatch(
            &second,
            "tool_settings",
            &json!({"action":"get","tool":"brush"})
        )
        .unwrap(),
        result
    );
    let eraser = server::dispatch(
        &second,
        "tool_settings",
        &json!({"action":"get","tool":"eraser"}),
    )
    .unwrap();
    assert_eq!(eraser["profile"]["settings"]["opacity"], 1.0);
    assert_eq!(eraser["revision"], 0);
    let e = root.lock().unwrap();
    assert_eq!(peerbrush::psd::encode(&e.doc).unwrap(), original);
    assert!(e.undo.is_empty() && e.redo.is_empty());
    let mut copy = Engine::new();
    copy.doc = Document::new_depth(8, 8, 16).unwrap();
    let loaded = workspace::register(&root, copy, None).unwrap();
    assert!(Arc::ptr_eq(
        &loaded.lock().unwrap().tool_settings,
        &e.tool_settings
    ));
}

#[test]
fn stale_invalid_and_missing_revision_updates_are_atomic_and_human_merges_preserve_newer_independent_fields(
) {
    let mut store = Store::default();
    let before = store.get("brush").unwrap().profile;
    let agent = Profile {
        settings: Settings {
            radius: 27.,
            flow: 0.25,
            ..Default::default()
        },
        ..Default::default()
    };
    store.set("brush", agent, "artist", Some(0)).unwrap();
    let human = Profile {
        settings: Settings {
            opacity: 0.4,
            ..Default::default()
        },
        ..Default::default()
    };
    let merged = store.human_delta("brush", &before, &human).unwrap();
    assert_eq!(merged.profile.settings.radius, 27.);
    assert_eq!(merged.profile.settings.flow, 0.25);
    assert_eq!(merged.profile.settings.opacity, 0.4);
    let original = store.state();
    assert!(store
        .set("brush", Profile::default(), "artist", Some(0))
        .is_err());
    assert!(store
        .set("brush", Profile::default(), "artist", None)
        .is_err());
    assert!(store
        .set("unknown", Profile::default(), "human", None)
        .is_err());
    assert!(store
        .set(
            "brush",
            Profile {
                settings: Settings {
                    opacity: f32::NAN,
                    ..Default::default()
                },
                ..Default::default()
            },
            "human",
            None
        )
        .is_err());
    assert_eq!(store.state(), original);
}

#[test]
fn tool_previews_return_real_coordinates_and_liquify_retains_only_its_supported_settings() {
    let shared = Arc::new(Mutex::new(Engine::new()));
    let result = server::dispatch(
        &shared,
        "tool_settings",
        &json!({"action":"preview","tool":"brush"}),
    )
    .unwrap();
    assert_eq!(result["images"][0]["coordinate_space"], "brush_preview");
    assert_eq!(result["images"][0]["rect"], json!([0, 0, 240, 48]));
    assert!(!result["images"][0]["data"].as_str().unwrap().is_empty());
    let result=server::dispatch(&shared,"tool_settings",&json!({"action":"set","tool":"liquify","actor":"human","settings":{"radius":35,"strength":0.7,"mode":"pinch"}})).unwrap();
    assert_eq!(result["profile"]["settings"]["mode"], "pinch");
    assert!((result["profile"]["settings"]["strength"].as_f64().unwrap() - 0.7).abs() < 0.00001);
    let before = server::dispatch(&shared, "tool_settings", &json!({"action":"get"})).unwrap();
    for parameters in [
        json!({"action":"set","tool":"liquify","actor":"human","settings":{"opacity":0.8}}),
        json!({"action":"set","tool":"brush","actor":"human","settings":{"unknown":1}}),
        json!({"action":"set","tool":"brush","actor":"human","expected_tool_revision":"zero"}),
    ] {
        assert!(server::dispatch(&shared, "tool_settings", &parameters).is_err());
    }
    assert_eq!(
        server::dispatch(&shared, "tool_settings", &json!({"action":"get"})).unwrap(),
        before
    );
    assert!(server::dispatch(
        &shared,
        "tool_settings",
        &json!({"action":"preview","tool":"liquify"})
    )
    .unwrap_err()
    .contains("explicit document"));
}
