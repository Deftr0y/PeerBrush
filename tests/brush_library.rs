use peerbrush::{
    brush::{self, Session, Settings},
    brush_library::{curated, preview, Library},
    collaboration,
    engine::{Document, Engine, Scope},
    psd,
    raster::Raster,
    server,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn curated_strokes_are_distinct_deterministic_and_all_tips_are_represented() {
    let presets = curated();
    assert_eq!(presets.len(), 21);
    let mut strokes = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut tips = std::collections::HashSet::new();
    for p in presets {
        p.settings.validate().unwrap();
        assert!(ids.insert(p.id));
        tips.insert(p.settings.tip);
        let r = preview(p.settings, 240, 48).unwrap();
        assert!(r.rgba().chunks_exact(4).any(|p| p[3] > 0));
        assert_eq!(r.rgba(), preview(p.settings, 240, 48).unwrap().rgba());
        assert!(strokes.insert(r.rgba()), "duplicate preview {}", p.name);
    }
    assert_eq!(tips.len(), 5);
}

#[test]
fn pressure_curve_is_analytic_native_and_default_is_legacy_identity() {
    for depth in [8, 16] {
        let mut raster = Raster::new_depth(64, 48, depth);
        let settings = Settings {
            radius: 12.,
            pressure_gamma: 2.,
            pressure_size: false,
            ..Default::default()
        };
        brush::paint_with_pressure(
            &mut raster,
            &[[32., 24.]],
            Some(&[0.5]),
            settings,
            [255; 4],
            false,
            None,
            None,
        )
        .unwrap();
        if depth == 16 {
            assert_eq!(raster.get16(32, 24)[3], 16384);
            assert_eq!(raster.get16(32, 24)[3] % 257, 193);
        } else {
            assert_eq!(raster.get(32, 24)[3], 64);
        }
        let a = Settings::from_command(&json!({"radius":7})).unwrap();
        let b = Settings::from_command(&json!({"radius":7,"pressure_gamma":1})).unwrap();
        assert_eq!(a, b);
        for gamma in [json!(0), json!(4.1), json!("1"), json!(false)] {
            assert!(Settings::from_command(&json!({"pressure_gamma":gamma})).is_err());
        }
    }
}

#[test]
fn every_curated_pressure_curve_matches_incremental_native_strokes_and_undo() {
    let points = [[10., 32.], [35., 22.], [66., 39.], [95., 24.]];
    let pressures = [0.2, 0.9, 0.5, 0.1];
    for depth in [8, 16] {
        let source = Raster::new_depth(112, 64, depth);
        for preset in curated() {
            let mut settings = preset.settings;
            settings.radius = settings.radius.min(12.);
            let mut session =
                Session::new(&source, settings, [170, 70, 29, 247], false, None, None).unwrap();
            for count in [1, 2, 4, 2] {
                let mut full = source.clone();
                brush::paint_with_pressure(
                    &mut full,
                    &points[..count],
                    Some(&pressures[..count]),
                    settings,
                    [170, 70, 29, 247],
                    false,
                    None,
                    None,
                )
                .unwrap();
                let incremental = session
                    .update(&points[..count], Some(&pressures[..count]))
                    .unwrap();
                assert_eq!(incremental.tiles, full.tiles);
                assert_eq!(incremental.samples16, full.samples16);
            }
        }
        let mut e = Engine::new();
        e.doc = Document::new_depth(112, 64, depth).unwrap();
        let before = e.doc.export_png().unwrap();
        let id = e.doc.layers[0].id.clone();
        e.edit("human",&[json!({"op":"paint","layer":id,"preset":"brush-pen","points":points,"pressures":pressures,"color":[170,70,29,247]})],None,None,"Brush pen").unwrap();
        assert_eq!(e.undo.len(), 1);
        assert_ne!(e.doc.export_png().unwrap(), before);
        let saved = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        assert_eq!(saved.bit_depth, depth);
        assert_eq!(
            saved.layers[0].pixels.rgba16(),
            e.doc.layers[0].pixels.rgba16()
        );
        e.undo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before);
    }
}

#[test]
fn shared_preset_settings_match_explicit_commands_for_paint_smudge_clone_and_heal() {
    for depth in [8, 16] {
        for op in ["paint", "smudge", "clone", "heal"] {
            let mut a = Engine::new();
            a.doc = Document::new_depth(96, 64, depth).unwrap();
            for y in 0..64 {
                for x in 0..96 {
                    a.doc.layers[0].pixels.set16(
                        x,
                        y,
                        [12001 + x as u16 * 251, 22003 + y as u16 * 113, 32007, 51013],
                    );
                }
            }
            let mut b = Engine::new();
            b.doc = a.doc.clone();
            let c = json!({"op":op,"layer":a.doc.layers[0].id,"preset":"wet-paint","radius":6,"pressure_gamma":1.6,"points":[[45,30],[75,35]],"pressures":[0.25,0.8],"source":[20,20],"color":[245,70,10,255]});
            let resolved = a
                .brush_library
                .lock()
                .unwrap()
                .resolve_commands(&[c.clone()])
                .unwrap();
            a.edit("human", &[c], None, None, op).unwrap();
            b.edit("human", &resolved, None, None, op).unwrap();
            assert_eq!(
                a.doc.layers[0].pixels.rgba16(),
                b.doc.layers[0].pixels.rgba16(),
                "{op} at {depth}"
            );
            assert_eq!(a.undo.len(), 1);
        }
    }
}

