//! Original procedural presets and instance-local preferences. No document/history sources.
use crate::brush::{self, Settings, TipKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const CATEGORIES: &[&str] = &["Sketch", "Ink", "Paint", "Airbrush", "Texture", "Blend"];
const LIMIT: usize = 256;
const FILE_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub category: String,
    pub settings: Settings,
    pub custom: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    presets: Vec<Preset>,
}

pub struct Library {
    custom: Vec<Preset>,
    path: Option<PathBuf>,
    version: Option<(u64, u128)>,
    pub error: Option<String>,
}
impl Default for Library {
    fn default() -> Self {
        Self {
            custom: vec![],
            path: None,
            version: None,
            error: None,
        }
    }
}
fn file_version(path: &Path) -> Result<Option<(u64, u128)>, String> {
    match fs::metadata(path) {
        Ok(m) => Ok(Some((
            m.len(),
            m.modified()
                .map_err(|e| e.to_string())?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn validate_text(text: &str, max: usize, label: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.chars().count() > max || text.chars().any(char::is_control) {
        return Err(format!(
            "Brush {label} must contain 1–{max} readable characters"
        ));
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
            library.version = file_version(&path)?;
            let Some((size, _)) = library.version else {
                return Ok(());
            };
            if size > FILE_LIMIT {
                return Err("Brush library exceeds 4 MiB".into());
            }
            let stored: Stored =
                serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("Unreadable brush library: {e}"))?;
            if stored.version != 1 || stored.presets.len() > LIMIT {
                return Err("Unsupported or oversized brush library".into());
            }
            let mut ids = std::collections::BTreeSet::new();
            for p in &stored.presets {
                validate_text(&p.name, 80, "name")?;
                validate_text(&p.category, 40, "category")?;
                p.settings.validate()?;
                if !p.custom
                    || !p.id.starts_with("custom-")
                    || p.id.len() > 64
                    || !p
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                    || !ids.insert(p.id.clone())
                {
                    return Err("Invalid or duplicate custom brush ID".into());
                }
            }
            if file_version(&path)? != library.version {
                return Err("Brush library changed during loading".into());
            }
            library.custom = stored.presets;
            Ok(())
        })();
        library.error = result.err();
        library
    }
    pub fn presets(&self) -> Vec<Preset> {
        let mut list = curated();
        list.extend(self.custom.clone());
        list
    }
    pub fn get(&self, id: &str) -> Result<Preset, String> {
        self.presets()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("Unknown brush preset: {id}"))
    }
    fn persist(&mut self, presets: Vec<Preset>) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(format!("Custom brushes preserved; repair or restore the library file before saving: {error}"));
        }
        if let Some(path) = &self.path {
            if file_version(path)? != self.version {
                return Err(
                    "Brush library changed on disk; restart to load it before saving".into(),
                );
            }
            let bytes = serde_json::to_vec_pretty(&Stored {
                version: 1,
                presets: presets.clone(),
            })
            .map_err(|e| e.to_string())?;
            crate::server::atomic_write(path, &bytes)?;
            self.version = file_version(path)?;
        }
        self.custom = presets;
        Ok(())
    }
    pub fn save(
        &mut self,
        id: Option<&str>,
        name: &str,
        category: &str,
        settings: Settings,
    ) -> Result<Preset, String> {
        validate_text(name, 80, "name")?;
        validate_text(category, 40, "category")?;
        settings.validate()?;
        let mut custom = self.custom.clone();
        let p = Preset {
            id: id
                .map(String::from)
                .unwrap_or_else(|| format!("custom-{}", crate::engine::id())),
            name: name.trim().into(),
            category: category.trim().into(),
            settings,
            custom: true,
        };
        if let Some(id) = id {
            let index = custom.iter().position(|p| p.id == id).ok_or(
                "Only an existing custom brush can be updated; save a new copy of a curated brush",
            )?;
            custom[index] = p.clone();
        } else {
            if custom.len() >= LIMIT {
                return Err("Custom brush library is full (256 brushes)".into());
            }
            custom.push(p.clone());
        }
        self.persist(custom)?;
        Ok(p)
    }
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let mut custom = self.custom.clone();
        let index = custom
            .iter()
            .position(|p| p.id == id)
            .ok_or("Only custom brushes can be deleted")?;
        custom.remove(index);
        self.persist(custom)
    }
    /// Freeze settings before permissions, scopes, previews and proposals are evaluated.
    pub fn resolve_commands(&self, commands: &[Value]) -> Result<Vec<Value>, String> {
        commands
            .iter()
            .map(|c| {
                let Some(preset) = c.get("preset") else {
                    return Ok(c.clone());
                };
                if !matches!(
                    c["op"].as_str(),
                    Some("paint" | "smudge" | "clone" | "heal")
                ) {
                    return Err("Brush presets apply to paint, smudge, clone or heal".into());
                }
                let p = self.get(preset.as_str().ok_or("Brush preset must be an ID")?)?;
                let mut result = serde_json::to_value(p.settings).map_err(|e| e.to_string())?;
                for (key, value) in c.as_object().ok_or("Command must be an object")? {
                    if key != "preset" {
                        result[key] = value.clone();
                    }
                }
                // Legacy soft is an explicit hardness override unless hardness is provided.
                if c.get("hardness").is_none() && c["soft"] == true {
                    result["hardness"] = json!(0.5);
                }
                Settings::from_command(&result)?;
                Ok(result)
            })
            .collect()
    }
}

