use base64::{engine::general_purpose::STANDARD, Engine as _};
use peerbrush::{engine::Engine, raster, server};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[test]
fn modern_async_open_exposes_progress_cancel_and_exact_native_sources() {
    let canvas = Canvas::new();
    let control = peerbrush::loading::Control::default();
    canvas.shared.lock().unwrap().loading = Some(control.clone());
    let call = modern_request(
        json!("cancel-load"),
        "tools/call",
        json!({"name":"peerbrush_document","arguments":{"action":"cancel_open"}}),
    );
    let reply: Value = response(canvas.modern(&call).send_json(call.clone()))
        .into_json()
        .unwrap();
    assert_eq!(reply["result"]["isError"], false);
    assert!(control.check().is_err());
    canvas.shared.lock().unwrap().loading = None;
    let mut doc = peerbrush::engine::Document::new_depth(32, 16, 16).unwrap();
    doc.layers[0]
        .pixels
        .set16(3, 7, [60001, 12347, 34569, 65535]);
    let path = canvas.dir.join("async-native.psd");
    std::fs::write(&path, peerbrush::psd::encode(&doc).unwrap()).unwrap();
    let call = modern_request(
        json!("open-load"),
        "tools/call",
        json!({"name":"peerbrush_document","arguments":{"action":"open_async","path":path}}),
    );
    let reply: Value = response(canvas.modern(&call).send_json(call.clone()))
        .into_json()
        .unwrap();
    assert_eq!(reply["result"]["isError"], false);
    for _ in 0..100 {
        let e = canvas.shared.lock().unwrap();
        if e.loading.is_none() {
            break;
        }
        drop(e);
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut e = canvas.shared.lock().unwrap();
    assert!(e.loading.is_none());
    assert_eq!(e.doc.id, doc.id);
    assert_eq!(
        e.doc.layers[0].pixels.get16(3, 7),
        [60001, 12347, 34569, 65535]
    );
    assert!(e.undo.is_empty());
    assert_eq!(e.state()["file_status"], "PSD opened");
}

struct Canvas {
    shared: server::Shared,
    connection: server::Connection,
    dir: PathBuf,
}
impl Canvas {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("peerbrush-http-{}", uuid::Uuid::new_v4()));
        let shared = Arc::new(Mutex::new(Engine::new()));
        let connection = server::start(shared.clone(), dir.clone()).unwrap();
        Self {
            shared,
            connection,
            dir,
        }
    }
    fn request(&self, method: &str, path: &str) -> ureq::Request {
        ureq::request(
            method,
            &format!("http://127.0.0.1:{}{path}", self.connection.port),
        )
        .set(
            "Authorization",
            &format!("Bearer {}", self.connection.token),
        )
        .set("Content-Type", "application/json")
        .set("Accept", "application/json, text/event-stream")
        .timeout(Duration::from_secs(10))
    }
    fn initialize(&self, version: &str, id: Value) -> (String, Value) {
        let response = response(self.request("POST", "/mcp").send_json(json!({
            "jsonrpc":"2.0","id":id,"method":"initialize","params":{
                "protocolVersion":version,"capabilities":{},"clientInfo":{"name":"Transport QA","version":"1"}
            }
        })));
        assert_eq!(response.status(), 200);
        assert_eq!(response.header("Content-Type"), Some("application/json"));
        let session = response.header("MCP-Session-Id").unwrap().to_owned();
        assert!(!session.is_empty() && session.bytes().all(|b| (0x21..=0x7e).contains(&b)));
        (session, response.into_json().unwrap())
    }
    fn modern(&self, q: &Value) -> ureq::Request {
        let mut request = self
            .request("POST", "/mcp")
            .set(
                "MCP-Protocol-Version",
                q["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"]
                    .as_str()
                    .unwrap(),
            )
            .set("Mcp-Method", q["method"].as_str().unwrap());
        if let Some(name) = q["params"]["name"].as_str() {
            request = request.set("Mcp-Name", name);
        }
        request
    }
}
impl Drop for Canvas {
    fn drop(&mut self) {
        self.connection.instance_lock.take();
        let _ = std::fs::remove_file(self.dir.join("connection.json"));
        let _ = std::fs::remove_file(self.dir.join("instance.lock"));
        let _ = std::fs::remove_dir(&self.dir);
    }
}
fn response(result: Result<ureq::Response, ureq::Error>) -> ureq::Response {
    match result {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("{e}"),
    }
}
fn assert_error(response: ureq::Response, status: u16, code: i32) -> Value {
    assert_eq!(response.status(), status);
    assert_eq!(response.header("Content-Type"), Some("application/json"));
    let body: Value = response.into_json().unwrap();
    assert_eq!(body["error"]["code"], code);
    body
}
fn modern_request(id: Value, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientInfo":{"name":"Modern QA","version":"1"},
        "io.modelcontextprotocol/clientCapabilities":{}
    });
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

