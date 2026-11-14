//! Bounded procedural editing. Scripts see frozen sources and emit shared commands;
//! only the ordinary atomic engine transaction can change a live document.
use crate::{
    engine::{
        self,
        view::{self, DocumentView},
        Document, Engine, Scope,
    },
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
const STACK_BYTES: usize = 2 * 1024 * 1024;
static RUNNING: AtomicUsize = AtomicUsize::new(0);
mod native_source;
mod evaluation;
mod worker;
mod command;
mod native;
#[cfg(test)]
pub(crate) use native_source::tests as native_fixture;
/// Internal worker entry: no preferences, source namespace or UI is initialized.
#[doc(hidden)]
pub fn main_entry() -> Option<i32> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !args.first().is_some_and(|arg| arg == "--code-worker") { return None; }
    Some(if args.len() == 1 && worker::run().is_ok() {0} else {2})
}
mod job;
pub(crate) use job::Job;
use job::{Info, Outcome, Ownership, Running};
fn fail(message: impl Into<String>) -> Box<EvalAltResult> {
    message.into().into()
}
fn script_error(error: &EvalAltResult) -> String {
    match error {
        EvalAltResult::ErrorTerminated(reason, _) => reason
            .clone()
            .try_cast::<String>()
            .unwrap_or_else(|| "Code canceled; no edits committed".into()),
        EvalAltResult::ErrorInFunctionCall(_, _, inner, _) => script_error(inner),
        _ => error.to_string(),
    }
}
fn contained(doc: DocumentView<'_>, declared: &[Scope], actual: &[Scope]) -> Result<(), String> {
    if view::scopes_contained(doc, declared, actual) {
        Ok(())
    } else {
        Err("Code edit exceeds its declared/owned scopes; current work preserved".into())
    }
}
fn guard(e: &mut Engine, job: &Job, source: DocumentView<'_>) -> Result<(), String> {
    e.ensure_open()?;
    e.expire();
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Code run canceled; no edits committed".into());
    }
    if job.document != source.id() || !view::current(e).same_observation(source) {
        return Err("Code source changed; current work preserved".into());
    }
    if view::current(e).read_only() {
        return Err("This document is read-only".into());
    }
    let lease = e
        .leases
        .iter()
        .find(|l| l.id == job.task && l.owner == job.actor)
        .ok_or("Code task ended, expired or was taken over; do not silently reacquire")?;
    contained(view::current(e), &lease.scopes, &job.scopes)?;
    e.check(&job.actor, &job.scopes)?;
    for layer in view::current(e).layers() {
        if layer.locked()
            && job.scopes.iter().any(|s| {
                s.target.is_none()
                    || s.target.as_deref() == Some(layer.id())
                    || s.target.as_ref().is_some_and(|id| {
                        view::descendant(view::current(e), layer.id(), id)
                            || view::descendant(view::current(e), id, layer.id())
                    })
            })
        {
            return Err("Unlock the declared layers before running code".into());
        }
    }
    Ok(())
}
pub fn dispatch(shared: &Shared, actor: &str, p: &Value) -> Result<Value, String> {
    dispatch_owned(shared,actor,p,&mut crate::server::reply::ReplyAdmission::new(false))
}
pub(crate) fn dispatch_owned(shared:&Shared,actor:&str,p:&Value,admission:&mut crate::server::reply::ReplyAdmission)->Result<Value,String> {
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
            if let Some(pool)=job.native.as_ref().map(|owner|&owner.pool).or_else(||e.native_document().map(|document|document.budget())) {
                admission.admit(pool,0)?;
            }
            let job=job.clone();
            if p["action"] == "cancel" {
                job.cancel.store(true, Ordering::Relaxed);
            }
            drop(e);
            job.state(admission)
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
            if view::current(&e).id() != document || view::current(&e).revision() != revision {
                return Err("Code needs the current source identity/revision".into());
            }
            let ownership=if let Some(source)=e.native_document() {
                worker::supported()?;
                admission.admit(source.budget(),4096)?;
                Some(native::ownership(source,p)?)
            } else {None};
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
            let scope_values=p["scopes"]
                .as_array()
                .filter(|values|!values.is_empty()&&values.len()<=16)
                .ok_or("Declare 1–16 code editing scopes")?;
            for value in scope_values {
                if value
                    .as_object()
                    .is_none_or(|s| s.keys().any(|k| k != "target" && k != "rect"))
                {
                    return Err("Unknown or invalid code scope field".into());
                }
            }
            let scopes: Vec<Scope> = serde::Deserialize::deserialize(&p["scopes"])
                .map_err(|_| "Invalid code scopes")?;
            if scopes.is_empty() || scopes.len() > 16 {
                return Err("Declare 1–16 code editing scopes".into());
            }
            for scope in &scopes {
                if let Some(id) = &scope.target {
                    if id != "@selection" && view::current(&e).find_layer(id).is_none() {
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
            let job = Job::new(Info {
                id: engine::id(),
                actor: actor.into(),
                document: document.into(),
                task: p["task"]
                    .as_str()
                    .ok_or("Code requires an active owned task")?
                    .into(),
                scopes,
                label,
                native: ownership,
            });
            let source=e.doc_snapshot();
            guard(&mut e,&job,source.view())?;
            drop(e);
            let file_operation=if let view::DocumentSnapshot::Native(document)=&source {
                let operation=crate::server::FileOperation::begin(shared)?;
                let runtime=crate::workspace::native_dispatch::runtime_for(shared)?;
                if !document.budget().same_pool(&runtime.resident()) {
                    return Err("Code source belongs to another configured service".into());
                }
                Some(operation)
            } else {None};
            let mut e=shared.lock().unwrap();
            if e.project_id!=project {return Err("Code project identity changed".into());}
            guard(&mut e,&job,source.view())?;
            if e.has_running_code() {
                return Err("A code run is already active in this project".into());
            }
            let permit=Running::acquire()?;
            let queued=job.state(admission)?;
            if e.code_jobs.len()<4 {e.code_jobs.try_reserve(1).map_err(|_|"Code record allocation refused")?;}
            e.mark_code_start(actor,&job.label,&job.scopes)?;
            while e.code_jobs.len() >= 4 {
                e.code_jobs.remove(0);
            }
            e.code_jobs.push(job.clone());
            drop(e);
            let shared = shared.clone();
            let worker_job = job.clone();
            let spawned = std::thread::Builder::new()
                .name("peerbrush-code".into())
                .stack_size(STACK_BYTES)
                .spawn(move || {
                    let _permit=permit;
                    let _operation=file_operation;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        match source {
                            view::DocumentSnapshot::Resident(source)=>run(&shared,&worker_job,Arc::unwrap_or_clone(source),&script,edge).map(Outcome::Resident),
                            view::DocumentSnapshot::Native(source)=>native::apply(&shared,&worker_job,&source,&script,edge,Instant::now())
                                .map(|completion|Outcome::Native(Arc::new(completion))),
                        }
                    }))
                    .unwrap_or_else(|_| Err("Code worker failed; current work preserved".into()));
                    drop(script);
                    let result=worker_job.recover_result(result);
                    *worker_job.result.lock().unwrap_or_else(|error|error.into_inner()) = Some(result);
                });
            if let Err(error) = spawned {
                *job.result.lock().unwrap_or_else(|error|error.into_inner()) =
                    Some(Err(format!("Cannot start code worker: {error}")));
                return Err("Cannot start code worker".into());
            }
            Ok(queued)
        }
        _ => Err("Unknown code action".into()),
    }
}

