//! Independent, instance-local brush-tool preferences, outside document history.
use crate::brush::Settings;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const FILE_LIMIT: u64 = 128 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    tools: BTreeMap<String, Profile>,
}
pub(crate) fn read_owned(path: &Path) -> Result<Option<Vec<u8>>, String> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Symbolic tool preference files remain protected".into());
    }
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Tool preferences need a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err("Tool preferences exceed 128 KiB".into());
    }
    Ok(Some(bytes))
}

pub const TOOLS: &[&str] = &["brush", "eraser", "smudge", "clone", "heal", "liquify"];
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub settings: Settings,
    pub preset: Option<String>,
    pub liquify_mode: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub profile: Profile,
    pub revision: u64,
    pub actor: String,
    #[serde(skip)]
    pub persistent: bool,
}
pub struct Store {
    tools: BTreeMap<String, Snapshot>,
    path: Option<PathBuf>,
    original: Option<Vec<u8>>,
    pub error: Option<String>,
    save_error: Option<String>,
    dirty: bool,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            tools: TOOLS
                .iter()
                .map(|key| {
                    (
                        (*key).into(),
                        Snapshot {
                            profile: if *key == "liquify" {
                                Profile {
                                    settings: Settings {
                                        opacity: 0.5,
                                        ..Default::default()
                                    },
                                    liquify_mode: Some("push".into()),
                                    ..Default::default()
                                }
                            } else {
                                Profile::default()
                            },
                            revision: 0,
                            actor: "human".into(),
                            persistent: false,
                        },
                    )
                })
                .collect(),
            path: None,
            original: None,
            error: None,
            save_error: None,
            dirty: false,
        }
    }
}
impl Store {
    pub fn load(path: PathBuf) -> Self {
        let mut store = Self::default();
        let result = (|| {
            store.original = read_owned(&path)?;
            let Some(bytes) = &store.original else {
                return Ok(());
            };
            let stored: Stored = serde_json::from_slice(bytes)
                .map_err(|e| format!("Unreadable tool preferences: {e}"))?;
            if stored.version != 1 || stored.tools.len() > TOOLS.len() {
                return Err("Unsupported tool preferences".into());
            }
            let mut validated = Self::default();
            for (tool, profile) in stored.tools {
                validated.set(&tool, profile, "human", None)?;
            }
            if read_owned(&path)? != store.original {
                return Err("Tool preferences changed during loading".into());
            }
            store.tools = validated.tools;
            Ok(())
        })();
        store.error = result.err();
        store.path = Some(path);
        for snapshot in store.tools.values_mut() {
            snapshot.revision = 0;
            snapshot.persistent = true;
        }
        store
    }
    pub fn get(&self, tool: &str) -> Result<Snapshot, String> {
        self.tools
            .get(tool)
            .cloned()
            .ok_or_else(|| "Choose brush, eraser, smudge, clone, heal or liquify".into())
    }
    pub fn state(&self) -> Value {
        json!({"tools":self.tools.iter().map(|(tool,snapshot)|(tool.clone(),snapshot.state(tool))).collect::<BTreeMap<_,_>>(),"persistence":if self.path.is_some(){"configured preferences directory"}else{"current instance"},"error":self.error,"save_error":self.save_error,"pending_save":self.dirty})
    }
    pub fn set(
        &mut self,
        tool: &str,
        profile: Profile,
        actor: &str,
        expected: Option<u64>,
    ) -> Result<Snapshot, String> {
        self.set_profile(tool, profile, actor, expected, true)
    }
    fn set_profile(
        &mut self,
        tool: &str,
        profile: Profile,
        actor: &str,
        expected: Option<u64>,
        persist: bool,
    ) -> Result<Snapshot, String> {
        let before = self.get(tool)?;
        if actor.trim().is_empty()
            || actor.chars().count() > 80
            || actor.chars().any(char::is_control)
        {
            return Err("Tool preference actor must contain 1–80 readable characters".into());
        }
        if actor != "human" && expected.is_none() {
            return Err("Observe tool preferences and provide expected_tool_revision".into());
        }
        if expected.is_some_and(|revision| revision != before.revision) {
            return Err("Tool preferences changed; observe again before updating".into());
        }
        profile.settings.validate()?;
        if tool == "liquify" {
            if profile.preset.is_some()
                || !matches!(
                    profile.liquify_mode.as_deref(),
                    Some("push" | "expand" | "pinch" | "restore")
                )
                || profile.settings
                    != (Settings {
                        radius: profile.settings.radius,
                        opacity: profile.settings.opacity,
                        ..Default::default()
                    })
            {
                return Err("Liquify preferences support radius, strength and mode".into());
            }
        } else if profile.liquify_mode.is_some() {
            return Err("Liquify mode belongs to the Liquify tool".into());
        }
        if profile.preset.as_ref().is_some_and(|id| {
            id.is_empty()
                || id.len() > 64
                || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }) {
            return Err("Invalid tool brush preset ID".into());
        }
        if profile == before.profile {
            if persist {
                self.flush()?;
            }
            return Ok(before);
        }
        let after = Snapshot {
            profile,
            revision: before
                .revision
                .checked_add(1)
                .ok_or("Tool preference revision exhausted")?,
            actor: actor.into(),
            persistent: before.persistent,
        };
        if persist {
            let mut profiles = self.profiles();
            profiles.insert(tool.into(), after.profile.clone());
            if let Err(error) = self.persist(profiles) {
                self.save_error = Some(error.clone());
                return Err(error);
            }
        } else {
            self.dirty = self.path.is_some();
        }
        self.tools.insert(tool.into(), after.clone());
        Ok(after)
    }
    fn profiles(&self) -> BTreeMap<String, Profile> {
        self.tools
            .iter()
            .map(|(key, snapshot)| (key.clone(), snapshot.profile.clone()))
            .collect()
    }
    fn persist(&mut self, profiles: BTreeMap<String, Profile>) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(format!(
                "Tool preferences preserved; repair or restore their file before saving: {error}"
            ));
        }
        if let Some(path) = &self.path {
            let _lease = crate::file_safety::Lease::acquire(path)?;
            if read_owned(path)? != self.original {
                return Err(
                    "Tool preferences changed on disk; restart to load them before saving".into(),
                );
            }
            let bytes = serde_json::to_vec_pretty(&Stored {
                version: 1,
                tools: profiles,
            })
            .map_err(|e| e.to_string())?;
            crate::server::atomic_write_checked(path, &bytes, |_| {
                if read_owned(path)? == self.original {
                    Ok(())
                } else {
                    Err("Tool preferences changed while saving".into())
                }
            })?;
            self.original = Some(bytes);
        }
        self.dirty = false;
        self.save_error = None;
        Ok(())
    }
    pub fn flush(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(error) = &self.save_error {
            return Err(error.clone());
        }
        let result = self.persist(self.profiles());
        if let Err(error) = &result {
            self.save_error = Some(error.clone());
        }
        result
    }
    /// Merge only controls changed by human input, retaining newer independent values.
    pub fn human_delta(
        &mut self,
        tool: &str,
        before: &Profile,
        after: &Profile,
    ) -> Result<Snapshot, String> {
        let latest = self.get(tool)?;
        let mut profile = latest.profile;
        let old = serde_json::to_value(before.settings).map_err(|e| e.to_string())?;
        let new = serde_json::to_value(after.settings).map_err(|e| e.to_string())?;
        let mut merged = serde_json::to_value(profile.settings).map_err(|e| e.to_string())?;
        for (key, value) in new.as_object().unwrap() {
            if old.get(key) != Some(value) {
                merged[key] = value.clone();
            }
        }
        profile.settings = serde_json::from_value(merged).map_err(|e| e.to_string())?;
        if before.preset != after.preset {
            profile.preset = after.preset.clone();
        }
        if before.liquify_mode != after.liquify_mode {
            profile.liquify_mode = after.liquify_mode.clone();
        }
        self.set_profile(tool, profile, "human", None, false)
    }
}

impl Snapshot {
    pub fn state(&self, tool: &str) -> Value {
        let settings = if tool == "liquify" {
            json!({"radius":self.profile.settings.radius,"strength":self.profile.settings.opacity,"mode":self.profile.liquify_mode})
        } else {
            serde_json::to_value(self.profile.settings).unwrap()
        };
        json!({"tool":tool,"profile":{"settings":settings,"preset":self.profile.preset},"revision":self.revision,"actor":self.actor,"persistence":if self.persistent{"configured preferences directory"}else{"current instance"}})
    }
}
