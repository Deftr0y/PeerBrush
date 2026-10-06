//! Explicit, local-only MCP registration. Called from a worker after Connect AI.
//! Client formats: learn.chatgpt.com/docs/extend/mcp, cursor.com/docs/mcp,
//! geminicli.com/docs/tools/mcp-server, modelcontextprotocol.io/docs/develop/connect-local-servers.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use toml_edit::{value, Array, DocumentMut, Item, Table};

const MAX_CONFIG_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub schema_version: u32,
    pub name: String,
    pub version: String,
    pub transport: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub connection_file: PathBuf,
    /// Resolve connection.url from connection_file; its bearer token stays there.
    pub http_endpoint_template: String,
    #[serde(default)]
    pub http_protocol_versions: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub registered_count: usize,
    pub manifest: PathBuf,
    pub manifest_error: Option<String>,
    pub clients: Vec<ClientResult>,
}
impl Report {
    pub fn needs_retry(&self) -> bool {
        self.manifest_error.is_some()
            || self
                .clients
                .iter()
                .any(|c| c.error.is_some() || c.requires_attention)
    }
    pub fn summary(&self) -> String {
        let names = self
            .clients
            .iter()
            .filter(|c| c.registered)
            .map(|c| c.client.as_str())
            .collect::<Vec<_>>();
        let mut lines = if names.is_empty() {
            vec!["No supported AI client was found. Manual setup is available.".to_owned()]
        } else {
            vec![format!("PeerBrush registered with {}. Reload or restart these clients, then approve PeerBrush if prompted.", names.join(", "))]
        };
        if let Some(error) = &self.manifest_error {
            lines.push(format!("Discovery manifest: {error}"));
        } else {
            lines.push(format!(
                "Other local agents can read {}.",
                self.manifest.display()
            ));
        }
        for client in &self.clients {
            if let Some(error) = &client.error {
                lines.push(format!("{}: {error}", client.client));
            } else if client.requires_attention
                || client.message.contains("previous")
                || client.server_name != "peerbrush"
            {
                lines.push(format!("{}: {}", client.client, client.message));
            }
        }
        lines.join("\n")
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ClientResult {
    pub client: String,
    pub config: PathBuf,
    pub server_name: String,
    pub registered: bool,
    pub changed: bool,
    pub backup: Option<PathBuf>,
    pub requires_attention: bool,
    pub message: String,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientKind {
    Codex,
    ClaudeDesktop,
    Cursor,
    Gemini,
}
impl ClientKind {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::Cursor => "Cursor",
            Self::Gemini => "Gemini CLI",
        }
    }
}
#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub kind: ClientKind,
    pub path: PathBuf,
    /// False entries are never written. Native detection uses config/app/executable presence.
    pub detected: bool,
}
/// Fully injected destinations keep tests independent from the real home and PATH.
#[derive(Clone, Debug)]
pub struct DiscoveryPaths {
    pub manifest: PathBuf,
    pub clients: Vec<ClientConfig>,
}

