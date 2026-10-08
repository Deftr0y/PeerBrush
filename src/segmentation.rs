//! Optional external inference. Only the final typed mask command edits shared state.
use crate::{
    engine::{self, Document},
    server::Shared,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{mpsc, Mutex},
    thread,
    time::{Duration, Instant},
};
const LIMIT: u64 = 16 * 1024 * 1024;
static ACTIVE: Mutex<()> = Mutex::new(());
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "timeout")]
    pub timeout_ms: u64,
}
fn timeout() -> u64 {
    60000
}
pub fn configured() -> bool {
    std::env::var_os("PEERBRUSH_SEGMENTATION_CONFIG").is_some_and(|p| PathBuf::from(p).is_file())
}
pub fn config() -> Result<Config, String> {
    let path = std::env::var_os("PEERBRUSH_SEGMENTATION_CONFIG").ok_or(
        "Automatic selection needs a configured local provider. See docs/segmentation.md.",
    )?;
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Cannot read segmentation provider config: {e}"))?;
    let mut bytes = vec![];
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 65536 {
        return Err("Segmentation provider config exceeds 64 KiB".into());
    }
    let config: Config = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Invalid segmentation provider config: {e}"))?;
    if !config.program.is_absolute()
        || !config.program.is_file()
        || config.args.len() > 32
        || config.args.iter().any(|a| a.len() > 4096)
        || !(1000..=180000).contains(&config.timeout_ms)
    {
        return Err("Provider needs an existing absolute executable, bounded arguments and a 1–180 second timeout".into());
    }
    Ok(config)
}
pub fn invoke(
    config: &Config,
    request: &Value,
    mut current: impl FnMut() -> bool,
) -> Result<Value, String> {
    if !config.program.is_absolute()
        || !config.program.is_file()
        || config.args.len() > 32
        || config.args.iter().any(|a| a.len() > 4096)
        || !(1000..=180000).contains(&config.timeout_ms)
    {
        return Err("Invalid segmentation provider configuration".into());
    }
    let _active = ACTIVE
        .try_lock()
        .map_err(|_| "A segmentation request is already running")?;
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("Segmentation request exceeds 16 MiB".into());
    }
    let mut command = Command::new(&config.program);
    command
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start segmentation provider: {e}"))?;
    let mut input = child.stdin.take().ok_or("Provider stdin unavailable")?;
    let output = child.stdout.take().ok_or("Provider stdout unavailable")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = vec![];
        let result = output
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
            .map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    let (write_tx, write_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = input.write_all(&bytes).map_err(|e| e.to_string());
        drop(input);
        let _ = write_tx.send(result);
    });
    let started = Instant::now();
    let result = (|| loop {
        if !current() {
            return Err("Project changed during automatic selection; no result was applied".into());
        }
        if started.elapsed() > Duration::from_millis(config.timeout_ms) {
            return Err("Segmentation provider timed out; no result was applied".into());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if !status.success() {
                return Err(
                    "Segmentation provider failed; inspect its setup/model outside PeerBrush"
                        .into(),
                );
            }
            write_rx
                .recv_timeout(Duration::from_secs(1))
                .map_err(|_| "Provider did not finish reading the request")??;
            let bytes = rx
                .recv_timeout(Duration::from_secs(1))
                .map_err(|_| "Provider did not finish its response")??;
            if bytes.len() as u64 > LIMIT {
                return Err("Segmentation response exceeds 16 MiB".into());
            }
            return serde_json::from_slice(&bytes)
                .map_err(|e| format!("Invalid segmentation provider response: {e}"));
        }
        thread::sleep(Duration::from_millis(50));
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}
fn request(doc: &Document, p: &Value) -> Result<Value, String> {
    if doc.read_only {
        return Err("Document is read-only".into());
    }
    if u64::from(doc.width) * u64::from(doc.height) > 16_000_000 {
        return Err("Automatic selection is limited to 16 megapixels".into());
    }
    let rect = if p.get("rect").is_some() {
        p.get("rect")
            .and_then(engine::rect)
            .ok_or("Invalid segmentation rectangle")?
    } else {
        [0, 0, doc.width as i32, doc.height as i32]
    };
    if rect[0] < 0
        || rect[1] < 0
        || rect[2] > doc.width as i32
        || rect[3] > doc.height as i32
        || rect[0] >= rect[2]
        || rect[1] >= rect[3]
    {
        return Err("Segmentation region must have positive area inside the canvas".into());
    }
    if let Some(point) = p.get("point") {
        let a = point
            .as_array()
            .filter(|p| p.len() == 2)
            .ok_or("Object point needs two document coordinates")?;
        for (i, bounds) in [[rect[0], rect[2]], [rect[1], rect[3]]].iter().enumerate() {
            let v = a[i].as_f64().ok_or("Invalid object point")?;
            if !v.is_finite() || v < bounds[0] as f64 || v >= bounds[1] as f64 {
                return Err("Object point must be inside the requested region".into());
            }
        }
    }
    let (w, h, pixels, area) = doc.preview(
        Some(rect),
        2048,
        p.get("layer").and_then(Value::as_str),
        false,
    )?;
    Ok(
        json!({"version":1,"document_id":doc.id,"source_revision":doc.revision,"document_rect":area,"width":w,"height":h,"png":STANDARD.encode(crate::raster::png(w,h,&pixels)?),"point":p.get("point")}),
    )
}
pub fn run(shared: &Shared, actor: &str, p: &Value) -> Result<Value, String> {
    run_with(shared, actor, p, &config()?)
}
pub fn run_with(shared: &Shared, actor: &str, p: &Value, config: &Config) -> Result<Value, String> {
    let doc = {
        let mut e = shared.lock().unwrap();
        e.expire();
        e.check(
            actor,
            &[engine::Scope {
                target: Some("@selection".into()),
                rect: None,
            }],
        )?;
        if let Some(task) = p.get("task").and_then(Value::as_str) {
            if !e.leases.iter().any(|l| l.id == task && l.owner == actor) {
                return Err("Task reservation expired or was released".into());
            }
        }
        e.doc.clone()
    };
    let expected = p
        .get("expected_revision")
        .and_then(Value::as_u64)
        .ok_or("Automatic selection requires the current expected_revision")?;
    if expected != doc.revision || p.get("document_id").is_some_and(|id| id != &doc.id) {
        return Err("Project changed before automatic selection. Observe again.".into());
    }
    let mode = p["mode"].as_str().unwrap_or("replace");
    if !["replace", "add", "subtract", "intersect"].contains(&mode) {
        return Err("Choose replace, add, subtract or intersect".into());
    }
    let request = request(&doc, p)?;
    let response = invoke(config, &request, || {
        let e = shared.lock().unwrap();
        e.doc.id == doc.id && e.doc.revision == doc.revision
    })?;
    if response["version"] != 1
        || response["document_id"] != doc.id
        || response["source_revision"] != doc.revision
        || response["document_rect"] != request["document_rect"]
        || response["channel"] != "luma"
    {
        return Err("Segmentation response does not match the requested project, revision, coordinates or luma confidence format".into());
    }
    let mut e = shared.lock().unwrap();
    if e.doc.id != doc.id || e.doc.revision != doc.revision {
        return Err(
            "Project changed before automatic selection completed; no result applied".into(),
        );
    }
    let command = json!({"op":"selection.import","document_id":doc.id,"source_revision":doc.revision,"rect":response["document_rect"],"png":response["png"],"channel":"luma","mode":mode});
    let result = e.edit(
        actor,
        &[command],
        Some(doc.revision),
        p.get("task").and_then(Value::as_str),
        "Automatic selection",
    )?;
    let mut result = result;
    result["document_id"] = json!(doc.id);
    result["document_rect"] = request["document_rect"].clone();
    result["source_revision"] = json!(doc.revision);
    Ok(result)
}