/// Curated for PeerBrush's own five procedural tips; names/settings are original.
pub fn curated() -> Vec<Preset> {
    use TipKind::*;
    let base = Settings {
        pressure_opacity: false,
        ..Default::default()
    };
    [
        (
            "graphite",
            "Graphite",
            "Sketch",
            Settings {
                radius: 3.,
                tip: Grain,
                density: 0.8,
                grain: 0.7,
                flow: 0.65,
                opacity: 0.85,
                pressure_opacity: true,
                pressure_gamma: 0.65,
                ..base
            },
        ),
        (
            "soft-pencil",
            "Soft pencil",
            "Sketch",
            Settings {
                radius: 5.,
                tip: Dry,
                hardness: 0.45,
                density: 0.7,
                grain: 0.8,
                flow: 0.35,
                pressure_opacity: true,
                ..base
            },
        ),
        (
            "charcoal",
            "Charcoal",
            "Sketch",
            Settings {
                radius: 14.,
                tip: Chalk,
                density: 0.65,
                grain: 3.,
                hardness: 0.65,
                flow: 0.5,
                pressure_opacity: true,
                ..base
            },
        ),
        (
            "hatching",
            "Hatching",
            "Sketch",
            Settings {
                radius: 7.,
                tip: Bristle,
                roundness: 0.3,
                angle: -30.,
                density: 0.55,
                grain: 2.,
                flow: 0.7,
                ..base
            },
        ),
        (
            "technical-pen",
            "Technical pen",
            "Ink",
            Settings {
                radius: 2.,
                pressure_size: false,
                smoothing: 0.25,
                ..base
            },
        ),
        (
            "brush-pen",
            "Brush pen",
            "Ink",
            Settings {
                radius: 12.,
                smoothing: 0.3,
                taper_start: 12.,
                taper_end: 24.,
                pressure_gamma: 1.5,
                ..base
            },
        ),
        (
            "calligraphy",
            "Calligraphy",
            "Ink",
            Settings {
                radius: 15.,
                roundness: 0.18,
                angle: -35.,
                smoothing: 0.2,
                pressure_gamma: 0.7,
                ..base
            },
        ),
        (
            "dry-ink",
            "Dry ink",
            "Ink",
            Settings {
                radius: 10.,
                tip: Dry,
                density: 0.75,
                grain: 1.5,
                taper_end: 20.,
                smoothing: 0.15,
                ..base
            },
        ),
        (
            "round-paint",
            "Round paint",
            "Paint",
            Settings {
                radius: 18.,
                hardness: 0.8,
                flow: 0.6,
                ..base
            },
        ),
        (
            "flat-paint",
            "Flat paint",
            "Paint",
            Settings {
                radius: 22.,
                tip: Bristle,
                roundness: 0.45,
                angle: 15.,
                density: 0.85,
                grain: 2.,
                flow: 0.6,
                ..base
            },
        ),
        (
            "dry-paint",
            "Dry paint",
            "Paint",
            Settings {
                radius: 24.,
                tip: Dry,
                hardness: 0.7,
                density: 0.45,
                grain: 3.5,
                flow: 0.5,
                ..base
            },
        ),
        (
            "chalk-paint",
            "Chalk paint",
            "Paint",
            Settings {
                radius: 20.,
                tip: Chalk,
                density: 0.85,
                grain: 4.,
                flow: 0.7,
                ..base
            },
        ),
        (
            "soft-airbrush",
            "Soft airbrush",
            "Airbrush",
            Settings {
                radius: 48.,
                hardness: 0.,
                flow: 0.1,
                spacing: 0.08,
                pressure_opacity: true,
                pressure_gamma: 0.7,
                ..base
            },
        ),
        (
            "build-airbrush",
            "Build airbrush",
            "Airbrush",
            Settings {
                radius: 30.,
                hardness: 0.25,
                flow: 0.25,
                pressure_opacity: true,
                ..base
            },
        ),
        (
            "fine-mist",
            "Fine mist",
            "Airbrush",
            Settings {
                radius: 28.,
                tip: Grain,
                hardness: 0.,
                density: 0.25,
                grain: 0.6,
                flow: 0.12,
                pressure_opacity: true,
                ..base
            },
        ),
        (
            "paper-grain",
            "Paper grain",
            "Texture",
            Settings {
                radius: 28.,
                tip: Grain,
                density: 0.6,
                grain: 1.5,
                flow: 0.5,
                seed: 17,
                ..base
            },
        ),
        (
            "broken-chalk",
            "Broken chalk",
            "Texture",
            Settings {
                radius: 25.,
                tip: Chalk,
                density: 0.45,
                grain: 7.,
                flow: 0.65,
                seed: 31,
                ..base
            },
        ),
        (
            "coarse-fiber",
            "Coarse fiber",
            "Texture",
            Settings {
                radius: 24.,
                tip: Bristle,
                density: 0.55,
                grain: 5.,
                angle: 20.,
                flow: 0.75,
                seed: 23,
                ..base
            },
        ),
        (
            "soft-blend",
            "Soft blend",
            "Blend",
            Settings {
                radius: 25.,
                hardness: 0.,
                opacity: 0.6,
                flow: 0.5,
                wetness: 0.9,
                load: 0.,
                pickup: 0.35,
                ..base
            },
        ),
        (
            "wet-paint",
            "Wet paint",
            "Blend",
            Settings {
                radius: 22.,
                tip: Bristle,
                hardness: 0.7,
                density: 0.85,
                flow: 0.5,
                wetness: 0.8,
                load: 0.4,
                pickup: 0.3,
                ..base
            },
        ),
        (
            "dry-drag",
            "Dry drag",
            "Blend",
            Settings {
                radius: 20.,
                tip: Dry,
                density: 0.6,
                flow: 0.45,
                wetness: 0.45,
                load: 0.1,
                pickup: 0.65,
                ..base
            },
        ),
    ]
    .into_iter()
    .map(|(id, name, category, settings)| Preset {
        id: id.into(),
        name: name.into(),
        category: category.into(),
        settings,
        custom: false,
    })
    .collect()
}