pub fn manifest_path() -> Result<PathBuf, String> {
    Ok(user_home()?.join(".peerbrush").join("mcp.json"))
}
pub fn discover() -> Result<Manifest, String> {
    let path = manifest_path()?;
    let bytes = read_existing(&path)?
        .ok_or("Connect AI in PeerBrush first to publish its local discovery manifest")?;
    serde_json::from_slice(&bytes)
        .map_err(|_| "PeerBrush discovery manifest is invalid; use Connect AI to refresh it".into())
}
pub fn connect(executable: &Path, state_dir: &Path) -> Result<Report, String> {
    connect_at(executable, state_dir, &native_paths()?)
}
pub fn connect_at(
    executable: &Path,
    state_dir: &Path,
    paths: &DiscoveryPaths,
) -> Result<Report, String> {
    if !executable.is_absolute() || !executable.is_file() {
        return Err("PeerBrush executable must be an existing absolute file path".into());
    }
    if !state_dir.is_absolute() || !paths.manifest.is_absolute() {
        return Err("PeerBrush state and discovery locations must be absolute paths".into());
    }
    let command = executable
        .to_str()
        .ok_or("PeerBrush executable path must be valid Unicode")?;
    let state = state_dir
        .to_str()
        .ok_or("PeerBrush state path must be valid Unicode")?;
    let args = vec!["mcp".to_owned(), "--state-dir".to_owned(), state.to_owned()];
    let manifest = Manifest {
        schema_version: 1,
        name: "PeerBrush".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        transport: "stdio".into(),
        command: executable.to_owned(),
        args: args.clone(),
        connection_file: state_dir.join("connection.json"),
        http_endpoint_template: "{connection.url}/mcp".into(),
        http_protocol_versions: crate::server::MCP_HTTP_VERSIONS
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|_| "Cannot encode PeerBrush discovery paths")?;
    bytes.push(b'\n');
    let manifest_error = (|| {
        let original = read_existing(&paths.manifest)?;
        write_backed_up(&paths.manifest, original.as_deref(), &bytes)?;
        Ok::<(), String>(())
    })()
    .err();
    let mut report = Report {
        registered_count: 0,
        manifest: paths.manifest.clone(),
        manifest_error,
        clients: vec![],
    };
    for client in paths.clients.iter().filter(|c| c.detected) {
        let mut result = ClientResult {
            client: client.kind.name().into(),
            config: client.path.clone(),
            server_name: "peerbrush".into(),
            registered: false,
            changed: false,
            backup: None,
            requires_attention: false,
            message: String::new(),
            error: None,
        };
        let operation = (|| {
            if !client.path.is_absolute() {
                return Err("Client configuration path must be absolute".into());
            }
            let original = read_existing(&client.path)?;
            let text =
                std::str::from_utf8(original.as_deref().unwrap_or_default()).map_err(|_| {
                    "Configuration is not valid UTF-8; edit it in the client before retrying"
                })?;
            let change = match client.kind {
                ClientKind::Codex => register_toml(text, command, &args, state_dir)?,
                _ => register_json(text, client.kind, command, &args, state_dir)?,
            };
            result.server_name = change.name;
            result.requires_attention = change.blocked;
            result.message = change.message;
            result.backup =
                write_backed_up(&client.path, original.as_deref(), change.text.as_bytes())?;
            result.changed = original.as_deref() != Some(change.text.as_bytes());
            result.registered = true;
            Ok::<(), String>(())
        })();
        if let Err(error) = operation {
            result.error = Some(error);
        }
        if result.registered {
            report.registered_count += 1;
        }
        report.clients.push(result);
    }
    Ok(report)
}

