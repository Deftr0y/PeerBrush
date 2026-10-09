//! Validated, instance-local filter presets. Library preferences never enter document history.
use crate::{effects, filters};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
pub const LIMIT: usize = 256;
pub const FILE_LIMIT: u64 = 4 * 1024 * 1024;
pub fn read(path: &Path) -> Result<Value, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err("Filter preset exceeds 4 MiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("Unreadable filter JSON: {e}"))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub category: String,
    pub kind: String,
    pub settings: Value,
    pub weight: f32,
    pub custom: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    presets: Vec<Preset>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Portable {
    format: String,
    version: u32,
    name: String,
    category: String,
    kind: String,
    settings: Value,
    weight: f32,
}
#[derive(Default)]
pub struct Library {
    custom: Vec<Preset>,
    path: Option<PathBuf>,
    version: Option<(u64, u128)>,
    pub error: Option<String>,
}
fn version(path: &Path) -> Result<Option<(u64, u128)>, String> {
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
fn text(value: &str, max: usize, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().count() > max || value.chars().any(char::is_control)
    {
        return Err(format!(
            "Filter {label} must contain 1–{max} readable characters"
        ));
    }
    Ok(())
}
fn validate(p: &mut Preset) -> Result<(), String> {
    text(&p.name, 80, "name")?;
    text(&p.category, 40, "category")?;
    if !filters::supports(&p.kind) {
        return Err("Unsupported filter preset kind".into());
    }
    effects::validate_weight(p.weight)?;
    if serde_json::to_vec(&p.settings)
        .map_err(|e| e.to_string())?
        .len()
        > 64 * 1024
    {
        return Err("Filter settings exceed 64 KiB".into());
    }
    let defaults = effects::defaults(&p.kind);
    if p.settings
        .as_object()
        .ok_or("Filter settings must be an object")?
        .keys()
        .any(|key| defaults.get(key).is_none())
    {
        return Err("Unknown filter preset setting".into());
    }
    p.settings = effects::normalized(&p.kind, &p.settings)?;
    Ok(())
}
impl Library {
    pub fn load(path: PathBuf) -> Self {
        let mut library = Self {
            path: Some(path.clone()),
            ..Default::default()
        };
        let result = (|| -> Result<(), String> {
            library.version = version(&path)?;
            let Some((size, _)) = library.version else {
                return Ok(());
            };
            if size > FILE_LIMIT {
                return Err("Filter library exceeds 4 MiB".into());
            }
            let mut stored: Stored = serde_json::from_value(read(&path)?)
                .map_err(|e| format!("Unreadable filter library: {e}"))?;
            if stored.version != 1 || stored.presets.len() > LIMIT {
                return Err("Unsupported or oversized filter library".into());
            }
            let mut ids = std::collections::HashSet::new();
            for p in &mut stored.presets {
                validate(p)?;
                if !p.custom
                    || !p.id.starts_with("custom-")
                    || p.id.len() > 64
                    || !p
                        .id
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                    || !ids.insert(p.id.clone())
                {
                    return Err("Invalid or duplicate custom filter ID".into());
                }
            }
            if version(&path)? != library.version {
                return Err("Filter library changed during loading".into());
            }
            library.custom = stored.presets;
            Ok(())
        })();
        library.error = result.err();
        library
    }
    pub fn presets(&self) -> Vec<Preset> {
        let mut p = curated();
        p.extend(self.custom.clone());
        p
    }
    pub fn get(&self, id: &str) -> Result<Preset, String> {
        self.presets()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or("Unknown filter preset".into())
    }
    fn persist(&mut self, presets: Vec<Preset>) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(format!(
                "Custom filters preserved; repair or restore the library before saving: {error}"
            ));
        }
        let bytes = serde_json::to_vec_pretty(&Stored {
            version: 1,
            presets: presets.clone(),
        })
        .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > FILE_LIMIT {
            return Err("Filter library exceeds 4 MiB".into());
        }
        if let Some(path) = &self.path {
            if version(path)? != self.version {
                return Err("Filter library changed on disk; reload before saving".into());
            }
            crate::server::atomic_write(path, &bytes)?;
            self.version = version(path)?;
        }
        self.custom = presets;
        Ok(())
    }
    pub fn save(
        &mut self,
        id: Option<&str>,
        name: &str,
        category: &str,
        kind: &str,
        settings: Value,
        weight: f32,
    ) -> Result<Preset, String> {
        let mut preset = Preset {
            id: id
                .map(str::to_owned)
                .unwrap_or_else(|| format!("custom-{}", crate::engine::id())),
            name: name.trim().into(),
            category: category.trim().into(),
            kind: kind.into(),
            settings,
            weight,
            custom: true,
        };
        validate(&mut preset)?;
        let mut custom = self.custom.clone();
        if let Some(id) = id {
            let index = custom
                .iter()
                .position(|p| p.id == id)
                .ok_or("Only an existing custom filter can be updated")?;
            custom[index] = preset.clone();
        } else {
            if custom.len() >= LIMIT {
                return Err("Custom filter library is full (256 filters)".into());
            }
            custom.push(preset.clone());
        }
        self.persist(custom)?;
        Ok(preset)
    }
    pub fn rename(&mut self, id: &str, name: &str) -> Result<Preset, String> {
        let p = self.get(id)?;
        self.save(Some(id), name, &p.category, &p.kind, p.settings, p.weight)
    }
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let mut custom = self.custom.clone();
        let index = custom
            .iter()
            .position(|p| p.id == id)
            .ok_or("Only custom filters can be deleted")?;
        custom.remove(index);
        self.persist(custom)
    }
    pub fn export(&self, id: &str) -> Result<Value, String> {
        let p = self.get(id)?;
        serde_json::to_value(Portable {
            format: "PeerBrush filter preset".into(),
            version: 1,
            name: p.name,
            category: p.category,
            kind: p.kind,
            settings: p.settings,
            weight: p.weight,
        })
        .map_err(|e| e.to_string())
    }
    pub fn import(&mut self, data: &Value) -> Result<Preset, String> {
        if serde_json::to_vec(data).map_err(|e| e.to_string())?.len() as u64 > FILE_LIMIT {
            return Err("Filter preset exceeds 4 MiB".into());
        }
        let p: Portable = serde_json::from_value(data.clone())
            .map_err(|e| format!("Invalid filter preset: {e}"))?;
        if p.format != "PeerBrush filter preset" || p.version != 1 {
            return Err("Unsupported filter preset format".into());
        }
        self.save(None, &p.name, &p.category, &p.kind, p.settings, p.weight)
    }
    pub fn resolve_commands(&self, commands: &[Value]) -> Result<Vec<Value>, String> {
        commands
            .iter()
            .map(|c| {
                if !c["op"].as_str().is_some_and(|op| op.starts_with("filter."))
                    || c.get("preset").is_none()
                {
                    return Ok(c.clone());
                }
                if !matches!(c["op"].as_str(), Some("filter.add" | "filter.update")) {
                    return Err("Filter presets apply to adding or updating a filter".into());
                }
                let p = self.get(c["preset"].as_str().ok_or("Filter preset must be an ID")?)?;
                let mut frozen = p.clone();
                let mut resolved = c.clone();
                resolved.as_object_mut().unwrap().remove("preset");
                resolved["kind"] = json!(p.kind);
                resolved["settings"] = p.settings;
                if let Some(settings) = c.get("settings") {
                    for (k, v) in settings
                        .as_object()
                        .ok_or("Filter settings must be an object")?
                    {
                        resolved["settings"][k] = v.clone();
                    }
                }
                if c.get("weight").is_none() {
                    resolved["weight"] = json!(p.weight);
                }
                frozen.settings = resolved["settings"].clone();
                frozen.weight = effects::command_weight(&resolved)?;
                validate(&mut frozen)?;
                resolved["settings"] = frozen.settings;
                Ok(resolved)
            })
            .collect()
    }
}
pub fn curated() -> Vec<Preset> {
    [
        ("soft-contrast","Soft contrast","Tone","adjust",json!({"brightness":0.02,"contrast":1.15,"saturation":1.0})),
        ("matte-tones","Matte tones","Tone","levels",json!({"black":0.0,"white":0.95,"gamma":1.12})),
        ("s-curve","S curve","Tone","curves",json!({"points":[[0.0,0.0],[0.25,0.15],[0.75,0.85],[1.0,1.0]],"interpolation":"smooth"})),
        ("brighten","Brighten","Tone","adjust",json!({"brightness":0.08,"contrast":1.0,"saturation":1.0})),
        ("posterize","Posterize","Tone","posterize",json!({"levels":4})),
        ("monochrome","Monochrome","Color","grayscale",json!({})),
        ("invert","Invert","Color","invert",json!({})),
        ("warm-light","Warm light","Color","color_balance",json!({"shadows":[-0.03,-0.03,0.04],"midtones":[0.0,0.0,0.0],"highlights":[0.18,0.07,-0.08],"preserve_luminosity":true})),
        ("cool-shadows","Cool shadows","Color","color_balance",json!({"shadows":[-0.15,0.01,0.2],"midtones":[0.0,0.0,0.0],"highlights":[0.0,0.0,0.0],"preserve_luminosity":true})),
        ("hue-shift","Hue shift","Color","hsl",json!({"hue":25.0,"saturation":0.0,"lightness":0.0})),
        ("vivid","Vivid","Color","hsl",json!({"hue":0.0,"saturation":0.2,"lightness":0.01})),
        ("muted","Muted","Color","hsl",json!({"hue":0.0,"saturation":-0.4,"lightness":0.0})),
        ("red-limit","Red limit","Color","channel_clamp",json!({"channel":"r","minimum":0.05,"maximum":0.85})),
        ("soft-bloom","Soft bloom","Blur & light","bloom",json!({"threshold":0.7,"spread":8.0,"strength":0.4})),
        ("gaussian-blur","Gaussian blur","Blur & light","blur",json!({"radius":4.0})),
    ].into_iter().map(|(id,name,category,kind,settings)|Preset {id:id.into(),name:name.into(),category:category.into(),kind:kind.into(),settings:effects::normalized(kind,&settings).expect("Curated filter settings"),weight:1.0,custom:false}).collect()
}
/// Thumbnails share one native reference image; selection previews use the actual project.
pub fn thumbnail(preset: &Preset) -> Result<Vec<u8>, String> {
    let mut doc = crate::engine::Document::new_depth(96, 64, 16)?;
    for y in 0..64 {
        for x in 0..96 {
            let warm = x < 48;
            let v = (x % 48) as u16 * 1100 + 4001;
            doc.layers[0].pixels.set16(
                x,
                y,
                if warm {
                    [v, 12001 + y as u16 * 600, 9003, 65535]
                } else {
                    [7001, 17003 + y as u16 * 600, v, 65535]
                },
            );
        }
    }
    let doc = crate::engine::Engine::preview_edits(
        doc,
        &[
            json!({"op":"filter.add","kind":preset.kind,"settings":preset.settings,"weight":preset.weight}),
        ],
    )?;
    Ok(doc.preview(None, 96, None, false)?.2)
}
