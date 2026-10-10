use peerbrush::{
    engine::{self, Document, Engine},
    file_ownership::Ownership,
    server, workspace,
};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("pb-open-owner-{}", engine::id()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> server::Shared {
    let mut e = Engine::new();
    e.doc = Document::new_depth(13, 9, 16).unwrap();
    e.doc.layers[0]
        .pixels
        .set16(2, 3, [12345, 23457, 45679, 65535]);
    Arc::new(Mutex::new(e))
}
fn source(dir: &Directory) -> PathBuf {
    let path = dir.0.join("Artwork.psd");
    let s = fixture();
    server::save(&s, &path).unwrap();
    path
}
#[test]
fn normalized_aliases_same_source_and_saves_cannot_create_duplicates_or_damage_work() {
    let dir = Directory::new();
    let path = source(&dir);
    let a = fixture();
    server::open(&a, &path).unwrap();
    let layer = a.lock().unwrap().doc.layers[0].id.clone();
    server::dispatch(&a,"edit",&json!({"actor":"human","commands":[{"op":"layer.update","layer":layer,"name":"Unsaved art"}]})).unwrap();
    let before = a.lock().unwrap().state();
    let png = a.lock().unwrap().doc.export_png().unwrap();
    fs::create_dir(dir.0.join("alias")).unwrap();
    let relative = dir.0.join("alias/../Artwork.psd");
    let hard = dir.0.join("Hard.psd");
    fs::hard_link(&path, &hard).unwrap();
    for alias in [&path, &relative, &hard] {
        let b = fixture();
        assert!(server::open(&b, alias)
            .unwrap_err()
            .starts_with("File already open"));
        let id = b.lock().unwrap().doc.id.clone();
        assert!(server::save_confirmed_source(&b, alias, &id, 0, true).is_err());
    }
    assert!(server::open(&a, &path).is_err());
    let mut after = a.lock().unwrap().state();
    after["file_status"] = before["file_status"].clone();
    assert_eq!(after, before);
    assert_eq!(a.lock().unwrap().doc.export_png().unwrap(), png);
    #[cfg(windows)]
    {
        assert!(server::open(&fixture(), &dir.0.join("ARTWORK.PSD")).is_err());
    }
    let other = dir.0.join("other");
    fs::create_dir(&other).unwrap();
    let distinct = other.join("Artwork.psd");
    fs::copy(&path, &distinct).unwrap();
    let b = fixture();
    server::open(&b, &distinct).unwrap();
    // Atomic Save owns its new identity, while the old hard-link inode becomes a separate file.
    server::save(&a, &path).unwrap();
    let fresh_alias = dir.0.join("Saved.psd");
    fs::hard_link(&path, &fresh_alias).unwrap();
    assert!(server::open(&fixture(), &fresh_alias).is_err());
    let old = fixture();
    server::open(&old, &hard).unwrap();
    let save_as = dir.0.join("New.psb");
    server::save(&a, &save_as).unwrap();
    let reopened = fixture();
    server::open(&reopened, &path).unwrap();
}
#[test]
fn failed_and_cancelled_loads_release_pending_ownership_and_close_releases_cloned_handles() {
    let dir = Directory::new();
    let path = source(&dir);
    let broken = dir.0.join("broken.psd");
    fs::write(&broken, b"broken").unwrap();
    for _ in 0..2 {
        assert!(!server::open(&fixture(), &broken)
            .unwrap_err()
            .starts_with("File already open"));
    }
    let control = peerbrush::loading::Control::default();
    control.cancel();
    assert!(server::open_progress(&fixture(), &path, &control).is_err());
    let a = fixture();
    server::open(&a, &path).unwrap();
    let snapshot = a.lock().unwrap().clone();
    let (project, doc, rev) = {
        let e = a.lock().unwrap();
        (e.project_id.clone(), e.doc.id.clone(), e.doc.revision)
    };
    workspace::close(&a, &project, &doc, rev, false, "human").unwrap();
    let b = fixture();
    server::open(&b, &path).unwrap();
    drop(snapshot);
    let registry = workspace::attach(&b);
    let review = {
        let e = b.lock().unwrap();
        workspace::lifecycle::Reviewed {
            project: e.project_id.clone(),
            document: e.doc.id.clone(),
            revision: e.doc.revision,
            discard: false,
        }
    };
    workspace::lifecycle::finish_exit(&registry, &[review]).unwrap();
    server::open(&fixture(), &path).unwrap();
}
#[test]
fn ownership_child() {
    let Some(path) = std::env::var_os("PB_TEST_OPEN_OWNER") else {
        return;
    };
    let result = Ownership::open(&PathBuf::from(path), "child-project");
    match std::env::var("PB_TEST_OPEN_MODE").unwrap().as_str() {
        "blocked" => assert!(result.is_err()),
        "hold" => {
            let _owner = result.unwrap();
            fs::write(std::env::var_os("PB_TEST_OPEN_READY").unwrap(), b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        _ => panic!("mode"),
    }
}
#[test]
fn pending_and_separate_processes_block_aliases_and_crash_releases_without_stale_records() {
    let dir = Directory::new();
    let path = source(&dir);
    let hard = dir.0.join("alias.psd");
    fs::hard_link(&path, &hard).unwrap();
    let owner = Ownership::open(&path, "pending-project").unwrap();
    assert!(server::open(&fixture(), &path).is_err());
    let child = |p: &std::path::Path| {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "ownership_child", "--nocapture"])
            .env("PB_TEST_OPEN_OWNER", p)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    };
    assert!(child(&hard)
        .env("PB_TEST_OPEN_MODE", "blocked")
        .status()
        .unwrap()
        .success());
    drop(owner);
    let ready = dir.0.join("ready");
    let mut c = child(&path)
        .env("PB_TEST_OPEN_MODE", "hold")
        .env("PB_TEST_OPEN_READY", &ready)
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !ready.exists() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists());
    assert!(server::open(&fixture(), &hard).is_err());
    c.kill().unwrap();
    c.wait().unwrap();
    server::open(&fixture(), &path).unwrap();
}