struct Change {
    text: String,
    name: String,
    blocked: bool,
    message: String,
}
fn owned(command: Option<&str>, args: Option<&Value>) -> bool {
    let base = command
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    (base.eq_ignore_ascii_case("peerbrush") || base.eq_ignore_ascii_case("peerbrush.exe"))
        && args
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("mcp")))
}
fn alternative_name(state_dir: &Path) -> String {
    // Stable across platforms/processes, unlike DefaultHasher; never exposes a token.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in state_dir.to_string_lossy().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("peerbrush-local-{hash:016x}")
}
fn registration_message(name: &str, previous: bool, blocked: bool) -> String {
    if blocked {
        format!("Registered '{name}', but Gemini's allow/exclude policy blocks it. Enable this server in Gemini's MCP settings; other permissions were preserved.")
    } else if name != "peerbrush" {
        format!("The existing 'peerbrush' name belongs to another server. Registered '{name}' alongside it.")
    } else if previous {
        "Updated the previous PeerBrush target to this canvas; its original configuration was backed up.".into()
    } else {
        "Ready for the client's next MCP reload; accept its normal tool approval if prompted."
            .into()
    }
}
fn register_toml(
    text: &str,
    command: &str,
    args: &[String],
    state: &Path,
) -> Result<Change, String> {
    let mut document = text.parse::<DocumentMut>().map_err(|_| {
        "Invalid TOML configuration; fix it in Codex settings, then retry (file unchanged)"
    })?;
    if document.get("mcp_servers").is_none() {
        document["mcp_servers"] = Item::Table(Table::new());
    }
    let servers = document
        .get_mut("mcp_servers")
        .and_then(Item::as_table_like_mut)
        .ok_or("Codex mcp_servers must be a TOML table; file unchanged")?;
    let is_owned = |item: &Item| {
        let args = item
            .get("args")
            .and_then(Item::as_array)
            .map(|a| Value::Array(a.iter().map(|v| json!(v.as_str())).collect()));
        owned(item.get("command").and_then(Item::as_str), args.as_ref())
            && item.get("url").is_none()
    };
    let name = match servers.get("peerbrush") {
        Some(item) if !is_owned(item) => alternative_name(state),
        _ => "peerbrush".into(),
    };
    if servers.get(&name).is_some_and(|item| !is_owned(item)) {
        return Err("A different server also uses the PeerBrush instance name; rename it in client settings before retrying (file unchanged)".into());
    }
    let same_args = |item: Option<&Item>| {
        item.and_then(Item::as_array).is_some_and(|a| {
            a.iter()
                .map(|v| v.as_str())
                .eq(args.iter().map(|s| Some(s.as_str())))
        })
    };
    let previous = servers.get(&name).is_some_and(|item| {
        item.get("command").and_then(Item::as_str) != Some(command) || !same_args(item.get("args"))
    });
    if servers.get(&name).is_none() {
        servers.insert(&name, Item::Table(Table::new()));
    }
    let entry = servers
        .get_mut(&name)
        .and_then(Item::as_table_like_mut)
        .ok_or("PeerBrush configuration must be a TOML table; file unchanged")?;
    let mut array = Array::new();
    for arg in args {
        array.push(arg.as_str());
    }
    if entry.get("command").and_then(Item::as_str) != Some(command) {
        entry.insert("command", value(command));
    }
    if !same_args(entry.get("args")) {
        entry.insert("args", value(array));
    }
    if entry.get("enabled").and_then(Item::as_bool) == Some(false) {
        entry.insert("enabled", value(true));
    }
    Ok(Change {
        text: document.to_string(),
        message: registration_message(&name, previous, false),
        name,
        blocked: false,
    })
}
fn register_json(
    text: &str,
    kind: ClientKind,
    command: &str,
    args: &[String],
    state: &Path,
) -> Result<Change, String> {
    let text = if text.trim().is_empty() { "{}\n" } else { text };
    let root = parse_json(text)?;
    let root_members = root
        .members
        .as_ref()
        .ok_or("Client configuration must be a JSON object; file unchanged")?;
    let servers = member(root_members, "mcpServers");
    if servers.is_some_and(|node| node.members.is_none()) {
        return Err("mcpServers must be a JSON object; file unchanged".into());
    }
    let current = servers.and_then(|s| member(s.members.as_ref().unwrap(), "peerbrush"));
    let known = |node: &Node| {
        owned(node.value["command"].as_str(), node.value.get("args"))
            && node.value.get("url").is_none()
    };
    let name = if current.is_some_and(|node| !known(node)) {
        alternative_name(state)
    } else {
        "peerbrush".into()
    };
    let existing = servers.and_then(|s| member(s.members.as_ref().unwrap(), &name));
    if existing.is_some_and(|node| !known(node)) {
        return Err("A different server also uses the PeerBrush instance name; rename it in client settings before retrying (file unchanged)".into());
    }
    let previous = existing
        .is_some_and(|node| node.value["command"] != command || node.value["args"] != json!(args));
    let mut settings = vec![
        ("command".to_owned(), json!(command)),
        ("args".to_owned(), json!(args)),
    ];
    if kind == ClientKind::Cursor {
        settings.push(("type".into(), json!("stdio")));
    }
    let mut entry = serde_json::Map::new();
    for (key, val) in &settings {
        entry.insert(key.clone(), val.clone());
    }
    let output = if let Some(existing) = existing {
        patch_object(text, existing, &settings)?
    } else if let Some(servers) = servers {
        patch_object(text, servers, &[(name.clone(), Value::Object(entry))])?
    } else {
        let mut entries = serde_json::Map::new();
        entries.insert(name.clone(), Value::Object(entry));
        patch_object(
            text,
            &root,
            &[("mcpServers".into(), Value::Object(entries))],
        )?
    };
    // Validate the final JSON/JSONC before any file write; never trust textual splicing alone.
    let parsed = parse_json(&output)?;
    let blocked = kind == ClientKind::Gemini
        && (parsed.value["mcp"]["allowed"]
            .as_array()
            .is_some_and(|a| !a.iter().any(|v| v.as_str() == Some(&name)))
            || parsed.value["mcp"]["excluded"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(&name))));
    Ok(Change {
        text: output,
        message: registration_message(&name, previous, blocked),
        name,
        blocked,
    })
}

