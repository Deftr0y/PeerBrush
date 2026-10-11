//! Instance-local export preferences; editable documents and history contain none of these settings.
use super::Settings;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
const LIMIT: usize = 64;
const FILE_LIMIT: usize = 128 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub settings: Settings,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    revision: u64,
    presets: Vec<Preset>,
}
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub revision: u64,
    pub presets: Vec<Preset>,
    pub error: Option<String>,
    pub actor: String,
}
#[derive(Default)]
pub struct Library {
    path: Option<PathBuf>,
    version: Option<crate::file_safety::Version>,
    revision: u64,
    presets: Vec<Preset>,
    pub error: Option<String>,
    actor: String,
    pub(crate) jobs: Vec<super::jobs::Job>,
}
fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err("Export preset names need 1–80 readable characters".into());
    }
    Ok(())
}
impl Library {
    pub fn load(path: PathBuf) -> Self {
        let mut library = Self {
            path: Some(path.clone()),
            ..Default::default()
        };
        let result = (|| {
            if std::fs::metadata(&path).is_ok_and(|m| m.len() > FILE_LIMIT as u64) {
                return Err("Export presets exceed 128 KiB".into());
            }
            library.version = crate::file_safety::snapshot(&path)?;
            let Some(bytes) = crate::tool_settings::read_owned(&path)? else {
                return Ok(());
            };
            let stored: Stored = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Unreadable export presets: {e}"))?;
            if stored.version != 1 || stored.presets.len() > LIMIT {
                return Err("Unsupported or oversized export preset library".into());
            }
            let mut ids = std::collections::HashSet::new();
            let mut names = std::collections::HashSet::new();
            for preset in &stored.presets {
                validate_name(&preset.name)?;
                preset.settings.validate()?;
                if !preset.id.starts_with("export-")
                    || preset.id.len() > 64
                    || !preset
                        .id
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                    || !ids.insert(preset.id.clone())
                    || !names.insert(preset.name.to_lowercase())
                {
                    return Err("Invalid or duplicate export preset identity/name".into());
                }
            }
            if crate::file_safety::snapshot(&path)? != library.version {
                return Err("Export presets changed while loading".into());
            }
            library.revision = stored.revision;
            library.presets = stored.presets;
            Ok(())
        })();
        library.error = result.err();
        library
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            presets: self.presets.clone(),
            error: self.error.clone(),
            actor: self.actor.clone(),
        }
    }
    pub fn get(&self, id: &str) -> Result<Preset, String> {
        self.presets
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or("Export preset is missing".into())
    }
    fn persist(&mut self, presets: Vec<Preset>, revision: u64, actor: &str) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(format!(
                "Preserve unreadable export presets before replacing them: {error}"
            ));
        }
        if revision != self.revision {
            return Err("Export presets changed; refresh before modifying them".into());
        }
        if actor.trim().is_empty() || actor.len() > 64 || actor.chars().any(char::is_control) {
            return Err("Use a readable export actor of up to 64 characters".into());
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or("Export preset revision exhausted")?;
        let bytes = serde_json::to_vec_pretty(&Stored {
            version: 1,
            revision,
            presets: presets.clone(),
        })
        .map_err(|e| e.to_string())?;
        if bytes.len() > FILE_LIMIT {
            return Err("Export presets exceed 128 KiB".into());
        }
        if let Some(path) = &self.path {
            let lease = crate::file_safety::Lease::acquire(path)?;
            if lease.expected != self.version {
                return Err("Export presets changed on disk; reload before saving".into());
            }
            let backup = lease.preserve()?;
            crate::server::atomic_write_checked(&lease.path, &bytes, |_| lease.validate())?;
            self.version = Some(crate::file_safety::published_version(&lease.path, &bytes)?);
            if let Some(backup) = backup {
                lease.prune(&backup);
            }
        }
        self.presets = presets;
        self.revision = revision;
        self.actor = actor.into();
        Ok(())
    }
    pub fn save(
        &mut self,
        id: Option<&str>,
        name: &str,
        settings: Settings,
        revision: u64,
        actor: &str,
    ) -> Result<Preset, String> {
        validate_name(name)?;
        settings.validate()?;
        let name = name.trim();
        if self
            .presets
            .iter()
            .any(|p| Some(p.id.as_str()) != id && p.name.to_lowercase() == name.to_lowercase())
        {
            return Err("An export preset already uses that name".into());
        }
        let preset = Preset {
            id: id
                .map(String::from)
                .unwrap_or_else(|| format!("export-{}", crate::engine::id())),
            name: name.into(),
            settings,
        };
        let mut presets = self.presets.clone();
        if let Some(id) = id {
            let index = presets
                .iter()
                .position(|p| p.id == id)
                .ok_or("Export preset is missing")?;
            presets[index] = preset.clone();
        } else {
            if presets.len() >= LIMIT {
                return Err("Export preset library is full (64 presets)".into());
            }
            presets.push(preset.clone());
        }
        self.persist(presets, revision, actor)?;
        Ok(preset)
    }
    pub fn delete(&mut self, id: &str, revision: u64, actor: &str) -> Result<(), String> {
        self.get(id)?;
        let presets = self
            .presets
            .iter()
            .filter(|p| p.id != id)
            .cloned()
            .collect();
        self.persist(presets, revision, actor)
    }
}
