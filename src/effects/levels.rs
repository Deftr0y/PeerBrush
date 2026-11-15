//! Exact input intervals are explicit so historic minimum-span mappings survive.
use serde_json::Value;

pub(crate) const PHOTOSHOP_BYTE: &str = "photoshop_byte";

pub(crate) fn byte_composition(kind: &str, settings: &Value) -> bool {
    kind == "levels" && settings["composition"] == PHOTOSHOP_BYTE
}

pub(crate) fn exact(settings: &Value) -> bool {
    settings["input_mapping"] == "exact"
}

pub(crate) fn extended(kind: &str, settings: &Value) -> bool {
    kind == "levels" && (exact(settings)
        || settings["gamma"].as_f64().unwrap_or(1.) > 5.)
}

pub(crate) fn source_version(kind: &str, settings: &Value) -> Option<u8> {
    if byte_composition(kind, settings) { Some(4) }
    else if channels(kind, settings) { Some(3) }
    else if extended(kind, settings) { Some(2) }
    else if super::has_levels_output(kind, settings) { Some(1) }
    else { None }
}

pub(crate) fn channels(kind: &str, settings: &Value) -> bool {
    kind == "levels" && settings.get("channels").is_some()
}

pub(crate) fn validate(settings: &Value) -> Result<(), String> {
    validate_record(settings, exact(settings))?;
    if settings.get("composition").is_some()
        && (!byte_composition("levels", settings) || !exact(settings) || !channels("levels", settings))
    {
        return Err("Photoshop Levels composition requires exact master and RGB settings".into());
    }
    if let Some(channels) = settings.get("channels") {
        let channels = channels.as_array().filter(|values| values.len() == 3)
            .ok_or("Levels channels must contain Red, Green and Blue settings")?;
        for channel in channels {
            let object = channel.as_object().ok_or("Levels channel settings must be an object")?;
            if object.keys().any(|key| !["black", "white", "gamma", "output_black", "output_white", "input_mapping"].contains(&key.as_str()))
                || channel.get("input_mapping").is_some_and(|value| value != "exact") {
                return Err("Levels channels require exact scalar settings".into());
            }
            validate_record(channel, true)?;
        }
    }
    Ok(())
}

fn validate_record(settings: &Value, exact: bool) -> Result<(), String> {
    if settings.get("input_mapping").is_some_and(|v| !matches!(v.as_str(), Some("exact" | "legacy"))) {
        return Err("Levels input_mapping must be exact or legacy".into());
    }
    for (key, min, max) in [("black", 0., if exact { 1. } else { f64::from(0.99_f32) }),
        ("white", if exact { 0. } else { f64::from(0.01_f32) }, 1.), ("gamma", 0.1, 9.99),
        ("output_black", 0., 1.), ("output_white", 0., 1.)] {
        if let Some(value) = settings.get(key) {
            let value = value.as_f64().ok_or("Invalid Levels setting")?;
            if !value.is_finite() || value < min || value > max {
                return Err(format!("{key} must be {min}–{max}"));
            }
        }
    }
    if settings["black"].as_f64().unwrap_or(0.) >= settings["white"].as_f64().unwrap_or(1.) {
        return Err("White point must be greater than black point".into());
    }
    Ok(())
}

pub(crate) fn map64(settings: &Value, value: f64) -> f64 {
    map(settings, value, exact(settings))
}

fn map(settings: &Value, value: f64, exact: bool) -> f64 {
    let number = |key: &str, default| settings[key].as_f64().unwrap_or(default);
    let black = number("black", 0.);
    let span = number("white", 1.) - black;
    let span = if exact { span } else { span.max(0.01) };
    let input = ((value - black) / span).clamp(0., 1.).powf(1. / number("gamma", 1.));
    let low = number("output_black", 0.);
    low + input * (number("output_white", 1.) - low)
}

pub(crate) fn channel64(settings: &Value, channel: usize, value: f64) -> f64 {
    map64(settings, map(&settings["channels"][channel], value, true))
}

pub(crate) fn channel8(settings: &Value, channel: usize, value: u8) -> u8 {
    if byte_composition("levels", settings) {
        return stage_byte(settings, stage_byte(&settings["channels"][channel], value));
    }
    let value = map(&settings["channels"][channel], f64::from(value) / 255., true);
    if exact(settings) {
        (map64(settings, value) * 255.).round() as u8
    } else {
        (super::map_value("levels", settings, value as f32) * 255.).round() as u8
    }
}

fn stage_byte(settings: &Value, value: u8) -> u8 {
    let sample = |key: &str, default| settings[key].as_f64().unwrap_or(default) * 255.;
    let black = sample("black", 0.);
    let span = sample("white", 1.) - black;
    if settings["gamma"].as_f64().unwrap_or(1.) == 1. && span > 0. {
        let low = sample("output_black", 0.);
        let high = sample("output_white", 1.);
        let input = (f64::from(value) - black).clamp(0., span);
        // Keep byte-grid endpoints in byte units so exact halves round up.
        return ((low * span + input * (high - low)) / span).round() as u8;
    }
    (map(settings, f64::from(value) / 255., true) * 255.).round() as u8
}