// A small JSONC span reader allows targeted edits without reformatting comments,
// unrelated server definitions, tokens or other user settings. No input is logged.
struct Node {
    start: usize,
    end: usize,
    value: Value,
    members: Option<Vec<Member>>,
    trailing_comma: bool,
}
struct Member {
    name: String,
    node: Node,
}
fn member<'a>(members: &'a [Member], name: &str) -> Option<&'a Node> {
    members.iter().find(|m| m.name == name).map(|m| &m.node)
}
struct Parser<'a> {
    text: &'a str,
    pos: usize,
    nodes: usize,
}
impl Parser<'_> {
    fn skip(&mut self) -> Result<(), String> {
        loop {
            let bytes = self.text.as_bytes();
            while self.pos < bytes.len() && bytes[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if bytes.get(self.pos..self.pos + 2) == Some(b"//") {
                while self.pos < bytes.len() && bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else if bytes.get(self.pos..self.pos + 2) == Some(b"/*") {
                self.pos += 2;
                while bytes.get(self.pos..self.pos + 2) != Some(b"*/") {
                    if self.pos + 1 >= bytes.len() {
                        return Err("Unterminated configuration comment; file unchanged".into());
                    }
                    self.pos += 1;
                }
                self.pos += 2;
            } else {
                return Ok(());
            }
        }
    }
    fn string(&mut self) -> Result<String, String> {
        let start = self.pos;
        if self.text.as_bytes().get(self.pos) != Some(&b'"') {
            return Err("Invalid JSON configuration; file unchanged".into());
        }
        self.pos += 1;
        while let Some(&byte) = self.text.as_bytes().get(self.pos) {
            self.pos += 1;
            if byte == b'\\' {
                self.pos += 1;
            } else if byte == b'"' {
                return serde_json::from_str(&self.text[start..self.pos])
                    .map_err(|_| "Invalid JSON string; file unchanged".into());
            }
        }
        Err("Unterminated JSON string; file unchanged".into())
    }
    fn node(&mut self, depth: usize) -> Result<Node, String> {
        self.nodes += 1;
        if depth > 64 || self.nodes > 50000 {
            return Err(
                "Client configuration is too complex to update safely; file unchanged".into(),
            );
        }
        self.skip()?;
        let start = self.pos;
        let byte = *self
            .text
            .as_bytes()
            .get(self.pos)
            .ok_or("Incomplete JSON configuration; file unchanged")?;
        let mut trailing_comma = false;
        let mut members = None;
        let val = match byte {
            b'{' => {
                self.pos += 1;
                let mut items = vec![];
                let mut object = serde_json::Map::new();
                self.skip()?;
                while self.text.as_bytes().get(self.pos) != Some(&b'}') {
                    let name = self.string()?;
                    if object.contains_key(&name) {
                        return Err(
                            "Duplicate JSON keys cannot be updated safely; file unchanged".into(),
                        );
                    }
                    self.skip()?;
                    if self.text.as_bytes().get(self.pos) != Some(&b':') {
                        return Err("Invalid JSON object; file unchanged".into());
                    }
                    self.pos += 1;
                    let node = self.node(depth + 1)?;
                    object.insert(name.clone(), node.value.clone());
                    items.push(Member { name, node });
                    self.skip()?;
                    match self.text.as_bytes().get(self.pos) {
                        Some(b',') => {
                            self.pos += 1;
                            self.skip()?;
                            trailing_comma = self.text.as_bytes().get(self.pos) == Some(&b'}');
                        }
                        Some(b'}') => break,
                        _ => return Err("Invalid JSON object separator; file unchanged".into()),
                    }
                }
                self.pos += 1;
                members = Some(items);
                Value::Object(object)
            }
            b'[' => {
                self.pos += 1;
                self.skip()?;
                let mut array = vec![];
                while self.text.as_bytes().get(self.pos) != Some(&b']') {
                    array.push(self.node(depth + 1)?.value);
                    self.skip()?;
                    match self.text.as_bytes().get(self.pos) {
                        Some(b',') => {
                            self.pos += 1;
                            self.skip()?;
                        }
                        Some(b']') => break,
                        _ => return Err("Invalid JSON array separator; file unchanged".into()),
                    }
                }
                self.pos += 1;
                Value::Array(array)
            }
            b'"' => Value::String(self.string()?),
            _ => {
                while let Some(&b) = self.text.as_bytes().get(self.pos) {
                    if b.is_ascii_whitespace() || [b',', b'}', b']', b'/'].contains(&b) {
                        break;
                    }
                    self.pos += 1;
                }
                serde_json::from_str(&self.text[start..self.pos])
                    .map_err(|_| "Invalid JSON value; file unchanged")?
            }
        };
        Ok(Node {
            start,
            end: self.pos,
            value: val,
            members,
            trailing_comma,
        })
    }
}
fn parse_json(text: &str) -> Result<Node, String> {
    let mut parser = Parser {
        text,
        pos: usize::from(text.starts_with('\u{feff}')) * 3,
        nodes: 0,
    };
    let node = parser.node(0)?;
    parser.skip()?;
    if parser.pos != text.len() {
        return Err("Unexpected data after JSON configuration; file unchanged".into());
    }
    Ok(node)
}
fn patch_object(text: &str, node: &Node, fields: &[(String, Value)]) -> Result<String, String> {
    let members = node
        .members
        .as_ref()
        .ok_or("Server configuration must be a JSON object; file unchanged")?;
    let mut patches = vec![];
    let mut additions = vec![];
    for (key, val) in fields {
        if let Some(old) = member(members, key) {
            if old.value != *val {
                patches.push((
                    old.start,
                    old.end,
                    serde_json::to_string(val).map_err(|_| "Cannot encode MCP configuration")?,
                ));
            }
        } else {
            additions.push((key, val));
        }
    }
    if !additions.is_empty() {
        let close = node.end - 1;
        let line = text[..close].rfind('\n').map_or(0, |n| n + 1);
        let whitespace = &text[line..close];
        let indent = if whitespace.chars().all(char::is_whitespace) {
            whitespace
        } else {
            ""
        };
        let mut insert = if members.is_empty() || node.trailing_comma {
            String::new()
        } else {
            ",".into()
        };
        for (index, (key, val)) in additions.iter().enumerate() {
            if index > 0 {
                insert.push(',');
            }
            insert.push_str(&format!(
                "\n{indent}  {}: {}",
                serde_json::to_string(key).unwrap(),
                serde_json::to_string(val).map_err(|_| "Cannot encode MCP configuration")?
            ));
        }
        insert.push_str(&format!("\n{indent}"));
        patches.push((close, close, insert));
    }
    patches.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut out = text.to_owned();
    for (start, end, replacement) in patches {
        out.replace_range(start..end, &replacement);
    }
    Ok(out)
}

fn read_existing(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot read configuration: {error}")),
    };
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_CONFIG_BYTES {
        return Err("Configuration exceeds the 2 MiB update limit; edit it manually".into());
    }
    let mut bytes = vec![];
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("Configuration exceeds the 2 MiB update limit; edit it manually".into());
    }
    Ok(Some(bytes))
}
fn write_backed_up(
    path: &Path,
    original: Option<&[u8]>,
    bytes: &[u8],
) -> Result<Option<PathBuf>, String> {
    if original == Some(bytes) {
        return Ok(None);
    }
    let target = if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        fs::canonicalize(path)
            .map_err(|_| "Cannot resolve configuration symlink; file unchanged")?
    } else {
        path.to_owned()
    };
    let parent = target
        .parent()
        .ok_or("Configuration needs a parent directory")?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("Cannot create client configuration folder: {e}"))?;
    if read_existing(&target)?.as_deref() != original {
        return Err(
            "Configuration changed while connecting. Retry to preserve the newer settings".into(),
        );
    }
    let permissions = fs::metadata(&target).ok().map(|m| m.permissions());
    let nonce = uuid::Uuid::new_v4();
    let backup = if let Some(original) = original {
        let name = target
            .file_name()
            .ok_or("Configuration needs a filename")?
            .to_string_lossy();
        let backup = parent.join(format!("{name}.peerbrush-{nonce}.bak"));
        write_new(&backup, original, permissions.as_ref())?;
        Some(backup)
    } else {
        None
    };
    let temp = parent.join(format!(".peerbrush-{nonce}.tmp"));
    let result = (|| {
        write_new(&temp, bytes, permissions.as_ref())?;
        // Recheck after durable backup/temp writes in case the client edited settings.
        if read_existing(&target)?.as_deref() != original {
            return Err(
                "Configuration changed while connecting. Retry to preserve the newer settings"
                    .into(),
            );
        }
        fs::rename(&temp, &target).map_err(|e| {
            format!("Cannot replace configuration: {e}; original settings remain backed up")
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map(|()| backup)
}
fn write_new(
    path: &Path,
    bytes: &[u8],
    permissions: Option<&fs::Permissions>,
) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("Cannot write client configuration: {e}"))?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions.clone())
            .map_err(|e| e.to_string())?;
    }
    #[cfg(unix)]
    if permissions.is_none() {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("Cannot save client configuration: {e}"))
}

