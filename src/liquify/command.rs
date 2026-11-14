//! Shared stroke input and effect selection; coverage storage stays with its engine.
use serde_json::{json, Value};

pub(crate) fn radius(command: &Value) -> f64 {
    command["radius"].as_f64().unwrap_or(40.)
}

pub(crate) fn stroke(
    command: &Value,
    origin: [i32; 2],
    selection: Option<[i32; 4]>,
    polygon: Option<&[[f32; 2]]>,
) -> Result<Value, String> {
    if command["mask"].as_bool().unwrap_or(false) {
        return Err("Liquify currently edits color; choose the Color channel".into());
    }
    let (mut points, pressures) = crate::brush::points_from_command(command)?;
    if pressures.is_some() {
        return Err("Liquify uses radius and strength, not per-point pressure".into());
    }
    for point in &mut points {
        for axis in 0..2 {
            point[axis] -= origin[axis] as f32;
        }
    }
    let selection = selection.map(|s| std::array::from_fn::<_, 4, _>(|i| s[i] - origin[i % 2]));
    let polygon = polygon.map(|points| {
        points
            .iter()
            .map(|p| [p[0] - origin[0] as f32, p[1] - origin[1] as f32])
            .collect::<Vec<_>>()
    });
    Ok(json!({
        "coverage":null, "mode":command["mode"].as_str().unwrap_or("push"),
        "points":points, "radius":radius(command),
        "strength":command["strength"].as_f64().unwrap_or(0.5),
        "selection":selection, "polygon":polygon,
    }))
}

/// An explicit enabled effect wins; otherwise append to the last enabled Liquify.
/// None requests a new effect. A zero weight does not disable an editable effect.
pub(crate) fn target<'a>(
    command: &Value,
    effects: impl Iterator<Item = (&'a str, &'a str, bool)>,
) -> Result<Option<usize>, String> {
    let requested = command["effect"].as_str();
    let mut count = 0;
    let mut selected = None;
    for (index, (id, kind, enabled)) in effects.enumerate() {
        count += 1;
        if kind == "liquify" && enabled && requested.is_none_or(|wanted| wanted == id) {
            selected = Some(index);
            if requested.is_some() {
                break;
            }
        }
    }
    if selected.is_none() {
        if requested.is_some() {
            return Err("Choose an enabled liquify effect".into());
        }
        if count >= 32 {
            return Err("Effect stack limit: 32".into());
        }
    }
    Ok(selected)
}

pub(crate) fn append(settings: &Value, stroke: Value) -> Result<Value, String> {
    let mut settings = crate::effects::normalized("liquify", settings)?;
    settings["strokes"]
        .as_array_mut()
        .ok_or("Invalid liquify stroke list")?
        .push(stroke);
    super::validate(&settings)?;
    Ok(settings)
}