/// A readable pressure stroke rendered by the shared rasterizer, never a decorative tip stamp.
pub fn preview(
    mut settings: Settings,
    width: u32,
    height: u32,
) -> Result<crate::raster::Raster, String> {
    if !(64..=512).contains(&width) || !(24..=128).contains(&height) {
        return Err("Brush preview size must be 64–512 by 24–128".into());
    }
    settings.validate()?;
    let scale = (height as f32 * 0.3 / settings.radius).min(1.0);
    settings.radius *= scale;
    settings.taper_start *= scale;
    settings.taper_end *= scale;
    let points = (0..40)
        .map(|i| {
            let t = i as f32 / 39.;
            [
                height as f32 * 0.4 + t * (width as f32 - height as f32 * 0.8),
                height as f32 * (0.5 + 0.12 * (t * std::f32::consts::TAU).sin()),
            ]
        })
        .collect::<Vec<_>>();
    let pressures = (0..40)
        .map(|i| 0.15 + 0.85 * (i as f32 / 39. * std::f32::consts::PI).sin())
        .collect::<Vec<_>>();
    let mut raster = crate::raster::Raster::new(width, height);
    brush::paint_with_pressure(
        &mut raster,
        &points,
        Some(&pressures),
        settings,
        [245, 242, 240, 255],
        false,
        None,
        None,
    )?;
    Ok(raster)
}
