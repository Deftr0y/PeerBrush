use peerbrush::discovery::{self, ClientConfig, ClientKind, DiscoveryPaths, Manifest};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

struct Fixture {
    root: PathBuf,
    executable: PathBuf,
    state: PathBuf,
    paths: DiscoveryPaths,
}
impl Fixture {
    fn new(kinds: &[ClientKind]) -> Self {
        let root =
            std::env::temp_dir().join(format!("peerbrush-discovery-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("bin").join(if cfg!(windows) {
            "peerbrush.exe"
        } else {
            "peerbrush"
        });
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"test executable; never launched").unwrap();
        let state = root.join("canvas state");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join("connection.json"),
            br#"{"token":"DO_NOT_LEAK_DUMMY_TOKEN","url":"http://127.0.0.1:1234"}"#,
        )
        .unwrap();
        let clients = kinds
            .iter()
            .enumerate()
            .map(|(i, &kind)| ClientConfig {
                kind,
                detected: true,
                path: root
                    .join(format!("client-{i}"))
                    .join(if kind == ClientKind::Codex {
                        "config.toml"
                    } else {
                        "config.json"
                    }),
            })
            .collect();
        let paths = DiscoveryPaths {
            manifest: root.join(".peerbrush/mcp.json"),
            clients,
        };
        Self {
            root,
            executable,
            state,
            paths,
        }
    }
    fn write(&self, index: usize, text: &str) {
        let path = &self.paths.clients[index].path;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn text(&self, index: usize) -> String {
        fs::read_to_string(&self.paths.clients[index].path).unwrap()
    }
    fn connect(&self) -> discovery::Report {
        discovery::connect_at(&self.executable, &self.state, &self.paths).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn all_detected_clients_register_exact_canvas_and_repeat_without_rewrites() {
    let f = Fixture::new(&[
        ClientKind::Codex,
        ClientKind::ClaudeDesktop,
        ClientKind::Cursor,
        ClientKind::Gemini,
    ]);
    let report = f.connect();
    assert_eq!(report.registered_count, 4);
    assert!(!report.needs_retry());
    assert!(report.summary().contains("Reload or restart"));
    let expected = json!(["mcp", "--state-dir", f.state]);
    let toml = f.text(0).parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(
        toml["mcp_servers"]["peerbrush"]["command"].as_str(),
        f.executable.to_str()
    );
    assert_eq!(
        toml["mcp_servers"]["peerbrush"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["mcp", "--state-dir", f.state.to_str().unwrap()]
    );
    for i in 1..4 {
        let config: Value = serde_json::from_str(&f.text(i)).unwrap();
        assert_eq!(
            config["mcpServers"]["peerbrush"]["command"],
            json!(f.executable)
        );
        assert_eq!(config["mcpServers"]["peerbrush"]["args"], expected);
        if i == 2 {
            assert_eq!(config["mcpServers"]["peerbrush"]["type"], "stdio");
        }
    }
    let raw_manifest = fs::read_to_string(&f.paths.manifest).unwrap();
    assert!(!raw_manifest.contains("DO_NOT_LEAK_DUMMY_TOKEN"));
    let manifest: Manifest = serde_json::from_str(&raw_manifest).unwrap();
    assert_eq!(manifest.connection_file, f.state.join("connection.json"));
    assert_eq!(manifest.http_endpoint_template, "{connection.url}/mcp");
    assert_eq!(
        manifest.http_protocol_versions,
        peerbrush::server::MCP_HTTP_VERSIONS
    );
    let snapshots = (0..4).map(|i| f.text(i)).collect::<Vec<_>>();
    let second = f.connect();
    assert_eq!(second.registered_count, 4);
    assert!(second
        .clients
        .iter()
        .all(|c| !c.changed && c.backup.is_none()));
    assert_eq!((0..4).map(|i| f.text(i)).collect::<Vec<_>>(), snapshots);
    assert_eq!(fs::read_to_string(&f.paths.manifest).unwrap(), raw_manifest);
}

#[test]
fn updating_known_peerbrush_preserves_comments_settings_and_exact_original_backup() {
    let f = Fixture::new(&[ClientKind::Codex, ClientKind::Cursor]);
    let codex = "# keep this comment\nmodel = 'test-model' # compact formatting\n\n[mcp_servers.other]\ncommand = 'other' # keep this too\nargs = ['serve']\n\n[mcp_servers.peerbrush]\ncommand = 'C:/previous/peerbrush.exe'\nargs = ['mcp']\nenabled = false\ntool_timeout_sec = 123 # custom timeout\n\n[mcp_servers.peerbrush.env]\nCUSTOM_VALUE = 'retain-me'\n";
    let cursor = r#"{
  // human comment remains byte-for-byte
  "theme": "dark",
  "mcpServers": {
    "other": {"command":"node", "args":["other"], /* keep */ "env":{"SECRET":"dummy"}},
    "peerbrush": {
      "command": "C:\\previous\\peerbrush.exe", // command note
      "args": ["mcp"],
      "env": {"CUSTOM_VALUE": "retain-me"},
    },
  },
}"#;
    f.write(0, codex);
    f.write(1, cursor);
    let report = f.connect();
    assert_eq!(report.registered_count, 2, "{}", report.summary());
    for (index, original) in [codex, cursor].iter().enumerate() {
        assert_eq!(
            fs::read_to_string(report.clients[index].backup.as_ref().unwrap()).unwrap(),
            *original
        );
        assert!(report.clients[index].message.contains("previous"));
    }
    let after = f.text(0);
    assert!(after.contains("model = 'test-model' # compact formatting"));
    assert!(after.contains("command = 'other' # keep this too"));
    assert!(after.contains("tool_timeout_sec = 123 # custom timeout"));
    assert!(after.contains("CUSTOM_VALUE = 'retain-me'"));
    assert_eq!(
        after.parse::<toml_edit::DocumentMut>().unwrap()["mcp_servers"]["peerbrush"]["enabled"]
            .as_bool(),
        Some(true)
    );
    let after = f.text(1);
    assert!(after.contains("// human comment remains byte-for-byte"));
    assert!(after.contains(
        r#""other": {"command":"node", "args":["other"], /* keep */ "env":{"SECRET":"dummy"}}"#
    ));
    assert!(after.contains("// command note"));
    assert!(after.contains(r#""env": {"CUSTOM_VALUE": "retain-me"}"#));
    let again = f.connect();
    assert!(again.clients.iter().all(|c| !c.changed));
}

#[test]
fn conflicting_server_names_are_kept_and_local_name_is_stable() {
    let f = Fixture::new(&[ClientKind::Codex, ClientKind::ClaudeDesktop]);
    let codex = "[mcp_servers.peerbrush]\ncommand = 'node'\nargs = ['unrelated-server']\n";
    let json =
        r#"{"mcpServers":{"peerbrush":{"command":"python","args":["unrelated"]}},"theme":"dark"}"#;
    f.write(0, codex);
    f.write(1, json);
    let first = f.connect();
    assert_eq!(first.registered_count, 2);
    for client in &first.clients {
        assert!(client.server_name.starts_with("peerbrush-local-"));
    }
    assert_eq!(first.clients[0].server_name, first.clients[1].server_name);
    assert!(f.text(0).contains(codex.trim()));
    let value: Value = serde_json::from_str(&f.text(1)).unwrap();
    assert_eq!(
        value["mcpServers"]["peerbrush"],
        json!({"command":"python","args":["unrelated"]})
    );
    let second = f.connect();
    assert!(second.clients.iter().all(|c| !c.changed));
    assert_eq!(first.clients[0].server_name, second.clients[0].server_name);
}

#[test]
fn partial_invalid_configs_leave_files_unchanged_and_successful_clients_work() {
    let f = Fixture::new(&[
        ClientKind::Codex,
        ClientKind::ClaudeDesktop,
        ClientKind::Cursor,
        ClientKind::Gemini,
    ]);
    let invalid_toml = "model = 'DO_NOT_LEAK_DUMMY_TOKEN\n";
    let invalid_json = r#"{"mcpServers":[], "keep":true}"#;
    let duplicate_json = r#"{"mcpServers":{},"mcpServers":{"x":{}}}"#;
    f.write(0, invalid_toml);
    f.write(2, invalid_json);
    f.write(3, duplicate_json);
    let report = f.connect();
    assert_eq!(report.registered_count, 1);
    assert!(report.needs_retry());
    assert!(report.clients[1].registered);
    assert!(
        report.clients[0].error.is_some()
            && report.clients[2].error.is_some()
            && report.clients[3].error.is_some()
    );
    assert_eq!(f.text(0), invalid_toml);
    assert_eq!(f.text(2), invalid_json);
    assert_eq!(f.text(3), duplicate_json);
    assert!(!report.summary().contains("DO_NOT_LEAK_DUMMY_TOKEN"));
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("DO_NOT_LEAK_DUMMY_TOKEN"));
}

#[test]
fn gemini_policy_is_reported_without_overriding_client_permissions() {
    let f = Fixture::new(&[ClientKind::Gemini]);
    let original = r#"{"mcp":{"allowed":["approved-only"],"excluded":["peerbrush"]},"security":{"trustedFolders":{"enabled":true}}}"#;
    f.write(0, original);
    let report = f.connect();
    assert_eq!(report.registered_count, 1);
    assert!(report.clients[0].requires_attention);
    assert!(report.needs_retry());
    let value: Value = serde_json::from_str(&f.text(0)).unwrap();
    assert_eq!(value["mcp"]["allowed"], json!(["approved-only"]));
    assert_eq!(value["mcp"]["excluded"], json!(["peerbrush"]));
    assert_eq!(value["security"]["trustedFolders"]["enabled"], true);
    assert!(report.summary().contains("allow/exclude"));
}

#[test]
fn undetected_clients_are_never_written_and_manifest_failure_is_partial() {
    let mut f = Fixture::new(&[ClientKind::Codex, ClientKind::Cursor]);
    f.paths.clients[1].detected = false;
    // A directory at the manifest filename forces an error without touching another user's files.
    fs::create_dir_all(&f.paths.manifest).unwrap();
    let report = f.connect();
    assert_eq!(report.registered_count, 1);
    assert!(report.manifest_error.is_some());
    assert!(report.needs_retry());
    assert!(!f.paths.clients[1].path.exists());
    assert_eq!(report.clients.len(), 1);
}

#[test]
fn a_new_canvas_updates_exact_target_and_keeps_previous_manifest_and_registration_backups() {
    let f = Fixture::new(&[ClientKind::Codex]);
    f.connect();
    let previous_manifest = fs::read(&f.paths.manifest).unwrap();
    let previous_config = f.text(0);
    let new_state = f.root.join("another canvas");
    let report = discovery::connect_at(&f.executable, &new_state, &f.paths).unwrap();
    assert_eq!(report.registered_count, 1);
    assert!(report.clients[0].changed);
    assert_eq!(
        fs::read_to_string(report.clients[0].backup.as_ref().unwrap()).unwrap(),
        previous_config
    );
    let entries = fs::read_dir(f.paths.manifest.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    assert!(entries
        .iter()
        .any(|e| e.file_name().to_string_lossy().ends_with(".bak")
            && fs::read(e.path()).unwrap() == previous_manifest));
    let manifest: Manifest = serde_json::from_slice(&fs::read(&f.paths.manifest).unwrap()).unwrap();
    assert_eq!(manifest.args[2], new_state.to_str().unwrap());
}

#[cfg(unix)]
#[test]
fn symlink_config_keeps_link_and_private_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let f = Fixture::new(&[ClientKind::Codex]);
    let target = f.root.join("dotfiles/codex.toml");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, "# private\nmodel='test'\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir_all(f.paths.clients[0].path.parent().unwrap()).unwrap();
    symlink(&target, &f.paths.clients[0].path).unwrap();
    let report = f.connect();
    assert_eq!(report.registered_count, 1);
    assert!(fs::symlink_metadata(&f.paths.clients[0].path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(report.clients[0].backup.as_ref().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn codex_inline_tables_preserve_other_servers_and_their_comments() {
    let f = Fixture::new(&[ClientKind::Codex]);
    let original = "# compact format\nmcp_servers = { other = { command = 'other', args = ['serve'] } } # keep inline note\n";
    f.write(0, original);
    let report = f.connect();
    assert_eq!(report.registered_count, 1, "{}", report.summary());
    let text = f.text(0);
    assert!(text.contains("# keep inline note"));
    let document = text.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(
        document["mcp_servers"]["other"]["command"].as_str(),
        Some("other")
    );
    assert_eq!(
        document["mcp_servers"]["peerbrush"]["command"].as_str(),
        f.executable.to_str()
    );
    assert!(f.connect().clients.iter().all(|client| !client.changed));
}

#[test]
fn published_manifest_is_ready_without_an_installed_client() {
    let fixture = Fixture::new(&[]);
    let report = fixture.connect();
    assert_eq!(report.registered_count, 0);
    assert!(report.manifest.exists());
    assert!(!report.needs_retry());
    assert!(report.summary().contains("Manual setup"));
}