#[test]
fn legacy_negotiates_versions_and_closes_only_its_own_session() {
    let canvas = Canvas::new();
    let mut sessions = Vec::new();
    for (index, version) in server::MCP_HTTP_VERSIONS[1..].iter().enumerate() {
        let id = json!(format!("initialize-{index}"));
        let (session, initialized) = canvas.initialize(version, id.clone());
        assert_eq!(initialized["id"], id);
        assert_eq!(initialized["result"]["protocolVersion"], *version);
        assert_eq!(
            initialized["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        let notified = response(
            canvas
                .request("POST", "/mcp")
                .set("MCP-Session-Id", &session)
                .set("MCP-Protocol-Version", version)
                .send_json(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        );
        assert_eq!(notified.status(), 202);
        assert!(notified.into_string().unwrap().is_empty());
        let listed: Value = response(
            canvas
                .request("POST", "/mcp")
                .set("MCP-Session-Id", &session)
                .set("MCP-Protocol-Version", version)
                .send_json(json!({"jsonrpc":"2.0","id":42,"method":"tools/list"})),
        )
        .into_json()
        .unwrap();
        assert_eq!(listed["id"], 42);
        assert_eq!(listed["result"]["tools"], server::tools());
        assert!(listed["result"].get("resultType").is_none());
        sessions.push(session);
    }
    let (fallback, initialized) = canvas.initialize("1900-01-01", json!(8));
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    sessions.push(fallback);
    let (unsupported_march, initialized) = canvas.initialize("2025-03-26", json!(10));
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    sessions.push(unsupported_march);
    assert!(!server::MCP_HTTP_VERSIONS.contains(&"2025-03-26"));
    assert_eq!(
        canvas.shared.lock().unwrap().mcp_clients.len(),
        sessions.len()
    );
    let deleted = response(
        canvas
            .request("DELETE", "/mcp")
            .set("MCP-Session-Id", &sessions[0])
            .set("MCP-Protocol-Version", "2025-11-25")
            .call(),
    );
    assert_eq!(deleted.status(), 204);
    assert!(deleted.into_string().unwrap().is_empty());
    assert_eq!(
        canvas.shared.lock().unwrap().mcp_clients.len(),
        sessions.len() - 1
    );
    assert_error(
        response(
            canvas
                .request("POST", "/mcp")
                .set("MCP-Session-Id", &sessions[0])
                .send_json(json!({"jsonrpc":"2.0","id":1,"method":"ping"})),
        ),
        404,
        -32600,
    );
    let mismatch = response(
        canvas
            .request("POST", "/mcp")
            .set("MCP-Session-Id", &sessions[1])
            .set("MCP-Protocol-Version", "2025-11-25")
            .send_json(json!({"jsonrpc":"2.0","id":9,"method":"ping"})),
    );
    assert_error(mismatch, 400, -32602);
}

#[test]
fn legacy_notifications_and_responses_have_no_body_and_cannot_edit() {
    let canvas = Canvas::new();
    let (session, _) = canvas.initialize("2025-11-25", json!(1));
    for message in [
        json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"peerbrush_edit","arguments":{"commands":[{"op":"layer.create","name":"Must not exist"}]}}}),
        json!({"jsonrpc":"2.0","method":"notifications/unknown"}),
        json!({"jsonrpc":"2.0","id":"server-request","result":{}}),
    ] {
        let accepted = response(
            canvas
                .request("POST", "/mcp")
                .set("MCP-Session-Id", &session)
                .send_json(message),
        );
        assert_eq!(accepted.status(), 202);
        assert!(accepted.into_string().unwrap().is_empty());
    }
    assert_eq!(canvas.shared.lock().unwrap().doc.revision, 0);
}

