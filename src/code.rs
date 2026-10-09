//! Bounded procedural editing. Scripts see frozen sources and emit shared commands;
//! only the ordinary atomic engine transaction can change a live document.
use crate::{
    engine::{self, Document, Engine, Scope},
    server::Shared,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rhai::{Array, Dynamic, EvalAltResult, ImmutableString, Map, INT};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub const MAX_PIXELS: usize = 262_144;
const MAX_BYTES: usize = 8 * 1024 * 1024;
static RUNNING: AtomicUsize = AtomicUsize::new(0);
#[derive(Clone)]
pub(crate) struct Job {
    id: String,
    actor: String,
    document: String,
    task: String,
    scopes: Vec<Scope>,
    label: String,
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<Value, String>>>>,
}
impl Job {
    fn state(&self) -> Value {
        let result = self.result.lock().unwrap();
        let mut state = json!({"run":self.id,"actor":self.actor,"document_id":self.document,"task":self.task,"description":self.label,"scopes":self.scopes,"status":if result.is_none(){"running"}else{"finished"}});
        if let Some(result) = result.as_ref() {
            match result {
                Ok(value) => {
                    state["status"] = json!("committed");
                    state["result"] = value.clone();
                    let images = state["result"]
                        .as_object_mut()
                        .unwrap()
                        .remove("images")
                        .unwrap_or(json!([]));
                    state["images"] = images;
                }
                Err(error) => {
                    state["status"] = json!("discarded");
                    state["error"] = json!(error);
                }
            }
        }
        state
    }
}
impl Engine {
    pub(crate) fn code_states(&self) -> Value {
        json!(self.code_jobs.iter().map(|j| {
            let result=j.result.lock().unwrap();
            json!({"run":j.id,"actor":j.actor,"document_id":j.document,"description":j.label,"scopes":j.scopes,"running":result.is_none(),"status":match result.as_ref(){None=>"Working",Some(Ok(_))=>"Applied",Some(Err(_))=>"Discarded"},"error":result.as_ref().and_then(|r|r.as_ref().err())})
        }).collect::<Vec<_>>())
    }
    pub(crate) fn cancel_code(&self, id: &str) {
        if let Some(job) = self.code_jobs.iter().find(|j| j.id == id) {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}
fn fail(message: impl Into<String>) -> Box<EvalAltResult> {
    message.into().into()
}
fn descendant(doc: &Document, child: &str, ancestor: &str) -> bool {
    let mut parent = doc
        .layers
        .iter()
        .find(|l| l.id == child)
        .and_then(|l| l.parent.as_deref());
    for _ in 0..16 {
        let Some(id) = parent else { return false };
        if id == ancestor {
            return true;
        }
        parent = doc
            .layers
            .iter()
            .find(|l| l.id == id)
            .and_then(|l| l.parent.as_deref());
    }
    false
}
fn covers(doc: &Document, outer: &Scope, inner: &Scope) -> bool {
    let target = match outer.target.as_deref() {
        None => true,
        Some(a) => inner.target.as_deref().is_some_and(|b| {
            let b = b.strip_prefix("@visibility:").unwrap_or(b);
            a == b || descendant(doc, b, a)
        }),
    };
    target
        && match (outer.rect, inner.rect) {
            (None, _) => true,
            (Some(a), Some(b)) => a[0] <= b[0] && a[1] <= b[1] && a[2] >= b[2] && a[3] >= b[3],
            _ => false,
        }
}
fn contained(doc: &Document, declared: &[Scope], actual: &[Scope]) -> Result<(), String> {
    if actual
        .iter()
        .all(|s| declared.iter().any(|d| covers(doc, d, s)))
    {
        Ok(())
    } else {
        Err("Code edit exceeds its declared/owned scopes; current work preserved".into())
    }
}
fn guard(e: &mut Engine, job: &Job, source: &Document) -> Result<(), String> {
    e.ensure_open()?;
    e.expire();
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Code run canceled; no edits committed".into());
    }
    if e.doc.id != source.id || e.doc.revision != source.revision {
        return Err("Code source changed; current work preserved".into());
    }
    if e.doc.read_only {
        return Err("This document is read-only".into());
    }
    let lease = e
        .leases
        .iter()
        .find(|l| l.id == job.task && l.owner == job.actor)
        .ok_or("Code task ended, expired or was taken over; do not silently reacquire")?;
    contained(&e.doc, &lease.scopes, &job.scopes)?;
    e.check(&job.actor, &job.scopes)?;
    for layer in &e.doc.layers {
        if layer.locked
            && job.scopes.iter().any(|s| {
                s.target.is_none()
                    || s.target.as_deref() == Some(&layer.id)
                    || s.target.as_ref().is_some_and(|id| {
                        descendant(&e.doc, &layer.id, id) || descendant(&e.doc, id, &layer.id)
                    })
            })
        {
            return Err("Unlock the declared layers before running code".into());
        }
    }
    Ok(())
}
pub fn dispatch(shared: &Shared, actor: &str, p: &Value) -> Result<Value, String> {
    let object = p.as_object().ok_or("Code arguments must be an object")?;
    for key in object.keys() {
        if ![
            "action",
            "actor",
            "project_id",
            "document_id",
            "expected_revision",
            "task",
            "description",
            "scopes",
            "script",
            "run",
            "max_edge",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("Unknown code argument: {key}"));
        }
    }
    let project = p["project_id"]
        .as_str()
        .ok_or("Code needs explicit project_id")?;
    let document = p["document_id"]
        .as_str()
        .ok_or("Code needs explicit document_id")?;
    let mut e = shared.lock().unwrap();
    if project != e.project_id {
        return Err("Code project identity changed".into());
    }
    match p["action"].as_str().ok_or("Missing code action")? {
        "status" | "cancel" => {
            let id = p["run"].as_str().ok_or("Missing code run ID")?;
            let job = e
                .code_jobs
                .iter()
                .find(|j| j.id == id && j.document == document)
                .ok_or("Code run not found in this project")?;
            if actor != "human" && actor != job.actor {
                return Err("Only the run owner or human may inspect/cancel code".into());
            }
            if p["action"] == "cancel" {
                job.cancel.store(true, Ordering::Relaxed);
            }
            Ok(job.state())
        }
        "start" => {
            if p["actor"].as_str() != Some(actor)
                || actor == "human"
                || actor.is_empty()
                || actor.len() > 128
            {
                return Err("Code needs a named AI actor".into());
            }
            let revision = p["expected_revision"]
                .as_u64()
                .ok_or("Code needs exact expected_revision")?;
            if e.doc.id != document || e.doc.revision != revision {
                return Err("Code needs the current source identity/revision".into());
            }
            let script = p["script"]
                .as_str()
                .filter(|s| !s.trim().is_empty() && s.len() <= 65536)
                .ok_or("Script must contain 1–65536 UTF-8 bytes")?
                .to_owned();
            let label = p["description"]
                .as_str()
                .filter(|s| !s.trim().is_empty() && s.len() <= 240)
                .ok_or("Describe what the code edits in 1–240 bytes")?
                .to_owned();
            for value in p["scopes"]
                .as_array()
                .ok_or("Code scopes must be an array")?
            {
                if value
                    .as_object()
                    .is_none_or(|s| s.keys().any(|k| k != "target" && k != "rect"))
                {
                    return Err("Unknown or invalid code scope field".into());
                }
            }
            let scopes: Vec<Scope> =
                serde_json::from_value(p["scopes"].clone()).map_err(|_| "Invalid code scopes")?;
            if scopes.is_empty() || scopes.len() > 16 {
                return Err("Declare 1–16 code editing scopes".into());
            }
            for scope in &scopes {
                if let Some(id) = &scope.target {
                    if id != "@selection" && !e.doc.layers.iter().any(|l| l.id == *id) {
                        return Err("Unknown declared layer".into());
                    }
                }
                if scope.rect.is_some_and(|r| {
                    r[0] >= r[2] || r[1] >= r[3] || r.iter().any(|v| !(-8192..=16384).contains(v))
                }) {
                    return Err("Invalid code scope rectangle".into());
                }
            }
            let edge = p.get("max_edge").map_or(Ok(512), |v| {
                v.as_u64()
                    .filter(|v| (32..=1024).contains(v))
                    .map(|v| v as u32)
                    .ok_or("Code feedback max_edge must be 32–1024")
            })?;
            let job = Job {
                id: engine::id(),
                actor: actor.into(),
                document: document.into(),
                task: p["task"]
                    .as_str()
                    .ok_or("Code requires an active owned task")?
                    .into(),
                scopes,
                label,
                cancel: Arc::new(AtomicBool::new(false)),
                result: Arc::new(Mutex::new(None)),
            };
            let source = e.doc.clone();
            guard(&mut e, &job, &source)?;
            if e.code_jobs
                .iter()
                .any(|j| j.result.lock().unwrap().is_none())
            {
                return Err("A code run is already active in this project".into());
            }
            if RUNNING
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                    (n < 2).then_some(n + 1)
                })
                .is_err()
            {
                return Err("Two code runs are already active; wait for completion".into());
            }
            while e.code_jobs.len() >= 4 {
                e.code_jobs.remove(0);
            }
            e.mark_ai(actor, &job.label, "canvas", job.scopes.clone());
            e.activity = job.label.clone();
            e.code_jobs.push(job.clone());
            let queued = job.state();
            drop(e);
            let shared = shared.clone();
            let worker_job = job.clone();
            let spawned = std::thread::Builder::new()
                .name("peerbrush-code".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run(&shared, &worker_job, source, &script, edge)
                    }))
                    .unwrap_or_else(|_| Err("Code worker failed; current work preserved".into()));
                    *worker_job.result.lock().unwrap() = Some(result);
                    RUNNING.fetch_sub(1, Ordering::SeqCst);
                });
            if let Err(error) = spawned {
                RUNNING.fetch_sub(1, Ordering::SeqCst);
                *job.result.lock().unwrap() =
                    Some(Err(format!("Cannot start code worker: {error}")));
                return Err("Cannot start code worker".into());
            }
            Ok(queued)
        }
        _ => Err("Unknown code action".into()),
    }
}

