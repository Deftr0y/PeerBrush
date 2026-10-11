//! Bounded native RGB and independent opacity stops; no bitmap or display projection.
use serde_json::Value;

pub fn positioned(kind: &str, value: &Value) -> bool {
    kind == "gradient_overlay"
        && (value.get("offset_x").is_some() || value.get("offset_y").is_some())
}

pub fn extended(kind: &str, value: &Value) -> bool {
    kind == "gradient_overlay"
        && (value.get("color_stops").is_some() || value.get("alpha_stops").is_some())
}
fn number(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or("Gradient stop requires a finite number".into())
}
pub fn validate(value: &Value, alpha: bool) -> Result<(), String> {
    let stops = value.as_array().ok_or("Gradient stops must be an array")?;
    if stops.is_empty() {
        return Ok(());
    }
    if !(2..=16).contains(&stops.len()) {
        return Err("Gradient requires 2–16 stops, or an empty array for defaults".into());
    }
    let mut previous = -1.;
    for (index, stop) in stops.iter().enumerate() {
        let fields = stop.as_object().ok_or("Gradient stop must be an object")?;
        if fields.keys().any(|k| {
            !matches!(k.as_str(), "position" | "midpoint")
                && k != if alpha { "opacity" } else { "color" }
        }) {
            return Err("Unsupported gradient stop property".into());
        }
        let position = number(&stop["position"])?;
        if !(0.0..=1.).contains(&position)
            || position <= previous
            || (index == 0 && position != 0.)
            || (index + 1 == stops.len() && position != 1.)
        {
            return Err("Gradient stop positions must strictly increase from 0 to 1".into());
        }
        previous = position;
        if let Some(midpoint) = stop.get("midpoint") {
            if !(0.01..=0.99).contains(&number(midpoint)?) {
                return Err("Gradient midpoint must be within 0.01–0.99".into());
            }
        }
        if alpha {
            if !(0.0..=1.).contains(&number(&stop["opacity"])?) {
                return Err("Gradient stop opacity must be within 0–1".into());
            }
        } else {
            let color = stop["color"]
                .as_array()
                .filter(|c| c.len() == 3)
                .ok_or("Gradient stop requires three native-normalized RGB channels")?;
            for c in color {
                if !(0.0..=1.).contains(&number(c)?) {
                    return Err("Gradient RGB channels must be within 0–1".into());
                }
            }
        }
    }
    Ok(())
}
fn interval(stops: &[Value], position: f64) -> (&Value, &Value, f64) {
    let position = position.clamp(0., 1.);
    let index = stops
        .windows(2)
        .position(|pair| position <= pair[1]["position"].as_f64().unwrap())
        .unwrap_or(stops.len() - 2);
    let (a, b) = (&stops[index], &stops[index + 1]);
    let t = (position - a["position"].as_f64().unwrap())
        / (b["position"].as_f64().unwrap() - a["position"].as_f64().unwrap());
    let midpoint = a["midpoint"].as_f64().unwrap_or(0.5);
    let t = if t <= midpoint {
        0.5 * t / midpoint
    } else {
        0.5 + 0.5 * (t - midpoint) / (1. - midpoint)
    };
    (a, b, t.clamp(0., 1.))
}
pub fn sample(value: &Value, position: f64) -> ([u16; 3], f64) {
    let colors = value["color_stops"].as_array().filter(|s| s.len() >= 2);
    let rgb = if let Some(stops) = colors {
        let (a, b, t) = interval(stops, position);
        std::array::from_fn(|c| {
            ((a["color"][c].as_f64().unwrap() * (1. - t) + b["color"][c].as_f64().unwrap() * t)
                * 65535.)
                .round() as u16
        })
    } else {
        std::array::from_fn(|c| {
            ((value["color"][c].as_f64().unwrap() * (1. - position)
                + value["color2"][c].as_f64().unwrap() * position)
                * 65535.)
                .round() as u16
        })
    };
    let opacity = if let Some(stops) = value["alpha_stops"].as_array().filter(|s| s.len() >= 2) {
        let (a, b, t) = interval(stops, position);
        a["opacity"].as_f64().unwrap() * (1. - t) + b["opacity"].as_f64().unwrap() * t
    } else {
        1.
    };
    (rgb, opacity)
}
