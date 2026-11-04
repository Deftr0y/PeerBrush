use peerbrush::{
    engine::{self, Document, Engine},
    file_safety::{self, Lease},
    server, workspace,
};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("pb-file-safety-{}", engine::id()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture(depth: u16) -> server::Shared {
    let mut e = Engine::new();
    e.doc = Document::new_depth(9, 7, depth).unwrap();
    for y in 0..7 {
        for x in 0..9 {
            e.doc.layers[0]
                .pixels
                .set16(x, y, [10001 + x as u16, 20003 + y as u16, 40007, 65535]);
        }
    }
    Arc::new(Mutex::new(e))
}
fn paint(s: &server::Shared, value: u16) {
    let mut e = s.lock().unwrap();
    e.doc.layers[0]
        .pixels
        .set16(4, 3, [value, 23459, 45671, 65535]);
    e.doc.revision += 1;
}
fn png(s: &server::Shared) -> Vec<u8> {
    s.lock().unwrap().doc.export_png().unwrap()
}

#[test]
fn impossible_raw_psd_and_psb_saves_reject_before_replacing_files_or_work() {
    for depth in [8, 16] {
        for extension in ["psd", "psb"] {
            let directory = Directory::new();
            let path = directory.0.join(format!("artwork.{extension}"));
            let shared = fixture(depth);
            server::save(&shared, &path).unwrap();
            let original = fs::read(&path).unwrap();
            let (before, saved_revision, file_version, source_path, history) = {
                let mut engine = shared.lock().unwrap();
                // This valid sparse canvas needs over 256 MiB of raw standard
                // channels even at eight bits, before sources or record metadata.
                engine.doc.width = 8192;
                engine.doc.height = 4096;
                engine.doc.layers[0].pixels =
                    peerbrush::raster::Raster::new_depth(8192, 4096, depth);
                engine.doc.layers[0]
                    .pixels
                    .set16(2, 3, [12345, 23457, 34569, 50001]);
                engine.doc.revision += 1;
                (
                    serde_json::to_value(&engine.doc).unwrap(),
                    engine.saved_revision,
                    engine.file_version,
                    engine.path.clone(),
                    (engine.undo.len(), engine.redo.len()),
                )
            };
            for _ in 0..2 {
                assert!(server::save(&shared, &path)
                    .unwrap_err()
                    .contains("Uncompressed Photoshop channels alone"));
                assert_eq!(fs::read(&path).unwrap(), original);
                let engine = shared.lock().unwrap();
                assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
                assert_eq!(engine.saved_revision, saved_revision);
                assert_eq!(engine.file_version, file_version);
                assert_eq!(engine.path, source_path);
                assert_eq!((engine.undo.len(), engine.redo.len()), history);
                assert_ne!(engine.saved_revision, engine.doc.revision);
            }
            assert!(versions(&directory).is_empty());
        }
    }
}
// Legacy instances and other editors can bypass modern open-file ownership.
// Keep content-based publication protection independently exercised.
fn legacy_open(s: &server::Shared, path: &std::path::Path) {
    let doc = peerbrush::psd::decode(&fs::read(path).unwrap()).unwrap();
    let mut e = s.lock().unwrap();
    e.replace(doc, Some(path.into())).unwrap();
    e.file_version = Some(file_safety::version(path).unwrap());
}
fn versions(dir: &Directory) -> Vec<PathBuf> {
    fs::read_dir(dir.0.join(".peerbrush-versions"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e == "psd" || e == "psb" || e == "png")
        })
        .collect()
}
fn reviewed(s: &server::Shared, discard: bool) -> workspace::lifecycle::Reviewed {
    let e = s.lock().unwrap();
    workspace::lifecycle::Reviewed {
        project: e.project_id.clone(),
        document: e.doc.id.clone(),
        revision: e.doc.revision,
        discard,
    }
}
fn standard_composite(bytes: &[u8], expected_png: &[u8]) {
    let u16_at = |p| u16::from_be_bytes(bytes[p..p + 2].try_into().unwrap());
    let u32_at = |p| u32::from_be_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
    assert_eq!(&bytes[..4], b"8BPS");
    let psb = u16_at(4) == 2;
    let channels = u16_at(12) as usize;
    let height = u32_at(14);
    let width = u32_at(18);
    let depth = u16_at(22);
    let mut p = 26;
    for _ in 0..2 {
        let n = u32_at(p);
        p += 4 + n;
    }
    let n = if psb {
        let n = u64::from_be_bytes(bytes[p..p + 8].try_into().unwrap()) as usize;
        p += 8;
        n
    } else {
        let n = u32_at(p);
        p += 4;
        n
    };
    p += n;
    assert_eq!(u16_at(p), 0, "independent raw merged-composite check");
    p += 2;
    let rgba = image::load_from_memory(expected_png).unwrap().into_rgba16();
    assert_eq!(
        (width, height),
        (rgba.width() as usize, rgba.height() as usize)
    );
    for c in 0..channels {
        for y in 0..height {
            for x in 0..width {
                let actual = if depth == 16 {
                    let v = u16_at(p);
                    p += 2;
                    v
                } else {
                    let v = bytes[p] as u16 * 257;
                    p += 1;
                    v
                };
                assert_eq!(actual, rgba.get_pixel(x as u32, y as u32).0[c]);
            }
        }
    }
    assert_eq!(p, bytes.len());
}