struct Patch {
    layer: String,
    rect: [i32; 4],
    words: Vec<u16>,
}
struct Context {
    doc: Document,
    scopes: Vec<Scope>,
    patches: BTreeMap<INT, Patch>,
    next: INT,
    pixels: usize,
    bytes: usize,
    commands: Vec<Value>,
}
impl Context {
    fn push(&mut self, command: Value) -> Result<(), String> {
        if self.commands.len() >= 32 {
            return Err("Code exceeds 32 commands".into());
        }
        self.bytes += serde_json::to_vec(&command)
            .map_err(|e| e.to_string())?
            .len();
        if self.bytes > MAX_BYTES {
            return Err("Code exceeds 8 MiB command output".into());
        }
        self.commands.push(command);
        Ok(())
    }
    fn pixel(&self, layer: &str, x: INT, y: INT) -> Result<[u16; 4], String> {
        if x < 0 || y < 0 || x >= self.doc.width as INT || y >= self.doc.height as INT {
            return Err("Pixel coordinates must be inside the document".into());
        }
        let l = self
            .doc
            .layers
            .iter()
            .find(|l| l.id == layer)
            .ok_or("Unknown pixel source")?;
        if l.kind != "paint" || l.source.is_some() {
            return Err("Native pixel access requires a paint layer".into());
        }
        Ok(if self.doc.bit_depth == 16 {
            l.pixels.get16(x as i32 - l.x, y as i32 - l.y)
        } else {
            l.pixels.get(x as i32 - l.x, y as i32 - l.y).map(u16::from)
        })
    }
}
fn rect_array(values: Array) -> Result<[i32; 4], Box<EvalAltResult>> {
    let value = rhai::serde::from_dynamic::<Value>(&Dynamic::from_array(values))
        .map_err(|e| fail(e.to_string()))?;
    engine::rect(&value)
        .filter(|r| r[0] < r[2] && r[1] < r[3])
        .ok_or_else(|| fail("Use a nonempty integer rectangle"))
}
fn run(
    shared: &Shared,
    job: &Job,
    source: Document,
    script: &str,
    edge: u32,
) -> Result<Value, String> {
    let start = Instant::now();
    let context = Arc::new(Mutex::new(Context {
        doc: source.clone(),
        scopes: job.scopes.clone(),
        patches: BTreeMap::new(),
        next: 0,
        pixels: 0,
        bytes: 0,
        commands: vec![],
    }));
    let mut runtime = rhai::Engine::new();
    runtime
        .set_max_operations(2_000_000)
        .set_max_variables(64)
        .set_max_functions(32)
        .set_max_call_levels(16)
        .set_max_expr_depths(32, 16)
        .set_max_array_size(4096)
        .set_max_map_size(1024)
        .set_max_string_size(16384)
        .set_optimization_level(rhai::OptimizationLevel::None);
    runtime.disable_symbol("eval");
    runtime.on_print(|_| {});
    runtime.on_debug(|_, _, _| {});
    let (live, control, frozen) = (shared.clone(), job.clone(), source.clone());
    runtime.on_progress(move |count| {
        if control.cancel.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(5) {
            return Some(Dynamic::from(
                "Code canceled or evaluation time limit exceeded",
            ));
        }
        if count % 1024 == 0 {
            if let Ok(mut e) = live.try_lock() {
                if let Err(error) = guard(&mut e, &control, &frozen) {
                    return Some(Dynamic::from(error));
                }
            }
        }
        None
    });
    let c = context.clone();
    runtime.register_fn(
        "read_pixel",
        move |layer: ImmutableString, x: INT, y: INT| -> Result<Array, Box<EvalAltResult>> {
            Ok(c.lock()
                .unwrap()
                .pixel(&layer, x, y)
                .map_err(fail)?
                .map(|v| Dynamic::from(v as INT))
                .to_vec())
        },
    );
    let c = context.clone();
    runtime.register_fn(
        "begin_pixels",
        move |layer: ImmutableString, values: Array| -> Result<INT, Box<EvalAltResult>> {
            let rect = rect_array(values)?;
            let mut c = c.lock().unwrap();
            if rect[0] < 0
                || rect[1] < 0
                || rect[2] > c.doc.width as i32
                || rect[3] > c.doc.height as i32
            {
                return Err(fail("Pixel rectangle must be inside the document"));
            }
            contained(
                &c.doc,
                &c.scopes,
                &[Scope {
                    target: Some(layer.to_string()),
                    rect: Some(rect),
                }],
            )
            .map_err(fail)?;
            let pixels = (rect[2] - rect[0]) as usize * (rect[3] - rect[1]) as usize;
            if c.pixels + pixels > MAX_PIXELS || c.patches.len() >= 8 {
                return Err(fail("Code exceeds 262144 patch pixels or 8 open buffers"));
            }
            let mut words = Vec::with_capacity(pixels * 4);
            for y in rect[1]..rect[3] {
                for x in rect[0]..rect[2] {
                    words.extend_from_slice(&c.pixel(&layer, x as INT, y as INT).map_err(fail)?);
                }
            }
            c.pixels += pixels;
            let handle = c.next;
            c.next += 1;
            c.patches.insert(
                handle,
                Patch {
                    layer: layer.into(),
                    rect,
                    words,
                },
            );
            Ok(handle)
        },
    );
    let c = context.clone();
    runtime.register_fn(
        "write_pixel",
        move |handle: INT, x: INT, y: INT, rgba: Array| -> Result<(), Box<EvalAltResult>> {
            let mut c = c.lock().unwrap();
            let max = if c.doc.bit_depth == 16 { 65535 } else { 255 };
            if rgba.len() != 4 {
                return Err(fail("RGBA needs four native integers"));
            }
            let mut pixel = [0u16; 4];
            for (i, v) in rgba.into_iter().enumerate() {
                pixel[i] = v
                    .as_int()
                    .ok()
                    .filter(|v| (0..=max).contains(v))
                    .ok_or_else(|| fail("Native channel value is out of range"))?
                    as u16;
            }
            let patch = c
                .patches
                .get_mut(&handle)
                .ok_or_else(|| fail("Unknown pixel buffer"))?;
            let r = patch.rect;
            if x < r[0] as INT || y < r[1] as INT || x >= r[2] as INT || y >= r[3] as INT {
                return Err(fail("Write exceeds the pixel buffer rectangle"));
            }
            let at = ((y - r[1] as INT) * (r[2] - r[0]) as INT + x - r[0] as INT) as usize * 4;
            patch.words[at..at + 4].copy_from_slice(&pixel);
            Ok(())
        },
    );
    let c = context.clone();
    runtime.register_fn("commit_pixels",move |handle:INT|->Result<(),Box<EvalAltResult>> {
        let mut c=c.lock().unwrap();let patch=c.patches.remove(&handle).ok_or_else(||fail("Unknown pixel buffer"))?;
        let depth=c.doc.bit_depth;
        let bytes=if depth==16 {patch.words.into_iter().flat_map(u16::to_le_bytes).collect()} else {patch.words.into_iter().map(|v|v as u8).collect::<Vec<_>>()};
        c.push(json!({"op":"pixels.replace","layer":patch.layer,"rect":patch.rect,"bit_depth":depth,"rgba":STANDARD.encode(bytes)})).map_err(fail)
    });
    let c = context.clone();
    runtime.register_fn("edit",move |map:Map|->Result<(),Box<EvalAltResult>> {
        let command=rhai::serde::from_dynamic::<Value>(&Dynamic::from_map(map)).map_err(|e|fail(e.to_string()))?;
        let op=command["op"].as_str().unwrap_or("");
        if !["paint","shape","gradient","fill","adjust","move","transform","layer.update","effect.add","effect.update","effect.delete","effect.reorder","mask.add","mask.delete","mask.paint","mask.update","mask.step.add","mask.step.update","mask.step.delete","mask.step.reorder","filter.add","filter.update","filter.delete","filter.reorder","source.update","selection","selection.refine"].contains(&op) {return Err(fail("Operation is not exposed to code; use native pixel buffers or documented editing commands"));}
        if command.as_object().unwrap().keys().any(|k|["path","png","rgba","preset","locked","source_revision","document_id"].contains(&k.as_str())) {return Err(fail("Code cannot access files, resolve mutable presets, change locks or override source guards"));}
        c.lock().unwrap().push(command).map_err(fail)
    });
    let metadata = json!({"width":source.width,"height":source.height,"bit_depth":source.bit_depth,"channel_max":if source.bit_depth==16{65535}else{255},"layers":source.layers.iter().map(|l|json!({"id":l.id,"name":l.name,"kind":l.kind,"parent":l.parent,"x":l.x,"y":l.y,"locked":l.locked})).collect::<Vec<_>>()});
    let mut scope = rhai::Scope::new();
    scope.push_constant_dynamic(
        "document",
        rhai::serde::to_dynamic(metadata).map_err(|e| e.to_string())?,
    );
    runtime
        .run_with_scope(&mut scope, script)
        .map_err(|e| format!("Code discarded: {e}"))?;
    let commands = {
        let c = context.lock().unwrap();
        if !c.patches.is_empty() {
            return Err("Commit every open pixel buffer before finishing the script".into());
        }
        c.commands.clone()
    };
    if commands.is_empty() {
        return Err("Code produced no edits".into());
    }
    let mut draft = source.clone();
    for command in &commands {
        if start.elapsed() > Duration::from_secs(30) {
            return Err("Code preparation time limit exceeded; no edits committed".into());
        }
        {
            let mut e = shared.lock().unwrap();
            guard(&mut e, job, &source)?;
        }
        let mut inspector = Engine::new();
        inspector.doc = draft.clone();
        let actual = inspector.scopes(command);
        contained(&draft, &job.scopes, &actual)?;
        if let Some(target) = command["layer"].as_str() {
            crate::placement::unlocked(&draft, target)?;
        }
        // Guard each successive footprint, including any prior selection/frame changes.
        draft = Engine::preview_edits(draft, &[command.clone()])?;
    }
    draft.revision = source.revision + 1;
    let (w, h, pixels, rect) = draft.preview(None, edge, None, false)?;
    let png = crate::raster::png(w, h, &pixels)?;
    if start.elapsed() > Duration::from_secs(30) {
        return Err("Code preparation time limit exceeded; no edits committed".into());
    }
    let mut e = shared.lock().unwrap();
    guard(&mut e, job, &source)?;
    let mut result = e.commit_code(&job.actor, &commands, &job.task, &job.label, draft)?;
    e.mark_ai(&job.actor, &job.label, "canvas", job.scopes.clone());
    result["document_id"] = json!(source.id);
    result["scopes"] = json!(job.scopes);
    result["images"] = json!([{"mime_type":"image/png","data":STANDARD.encode(png),"width":w,"height":h,"document_rect":rect,"revision":source.revision+1}]);
    Ok(result)
}

