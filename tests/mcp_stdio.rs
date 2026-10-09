use peerbrush::{engine::Engine, server};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

struct Adapter {
    child: Child,
    replies: mpsc::Receiver<Value>,
    dir: PathBuf,
}
impl Adapter {
    fn start(dir: PathBuf) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_peerbrush"))
            .arg("mcp")
            .arg("--state-dir")
            .arg(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (send, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let value = serde_json::from_str(&line.unwrap()).expect("JSON-only MCP stdout");
                if send.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            replies,
            dir,
        }
    }
    fn send(&mut self, value: &Value) -> Value {
        self.send_raw(&value.to_string());
        self.reply()
    }
    fn send_raw(&mut self, text: &str) {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{text}").unwrap();
        input.flush().unwrap();
    }
    fn reply(&self) -> Value {
        self.replies
            .recv_timeout(Duration::from_secs(10))
            .expect("MCP adapter reply")
    }
    fn stop(&mut self) {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "adapter stopped with {status}");
                return;
            }
            assert!(Instant::now() < deadline, "adapter failed to stop at EOF");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Adapter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(self.dir.join("connection.json"));
        let _ = std::fs::remove_file(self.dir.join("instance.lock"));
        let _ = std::fs::remove_dir(&self.dir);
    }
}
fn isolated_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peerbrush-mcp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn adapter_keeps_tools_available_with_a_closed_canvas_and_stale_connection() {
    let dir = isolated_dir();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    std::fs::write(
        dir.join("connection.json"),
        json!({"url":format!("http://127.0.0.1:{port}"),"token":"stale"}).to_string(),
    )
    .unwrap();
    let mut adapter = Adapter::start(dir);
    let initialized = adapter.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"}));
    assert_eq!(
        initialized["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "PeerBrush");
    adapter.send_raw(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string());
    assert_eq!(
        adapter.send(&json!({"id":2,"method":"ping"}))["result"],
        json!({})
    );
    let listed = adapter.send(&json!({"id":3,"method":"tools/list"}));
    assert_eq!(listed["result"]["tools"], server::tools());
    let unavailable = adapter.send(&json!({"id":4,"method":"tools/call","params":{"name":"peerbrush_observe","arguments":{"image":false}}}));
    assert_eq!(unavailable["result"]["isError"], true);
    assert!(
        unavailable.get("error").is_none(),
        "tool failures use MCP result content"
    );
    let message = unavailable["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(message.contains("Open the application"));
    assert!(message.contains("adapter remains ready"));
    adapter.send_raw("{broken");
    assert_eq!(adapter.reply()["error"]["code"], -32700);
    assert_eq!(adapter.send(&json!([1, 2]))["error"]["code"], -32600);
    assert_eq!(
        adapter.send(&json!({"id":5,"method":"ping"}))["result"],
        json!({})
    );
    adapter.stop();
}

#[test]
fn initialized_adapter_reconnects_when_canvas_opens_and_clears_presence_at_eof() {
    let dir = isolated_dir();
    let mut adapter = Adapter::start(dir.clone());
    assert!(adapter
        .send(&json!({"id":1,"method":"initialize"}))
        .get("result")
        .is_some());
    let shared = Arc::new(Mutex::new(Engine::new()));
    let connection = server::start(shared.clone(), dir).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while shared.lock().unwrap().mcp_clients.is_empty() {
        assert!(
            Instant::now() < deadline,
            "five-second heartbeat did not reconnect"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let observed = adapter.send(&json!({"id":2,"method":"tools/call","params":{"name":"peerbrush_observe","arguments":{"image":false}}}));
    assert_eq!(observed["result"]["isError"], false);
    let state: Value =
        serde_json::from_str(observed["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(state["revision"], 0);
    let brushes=adapter.send(&json!({"id":3,"method":"tools/call","params":{"name":"peerbrush_brushes","arguments":{"action":"preview","id":"brush-pen"}}}));
    assert_eq!(brushes["result"]["isError"], false);
    assert!(brushes["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["type"] == "image" && c["mimeType"] == "image/png"));
    adapter.stop();
    assert!(shared.lock().unwrap().mcp_clients.is_empty());
    drop(connection);
}