#[test]
fn stale_two_instance_saves_close_orders_and_recovery_preserve_manual_artwork() {
    for depth in [8, 16] {
        for first_closes_first in [false, true] {
            let dir = Directory::new();
            let path = dir.0.join("drawing.psd");
            let initial = fixture(depth);
            server::save(&initial, &path).unwrap();
            let initial_file = fs::read(&path).unwrap();
            drop(initial);
            let stale = fixture(depth);
            let newer = fixture(depth);
            legacy_open(&stale, &path);
            legacy_open(&newer, &path);
            let stale_registry = workspace::attach(&stale);
            let newer_registry = workspace::attach(&newer);
            paint(&newer, 54321);
            let artwork = png(&newer);
            server::save(&newer, &path).unwrap();
            let saved = fs::read(&path).unwrap();
            standard_composite(&saved, &artwork);
            assert!(server::save(&stale, &path).is_err());
            let id = stale.lock().unwrap().doc.id.clone();
            let rev = stale.lock().unwrap().doc.revision;
            assert!(server::save_confirmed_source(&stale, &path, &id, rev, true).is_err());
            assert!(
                workspace::lifecycle::finish_exit(&stale_registry, &[reviewed(&stale, false)])
                    .is_err()
            );
            assert!(!stale.lock().unwrap().closed);
            for (registry, project, discard) in if first_closes_first {
                [
                    (&stale_registry, &stale, true),
                    (&newer_registry, &newer, false),
                ]
            } else {
                [
                    (&newer_registry, &newer, false),
                    (&stale_registry, &stale, true),
                ]
            } {
                workspace::lifecycle::finish_exit(registry, &[reviewed(project, discard)]).unwrap();
                server::checkpoint_projects(
                    registry,
                    &dir.0.join("recovery"),
                    &mut Default::default(),
                );
                assert_eq!(fs::read(&path).unwrap(), saved);
            }
            let reopened = fixture(depth);
            server::open(&reopened, &path).unwrap();
            assert_eq!(png(&reopened), artwork);
            assert_eq!(reopened.lock().unwrap().doc.bit_depth, depth);
            let backups = versions(&dir);
            assert_eq!(backups.len(), 1);
            assert_eq!(fs::read(&backups[0]).unwrap(), initial_file);
        }
    }
}

