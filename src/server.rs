use crate::{
    engine::{self, Engine, Scope},
    psd, raster,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

pub type Shared = Arc<Mutex<Engine>>;
pub struct Connection {
    pub port: u16,
    pub token: String,
    pub state_dir: PathBuf,
    pub instance_lock: Option<fs::File>,
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("{}.tmp", engine::id()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
fn file_version(path: &Path) -> Result<(u64, u128), String> {
    let m = fs::metadata(path).map_err(|e| e.to_string())?;
    Ok((
        m.len(),
        m.modified()
            .map_err(|e| e.to_string())?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    ))
}
pub fn open(shared: &Shared, path: &Path) -> Result<(), String> {
    let before = {
        let e = shared.lock().unwrap();
        (e.doc.id.clone(), e.doc.revision)
    };
    let version = file_version(path)?;
    if version.0 > 256 * 1024 * 1024 {
        return Err("PSD exceeds the initial 256 MiB file limit".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if file_version(path)? != version {
        return Err("File changed while opening; try again".into());
    }
    let mut doc = psd::decode(&bytes)?;
    doc.name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into();
    let mut e = shared.lock().unwrap();
    if (e.doc.id.clone(), e.doc.revision) != before {
        return Err("Canvas changed while opening. Save the current work and try again.".into());
    }
    e.replace(doc, Some(path.into()))?;
    e.file_version = Some(version);
    Ok(())
}
pub fn save(shared: &Shared, path: &Path) -> Result<(), String> {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| !e.eq_ignore_ascii_case("psd"))
        .unwrap_or(true)
    {
        return Err("Working documents must use .psd".into());
    }
    let (doc, expected) = {
        let e = shared.lock().unwrap();
        (
            e.doc.clone(),
            if e.path.as_deref() == Some(path) {
                e.file_version
            } else {
                None
            },
        )
    };
    if let Some(expected) = expected {
        if file_version(path)? != expected {
            return Err(
                "PSD changed outside PeerBrush. Use Save As to preserve both versions.".into(),
            );
        }
    }
    let bytes = psd::encode(&doc)?;
    if let Some(expected) = expected {
        if file_version(path)? != expected {
            return Err("PSD changed while preparing the save. Use Save As.".into());
        }
    }
    atomic_write(path, &bytes)?;
    let version = file_version(path)?;
    let mut e = shared.lock().unwrap();
    if e.doc.id == doc.id {
        e.path = Some(path.into());
        e.file_version = Some(version);
        e.doc.name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into();
        e.saved_revision = doc.revision;
    }
    Ok(())
}
pub fn export(shared: &Shared, path: &Path) -> Result<(), String> {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| !e.eq_ignore_ascii_case("png"))
        .unwrap_or(true)
    {
        return Err("PNG is the only export format".into());
    }
    let doc = shared.lock().unwrap().doc.clone();
    atomic_write(path, &doc.export_png()?)
}

pub fn tools() -> Value {
    json!([
        {"name":"peerbrush_observe","description":"Inspect live document structure and actual PNG image content. Coordinates are document pixels, origin top left. Request layer/mask/rect views, max_edge and since_revision to reduce image traffic.","inputSchema":{"type":"object","properties":{"layer":{"type":"string"},"mask":{"type":"boolean"},"rect":{"type":"array","items":{"type":"integer"},"minItems":4,"maxItems":4},"max_edge":{"type":"integer","minimum":32,"maximum":4096},"since_revision":{"type":"integer"},"image":{"type":"boolean"}},"additionalProperties":false}},
        {"name":"peerbrush_edit","description":"Atomically apply typed editing commands to the shared document. First observe for IDs and revision. Include expected_revision and task when reserved. Inspect capabilities for command examples. All changes are undoable; no screen-coordinate clicking or code evaluation.","inputSchema":{"type":"object","properties":{"actor":{"type":"string"},"commands":{"type":"array","items":{"type":"object"},"minItems":1,"maxItems":100},"expected_revision":{"type":"integer"},"task":{"type":"string"},"label":{"type":"string"},"feedback":{"type":"string","enum":["batch","request","always"]},"max_edge":{"type":"integer"}},"required":["commands"]}},
        {"name":"peerbrush_task","description":"Begin/update/end a selective reservation for layers or rectangular document regions. Scopes have optional target (layer ID) and rect [left,top,right,bottom]. Descriptions appear live in the top bar: write concise natural-language activity, update as you work. Empty scopes permit cooperative edits without locking. Reservations expire after five idle minutes; user takeover revokes them. Never silently reacquire after takeover.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["begin","update","end","status"]},"actor":{"type":"string"},"task":{"type":"string"},"description":{"type":"string"},"scopes":{"type":"array","items":{"type":"object","properties":{"target":{"type":["string","null"]},"rect":{"type":["array","null"],"items":{"type":"integer"},"minItems":4,"maxItems":4}}}},"feedback":{"type":"string","enum":["batch","request","always"]}},"required":["action"]}},
        {"name":"peerbrush_document","description":"New/open/save PSD or export PNG. Use explicit local paths. Opening replaces the current document and refuses to discard unsaved work unless discard=true.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["new","open","save","export"]},"path":{"type":"string"},"width":{"type":"integer"},"height":{"type":"integer"},"discard":{"type":"boolean"}},"required":["action"]}},
        {"name":"peerbrush_history","description":"Work like an artist: inspect each result, undo unsatisfactory attempts, revise and try again, or redo to compare. AI undo/redo requires expected_revision and only reverses its own latest batch; preserve interleaved human work. Returns visual feedback.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["list","undo","redo"]},"actor":{"type":"string"},"expected_revision":{"type":"integer"},"feedback":{"type":"string","enum":["batch","request"]},"max_edge":{"type":"integer"}}}},
        {"name":"peerbrush_place_image","description":"Place generated or edited image pixels in one undoable operation. Supply an absolute local PNG/JPEG path or base64 PNG, and exact destination rect [left,top,right,bottom] in document pixels. new_layer defaults true; layer chooses sibling/folder context and parent can override it. Set new_layer:false to modify that layer; mode replace replaces transparent pixels too, over composites. Surrounding pixels, masks and editable effects are retained. Returns placed layer ID, rectangle and cropped PNG feedback.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"png":{"type":"string"},"rect":{"type":"array","items":{"type":"integer"},"minItems":4,"maxItems":4},"layer":{"type":"string"},"parent":{"type":["string","null"]},"new_layer":{"type":"boolean"},"name":{"type":"string"},"mode":{"type":"string","enum":["over","replace"]},"actor":{"type":"string"},"expected_revision":{"type":"integer"},"task":{"type":"string"},"label":{"type":"string"},"feedback":{"type":"string","enum":["batch","request","always"]},"max_edge":{"type":"integer","minimum":32,"maximum":4096}},"required":["rect"],"oneOf":[{"required":["path"]},{"required":["png"]}],"additionalProperties":false}},
        {"name":"peerbrush_capabilities","description":"Get concise supported operations and runnable JSON examples before editing.","inputSchema":{"type":"object","properties":{}}}
    ])
}
pub fn capabilities() -> Value {
    static DATA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    DATA.get_or_init(|| {
        let mut value: Value =
            serde_json::from_str(include_str!("capabilities.json")).expect("bundled capabilities");
        value["version"] = json!(env!("CARGO_PKG_VERSION"));
        value
    })
    .clone()
}

fn observation(shared: &Shared, p: &Value) -> Result<Value, String> {
    let (state, doc) = {
        let mut e = shared.lock().unwrap();
        (e.state(), e.doc.clone())
    };
    let revision = doc.revision;
    let image = p.get("image").and_then(Value::as_bool).unwrap_or(true)
        && p.get("since_revision").and_then(Value::as_u64) != Some(revision);
    let mut result = json!({"state":state,"revision":revision,"images":[]});
    if image {
        let (w, h, rgba, rect) = doc.preview(
            p.get("rect").and_then(engine::rect),
            p.get("max_edge")
                .and_then(Value::as_u64)
                .unwrap_or(1024)
                .clamp(32, 4096) as u32,
            p.get("layer").and_then(Value::as_str),
            p.get("mask").and_then(Value::as_bool).unwrap_or(false),
        )?;
        let bytes = raster::png(w, h, &rgba)?;
        result["images"] = json!([{"mime_type":"image/png","data":STANDARD.encode(bytes),"width":w,"height":h,"document_rect":rect,"revision":revision}]);
    }
    Ok(result)
}
pub fn dispatch(shared: &Shared, method: &str, p: &Value) -> Result<Value, String> {
    let actor = p.get("actor").and_then(Value::as_str).unwrap_or("agent");
    match method {
        "capabilities" => Ok(capabilities()),
        "focus" => {
            shared.lock().unwrap().focus_requested = true;
            Ok(json!({"focused":true}))
        }
        "observe" => observation(shared, p),
        "client_state" => {
            let client = p
                .get("client")
                .and_then(Value::as_str)
                .ok_or("Missing client ID")?;
            let mut e = shared.lock().unwrap();
            if p.get("connected") == Some(&Value::Bool(false)) {
                e.mcp_clients.remove(client);
            } else {
                e.mcp_clients.insert(client.into(), engine::now() + 90);
            }
            Ok(json!({"connected":!e.mcp_clients.is_empty()}))
        }
        "capture_ui" => {
            let path = p
                .get("path")
                .and_then(Value::as_str)
                .ok_or("Missing PNG path")?;
            let panel = p.get("panel").and_then(Value::as_str);
            if panel.is_some_and(|name| !["workspace", "brush", "color"].contains(&name)) {
                return Err("Unknown capture panel".into());
            }
            let mut engine = shared.lock().unwrap();
            engine.capture_ui = Some(PathBuf::from(path));
            engine.capture_panel = panel.map(String::from);
            Ok(json!({"queued":path}))
        }
        "place_image" => {
            let area = p
                .get("rect")
                .and_then(engine::rect)
                .ok_or("Choose an explicit image destination rect")?;
            let mut command = p.clone();
            let object = command
                .as_object_mut()
                .ok_or("Image placement arguments must be an object")?;
            for field in [
                "actor",
                "expected_revision",
                "task",
                "label",
                "feedback",
                "max_edge",
            ] {
                object.remove(field);
            }
            command["op"] = json!("image.place");
            let mut edit = p.clone();
            edit["commands"] = json!([command]);
            edit["feedback_rect"] = json!(area);
            if p.get("label").is_none() {
                edit["label"] = json!("Place image");
            }
            let mut result = dispatch(shared, "edit", &edit)?;
            let layer = if p["new_layer"] == false {
                p.get("layer").cloned().unwrap_or(Value::Null)
            } else {
                result["created"][0].clone()
            };
            result["placement"] =
                json!({"layer":layer,"rect":area,"new_layer":p["new_layer"] != false});
            Ok(result)
        }
        "edit" => {
            let commands = p
                .get("commands")
                .and_then(Value::as_array)
                .ok_or("commands must be an array")?;
            let result = shared.lock().unwrap().edit(
                actor,
                commands,
                p.get("expected_revision").and_then(Value::as_u64),
                p.get("task").and_then(Value::as_str),
                p.get("label").and_then(Value::as_str).unwrap_or("AI edit"),
            )?;
            let feedback = p
                .get("feedback")
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| shared.lock().unwrap().feedback.get(actor).cloned())
                .unwrap_or("batch".into());
            let mut out = result;
            if feedback != "request" {
                let view = observation(
                    shared,
                    &json!({"max_edge":p.get("max_edge").and_then(Value::as_u64).unwrap_or(768),"rect":p.get("feedback_rect")}),
                )?;
                out["images"] = view["images"].clone();
            }
            Ok(out)
        }
        "task" => {
            let action = p.get("action").and_then(Value::as_str).unwrap_or("status");
            let mut e = shared.lock().unwrap();
            e.expire();
            if let Some(f) = p.get("feedback").and_then(Value::as_str) {
                e.feedback.insert(actor.into(), f.into());
            }
            match action {
                "begin" => {
                    let scopes: Vec<Scope> =
                        serde_json::from_value(p.get("scopes").cloned().unwrap_or(json!([])))
                            .map_err(|e| e.to_string())?;
                    Ok(json!(e.reserve(
                        actor,
                        p.get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("AI editing"),
                        scopes
                    )?))
                }
                "end" => {
                    let task = p
                        .get("task")
                        .and_then(Value::as_str)
                        .ok_or("Missing task ID")?;
                    if !e.leases.iter().any(|l| l.id == task && l.owner == actor) {
                        return Err("Task not owned or already released".into());
                    }
                    e.leases.retain(|l| l.id != task);
                    if e.leases.is_empty() {
                        e.activity.clear();
                    }
                    Ok(json!({"released":task}))
                }
                "update" => {
                    let task = p
                        .get("task")
                        .and_then(Value::as_str)
                        .ok_or("Missing task ID")?;
                    let pos = e
                        .leases
                        .iter()
                        .position(|l| l.id == task && l.owner == actor)
                        .ok_or("Task expired or was released; do not silently reacquire")?;
                    if let Some(scopes) = p.get("scopes") {
                        let scopes: Vec<Scope> =
                            serde_json::from_value(scopes.clone()).map_err(|e| e.to_string())?;
                        e.check(actor, &scopes)?;
                        e.leases[pos].scopes = scopes;
                    }
                    if let Some(d) = p.get("description").and_then(Value::as_str) {
                        e.leases[pos].description = d.into();
                        e.activity = d.into();
                    }
                    e.leases[pos].expires = engine::now() + 300;
                    Ok(json!(e.leases[pos]))
                }
                "status" => Ok(json!(e.leases)),
                _ => Err("Unknown task action".into()),
            }
        }
        "history" => {
            let mut e = shared.lock().unwrap();
            let action = p.get("action").and_then(Value::as_str).unwrap_or("list");
            if action != "list" {
                if actor != "human" && p.get("expected_revision").and_then(Value::as_u64).is_none()
                {
                    return Err(
                        "AI undo/redo requires expected_revision from the latest observation"
                            .into(),
                    );
                }
                if p.get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_some_and(|r| r != e.doc.revision)
                {
                    return Err("Document changed before undo/redo. Observe again.".into());
                }
            }
            match action {
                "undo" => e.undo(actor)?,
                "redo" => e.redo(actor)?,
                "list" => {}
                _ => return Err("Unknown history action".into()),
            }
            let mut result = json!({"revision":e.doc.revision,"undo":e.undo.iter().map(|h|json!({"actor":h.actor,"label":h.label,"task":h.task})).collect::<Vec<_>>(),"redo_count":e.redo.len()});
            drop(e);
            if action != "list" && p["feedback"] != "request" {
                result["images"] = observation(
                    shared,
                    &json!({"max_edge":p.get("max_edge").and_then(Value::as_u64).unwrap_or(768)}),
                )?["images"]
                    .clone();
            }
            Ok(result)
        }
        "document" => {
            let action = p
                .get("action")
                .and_then(Value::as_str)
                .ok_or("Missing action")?;
            let path = p.get("path").and_then(Value::as_str).map(PathBuf::from);
            if action == "new" || action == "open" {
                let e = shared.lock().unwrap();
                if e.doc.revision != e.saved_revision
                    && p.get("discard") != Some(&Value::Bool(true))
                {
                    return Err("Unsaved work: save first or explicitly pass discard=true".into());
                }
            }
            match action {
                "new" => {
                    let doc = engine::Document::new(
                        p.get("width").and_then(Value::as_u64).unwrap_or(1024) as u32,
                        p.get("height").and_then(Value::as_u64).unwrap_or(768) as u32,
                    )?;
                    shared.lock().unwrap().replace(doc, None)?;
                }
                "open" => open(shared, path.as_deref().ok_or("Missing path")?)?,
                "save" => {
                    let path = path
                        .or_else(|| shared.lock().unwrap().path.clone())
                        .ok_or("Choose a PSD path")?;
                    save(shared, &path)?;
                }
                "export" => export(shared, path.as_deref().ok_or("Missing path")?)?,
                _ => return Err("Unknown document action".into()),
            }
            Ok(shared.lock().unwrap().state())
        }
        _ => Err(format!("Unknown API method: {method}")),
    }
}

fn initialization() -> Value {
    json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"PeerBrush","version":env!("CARGO_PKG_VERSION")},"instructions":"Observe before edits. Use explicit IDs, expected_revision, optional selective task reservations, and PNG feedback. Never silently reacquire after user takeover. Work like an artist: inspect actual images after edits, undo weak attempts, refine and retry; use redo to compare. Publish concise human-language activity through task descriptions. Do not assume the first edit is final."})
}

pub fn mcp(shared: &Shared, request: &Value) -> Value {
    let request_id = request.get("id").cloned().unwrap_or(Value::Null);
    let p = request.get("params").cloned().unwrap_or(json!({}));
    let client_id = request
        .get("_client")
        .and_then(Value::as_str)
        .unwrap_or("direct-mcp");
    {
        let mut e = shared.lock().unwrap();
        if request["method"] == "initialize" || e.mcp_clients.contains_key(client_id) {
            e.mcp_clients.insert(client_id.into(), engine::now() + 45);
        }
    }
    let result: Result<Value, String> = match request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
    {
        "initialize" => Ok(initialization()),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools":tools()})),
        "tools/call" => {
            let name = p.get("name").and_then(Value::as_str).unwrap_or("");
            let method = name.strip_prefix("peerbrush_").unwrap_or(name);
            if ![
                "capabilities",
                "observe",
                "edit",
                "place_image",
                "task",
                "document",
                "history",
            ]
            .contains(&method)
            {
                return json!({"jsonrpc":"2.0","id":request_id,"result":{"content":[{"type":"text","text":"Unknown PeerBrush tool"}],"isError":true}});
            }
            {
                let mut e = shared.lock().unwrap();
                if e.activity.is_empty() || e.leases.is_empty() {
                    e.activity = match method {
                        "observe" => "Looking at the canvas",
                        "edit" => "Updating the shared canvas",
                        "place_image" => "Placing image pixels",
                        "document" => "Working with your document",
                        "history" => "Reviewing recent changes",
                        _ => "Getting ready to work together",
                    }
                    .into();
                }
            }
            match dispatch(shared, method, p.get("arguments").unwrap_or(&json!({}))) {
                Ok(mut result) => {
                    let images = result
                        .get("images")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if let Some(o) = result.as_object_mut() {
                        o.remove("images");
                    }
                    if !images.is_empty() {
                        result["views"] = json!(images
                            .iter()
                            .map(|i| {
                                let mut meta = i.clone();
                                meta.as_object_mut().unwrap().remove("data");
                                meta
                            })
                            .collect::<Vec<_>>());
                    }
                    let mut content =
                        vec![json!({"type":"text","text":serde_json::to_string(&result).unwrap()})];
                    for img in images {
                        content.push(
                            json!({"type":"image","mimeType":"image/png","data":img["data"]}),
                        );
                    }
                    Ok(json!({"content":content,"isError":false}))
                }
                Err(e) => Ok(json!({"content":[{"type":"text","text":e}],"isError":true})),
            }
        }
        method if method.starts_with("notifications/") => return Value::Null,
        _ => Err("Method not found".into()),
    };
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":request_id,"result":result}),
        Err(error) => {
            json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":error}})
        }
    }
}
pub fn start(shared: Shared, state_dir: PathBuf) -> Result<Connection, String> {
    fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
    let instance_lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state_dir.join("instance.lock"))
        .map_err(|e| e.to_string())?;
    instance_lock.try_lock().map_err(|_|"PeerBrush is already running for this workspace. Use that window or a different --state-dir.".to_string())?;
    let http = tiny_http::Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = http
        .server_addr()
        .to_ip()
        .ok_or("No server address")?
        .port();
    let token = engine::id();
    let auth = format!("Bearer {token}");
    atomic_write(&state_dir.join("connection.json"),serde_json::to_string_pretty(&json!({"url":format!("http://127.0.0.1:{port}"),"token":token,"pid":std::process::id()})).unwrap().as_bytes())?;
    let recovery_dir = state_dir.clone();
    let recovery = shared.clone();
    thread::spawn(move || {
        let mut previous = None;
        loop {
            thread::sleep(std::time::Duration::from_secs(5));
            let doc = {
                let e = recovery.lock().unwrap();
                if e.doc.read_only || e.doc.revision == e.saved_revision {
                    continue;
                }
                e.doc.clone()
            };
            if previous == Some((doc.id.clone(), doc.revision)) {
                continue;
            }
            if let Ok(bytes) = psd::encode(&doc) {
                if atomic_write(&recovery_dir.join("recovery.psd"), &bytes).is_ok() {
                    previous = Some((doc.id, doc.revision));
                }
            }
        }
    });
    thread::spawn(move || {
        for mut request in http.incoming_requests() {
            let authenticated = request
                .headers()
                .iter()
                .any(|h| h.field.equiv("Authorization") && h.value.as_str() == auth);
            if !authenticated {
                let _ = request.respond(
                    tiny_http::Response::from_string("Unauthorized").with_status_code(401),
                );
                continue;
            }
            if request.method() != &tiny_http::Method::Post {
                let _ = request.respond(
                    tiny_http::Response::from_string("POST required").with_status_code(405),
                );
                continue;
            }
            if request.body_length().unwrap_or(0) > 4 * 1024 * 1024 {
                let _ = request.respond(
                    tiny_http::Response::from_string("Request too large").with_status_code(413),
                );
                continue;
            }
            let is_mcp = request.url() == "/mcp";
            if !is_mcp && request.url() != "/rpc" {
                let _ = request
                    .respond(tiny_http::Response::from_string("Not found").with_status_code(404));
                continue;
            }
            let mut body = String::new();
            let _ = request
                .as_reader()
                .take(4 * 1024 * 1024 + 1)
                .read_to_string(&mut body);
            let result = match serde_json::from_str::<Value>(&body) {
                Ok(q) => {
                    if is_mcp {
                        mcp(&shared, &q)
                    } else {
                        match dispatch(
                            &shared,
                            q.get("method").and_then(Value::as_str).unwrap_or(""),
                            q.get("params").unwrap_or(&json!({})),
                        ) {
                            Ok(v) => json!({"ok":true,"result":v}),
                            Err(e) => json!({"ok":false,"error":e}),
                        }
                    }
                }
                Err(e) => json!({"ok":false,"error":e.to_string()}),
            };
            let code = if result.is_null() { 202 } else { 200 };
            let response = tiny_http::Response::from_string(if result.is_null() {
                String::new()
            } else {
                result.to_string()
            })
            .with_status_code(code)
            .with_header(
                tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
            );
            let _ = request.respond(response);
        }
    });
    Ok(Connection {
        port,
        token,
        state_dir,
        instance_lock: Some(instance_lock),
    })
}
use std::io::Read;
pub fn default_state_dir() -> PathBuf {
    std::env::temp_dir().join("PeerBrush")
}
pub fn client(state_dir: &Path, endpoint: &str, payload: &Value) -> Result<Value, String> {
    client_with_timeout(
        state_dir,
        endpoint,
        payload,
        std::time::Duration::from_secs(60),
    )
}
pub fn focus_existing(state_dir: &Path) -> bool {
    client_with_timeout(
        state_dir,
        "rpc",
        &json!({"method":"focus","params":{}}),
        std::time::Duration::from_millis(500),
    )
    .is_ok_and(|r| r["ok"] == true)
}
fn client_with_timeout(
    state_dir: &Path,
    endpoint: &str,
    payload: &Value,
    timeout: std::time::Duration,
) -> Result<Value, String> {
    let conn: Value = serde_json::from_slice(
        &fs::read(state_dir.join("connection.json"))
            .map_err(|_| "PeerBrush is not running. Open the application, then retry.")?,
    )
    .map_err(|_| "PeerBrush connection information is invalid. Reopen the application.")?;
    let url = conn["url"]
        .as_str()
        .ok_or("Invalid PeerBrush connection information. Reopen the application.")?;
    if !url.starts_with("http://127.0.0.1:") {
        return Err("Connection must use the local PeerBrush endpoint".into());
    }
    let token = conn["token"]
        .as_str()
        .ok_or("Invalid PeerBrush connection token. Reopen the application.")?;
    ureq::post(&format!("{url}/{endpoint}"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(timeout)
        .send_json(payload.clone())
        .map_err(|error| match error {
            ureq::Error::Transport(ref transport)
                if transport.kind() == ureq::ErrorKind::ConnectionFailed =>
            {
                "PeerBrush is not running or cannot be reached. Open the application, then retry."
                    .into()
            }
            ureq::Error::Transport(ref transport)
                if transport.kind() == ureq::ErrorKind::Io =>
            {
                "The PeerBrush connection was interrupted or timed out. Check the application and observe the current document before retrying an edit."
                    .into()
            }
            ureq::Error::Status(401, _) => {
                "PeerBrush rejected stale connection information. Reopen the application, then retry."
                    .into()
            }
            _ => error.to_string(),
        })?
        .into_json()
        .map_err(|e| e.to_string())
}

fn stdio_static_response(request: &Value) -> Option<Value> {
    let result = match request.get("method").and_then(Value::as_str)? {
        "initialize" => initialization(),
        "ping" => json!({}),
        "tools/list" => json!({"tools":tools()}),
        _ => return None,
    };
    Some(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
}

fn stdio_unavailable(request: &Value, error: &str) -> Value {
    if request["method"] == "tools/call" {
        json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":format!("{error} The MCP adapter remains ready and will reconnect when PeerBrush opens.")}],"isError":true}})
    } else {
        json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":error}})
    }
}

fn announce_presence(state_dir: &Path, client_id: &str, connected: bool) {
    let _ = client_with_timeout(
        state_dir,
        "rpc",
        &json!({"method":"client_state","params":{"client":client_id,"connected":connected}}),
        std::time::Duration::from_secs(1),
    );
}

pub fn stdio(state_dir: &Path) -> Result<(), String> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let client_id = format!("stdio-{}", engine::id());
    let initialized = Arc::new(AtomicBool::new(false));
    let (heartbeat_stop, stop) = mpsc::channel::<()>();
    let (ready, dir, id) = (initialized.clone(), state_dir.to_owned(), client_id.clone());
    let heartbeat = thread::spawn(move || loop {
        match stop.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if ready.load(Ordering::Relaxed) {
                    announce_presence(&dir, &id, true);
                }
            }
        }
    });
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let result = (|| -> Result<(), String> {
        for line in stdin.lock().lines() {
            let line = line.map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                continue;
            }
            let mut q: Value = match serde_json::from_str(&line) {
                Ok(q) => q,
                Err(_) => {
                    writeln!(stdout, "{}", json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}))
                        .map_err(|e| e.to_string())?;
                    stdout.flush().map_err(|e| e.to_string())?;
                    continue;
                }
            };
            if !q.is_object() || !q.get("method").is_some_and(Value::is_string) {
                writeln!(stdout, "{}", json!({"jsonrpc":"2.0","id":q.get("id").cloned().unwrap_or(Value::Null),"error":{"code":-32600,"message":"Invalid Request"}}))
                    .map_err(|e| e.to_string())?;
                stdout.flush().map_err(|e| e.to_string())?;
                continue;
            }
            q["_client"] = json!(client_id);
            if q.get("id").is_none() {
                // Notifications never need a reply, including while the GUI is closed.
                let _ =
                    client_with_timeout(state_dir, "mcp", &q, std::time::Duration::from_secs(1));
                continue;
            }
            let response = if let Some(response) = stdio_static_response(&q) {
                if q["method"] == "initialize" {
                    initialized.store(true, Ordering::Relaxed);
                    announce_presence(state_dir, &client_id, true);
                }
                response
            } else {
                if initialized.load(Ordering::Relaxed) {
                    announce_presence(state_dir, &client_id, true);
                }
                client(state_dir, "mcp", &q).unwrap_or_else(|error| stdio_unavailable(&q, &error))
            };
            writeln!(stdout, "{}", response).map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    let _ = heartbeat_stop.send(());
    // Join before removing presence: an in-flight heartbeat must not revive a closed client.
    let _ = heartbeat.join();
    announce_presence(state_dir, &client_id, false);
    result
}
