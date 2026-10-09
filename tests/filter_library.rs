use peerbrush::{
    collaboration,
    engine::{Document, Engine},
    filter_library::{self, Library},
    server,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn curated_filters_have_deterministic_native_rendered_thumbnails_and_unique_ids() {
    let presets = filter_library::curated();
    assert_eq!(presets.len(), 15);
    let mut ids = std::collections::HashSet::new();
    let mut images = std::collections::HashSet::new();
    for p in presets {
        assert!(ids.insert(p.id.clone()));
        let image = filter_library::thumbnail(&p).unwrap();
        assert_eq!(image.len(), 96 * 64 * 4);
        assert_eq!(image, filter_library::thumbnail(&p).unwrap());
        assert!(images.insert(image), "Duplicate thumbnail {}", p.name);
    }
}
#[test]
fn custom_presets_roundtrip_rename_import_export_and_preserve_bad_or_changed_files() {
    let dir = std::env::temp_dir().join(format!(
        "peerbrush-filter-library-{}",
        peerbrush::engine::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("filters.json");
    let mut l = Library::load(path.clone());
    let p = l
        .save(
            None,
            "My poster",
            "Tone",
            "posterize",
            json!({"levels":7}),
            0.4,
        )
        .unwrap();
    let mut l = Library::load(path.clone());
    assert!(l.error.is_none());
    assert_eq!(l.get(&p.id).unwrap().settings, json!({"levels":7}));
    l.rename(&p.id, "Seven colors").unwrap();
    let data = l.export(&p.id).unwrap();
    assert_eq!(data["name"], "Seven colors");
    assert!(data.get("id").is_none());
    let copy = l.import(&data).unwrap();
    assert_ne!(copy.id, p.id);
    assert_eq!(copy.weight, 0.4);
    assert_eq!(copy.settings, p.settings);
    for bad in [
        json!({"levels":1}),
        json!({"levels":2.5}),
        json!({"levels":7,"ignored":true}),
    ] {
        assert!(l
            .save(None, "Invalid", "Tone", "posterize", bad, 1.)
            .is_err());
    }
    let mut unknown = data.clone();
    unknown["version"] = json!(2);
    assert!(l.import(&unknown).is_err());
    let mut unknown = data.clone();
    unknown["code"] = json!("ignored");
    assert!(l.import(&unknown).is_err());
    assert!(l.rename("posterize", "Curated").is_err());
    assert!(l.delete("posterize").is_err());
    l.delete(&p.id).unwrap();
    assert!(Library::load(path.clone()).get(&p.id).is_err());
    std::fs::write(&path, b"external human library contents").unwrap();
    assert!(l
        .save(None, "Preserve", "Tone", "invert", json!({}), 1.)
        .unwrap_err()
        .contains("changed on disk"));
    let mut bad = Library::load(path.clone());
    assert!(bad.error.is_some());
    assert!(bad
        .save(None, "Preserve", "Tone", "invert", json!({}), 1.)
        .is_err());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"external human library contents"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn preset_commands_are_frozen_and_shared_across_tabs_and_proposals() {
    let mut e = Engine::new();
    e.doc = Document::new_depth(12, 8, 16).unwrap();
    e.doc.layers[0]
        .pixels
        .set16(2, 2, [12345, 23456, 45678, 65535]);
    let shared = Arc::new(Mutex::new(e));
    let library = shared.lock().unwrap().filter_library.clone();
    let p = library
        .lock()
        .unwrap()
        .save(None, "Draft", "Tone", "posterize", json!({"levels":3}), 0.5)
        .unwrap();
    let source = shared.lock().unwrap().doc.clone();
    let draft=collaboration::propose(&shared,"agent",&json!({"document_id":source.id,"expected_revision":0,"commands":[{"op":"filter.add","preset":p.id}]})).unwrap();
    let id = draft["id"].as_str().unwrap();
    let expected = shared
        .lock()
        .unwrap()
        .proposal_document(id)
        .unwrap()
        .export_png()
        .unwrap();
    library
        .lock()
        .unwrap()
        .save(Some(&p.id), "Changed", "Tone", "invert", json!({}), 1.)
        .unwrap();
    shared
        .lock()
        .unwrap()
        .accept_proposal("human", id, &source.id, 0)
        .unwrap();
    assert_eq!(shared.lock().unwrap().doc.export_png().unwrap(), expected);
    let workspace = peerbrush::workspace::attach(&shared);
    let other = peerbrush::workspace::register_in(&workspace, Engine::new(), None).unwrap();
    assert!(Arc::ptr_eq(&other.lock().unwrap().filter_library, &library));
    other
        .lock()
        .unwrap()
        .edit(
            "human",
            &[json!({"op":"filter.add","preset":p.id})],
            None,
            None,
            "Library filter",
        )
        .unwrap();
    assert_eq!(other.lock().unwrap().doc.filters[0].kind, "invert");
}
#[test]
fn protocol_discovery_thumbnail_and_source_bound_preview_have_distinct_coordinates() {
    let mut e = Engine::new();
    e.doc = Document::new_depth(12, 8, 16).unwrap();
    e.doc.layers[0]
        .pixels
        .set16(2, 2, [12345, 23456, 45678, 65535]);
    let project = e.project_id.clone();
    let doc = e.doc.clone();
    let shared = Arc::new(Mutex::new(e));
    assert!(server::tools()
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "peerbrush_filters"));
    assert_eq!(
        server::dispatch(&shared, "filters", &json!({"action":"list"})).unwrap()["presets"]
            .as_array()
            .unwrap()
            .len(),
        15
    );
    let thumbnail = server::dispatch(
        &shared,
        "filters",
        &json!({"action":"thumbnail","id":"posterize"}),
    )
    .unwrap();
    assert_eq!(
        thumbnail["images"][0]["coordinate_space"],
        "filter_thumbnail"
    );
    assert!(thumbnail["images"][0].get("document_rect").is_none());
    let request = json!({"action":"preview","id":"posterize","project_id":project,"document_id":doc.id,"expected_revision":0,"weight":0.5,"settings":{"levels":3},"max_edge":32});
    let preview = server::dispatch(&shared, "filters", &request).unwrap();
    assert_eq!(preview["images"][0]["document_rect"], json!([0, 0, 12, 8]));
    assert_eq!(preview["commands"][0]["settings"]["levels"], 3);
    assert!(preview["commands"][0].get("preset").is_none());
    let mut wrong = request.clone();
    wrong["expected_revision"] = json!(1);
    assert!(server::dispatch(&shared, "filters", &wrong).is_err());
    let mut wrong = request.clone();
    wrong["document_id"] = json!("changed");
    assert!(server::dispatch(&shared, "filters", &wrong).is_err());
    let mut e = shared.lock().unwrap();
    assert!(e.undo.is_empty());
    assert_eq!(e.doc.revision, 0);
    let commands = preview["commands"].as_array().unwrap();
    let expected = Engine::preview_edits(doc, commands)
        .unwrap()
        .preview(None, 32, None, false)
        .unwrap()
        .2;
    e.edit("human", commands, Some(0), None, "Apply").unwrap();
    assert_eq!(e.doc.preview(None, 32, None, false).unwrap().2, expected);
    assert_eq!(
        e.state()["document"]["filters"].as_array().unwrap().len(),
        1
    );
    assert_eq!(e.undo.len(), 1);
    let revision = e.doc.revision;
    assert!(e
        .edit("human", commands, Some(revision), None, "Stale preview")
        .is_err());
    assert_eq!(e.undo.len(), 1);
}
