//! Independent, instance-local brush-tool preferences, outside document history.
use crate::brush::Settings;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

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
}
pub struct Store {
    tools: BTreeMap<String, Snapshot>,
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
                        },
                    )
                })
                .collect(),
        }
    }
}
impl Store {
    pub fn get(&self, tool: &str) -> Result<Snapshot, String> {
        self.tools
            .get(tool)
            .cloned()
            .ok_or_else(|| "Choose brush, eraser, smudge, clone, heal or liquify".into())
    }
    pub fn state(&self) -> Value {
        json!({"tools":self.tools.iter().map(|(tool,snapshot)|(tool.clone(),snapshot.state(tool))).collect::<BTreeMap<_,_>>(),"persistence":"current instance"})
    }
    pub fn set(
        &mut self,
        tool: &str,
        profile: Profile,
        actor: &str,
        expected: Option<u64>,
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
            return Ok(before);
        }
        let after = Snapshot {
            profile,
            revision: before
                .revision
                .checked_add(1)
                .ok_or("Tool preference revision exhausted")?,
            actor: actor.into(),
        };
        self.tools.insert(tool.into(), after.clone());
        Ok(after)
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
        self.set(tool, profile, "human", None)
    }
}

impl Snapshot {
    pub fn state(&self, tool: &str) -> Value {
        let settings = if tool == "liquify" {
            json!({"radius":self.profile.settings.radius,"strength":self.profile.settings.opacity,"mode":self.profile.liquify_mode})
        } else {
            serde_json::to_value(self.profile.settings).unwrap()
        };
        json!({"tool":tool,"profile":{"settings":settings,"preset":self.profile.preset},"revision":self.revision,"actor":self.actor,"persistence":"current instance"})
    }
}