#[test]
fn transport_rejects_unsafe_origins_and_requires_local_authentication() {
    let canvas = Canvas::new();
    let q = json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
    assert_error(
        response(
            canvas
                .request("POST", "/mcp")
                .set("Authorization", "Bearer wrong")
                .send_json(q.clone()),
        ),
        401,
        -32600,
    );
    for origin in ["https://attacker.example", "null", "http://127.0.0.1:1"] {
        assert_error(
            response(
                canvas
                    .request("POST", "/mcp")
                    .set("Origin", origin)
                    .send_json(q.clone()),
            ),
            403,
            -32600,
        );
    }
    assert_error(
        response(
            canvas
                .request("POST", "/mcp")
                .set("Host", "attacker.example")
                .send_json(q.clone()),
        ),
        403,
        -32600,
    );
    let local = format!("http://127.0.0.1:{}", canvas.connection.port);
    assert_eq!(
        response(
            canvas
                .request("POST", "/mcp")
                .set("Origin", &local)
                .send_json(q)
        )
        .status(),
        200
    );
    for method in ["GET", "PUT", "PATCH"] {
        let r = response(canvas.request(method, "/mcp").call());
        assert_eq!(r.header("Allow"), Some("POST, DELETE"));
        assert_error(r, 405, -32600);
    }
    assert_error(
        response(canvas.request("POST", "/different").send_json(json!({}))),
        404,
        -32601,
    );
    assert!(canvas.shared.lock().unwrap().doc.revision == 0);
}

#[test]
fn transport_validates_json_content_and_session_headers() {
    let canvas = Canvas::new();
    let q = json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
    let default_initialized: Value = response(canvas.request("POST", "/mcp").send_json(q.clone()))
        .into_json()
        .unwrap();
    assert_eq!(
        default_initialized["result"]["protocolVersion"],
        "2025-06-18"
    );
    assert_error(
        response(
            canvas
                .request("POST", "/mcp")
                .set("MCP-Protocol-Version", "2025-03-26")
                .send_json(q.clone()),
        ),
        400,
        -32022,
    );
    assert_error(
        response(
            canvas
                .request("POST", "/mcp")
                .set("Accept", "text/html")
                .send_json(q.clone()),
        ),
        406,
        -32600,
    );
    assert_error(
        response(
            canvas
                .request("POST", "/mcp")
                .set("Content-Type", "text/plain")
                .send_string(&q.to_string()),
        ),
        415,
        -32600,
    );
    assert_error(
        response(canvas.request("POST", "/mcp").send_string("{broken")),
        400,
        -32700,
    );
    for invalid in [
        json!([q.clone()]),
        json!({"id":1,"method":"initialize"}),
        json!({"jsonrpc":"2.0","id":null,"method":"initialize"}),
        json!({"jsonrpc":"2.0","id":1,"method":"notifications/initialized"}),
    ] {
        assert_error(
            response(canvas.request("POST", "/mcp").send_json(invalid)),
            400,
            -32600,
        );
    }
    assert_error(
        response(
            canvas
                .request("POST", "/mcp")
                .send_json(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})),
        ),
        400,
        -32600,
    );
    assert_error(
        response(canvas.request("DELETE", "/mcp").call()),
        400,
        -32600,
    );
    assert_error(
        response(
            canvas
                .request("POST", "/mcp")
                .send_string(&" ".repeat(4 * 1024 * 1024 + 1)),
        ),
        413,
        -32600,
    );
    // Keep the original private JSON API behavior available to CLI and stdio clients.
    let rpc: Value = response(
        canvas
            .request("POST", "/rpc")
            .send_json(json!({"method":"observe","params":{"image":false}})),
    )
    .into_json()
    .unwrap();
    assert_eq!(rpc["ok"], true);
    assert_eq!(rpc["result"]["revision"], 0);
}

