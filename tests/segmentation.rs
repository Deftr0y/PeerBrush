use base64::{engine::general_purpose::STANDARD, Engine as _};
use peerbrush::{
    engine::{Document, Engine, Scope},
    raster,
    segmentation::{self, Config},
    server::Shared,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
static SERIAL: Mutex<()> = Mutex::new(());
struct Provider {
    config: Config,
    path: PathBuf,
}
impl Provider {
    fn new(response: &Value, delay: bool) -> Self {
        let stem =
            std::env::temp_dir().join(format!("peerbrush-provider-{}", uuid::Uuid::new_v4()));
        #[cfg(windows)]
        let (path, program, args, script) = {
            let path = stem.with_extension("ps1");
            let program = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let args = vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-File".into(),
                path.to_string_lossy().into_owned(),
            ];
            let script = format!(
                "$null=[Console]::In.ReadToEnd()\n{}\n[Console]::Out.Write('{}')",
                if delay { "Start-Sleep -Seconds 8" } else { "" },
                response.to_string().replace('\'', "''")
            );
            (path, program, args, script)
        };
        #[cfg(not(windows))]
        let (path, program, args, script) = {
            let path = stem.with_extension("sh");
            let args = vec![path.to_string_lossy().into_owned()];
            let script = format!(
                "cat >/dev/null\n{}\nprintf '%s' '{}'",
                if delay { "sleep 8" } else { "" },
                response.to_string().replace('\'', "'\\''")
            );
            (path, PathBuf::from("/bin/sh"), args, script)
        };
        std::fs::write(&path, script).unwrap();
        Self {
            config: Config {
                program,
                args,
                timeout_ms: 30000,
            },
            path,
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
fn fixture() -> (Shared, Value, Value) {
    let mut e = Engine::new();
    e.doc = Document::new_depth(16, 12, 16).unwrap();
    e.doc.layers[0].pixels.set16(4, 4, [123, 271, 539, 65535]);
    let p = json!({"expected_revision":e.doc.revision,"rect":[4,3,8,5]});
    let reply = json!({"version":1,"document_id":e.doc.id,"source_revision":e.doc.revision,"document_rect":[4,3,8,5],"channel":"luma","png":STANDARD.encode(raster::png(2,1,&[0,0,0,255,255,255,255,255]).unwrap())});
    (Arc::new(Mutex::new(e)), p, reply)
}
#[test]
fn provider_imports_one_undoable_native_safe_confidence_selection_and_rejects_wrong_snapshot() {
    let _serial = SERIAL.lock().unwrap();
    let (shared, p, mut reply) = fixture();
    let before = shared.lock().unwrap().doc.export_png().unwrap();
    let provider = Provider::new(&reply, false);
    let result = segmentation::run_with(&shared, "human", &p, &provider.config).unwrap();
    assert_eq!(result["document_rect"], p["rect"]);
    {
        let mut e = shared.lock().unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before);
        assert_eq!(e.doc.selection, Some([5, 3, 8, 5]));
        assert_eq!(e.doc.layers[0].pixels.get16(4, 4), [123, 271, 539, 65535]);
        e.undo("human").unwrap();
        assert!(e.doc.selection.is_none());
    }
    let p = json!({"expected_revision":shared.lock().unwrap().doc.revision});
    reply["document_id"] = json!("wrong project");
    let provider = Provider::new(&reply, false);
    assert!(
        segmentation::run_with(&shared, "human", &p, &provider.config)
            .unwrap_err()
            .contains("does not match")
    );
    assert!(shared.lock().unwrap().doc.selection.is_none());
}
#[test]
fn inference_cancels_on_human_change_timeout_and_busy_provider_without_mutating_selection() {
    let _serial = SERIAL.lock().unwrap();
    let (shared, p, reply) = fixture();
    let provider = Provider::new(&reply, true);
    let edit_shared = shared.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        let mut e = edit_shared.lock().unwrap();
        let id = e.doc.layers[0].id.clone();
        e.edit(
            "human",
            &[json!({"op":"layer.update","layer":id,"name":"Human rename"})],
            None,
            None,
            "Rename",
        )
        .unwrap();
    });
    assert!(
        segmentation::run_with(&shared, "human", &p, &provider.config)
            .unwrap_err()
            .contains("changed")
    );
    writer.join().unwrap();
    assert!(shared.lock().unwrap().doc.selection.is_none());
    assert_eq!(shared.lock().unwrap().doc.layers[0].name, "Human rename");
    let mut provider = Provider::new(&reply, true);
    provider.config.timeout_ms = 1000;
    assert!(segmentation::invoke(&provider.config, &json!({}), || true)
        .unwrap_err()
        .contains("timed out"));
    assert!(segmentation::invoke(&provider.config, &json!({}), || false)
        .unwrap_err()
        .contains("changed"));
    let busy_provider = Provider::new(&reply, true);
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        segmentation::invoke(&busy_provider.config, &json!({}), || {
            let _ = tx.send(());
            !worker_cancelled.load(std::sync::atomic::Ordering::SeqCst)
        })
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(segmentation::invoke(&provider.config, &json!({}), || true)
        .unwrap_err()
        .contains("already running"));
    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(worker.join().unwrap().unwrap_err().contains("changed"));
}
#[test]
fn invalid_revision_region_point_and_reservation_fail_before_inference() {
    let _serial = SERIAL.lock().unwrap();
    let (shared, p, reply) = fixture();
    let provider = Provider::new(&reply, false);
    for extra in [
        json!({"expected_revision":999}),
        json!({"rect":[0,0,17,4]}),
        json!({"point":[0,0]}),
        json!({"mode":"bad"}),
        json!({"task":"released"}),
    ] {
        let mut p = p.clone();
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        assert!(segmentation::run_with(&shared, "human", &p, &provider.config).is_err());
        assert!(shared.lock().unwrap().doc.selection.is_none());
    }
    shared
        .lock()
        .unwrap()
        .reserve(
            "other",
            "Selection",
            vec![Scope {
                target: Some("@selection".into()),
                rect: None,
            }],
        )
        .unwrap();
    assert!(segmentation::run_with(&shared, "human", &p, &provider.config).is_err());
}
