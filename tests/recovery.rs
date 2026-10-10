use peerbrush::{
    effects::Effect,
    engine::{self, Document, Engine, Layer, Mask, MaskStep, Scope},
    psd,
    raster::Raster,
    recovery, server, workspace,
};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pb-recovery-{}", engine::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}
fn fixture(depth: u16) -> server::Shared {
    let mut e = Engine::new();
    e.doc = Document::new_depth(16, 12, depth).unwrap();
    let mut folder = Layer::new("Artwork", "group", 16, 12);
    folder.pixels = Raster::new_depth(16, 12, depth);
    let mut hidden = Layer::new("Hidden source", "paint", 16, 12);
    hidden.pixels = Raster::new_depth(16, 12, depth);
    hidden.pixels.set16(8, 9, [11113, 22227, 33339, 0]);
    hidden.parent = Some(folder.id.clone());
    hidden.visible = false;
    let paint = &mut e.doc.layers[0];
    paint.parent = Some(folder.id.clone());
    for y in 0..12 {
        for x in 0..16 {
            paint.pixels.set16(x, y, [12347, 23459, 34571, 65535]);
        }
    }
    paint.effects.push(Effect {
        id: engine::id(),
        kind: "invert".into(),
        enabled: true,
        weight: 0.4,
        settings: json!({}),
    });
    let mut mask = Raster::new_depth(16, 12, depth);
    mask.set16(2, 3, [45679, 45679, 45679, 65535]);
    paint.mask = Some(Mask {
        enabled: true,
        cache_key: engine::id(),
        steps: vec![
            MaskStep {
                id: engine::id(),
                kind: "fill".into(),
                enabled: true,
                weight: 1.,
                value: 255.,
                pixels: Raster::new_depth(16, 12, depth),
                settings: json!({}),
            },
            MaskStep {
                id: engine::id(),
                kind: "paint".into(),
                enabled: true,
                weight: 1.,
                value: 0.,
                pixels: mask,
                settings: json!({}),
            },
        ],
    });
    e.doc.layers.push(hidden);
    e.doc.layers.push(folder);
    // Keep the standard merged comparison opaque; alpha/mask/source words are
    // checked independently below without Photoshop's white rematting ambiguity.
    let mut background = Layer::new("Background", "paint", 16, 12);
    background.pixels = Raster::new_depth(16, 12, depth);
    for y in 0..12 {
        for x in 0..16 {
            background.pixels.set16(x, y, [37011, 28007, 19001, 65535]);
        }
    }
    e.doc.layers.push(background);
    let target = e
        .doc
        .layers
        .iter()
        .find(|l| l.name == "Paint 1")
        .unwrap()
        .id
        .clone();
    e.edit(
        "human",
        &[json!({"op":"transform","layer":target,"angle":12,"selection_only":false})],
        None,
        None,
        "Retain original",
    )
    .unwrap();
    let source = peerbrush::source::Source {
        width: 16,
        height: 12,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: peerbrush::source::Content::Shape {
            shape: "rectangle".into(),
            bounds: [3., 3., 7., 9.],
            points: vec![],
            closed: true,
            fill: [13001, 23003, 33007, 65535],
            stroke: [0; 4],
            stroke_width: 0.,
        },
    };
    e.edit(
        "human",
        &[json!({"op":"source.add","source":source,"x":0,"y":0})],
        None,
        None,
        "Editable shape",
    )
    .unwrap();
    e.saved_revision = u64::MAX;
    e.doc.srgb_tagged = true;
    Arc::new(Mutex::new(e))
}
fn rename(shared: &server::Shared, name: &str) {
    let mut e = shared.lock().unwrap();
    let layer = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"layer.update","layer":layer,"name":name})],
        None,
        None,
        "Edit",
    )
    .unwrap();
}
fn standard(bytes: &[u8]) -> Vec<u8> {
    let color = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let resources = 30 + color;
    let n = u32::from_be_bytes(bytes[resources..resources + 4].try_into().unwrap()) as usize;
    let mut out = bytes[..resources].to_vec();
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&bytes[resources + 4 + n..]);
    out
}
fn retained_samples_and_placement(new: &Raster, old: &Raster) {
    match (&new.retained, &old.retained) {
        (Some(new), Some(old)) => {
            assert_eq!(
                (new.pixels.width, new.pixels.height, new.pixels.depth),
                (old.pixels.width, old.pixels.height, old.pixels.depth)
            );
            assert_eq!(new.pixels.rgba16(), old.pixels.rgba16());
            // JSON loading can differ by a few floating-point ULPs; source channel
            // words remain exact and the affine placement is numerically equivalent.
            for (new, old) in new.matrix.iter().zip(old.matrix) {
                assert!((new - old).abs() < 1e-12);
            }
        }
        (None, None) => (),
        _ => panic!("Retained original presence changed during recovery"),
    }
}
#[test]
fn named_and_untitled_native_versions_restore_independently_with_standard_composites() {
    for depth in [8, 16] {
        let dir = dir();
        let root = fixture(depth);
        let workspace = workspace::attach(&root);
        let named =
            workspace::register_in(&workspace, fixture(depth).lock().unwrap().clone(), None)
                .unwrap();
        let original = dir.join("original.psd");
        server::save(&named, &original).unwrap();
        let original_bytes = fs::read(&original).unwrap();
        rename(&named, "Dirty named work");
        let baselines: Vec<_> = [&root, &named]
            .iter()
            .map(|s| s.lock().unwrap().doc.clone())
            .collect();
        server::checkpoint_projects(&workspace, &dir, &mut Default::default());
        assert_eq!(recovery::catalog(&dir).len(), 2);
        // A restart need not trust either mutable legacy index.
        fs::write(dir.join("recoveries.json"), b"interrupted index").unwrap();
        let restarted = fixture(depth);
        let registry = workspace::attach(&restarted);
        for entry in recovery::catalog(&dir) {
            let before = baselines
                .iter()
                .find(|d| d.id == entry.document_id)
                .unwrap();
            let bytes = fs::read(dir.join(&entry.file)).unwrap();
            let standard = psd::decode(&standard(&bytes)).unwrap();
            assert_eq!(standard.bit_depth, depth);
            if depth == 16 {
                assert_eq!(
                    peerbrush::depth16::render(&standard).unwrap().words,
                    peerbrush::depth16::render(before).unwrap().words
                );
            } else {
                assert_eq!(
                    image::load_from_memory(&standard.export_png().unwrap())
                        .unwrap()
                        .to_rgba8(),
                    image::load_from_memory(&before.export_png().unwrap())
                        .unwrap()
                        .to_rgba8()
                );
            }
            let restored =
                recovery::restore(&registry, &dir, &entry.snapshot, &Default::default(), None)
                    .unwrap();
            let e = restored.lock().unwrap();
            assert!(e.path.is_none() && e.file_version.is_none());
            assert_ne!(e.doc.revision, e.saved_revision);
            assert_ne!(e.doc.id, before.id);
            assert_eq!(e.doc.bit_depth, depth);
            assert_eq!(e.doc.srgb_tagged, before.srgb_tagged);
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            for old in &before.layers {
                let new = e.doc.layers.iter().find(|l| l.id == old.id).unwrap();
                assert_eq!(new.pixels.rgba16(), old.pixels.rgba16());
                assert_eq!(new.parent, old.parent);
                assert_eq!(json!(new.effects), json!(old.effects));
                match (&new.mask, &old.mask) {
                    (Some(new), Some(old)) => {
                        assert_eq!(new.enabled, old.enabled);
                        assert_eq!(new.steps.len(), old.steps.len());
                        for (new, old) in new.steps.iter().zip(&old.steps) {
                            assert_eq!(
                                (
                                    &new.id,
                                    &new.kind,
                                    new.enabled,
                                    new.weight,
                                    new.value,
                                    &new.settings
                                ),
                                (
                                    &old.id,
                                    &old.kind,
                                    old.enabled,
                                    old.weight,
                                    old.value,
                                    &old.settings
                                )
                            );
                            assert_eq!(new.pixels.rgba16(), old.pixels.rgba16());
                            retained_samples_and_placement(&new.pixels, &old.pixels);
                        }
                    }
                    (None, None) => (),
                    _ => panic!("Mask presence changed during recovery"),
                }
                assert_eq!(json!(new.source), json!(old.source));
                retained_samples_and_placement(&new.pixels, &old.pixels);
            }
            drop(e);
            rename(&restored, "Recovered edit");
            let output = dir.join(format!("restored-{}.psd", entry.snapshot));
            server::save(&restored, &output).unwrap();
            let reopened = psd::decode(&fs::read(output).unwrap()).unwrap();
            assert_eq!(
                reopened.export_png().unwrap(),
                restored.lock().unwrap().doc.export_png().unwrap()
            );
        }
        assert_eq!(fs::read(original).unwrap(), original_bytes);
        assert_eq!(
            recovery::catalog(&dir).len(),
            2,
            "Restoring never consumes the only recovery"
        );
        assert_eq!(workspace::entries_in(&registry).len(), 3);
    }
}
#[test]
fn interrupted_checkpoint_or_index_and_damaged_latest_leave_an_earlier_complete_version() {
    let dir = dir();
    let root = fixture(16);
    let workspace = workspace::attach(&root);
    let mut previous = Default::default();
    for name in ["One", "Two", "Three", "Four", "Five"] {
        rename(&root, name);
        server::checkpoint_projects(&workspace, &dir, &mut previous);
    }
    let versions = recovery::catalog(&dir);
    assert_eq!(versions.len(), 3);
    let revisions: Vec<_> = versions.iter().map(|e| e.revision).collect();
    let latest = root.lock().unwrap().doc.revision;
    assert_eq!(revisions, vec![latest, latest - 1, latest - 2]);
    let project = &versions[0].project_id;
    let orphan = dir.join(format!("recovery-{project}-{}.psd", engine::id()));
    fs::write(&orphan, b"incomplete checkpoint").unwrap();
    let tmp = dir.join(format!(
        "recovery-{project}-{}.{}.tmp",
        engine::id(),
        engine::id()
    ));
    fs::write(&tmp, b"incomplete data").unwrap();
    let unrelated = dir.join("unrelated.psd");
    fs::write(&unrelated, b"human file").unwrap();
    fs::write(dir.join("recoveries.json"), b"partial index").unwrap();
    fs::write(dir.join(&versions[0].file), b"truncated version").unwrap();
    let before = root.lock().unwrap().doc.export_png().unwrap();
    assert!(recovery::restore(
        &workspace,
        &dir,
        &versions[0].snapshot,
        &Default::default(),
        None
    )
    .err()
    .unwrap()
    .contains("damaged"));
    assert_eq!(workspace::entries_in(&workspace).len(), 1);
    assert_eq!(root.lock().unwrap().doc.export_png().unwrap(), before);
    let restored = recovery::restore(
        &workspace,
        &dir,
        &versions[1].snapshot,
        &Default::default(),
        None,
    )
    .unwrap();
    assert_eq!(restored.lock().unwrap().doc.layers[0].name, "Four");
    server::checkpoint_projects(&workspace, &dir, &mut previous);
    assert!(!orphan.exists() && !tmp.exists());
    assert_eq!(fs::read(unrelated).unwrap(), b"human file");
}
#[test]
fn autosave_failure_is_visible_retried_and_never_marks_work_saved() {
    let dir = dir();
    let root = fixture(16);
    let workspace = workspace::attach(&root);
    let mut previous = Default::default();
    rename(&root, "First complete");
    server::checkpoint_projects(&workspace, &dir, &mut previous);
    let complete = recovery::catalog(&dir)[0].clone();
    let bytes = fs::read(dir.join(&complete.file)).unwrap();
    rename(&root, "Newer unsaved work");
    let before = root.lock().unwrap().doc.clone();
    let blocked = dir.join("blocked");
    fs::write(&blocked, b"not a directory").unwrap();
    server::checkpoint_projects(&workspace, &blocked, &mut previous);
    assert!(workspace
        .lock()
        .unwrap()
        .recovery_error
        .as_ref()
        .unwrap()
        .contains("Autosave failed"));
    assert_ne!(root.lock().unwrap().saved_revision, before.revision);
    assert_eq!(fs::read(dir.join(&complete.file)).unwrap(), bytes);
    server::checkpoint_projects(&workspace, &dir, &mut previous);
    assert!(workspace.lock().unwrap().recovery_error.is_none());
    assert_eq!(recovery::catalog(&dir).len(), 2);
    assert_eq!(
        root.lock().unwrap().doc.export_png().unwrap(),
        before.export_png().unwrap()
    );
}
#[test]
fn explicit_close_cleans_only_its_recoveries_and_keeps_other_session_work() {
    let dir = dir();
    let old = fixture(16);
    server::checkpoint_projects(&workspace::attach(&old), &dir, &mut Default::default());
    let old_version = recovery::catalog(&dir)[0].clone();
    let root = fixture(8);
    let registry = workspace::attach(&root);
    server::checkpoint_projects(&registry, &dir, &mut Default::default());
    let (project, document, revision) = {
        let e = root.lock().unwrap();
        (e.project_id.clone(), e.doc.id.clone(), e.doc.revision)
    };
    workspace::close_in(&registry, &project, &document, revision, true, "human").unwrap();
    server::checkpoint_projects(&registry, &dir, &mut Default::default());
    let versions = recovery::catalog(&dir);
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].snapshot, old_version.snapshot);
}
#[test]
fn recovery_protocol_preserves_reserved_work_and_guards_explicit_discard() {
    let dir = dir();
    let root = fixture(16);
    let registry = workspace::attach(&root);
    server::checkpoint_projects(&registry, &dir, &mut Default::default());
    let before = root.lock().unwrap().doc.export_png().unwrap();
    root.lock()
        .unwrap()
        .reserve(
            "artist",
            "Painting",
            vec![Scope {
                target: None,
                rect: None,
            }],
        )
        .unwrap();
    let list = server::dispatch(&root, "recovery", &json!({"action":"list"})).unwrap();
    let mcp = server::mcp(
        &root,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_recovery","arguments":{"action":"list"}}}),
    );
    assert_ne!(mcp["result"]["isError"], true, "{mcp}");
    assert!(mcp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("versions_per_project"));
    let snapshot = &list["versions"][0]["snapshot"];
    assert!(server::dispatch(
        &root,
        "recovery",
        &json!({"action":"discard","snapshot":snapshot,"actor":"agent"})
    )
    .unwrap_err()
    .contains("human"));
    let result = server::dispatch(
        &root,
        "recovery",
        &json!({"action":"restore","snapshot":snapshot,"actor":"agent"}),
    )
    .unwrap();
    assert_eq!(result["state"]["document"]["bit_depth"], 16);
    assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 16, 12]));
    assert_eq!(root.lock().unwrap().doc.export_png().unwrap(), before);
    assert_eq!(
        workspace::active_id_in(&registry),
        root.lock().unwrap().project_id
    );
    server::dispatch(
        &root,
        "recovery",
        &json!({"action":"discard","snapshot":snapshot,"actor":"human"}),
    )
    .unwrap();
    assert!(recovery::catalog(&dir).is_empty());
    assert!(server::dispatch(
        &root,
        "recovery",
        &json!({"action":"restore","snapshot":snapshot})
    )
    .is_err());
    assert_eq!(workspace::entries_in(&registry).len(), 2);
}
#[test]
fn full_recovery_storage_preserves_every_complete_project_until_explicit_discard() {
    let dir = dir();
    let root = fixture(8);
    server::checkpoint_projects(&workspace::attach(&root), &dir, &mut Default::default());
    let base = recovery::catalog(&dir)[0].clone();
    let bytes = fs::read(dir.join(&base.file)).unwrap();
    for _ in 1..recovery::MAX_PROJECTS {
        let mut entry = base.clone();
        entry.project_id = engine::id();
        entry.snapshot = engine::id();
        entry.file = format!("recovery-{}-{}.psd", entry.project_id, entry.snapshot);
        fs::write(dir.join(&entry.file), &bytes).unwrap();
        fs::write(
            dir.join(&entry.file).with_extension("json"),
            serde_json::to_vec(&entry).unwrap(),
        )
        .unwrap();
    }
    let new = fixture(16);
    let registry = workspace::attach(&new);
    let mut previous = Default::default();
    server::checkpoint_projects(&registry, &dir, &mut previous);
    assert!(registry
        .lock()
        .unwrap()
        .recovery_error
        .as_ref()
        .unwrap()
        .contains("storage is full"));
    assert_eq!(recovery::catalog(&dir).len(), recovery::MAX_PROJECTS);
    {
        let busy = new.lock().unwrap();
        server::checkpoint_projects(&registry, &dir, &mut previous);
        assert!(registry
            .lock()
            .unwrap()
            .recovery_error
            .as_ref()
            .unwrap()
            .contains("storage is full"));
        drop(busy);
    }
    recovery::discard(&dir, &base.snapshot).unwrap();
    server::checkpoint_projects(&registry, &dir, &mut previous);
    assert!(registry.lock().unwrap().recovery_error.is_none());
    assert_eq!(recovery::catalog(&dir).len(), recovery::MAX_PROJECTS);
}
#[cfg(windows)]
#[test]
fn interrupted_or_blocked_ordinary_save_preserves_original_and_dirty_revision() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = dir();
    let root = fixture(16);
    let file = dir.join("original.psd");
    server::save(&root, &file).unwrap();
    let original = fs::read(&file).unwrap();
    rename(&root, "Unsaved edit");
    let guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&file)
        .unwrap();
    assert!(server::save(&root, &file).is_err());
    {
        let e = root.lock().unwrap();
        assert_ne!(e.doc.revision, e.saved_revision);
    }
    drop(guard);
    assert_eq!(fs::read(&file).unwrap(), original);
    server::save(&root, &file).unwrap();
    let reopened = psd::decode(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(reopened.layers[0].name, "Unsaved edit");
}