#[test]
fn modern_discovery_and_catalog_are_stateless_and_include_required_result_metadata() {
    let canvas = Canvas::new();
    let q = modern_request(json!("discover"), "server/discover", json!({}));
    let r = response(
        canvas
            .modern(&q)
            .set("MCP-Session-Id", "ignored-old-session")
            .send_json(q.clone()),
    );
    assert_eq!(r.status(), 200);
    assert!(r.header("MCP-Session-Id").is_none());
    let discovered: Value = r.into_json().unwrap();
    assert_eq!(discovered["id"], "discover");
    assert_eq!(
        discovered["result"]["supportedVersions"],
        json!(server::MCP_HTTP_VERSIONS)
    );
    assert_eq!(discovered["result"]["resultType"], "complete");
    assert_eq!(
        discovered["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    for index in 0..20 {
        let mut q = modern_request(json!(index), "tools/list", json!({}));
        q["params"]["_meta"]["io.modelcontextprotocol/clientInfo"]["name"] =
            json!(format!("Client {index}"));
        let listed: Value = response(canvas.modern(&q).send_json(q.clone()))
            .into_json()
            .unwrap();
        assert_eq!(listed["result"]["tools"], server::tools());
        assert_eq!(listed["result"]["resultType"], "complete");
        assert_eq!(listed["result"]["cacheScope"], "private");
        assert!(listed["result"]["ttlMs"].as_u64().is_some());
    }
    assert_eq!(
        canvas.shared.lock().unwrap().mcp_clients.len(),
        1,
        "stateless identity must not grow per request or self-reported client"
    );
    assert_error(
        response(
            canvas
                .request("DELETE", "/mcp")
                .set("MCP-Protocol-Version", "2026-07-28")
                .call(),
        ),
        405,
        -32600,
    );
}

#[test]
fn modern_validates_mirrored_headers_versions_and_request_metadata() {
    let canvas = Canvas::new();
    let q = modern_request(json!(12), "tools/list", json!({}));
    assert_error(
        response(canvas.request("POST", "/mcp").send_json(q.clone())),
        400,
        -32020,
    );
    assert_error(
        response(
            canvas
                .modern(&q)
                .set("Mcp-Method", "tools/call")
                .send_json(q.clone()),
        ),
        400,
        -32020,
    );
    assert_error(
        response(
            canvas
                .modern(&q)
                .set("MCP-Protocol-Version", "2025-11-25")
                .send_json(q.clone()),
        ),
        400,
        -32020,
    );
    let mut missing = q.clone();
    missing["params"]["_meta"]
        .as_object_mut()
        .unwrap()
        .remove("io.modelcontextprotocol/clientCapabilities");
    assert_error(
        response(canvas.modern(&missing).send_json(missing.clone())),
        400,
        -32602,
    );
    let mut future = q.clone();
    future["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2027-01-01");
    let error = assert_error(
        response(canvas.modern(&future).send_json(future.clone())),
        400,
        -32022,
    );
    assert_eq!(
        error["error"]["data"]["supported"],
        json!(server::MCP_HTTP_VERSIONS)
    );
    let unknown = modern_request(json!(13), "subscriptions/listen", json!({}));
    assert_error(
        response(canvas.modern(&unknown).send_json(unknown.clone())),
        404,
        -32601,
    );
    let call = modern_request(
        json!(14),
        "tools/call",
        json!({"name":"peerbrush_observe","arguments":{"image":false}}),
    );
    assert_error(
        response(
            canvas
                .modern(&call)
                .set("Mcp-Name", "different_tool")
                .send_json(call.clone()),
        ),
        400,
        -32020,
    );
    let encoded = format!("=?base64?{}?=", STANDARD.encode("peerbrush_observe"));
    let observed: Value = response(
        canvas
            .modern(&call)
            .set("Mcp-Name", &encoded)
            .send_json(call.clone()),
    )
    .into_json()
    .unwrap();
    assert_eq!(observed["result"]["isError"], false);
    assert_error(
        response(
            canvas
                .modern(&call)
                .set("Mcp-Name", "=?base64?broken!?=")
                .send_json(call.clone()),
        ),
        400,
        -32020,
    );
    let r = response(
        canvas
            .request("POST", "/mcp")
            .set("MCP-Protocol-Version", "2026-07-28")
            .send_json(json!({"jsonrpc":"2.0","method":"notifications/experimental"})),
    );
    assert_eq!(r.status(), 202);
    assert!(r.into_string().unwrap().is_empty());
}

#[test]
fn modern_places_image_pixels_and_returns_actual_png_feedback() {
    let canvas = Canvas::new();
    let png = raster::png(1, 1, &[32, 96, 192, 255]).unwrap();
    let q = modern_request(
        json!("place"),
        "tools/call",
        json!({"name":"peerbrush_place_image","arguments":{
            "png":STANDARD.encode(png),"rect":[4,4,8,8],"name":"Generated pixels","expected_revision":0,"max_edge":32
        }}),
    );
    let r = response(canvas.modern(&q).send_json(q.clone()));
    assert_eq!(r.status(), 200);
    let placed: Value = r.into_json().unwrap();
    assert_eq!(placed["id"], "place");
    assert_eq!(placed["result"]["resultType"], "complete");
    assert_eq!(placed["result"]["isError"], false);
    let content = placed["result"]["content"].as_array().unwrap();
    let metadata: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["placement"]["rect"], json!([4, 4, 8, 8]));
    assert_eq!(metadata["revision"], 1);
    let feedback = content.iter().find(|item| item["type"] == "image").unwrap();
    assert_eq!(feedback["mimeType"], "image/png");
    let decoded =
        image::load_from_memory(&STANDARD.decode(feedback["data"].as_str().unwrap()).unwrap())
            .unwrap()
            .to_rgba8();
    assert_eq!(decoded.dimensions(), (4, 4));
    assert_eq!(decoded.get_pixel(2, 2).0, [32, 96, 192, 255]);
    assert_eq!(canvas.shared.lock().unwrap().doc.revision, 1);
}