#[test]
fn preset_footprints_respect_reservations_overrides_and_atomic_failures() {
    let mut e = Engine::new();
    e.doc = Document::new(96, 64).unwrap();
    let layer = e.doc.layers[0].id.clone();
    e.reserve(
        "other",
        "Protected pixels",
        vec![Scope {
            target: Some(layer.clone()),
            rect: Some([64, 20, 72, 30]),
        }],
    )
    .unwrap();
    let c = json!({"op":"paint","layer":layer,"preset":"soft-airbrush","points":[[24,24]],"color":[255,0,0,255]});
    assert!(e
        .edit("human", &[c.clone()], None, None, "Blocked")
        .unwrap_err()
        .contains("Reserved"));
    assert!(e.undo.is_empty());
    assert_eq!(e.doc.revision, 0);
    let mut small = c.clone();
    small["radius"] = json!(1);
    e.edit("human", &[small], None, None, "Small override")
        .unwrap();
    assert_eq!(e.doc.revision, 1);
    let before = e.doc.export_png().unwrap();
    let mut bad = c;
    bad["preset"] = json!("unknown");
    assert!(e
        .edit(
            "human",
            &[
                json!({"op":"layer.update","layer":layer,"name":"Do not commit"}),
                bad
            ],
            None,
            None,
            "Invalid"
        )
        .is_err());
    assert_eq!(e.doc.export_png().unwrap(), before);
    assert_ne!(e.doc.layers[0].name, "Do not commit");
    assert_eq!(e.undo.len(), 1);
}

#[test]
fn custom_preferences_persist_without_document_edits_and_protect_bad_or_changed_files() {
    let dir =
        std::env::temp_dir().join(format!("peerbrush-brush-test-{}", peerbrush::engine::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("brushes.json");
    let mut library = Library::load(path.clone());
    let p = library
        .save(None, "My pencil", "Sketch", curated()[0].settings)
        .unwrap();
    let mut reloaded = Library::load(path.clone());
    assert!(reloaded.error.is_none());
    assert_eq!(reloaded.get(&p.id).unwrap().settings, p.settings);
    let settings = Settings {
        pressure_gamma: 1.7,
        ..p.settings
    };
    reloaded
        .save(Some(&p.id), "Edited pencil", "Sketch", settings)
        .unwrap();
    assert_eq!(
        Library::load(path.clone()).get(&p.id).unwrap().settings,
        settings
    );
    assert!(reloaded
        .save(Some("graphite"), "Oops", "Sketch", settings)
        .is_err());
    assert!(reloaded.delete("graphite").is_err());
    reloaded.delete(&p.id).unwrap();
    assert!(Library::load(path.clone()).get(&p.id).is_err());
    std::fs::write(&path, b"human library contents changed externally").unwrap();
    assert!(reloaded
        .save(None, "Keep", "Paint", settings)
        .unwrap_err()
        .contains("changed on disk"));
    let mut bad = Library::load(path.clone());
    assert!(bad.error.is_some());
    assert!(bad.save(None, "Keep", "Paint", settings).is_err());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"human library contents changed externally"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn proposals_freeze_custom_settings_even_when_the_library_changes() {
    let shared = Arc::new(Mutex::new(Engine::new()));
    let (doc, library) = {
        let mut e = shared.lock().unwrap();
        e.doc = Document::new_depth(96, 64, 16).unwrap();
        (e.doc.clone(), e.brush_library.clone())
    };
    let p = library
        .lock()
        .unwrap()
        .save(
            None,
            "Draft brush",
            "Ink",
            Settings {
                radius: 7.,
                pressure_gamma: 1.8,
                ..Default::default()
            },
        )
        .unwrap();
    let proposal=collaboration::propose(&shared,"agent",&json!({"document_id":doc.id,"expected_revision":0,"commands":[{"op":"paint","layer":doc.layers[0].id,"preset":p.id,"points":[[20,32],[75,32]],"pressures":[0.4,0.7],"color":[230,90,20,255]}]})).unwrap();
    let id = proposal["id"].as_str().unwrap();
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
        .save(
            Some(&p.id),
            "Changed brush",
            "Ink",
            Settings {
                radius: 45.,
                ..Default::default()
            },
        )
        .unwrap();
    let mut e = shared.lock().unwrap();
    assert!(e.undo.is_empty());
    e.accept_proposal("human", id, &doc.id, 0).unwrap();
    assert_eq!(e.doc.export_png().unwrap(), expected);
    assert_eq!(e.undo.len(), 1);
}

#[test]
fn mcp_library_returns_real_png_in_preview_coordinates_and_never_marks_document_dirty() {
    let shared = Arc::new(Mutex::new(Engine::new()));
    assert!(server::tools()
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "peerbrush_brushes"));
    let call = |args| {
        server::mcp(
            &shared,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_brushes","arguments":args}}),
        )
    };
    let list = call(json!({"action":"list"}));
    assert_eq!(list["result"]["isError"], false);
    let preview = call(json!({"action":"preview","id":"graphite","width":240,"height":48}));
    assert_eq!(preview["result"]["isError"], false);
    assert!(preview["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["type"] == "image" && c["mimeType"] == "image/png"));
    let text = preview["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("brush_preview"));
    assert!(!text.contains("document_rect"));
    assert_eq!(
        call(json!({"action":"preview","id":"graphite","width":4096}))["result"]["isError"],
        true
    );
    let save=server::dispatch(&shared,"brushes",&json!({"action":"save","name":"Tiny","category":"Ink","settings":{"radius":2,"pressure_gamma":0.5}})).unwrap();
    assert_eq!(save["preset"]["settings"]["pressure_gamma"], 0.5);
    let e = shared.lock().unwrap();
    assert_eq!(e.doc.revision, 0);
    assert_eq!(e.saved_revision, 0);
    assert!(e.undo.is_empty());
}