fn run(
    shared: &Shared,
    job: &Job,
    source: Document,
    script: &str,
    edge: u32,
) -> Result<Value, String> {
    let start = Instant::now();
    let commands = evaluation::commands(shared, job, &source, script, start)?;
    let mut draft = source.clone();
    for command in &commands {
        if start.elapsed() > Duration::from_secs(30) {
            return Err("Code preparation time limit exceeded; no edits committed".into());
        }
        {
            let mut e = shared.lock().unwrap();
            guard(&mut e, job, DocumentView::from(&source))?;
        }
        let inspector = Engine::from_document(draft);
        let actual = inspector.scopes(command);
        draft = inspector.into_resident_document()?;
        contained(DocumentView::from(&draft), &job.scopes, &actual)?;
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
    guard(&mut e, job, DocumentView::from(&source))?;
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
    replacement_shape([doc.width, doc.height], doc.bit_depth, c)?;
    let bytes = c["rgba"]
        .as_str()
        .filter(|s| s.len() <= MAX_PIXELS * 8 * 4 / 3 + 4)
        .ok_or("Invalid bounded base64 RGBA data")?;
    let bytes = STANDARD
        .decode(bytes)
        .map_err(|_| "Invalid base64 RGBA data")?;
    replace_pixels_bytes(doc, c, &bytes, SampleOrder::Little)
}

/// Canonical backing is BE; the existing JSON protocol remains LE.
#[derive(Clone, Copy)]
pub(crate) enum SampleOrder {
    Little,
    Big,
}

pub(crate) fn replacement_shape(
    size: [u32; 2],
    depth: u16,
    c: &Value,
) -> Result<([i32; 4], usize), String> {
    if c["mask"] == true {
        return Err("Native pixel replacement targets layer color".into());
    }
    if c["bit_depth"].as_u64() != Some(depth as u64) {
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
                && r[2] <= size[0] as i32
                && r[3] <= size[1] as i32
        })
        .ok_or("Pixel rectangle must be inside the document")?;
    let count = (r[2] - r[0]) as usize * (r[3] - r[1]) as usize;
    if count > MAX_PIXELS {
        return Err("Native pixel patch exceeds 262144 pixels".into());
    }
    Ok((r, count))
}