#[test]
fn legacy_session_memory_is_bounded() {
    let canvas = Canvas::new();
    let mut sessions = Vec::new();
    for id in 0..128 {
        sessions.push(canvas.initialize("2025-11-25", json!(id)).0);
    }
    assert_error(
        response(
            canvas
                .request("POST", "/mcp")
                .send_json(json!({"jsonrpc":"2.0","id":129,"method":"initialize"})),
        ),
        429,
        -32000,
    );
    assert_eq!(canvas.shared.lock().unwrap().mcp_clients.len(), 128);
    assert_eq!(
        response(
            canvas
                .request("DELETE", "/mcp")
                .set("MCP-Session-Id", &sessions[0])
                .call()
        )
        .status(),
        204
    );
    canvas.initialize("2025-11-25", json!(130));
    assert_eq!(canvas.shared.lock().unwrap().mcp_clients.len(), 128);
}

#[test]
fn modern_edits_native_vector_sources_and_returns_actual_png_and_properties() {
    let canvas = Canvas::new();
    {
        let mut e = canvas.shared.lock().unwrap();
        e.doc = peerbrush::engine::Document::new_depth(32, 32, 16).unwrap();
    }
    let source = json!({"width":16,"height":16,"content":{"kind":"shape","shape":"rectangle","bounds":[2,2,14,14],"points":[],"closed":true,"fill":[12345,23457,34569,65535],"stroke":[0,0,0,0],"stroke_width":0}});
    let q = modern_request(
        json!("source"),
        "tools/call",
        json!({"name":"peerbrush_edit","arguments":{"actor":"vector-agent","expected_revision":0,"commands":[{"op":"source.add","source":source,"x":4,"y":6}],"max_edge":32}}),
    );
    let r: Value = response(canvas.modern(&q).send_json(q.clone()))
        .into_json()
        .unwrap();
    assert_eq!(r["result"]["isError"], false);
    let content = r["result"]["content"].as_array().unwrap();
    let feedback = content.iter().find(|v| v["type"] == "image").unwrap();
    let png = STANDARD.decode(feedback["data"].as_str().unwrap()).unwrap();
    let image = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(image.get_pixel(8, 10).0, [48, 91, 135, 255]);
    let mut e = canvas.shared.lock().unwrap();
    let observed: peerbrush::source::Source =
        serde_json::from_value(e.state()["layers"][0]["source"].clone()).unwrap();
    let requested: peerbrush::source::Source = serde_json::from_value(source).unwrap();
    assert_eq!(observed, requested);
    assert_eq!(
        e.doc.layers[0].pixels.get16(4, 4),
        [12345, 23457, 34569, 65535]
    );
    assert_eq!(e.undo.len(), 1);
    assert_eq!(e.ai_change.as_ref().unwrap().tool, "layers");
}