fn user_home() -> Result<PathBuf, String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| "Cannot locate the user home folder for MCP discovery".into())
}
fn native_paths() -> Result<DiscoveryPaths, String> {
    let home = user_home()?;
    let search_dirs = std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .unwrap_or_default();
    let on_path = |name: &str| {
        search_dirs.iter().any(|dir| {
            dir.join(name).is_file()
                || (cfg!(windows)
                    && ["exe", "cmd", "bat"]
                        .iter()
                        .any(|ext| dir.join(format!("{name}.{ext}")).is_file()))
        })
    };
    let configured = |path: &Path| path.is_file() || path.parent().is_some_and(Path::is_dir);
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".codex"));
    let (codex_app, cursor_app, claude_installed, claude_path) = platform_apps(&home);
    let codex_installed = on_path("codex") || codex_app;
    let cursor_installed = on_path("cursor") || on_path("cursor-agent") || cursor_app;
    let codex = codex_home.join("config.toml");
    let cursor = home.join(".cursor/mcp.json");
    let gemini = home.join(".gemini/settings.json");
    let mut clients = vec![
        ClientConfig {
            kind: ClientKind::Codex,
            detected: codex_installed || configured(&codex),
            path: codex,
        },
        ClientConfig {
            kind: ClientKind::Cursor,
            detected: cursor_installed || configured(&cursor),
            path: cursor,
        },
        ClientConfig {
            kind: ClientKind::Gemini,
            detected: on_path("gemini") || configured(&gemini),
            path: gemini,
        },
    ];
    if let Some(path) = claude_path {
        clients.push(ClientConfig {
            kind: ClientKind::ClaudeDesktop,
            detected: claude_installed || configured(&path),
            path,
        });
    }
    Ok(DiscoveryPaths {
        manifest: home.join(".peerbrush/mcp.json"),
        clients,
    })
}
#[cfg(windows)]
fn platform_apps(home: &Path) -> (bool, bool, bool, Option<PathBuf>) {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Local"));
    let roaming = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Roaming"));
    (
        contains_executable(&local.join("OpenAI/Codex/bin"), "codex.exe", None),
        local.join("Programs/cursor/Cursor.exe").is_file(),
        local.join("Claude/claude.exe").is_file()
            || contains_executable(&local.join("Claude"), "claude.exe", Some("app-")),
        Some(roaming.join("Claude/claude_desktop_config.json")),
    )
}
#[cfg(target_os = "macos")]
fn platform_apps(home: &Path) -> (bool, bool, bool, Option<PathBuf>) {
    let installed = |name: &str| {
        Path::new("/Applications")
            .join(format!("{name}.app"))
            .is_dir()
            || home
                .join("Applications")
                .join(format!("{name}.app"))
                .is_dir()
    };
    (
        installed("Codex"),
        installed("Cursor"),
        installed("Claude"),
        Some(home.join("Library/Application Support/Claude/claude_desktop_config.json")),
    )
}
#[cfg(not(any(windows, target_os = "macos")))]
fn platform_apps(_home: &Path) -> (bool, bool, bool, Option<PathBuf>) {
    (false, false, false, None)
}
#[cfg(windows)]
fn contains_executable(folder: &Path, filename: &str, prefix: Option<&str>) -> bool {
    fs::read_dir(folder).is_ok_and(|entries| {
        entries.take(256).filter_map(Result::ok).any(|entry| {
            entry.file_type().is_ok_and(|t| t.is_dir())
                && prefix.is_none_or(|p| entry.file_name().to_string_lossy().starts_with(p))
                && entry.path().join(filename).is_file()
        })
    })
}