pub(crate) fn replace_pixels_bytes(
    doc: &mut Document,
    c: &Value,
    bytes: &[u8],
    order: SampleOrder,
) -> Result<(), String> {
    let (r, count) = replacement_shape([doc.width, doc.height], doc.bit_depth, c)?;
    let stride = if doc.bit_depth == 16 { 8 } else { 4 };
    if bytes.len() != count * stride {
        return Err("Native RGBA length does not match rectangle/depth".into());
    }
    let target = c["layer"].as_str().ok_or("Choose a paint layer")?;
    crate::placement::unlocked(doc, target)?;
    let selected: Vec<bool> = (r[1]..r[3])
        .flat_map(|y| (r[0]..r[2]).map(move |x| (x, y)))
        .map(|(x, y)| {
            doc.selection
                .is_none_or(|s| x >= s[0] && y >= s[1] && x < s[2] && y < s[3])
                && crate::selection::contains_pixel(doc, x, y)
        })
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
            layer.pixels.set16(
                x - layer.x,
                y - layer.y,
                replacement_pixel(bytes, doc.bit_depth, order, at),
            );
        }
    }
    Ok(())
}

/// Callers first validate the complete buffer length and rectangle/depth.
pub(crate) fn replacement_pixel(
    bytes: &[u8],
    depth: u16,
    order: SampleOrder,
    at: usize,
) -> [u16; 4] {
    std::array::from_fn(|i| {
        if depth == 8 {
            u16::from(bytes[at + i]) * 257
        } else {
            let word = [bytes[at + i * 2], bytes[at + i * 2 + 1]];
            match order {
                SampleOrder::Little => u16::from_le_bytes(word),
                SampleOrder::Big => u16::from_be_bytes(word),
            }
        }
    })
}

#[cfg(test)]
mod replacement_tests {
    use super::*;
    #[test]
    fn replacement_shape_limit_depth_and_sample_order_are_exact() {
        let valid = json!({"op":"pixels.replace","bit_depth":16,"rect":[0,0,512,512]});
        assert_eq!(
            replacement_shape([1024, 1024], 16, &valid).unwrap().1,
            MAX_PIXELS
        );
        let mut too_large = valid.clone();
        too_large["rect"] = json!([0, 0, 513, 512]);
        assert!(replacement_shape([1024, 1024], 16, &too_large)
            .unwrap_err()
            .contains("262144"));
        assert!(replacement_shape([1024, 1024], 8, &valid)
            .unwrap_err()
            .contains("depth"));
        let words = [1u16, 257, 65534, 42];
        let little: Vec<u8> = words.into_iter().flat_map(u16::to_le_bytes).collect();
        let big: Vec<u8> = words.into_iter().flat_map(u16::to_be_bytes).collect();
        assert_eq!(
            replacement_pixel(&little, 16, SampleOrder::Little, 0),
            words
        );
        assert_eq!(replacement_pixel(&big, 16, SampleOrder::Big, 0), words);
        assert_eq!(
            replacement_pixel(&[0, 1, 127, 255], 8, SampleOrder::Little, 0),
            [0, 257, 32639, 65535]
        );
    }
}
