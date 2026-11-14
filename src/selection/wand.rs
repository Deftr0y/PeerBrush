//! Shared replay-command normalization; replay pixels stay with their document owner.
use serde::{ser::SerializeMap, Serialize, Serializer};
use serde_json::Value;

pub(crate) struct Command<'a>(pub &'a Value);
impl Serialize for Command<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let c = self.0;
        let mut map = serializer.serialize_map(None)?;
        let number = |key, default| c[key].as_f64().filter(|n| n.is_finite()).unwrap_or(default);
        // Match serde_json::Value key order for native replay's exact wire check.
        map.serialize_entry("color_similarity", &(c["color_similarity"] == true))?;
        map.serialize_entry("contiguous", &(c["contiguous"] != false))?;
        map.serialize_entry("feather", &number("feather", 0.).clamp(0., 64.))?;
        map.serialize_entry("kind", "wand")?;
        for key in ["layer", "mode"] {
            if let Some(value) = c.get(key) {
                map.serialize_entry(key, value)?;
            }
        }
        map.serialize_entry("op", "selection")?;
        for key in ["point", "points", "rect"] {
            if let Some(value) = c.get(key) {
                map.serialize_entry(key, value)?;
            }
        }
        map.serialize_entry("sample_merged", &(c["sample_merged"] == true))?;
        map.serialize_entry("tolerance", &number("tolerance", 24.).clamp(0., 255.))?;
        map.end()
    }
}

pub(crate) fn update(command: &mut Value, c: &Value) -> Result<(), String> {
    for (key, limit) in [("tolerance", 255.), ("feather", 64.)] {
        if let Some(value) = c.get(key) {
            let n = value
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0. && *n <= limit)
                .ok_or_else(|| format!("Wand {key} must be between 0 and {limit}"))?;
            command[key] = serde_json::json!(n);
        }
    }
    for key in ["contiguous", "sample_merged", "color_similarity"] {
        if let Some(value) = c.get(key) {
            command[key] = serde_json::json!(value
                .as_bool()
                .ok_or("Wand sampling options must be boolean")?);
        }
    }
    Ok(())
}