#[test]
fn modern_transform_cycles_preserve_native_originals_and_return_current_pixels() {
    let canvas = Canvas::new();
    let (id, before) = {
        let mut e = canvas.shared.lock().unwrap();
        e.doc = peerbrush::engine::Document::new_depth(32, 32, 16).unwrap();
        for y in 4..28 {
            for x in 4..28 {
                e.doc.layers[0].pixels.set16(
                    x,
                    y,
                    [12345 + x as u16 * 347, 23457 + y as u16 * 283, 34569, 65535],
                );
            }
        }
        (e.doc.layers[0].id.clone(), e.doc.export_png().unwrap())
    };
    let commands = [
        json!({"op":"transform","layer":id,"angle":31,"pivot":[16,16],"selection_only":false}),
        json!({"op":"transform","layer":id,"angle":-31,"pivot":[16,16],"selection_only":false}),
        json!({"op":"transform","layer":id,"scale_x":0.1,"scale_y":0.1,"pivot":[16,16],"selection_only":false}),
        json!({"op":"transform","layer":id,"scale_x":10,"scale_y":10,"pivot":[16,16],"selection_only":false}),
    ];
    let q = modern_request(
        json!("retained"),
        "tools/call",
        json!({"name":"peerbrush_edit","arguments":{"actor":"transform-agent","expected_revision":0,"commands":commands,"max_edge":32}}),
    );
    let r: Value = response(canvas.modern(&q).send_json(q.clone()))
        .into_json()
        .unwrap();
    assert_eq!(r["result"]["isError"], false);
    let feedback = r["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == "image")
        .unwrap();
    let png = STANDARD.decode(feedback["data"].as_str().unwrap()).unwrap();
    let image = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(
        image.get_pixel(12, 12).0,
        [
            peerbrush::raster::project16(12345 + 12 * 347),
            peerbrush::raster::project16(23457 + 12 * 283),
            peerbrush::raster::project16(34569),
            255
        ]
    );
    let mut e = canvas.shared.lock().unwrap();
    assert!(e.doc.export_png().unwrap() == before);
    assert_eq!(e.undo.len(), 1);
    assert_eq!(e.state()["layers"][0]["transform_source"]["bit_depth"], 16);
    assert_eq!(e.state()["layers"][0]["transform_source"]["width"], 32);
    assert_eq!(e.ai_change.as_ref().unwrap().tool, "scale");
    e.undo("transform-agent").unwrap();
    assert!(e.doc.export_png().unwrap() == before);
    assert!(e.doc.layers[0].pixels.retained.is_none());
}
