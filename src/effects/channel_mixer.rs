//! Channel Mixer with Photoshop's fixed-point RGB weights.
use serde_json::{Value, json};

pub(crate) const KIND: &str = "channel_mixer";
pub(crate) const ROWS: [&str; 4] = ["red", "green", "blue", "gray"];
pub(crate) const DEFAULT_ROWS: [[i16; 4]; 4] = [
    [100, 0, 0, 0], [0, 100, 0, 0], [0, 0, 100, 0], [40, 40, 20, 0],
];

pub(crate) struct Mixer {
    pub monochrome: bool,
    pub rows: [[i16; 4]; 4],
}
impl Mixer {
    pub(crate) fn new(settings: &Value) -> Result<Self, String> {
        let fields = settings.as_object().ok_or("Channel Mixer settings must be an object")?;
        if fields.keys().any(|key| !matches!(key.as_str(), "monochrome" | "preset") && !ROWS.contains(&key.as_str())) {
            return Err("Unknown Channel Mixer setting".into());
        }
        super::adjustment_preset(settings)?;
        let monochrome = match fields.get("monochrome") {
            Some(value) => value.as_bool().ok_or("Channel Mixer monochrome must be a boolean")?,
            None => false,
        };
        let mut rows = DEFAULT_ROWS;
        for (name, row) in ROWS.into_iter().zip(&mut rows) {
            let Some(value) = fields.get(name) else { continue };
            let entries = value.as_array().filter(|entries| entries.len() == row.len())
                .ok_or("Each Channel Mixer row requires red, green, blue and constant")?;
            for (target, value) in row.iter_mut().zip(entries) {
                *target = value.as_f64()
                    .filter(|value| (-200. ..=200.).contains(value) && value.fract() == 0.)
                    .ok_or("Channel Mixer percentages must be integers from -200 to 200")? as i16;
            }
        }
        Ok(Self { monochrome, rows })
    }
    pub(crate) fn settings(&self) -> Value {
        let [red, green, blue, gray] = self.rows;
        json!({"monochrome":self.monochrome, "red":red, "green":green, "blue":blue, "gray":gray})
    }
    pub(crate) fn prepare(&self) -> Prepared {
        let rows = if self.monochrome { [self.rows[3]; 3] } else {
            [self.rows[0], self.rows[1], self.rows[2]]
        };
        Prepared(rows.map(|row| row.map(|percent| i32::from(percent) * 1024 / 100)))
    }
}

pub(crate) fn defaults() -> Value {
    Mixer { monochrome: false, rows: DEFAULT_ROWS }.settings()
}

pub(crate) struct Prepared([[i32; 4]; 3]);
impl Prepared {
    pub(crate) fn sample(&self, [red, green, blue]: [u16; 3], max: u16) -> [u16; 3] {
        let [red, green, blue, max] = [red, green, blue, max].map(i32::from);
        self.0.map(|[r, g, b, bias]| {
            let fixed = red * r + green * g + blue * b + max * bias;
            ((fixed + 512) >> 10).clamp(0, max) as u16
        })
    }
}