/// Native rectangular replacement is shared by UI/protocol/code, with source guards,
/// selection confidence, locks, reservations and rollback supplied by the engine.
pub(crate) fn replace_pixels(doc: &mut Document, c: &Value) -> Result<(), String> {
    if c["mask"] == true {
        return Err("Native pixel replacement targets layer color".into());
    }
    if c["bit_depth"].as_u64() != Some(doc.bit_depth as u64) {
        return Err("Native pixel depth must match the document".into());
    }
    let r = c
        .get("rect")
        .and_then(engine::rect)
        .filter(|r| {
            r[0] >= 0
                && r[1] >= 0
                && r[0] < r[2]
                && r[1] < r[3]
                && r[2] <= doc.width as i32
                && r[3] <= doc.height as i32
        })
        .ok_or("Pixel rectangle must be inside the document")?;
    let count = (r[2] - r[0]) as usize * (r[3] - r[1]) as usize;
    if count > MAX_PIXELS {
        return Err("Native pixel patch exceeds 262144 pixels".into());
    }
    let bytes = c["rgba"]
        .as_str()
        .filter(|s| s.len() <= MAX_PIXELS * 8 * 4 / 3 + 4)
        .ok_or("Invalid bounded base64 RGBA data")?;
    let bytes = STANDARD
        .decode(bytes)
        .map_err(|_| "Invalid base64 RGBA data")?;
    let stride = if doc.bit_depth == 16 { 8 } else { 4 };
    if bytes.len() != count * stride {
        return Err("Native RGBA length does not match rectangle/depth".into());
    }
    let target = c["layer"].as_str().ok_or("Choose a paint layer")?;
    crate::placement::unlocked(doc, target)?;
    let selected: Vec<bool> = (r[1]..r[3])
        .flat_map(|y| (r[0]..r[2]).map(move |x| (x, y)))
        .map(|(x, y)| crate::selection::contains_pixel(doc, x, y))
        .collect();
    let layer = doc
        .layers
        .iter_mut()
        .find(|l| l.id == target)
        .ok_or("Unknown paint layer")?;
    if layer.kind != "paint" || layer.source.is_some() {
        return Err("Native pixel replacement requires a paint layer".into());
    }
    for y in r[1]..r[3] {
        for x in r[0]..r[2] {
            if !selected[(y - r[1]) as usize * (r[2] - r[0]) as usize + (x - r[0]) as usize] {
                continue;
            }
            let at = ((y - r[1]) as usize * (r[2] - r[0]) as usize + (x - r[0]) as usize) * stride;
            if doc.bit_depth == 16 {
                let pixel = std::array::from_fn(|i| {
                    u16::from_le_bytes([bytes[at + i * 2], bytes[at + i * 2 + 1]])
                });
                layer.pixels.set16(x - layer.x, y - layer.y, pixel);
            } else {
                layer.pixels.set(
                    x - layer.x,
                    y - layer.y,
                    bytes[at..at + 4].try_into().unwrap(),
                );
            }
        }
    }
    Ok(())
}