#[test]
fn same_length_restored_timestamp_replacement_and_path_aliases_cannot_bypass_conflicts() {
    let dir = Directory::new();
    let path = dir.0.join("drawing.psd");
    let s = fixture(16);
    server::save(&s, &path).unwrap();
    let version = file_safety::version(&path).unwrap();
    let stamp = fs::metadata(&path).unwrap().modified().unwrap();
    let mut bytes = fs::read(&path).unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    fs::write(&path, &bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(stamp))
        .unwrap();
    assert_ne!(file_safety::version(&path).unwrap(), version);
    fs::create_dir(dir.0.join("alias")).unwrap();
    let alias = dir.0.join("alias").join("..").join("drawing.psd");
    assert!(file_safety::same_destination(&path, &alias).unwrap());
    assert!(server::save(&s, &alias).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let link = dir.0.join("hardlink.psd");
    fs::hard_link(&path, &link).unwrap();
    assert!(file_safety::same_destination(&path, &link).unwrap());
    assert!(server::save(&s, &link).is_err());
    let fresh = dir.0.join("new.psd");
    server::save(&s, &fresh).unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn save_as_and_exports_require_consent_and_keep_exact_previous_versions() {
    for depth in [8, 16] {
        let dir = Directory::new();
        let target = dir.0.join("existing.psb");
        let old = fixture(depth);
        server::save(&old, &target).unwrap();
        let old_file = fs::read(&target).unwrap();
        drop(old);
        let s = fixture(depth);
        paint(&s, 54321);
        let before = png(&s);
        assert!(server::save(&s, &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), old_file);
        let e = s.lock().unwrap();
        let (id, rev) = (e.doc.id.clone(), e.doc.revision);
        drop(e);
        server::save_confirmed_source(&s, &target, &id, rev, true).unwrap();
        standard_composite(&fs::read(&target).unwrap(), &before);
        assert_eq!(fs::read(versions(&dir).pop().unwrap()).unwrap(), old_file);
        let export = dir.0.join("existing.png");
        fs::write(&export, b"previous exact export").unwrap();
        assert!(server::export(&s, &export).is_err());
        server::export_confirmed_source(&s, &export, &id, rev).unwrap();
        assert_eq!(fs::read(&export).unwrap(), before);
        assert!(versions(&dir)
            .iter()
            .any(|p| fs::read(p).unwrap() == b"previous exact export"));
        assert_eq!(png(&s), before);
    }
}

#[test]
fn save_lease_child() {
    let Some(path) = std::env::var_os("PB_TEST_SAVE_LEASE") else {
        return;
    };
    let result = Lease::acquire(&PathBuf::from(path));
    match std::env::var("PB_TEST_SAVE_MODE").unwrap().as_str() {
        "blocked" => assert!(result.is_err()),
        "hold" => {
            let _lease = result.unwrap();
            fs::write(std::env::var_os("PB_TEST_SAVE_READY").unwrap(), b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        _ => panic!("invalid test mode"),
    }
}
#[test]
fn separate_process_save_lock_rejects_concurrent_writer_and_releases_after_crash() {
    let dir = Directory::new();
    let path = dir.0.join("drawing.psd");
    fs::write(&path, b"original").unwrap();
    let lease = Lease::acquire(&path).unwrap();
    let child = || {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "save_lease_child", "--nocapture"])
            .env("PB_TEST_SAVE_LEASE", &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    };
    assert!(child()
        .env("PB_TEST_SAVE_MODE", "blocked")
        .status()
        .unwrap()
        .success());
    drop(lease);
    let ready = dir.0.join("ready");
    let mut c = child()
        .env("PB_TEST_SAVE_MODE", "hold")
        .env("PB_TEST_SAVE_READY", &ready)
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while !ready.exists() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists());
    assert!(Lease::acquire(&path).is_err());
    c.kill().unwrap();
    c.wait().unwrap();
    assert!(Lease::acquire(&path).is_ok());
    assert_eq!(fs::read(&path).unwrap(), b"original");
}

#[test]
fn failures_preserve_destination_and_work_and_successful_retention_is_bounded() {
    let dir = Directory::new();
    let path = dir.0.join("drawing.psd");
    let s = fixture(16);
    server::save(&s, &path).unwrap();
    let original = fs::read(&path).unwrap();
    let storage = dir.0.join(".peerbrush-versions");
    let full = storage.join("unrelated-preserved-file");
    fs::File::create(&full)
        .unwrap()
        .set_len(1024 * 1024 * 1024)
        .unwrap();
    paint(&s, 54321);
    let artwork = png(&s);
    assert!(server::save(&s, &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(png(&s), artwork);
    {
        let e = s.lock().unwrap();
        assert_ne!(e.saved_revision, e.doc.revision);
    }
    fs::remove_file(full).unwrap();
    for i in 0..5 {
        paint(&s, 50001 + i);
        server::save(&s, &path).unwrap();
    }
    assert_eq!(versions(&dir).len(), 3);
    let lease = Lease::acquire(&path).unwrap();
    let saved = fs::read(&path).unwrap();
    let backup = lease.preserve().unwrap().unwrap();
    fs::write(&path, b"external replacement").unwrap();
    assert!(lease.validate().is_err());
    assert_eq!(fs::read(backup).unwrap(), saved);
}

#[test]
fn protocol_overwrite_is_explicit_and_does_not_override_stale_source() {
    let dir = Directory::new();
    let target = dir.0.join("existing.psd");
    let old = fixture(8);
    server::save(&old, &target).unwrap();
    drop(old);
    let s = fixture(16);
    assert!(server::dispatch(&s, "document", &json!({"action":"save","path":target})).is_err());
    server::dispatch(
        &s,
        "document",
        &json!({"action":"save","path":target,"overwrite":true}),
    )
    .unwrap();
    server::atomic_write(&target, &fs::read(&target).unwrap()).unwrap();
    assert!(server::dispatch(
        &s,
        "document",
        &json!({"action":"save","path":target,"overwrite":true})
    )
    .is_err());
}
#[test]
fn export_never_replaces_a_loaded_working_source_even_with_a_misleading_extension() {
    let dir = Directory::new();
    let path = dir.0.join("working.png");
    let s = fixture(16);
    fs::write(
        &path,
        peerbrush::psd::encode(&s.lock().unwrap().doc).unwrap(),
    )
    .unwrap();
    server::open(&s, &path).unwrap();
    let original = fs::read(&path).unwrap();
    let e = s.lock().unwrap();
    let (id, rev) = (e.doc.id.clone(), e.doc.revision);
    drop(e);
    assert!(server::export_confirmed_source(&s, &path, &id, rev).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}
#[test]
fn previous_version_catalog_never_mixes_different_destinations() {
    let dir = Directory::new();
    let one = dir.0.join("one.psd");
    let two = dir.0.join("two.psb");
    for path in [&one, &two] {
        let s = fixture(16);
        server::save(&s, path).unwrap();
        paint(&s, 54321);
        server::save(&s, path).unwrap();
    }
    let a = file_safety::previous_versions(&one).unwrap();
    let b = file_safety::previous_versions(&two).unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert_ne!(a[0].path, b[0].path);
    assert!(a[0].bytes > 0 && a[0].timestamp_ms > 0);
    fs::remove_file(&one).unwrap();
    assert_eq!(file_safety::previous_versions(&one).unwrap().len(), 1);
}
