//! Safe local transport fixtures. No document mutation or source runtime runs.
use super::*;
use std::{net::TcpListener, time::Duration};

const CANARY: &str = "SYNTHETIC_STREAM_PRIVATE_CANARY";
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!("peerbrush-stream-{}", engine::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn connection(&self, port: u16) {
        fs::write(
            self.0.join("connection.json"),
            json!({"url":format!("http://127.0.0.1:{port}"),"token":CANARY}).to_string(),
        )
        .unwrap();
    }
    fn server(
        &self,
        reply: impl FnOnce(tiny_http::Request) + Send + 'static,
    ) -> thread::JoinHandle<()> {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        self.connection(server.server_addr().to_ip().unwrap().port());
        thread::spawn(move || {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(request.url(), "/rpc");
            assert!(request
                .headers()
                .iter()
                .any(|h| h.field.equiv("Authorization")
                    && h.value.as_str() == format!("Bearer {CANARY}")));
            assert!(request
                .headers()
                .iter()
                .any(|h| h.field.equiv("Accept-Encoding") && h.value.as_str() == "identity"));
            let body: Value = serde_json::from_reader(request.as_reader()).unwrap();
            assert_eq!(body["method"], "mcp_stream");
            assert_eq!(body["params"]["request"]["id"], 17);
            reply(request);
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.join("connection.json"));
        let _ = fs::remove_dir(&self.0);
    }
}
fn query() -> Value {
    json!({"jsonrpc":"2.0","id":17,"method":"tools/call","params":{"name":"peerbrush_observe"}})
}

#[test]
fn actual_http_framing_keeps_native_charge_through_send_and_write_failure() {
    let pool = crate::large_document::ResidentBudget::new(32 * 1024 * 1024).unwrap();
    for fail in [false, true] {
        let source = vec![137; 1024 * 1024 + 3];
        let mut admission = reply::ReplyAdmission::new(true);
        admission.admit(&pool, 8192).unwrap();
        let mut check = || Ok(());
        let mut png = reply::SegmentWriter::new(&pool, source.len(), &mut check).unwrap();
        png.write_all(&source).unwrap();
        admission.image(0, png.finish().unwrap()).unwrap();
        let result = admission
            .finish(json!({"images":[{"mime_type":"image/png","data":null}]}))
            .unwrap()
            .tool_result()
            .unwrap()
            .rpc(&json!(17))
            .unwrap()
            .ordered_rpc()
            .unwrap();
        let response = owned_response(result).unwrap();
        assert!(pool.used() > source.len());
        struct Sink<'a> {
            pool: &'a crate::large_document::ResidentBudget,
            bytes: Vec<u8>,
            fail: bool,
        }
        impl Write for Sink<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                assert!(self.pool.used() > 0);
                if self.fail && self.bytes.len() > 1024 {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut sink = Sink {
            pool: &pool,
            bytes: Vec::new(),
            fail,
        };
        let sent = response.raw_print(&mut sink, tiny_http::HTTPVersion(1, 1), &[], false, None);
        assert_eq!(sent.is_err(), fail);
        assert_eq!(pool.used(), 0);
        if !fail {
            let split = sink
                .bytes
                .windows(4)
                .position(|b| b == b"\r\n\r\n")
                .unwrap()
                + 4;
            let headers = std::str::from_utf8(&sink.bytes[..split]).unwrap();
            assert!(headers.contains("Content-Length:"));
            assert!(!headers.contains("Transfer-Encoding:"));
            let value: Value = serde_json::from_slice(&sink.bytes[split..]).unwrap();
            assert_eq!(
                value["result"]["content"][1]["data"],
                STANDARD.encode(source)
            );
        }
    }
}

#[test]
fn actual_owned_http_sender_and_stdio_bridge_preserve_payload_and_retire_admission() {
    let fixture = Fixture::new();
    let pool = crate::large_document::ResidentBudget::new(32 * 1024 * 1024).unwrap();
    let source: Vec<u8> = (0..1024 * 1024 + 7).map(|n| (n * 13) as u8).collect();
    let expected = STANDARD.encode(&source);
    let mut admission = reply::ReplyAdmission::new(true);
    admission.admit(&pool, 8192).unwrap();
    let mut check = || Ok(());
    let mut png = reply::SegmentWriter::new(&pool, source.len(), &mut check).unwrap();
    png.write_all(&source).unwrap();
    admission.image(0, png.finish().unwrap()).unwrap();
    let result=admission.finish(json!({"revision":9,"images":[{"data":null,"width":13,"height":7,"document_rect":[1,2,14,9],"revision":9,"mime_type":"image/png"}]})).unwrap()
        .tool_result().unwrap().rpc(&json!(17)).unwrap().ordered_rpc().unwrap();
    assert!(pool.used() > source.len());
    let worker = fixture.server(move |request| respond_owned(request, result));
    let mut output = Vec::new();
    stdio_stream(&fixture.0, &query(), &mut output).unwrap();
    worker.join().unwrap();
    assert_eq!(pool.used(), 0);
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["id"], 17);
    assert_eq!(value["result"]["content"][1]["data"], expected);
    let metadata: Value =
        serde_json::from_str(value["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["views"][0]["document_rect"], json!([1, 2, 14, 9]));
}

#[test]
fn authenticated_stream_rejects_wrong_id_auth_and_content_type_before_stdout() {
    let fixture = Fixture::new();
    for (status, content, body) in [
        (401, "application/json", CANARY.to_owned()),
        (200, "text/html", CANARY.to_owned()),
        (
            200,
            "application/json",
            "{\"id\":18,\"jsonrpc\":\"2.0\",\"result\":{}}".to_owned(),
        ),
    ] {
        let worker = fixture.server(move |request| {
            request
                .respond(
                    tiny_http::Response::from_string(body)
                        .with_status_code(status)
                        .with_header(
                            tiny_http::Header::from_bytes("Content-Type", content).unwrap(),
                        ),
                )
                .unwrap();
        });
        let mut output = Vec::new();
        let failure = stdio_stream(&fixture.0, &query(), &mut output).unwrap_err();
        worker.join().unwrap();
        assert!(!failure.started);
        assert!(output.is_empty());
        assert!(!failure.message().contains(CANARY));
    }
    // A credential-bearing request must never reach an origin containing a path.
    let trap = TcpListener::bind("127.0.0.1:0").unwrap();
    trap.set_nonblocking(true).unwrap();
    fs::write(fixture.0.join("connection.json"),json!({"url":format!("http://127.0.0.1:{}/forbidden",trap.local_addr().unwrap().port()),"token":CANARY}).to_string()).unwrap();
    assert!(stdio_stream(&fixture.0, &query(), &mut Vec::new()).is_err());
    assert_eq!(
        trap.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn actual_truncated_http_body_reports_terminal_partial_stdout() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    fixture.connection(listener.local_addr().unwrap().port());
    let worker = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "local stream request did not arrive"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("local listener error: {:?}", error.kind()),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 16384);
        }
        let request = String::from_utf8(request).unwrap();
        let length = request
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut payload = vec![0; length];
        socket.read_exact(&mut payload).unwrap();
        let body = format!(
            "{{\"id\":17,\"jsonrpc\":\"2.0\",\"result\":\"{}",
            "a".repeat(64 * 1024)
        );
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",body.len()+100).as_bytes()).unwrap();
        socket.write_all(body.as_bytes()).unwrap();
        socket.shutdown(std::net::Shutdown::Both).unwrap();
    });
    let mut output = Vec::new();
    let failure = stdio_stream(&fixture.0, &query(), &mut output).unwrap_err();
    worker.join().unwrap();
    assert!(failure.started);
    assert!(!output.is_empty());
    assert!(!output.ends_with(b"\n"));
    assert!(!failure.message().contains(CANARY));
}
