//! Photoshop's six-color Black & White conversion and optional tint.
use serde_json::{Value, json};

pub(crate) const KIND: &str = "black_white";
pub(crate) const COLORS: [&str; 6] = ["reds", "yellows", "greens", "cyans", "blues", "magentas"];
const DEFAULT_WEIGHTS: [i32; 6] = [40, 60, 40, 60, 20, 80];
const DEFAULT_TINT: [f64; 3] = [225., 211., 179.];

pub(crate) struct Settings {
    pub weights: [i32; 6],
    pub use_tint: bool,
    pub tint: [f64; 3],
}
impl Settings {
    pub(crate) fn new(settings: &Value) -> Result<Self, String> {
        let fields = settings.as_object().ok_or("Black & White settings must be an object")?;
        if fields.keys().any(|key| !matches!(key.as_str(), "use_tint" | "tint_color" | "preset")
            && !COLORS.contains(&key.as_str()))
        {
            return Err("Unknown Black & White setting".into());
        }
        super::adjustment_preset(settings)?;
        let mut weights = DEFAULT_WEIGHTS;
        for (key, weight) in COLORS.into_iter().zip(&mut weights) {
            if let Some(value) = fields.get(key) {
                *weight = value.as_f64()
                    .filter(|value| (-200. ..=300.).contains(value) && value.fract() == 0.)
                    .ok_or("Black & White weights must be integers from -200 to 300")? as i32;
            }
        }
        let use_tint = fields.get("use_tint").map(|value| {
            value.as_bool().ok_or("Black & White use_tint must be a boolean")
        }).transpose()?.unwrap_or(false);
        let mut tint = DEFAULT_TINT;
        if let Some(value) = fields.get("tint_color") {
            let channels = value.as_array().filter(|channels| channels.len() == 3)
                .ok_or("Black & White tint requires three RGB channels")?;
            for (target, value) in tint.iter_mut().zip(channels) {
                *target = value.as_f64().filter(|value| (0. ..=255.).contains(value))
                    .ok_or("Black & White tint channels must be within 0–255")?;
            }
        }
        Ok(Self { weights, use_tint, tint })
    }
    pub(crate) fn prepare(&self, max: u16) -> Prepared {
        let byte = max == 255;
        let tint = self.use_tint.then(|| {
            let rgb = self.tint.map(|value| {
                if byte { value.round() } else { value * f64::from(max) / 255. }
            });
            crate::raster::centered_at_luma(rgb, byte)
        });
        Prepared { weights: self.weights, tint, max }
    }
}

pub(crate) fn defaults() -> Value {
    let [reds, yellows, greens, cyans, blues, magentas] = DEFAULT_WEIGHTS;
    json!({"reds":reds,"yellows":yellows,"greens":greens,"cyans":cyans,"blues":blues,
        "magentas":magentas,"use_tint":false,"tint_color":DEFAULT_TINT})
}

pub(crate) struct Prepared {
    weights: [i32; 6],
    tint: Option<[f64; 3]>,
    max: u16,
}
impl Prepared {
    pub(crate) fn sample(&self, pixel: [u16; 3]) -> [u16; 3] {
        let [r, g, b] = pixel.map(i32::from);
        let [reds, yellows, greens, cyans, blues, magentas] = self.weights;
        let (primary, secondary) = if r >= g && r >= b {
            (reds, if g >= b { yellows } else { magentas })
        } else if g >= b {
            (greens, if r >= b { yellows } else { cyans })
        } else {
            (blues, if g >= r { cyans } else { magentas })
        };
        let high = r.max(g).max(b);
        let low = r.min(g).min(b);
        let mid = r + g + b - high - low;
        let hundredths = low * 100 + (mid - low) * secondary + (high - mid) * primary;
        let gray = f64::from(hundredths) / 100.;
        let max = f64::from(self.max);
        if let Some(tint) = self.tint {
            let target = if self.max == 255 { gray.round() } else { gray }.clamp(0., max);
            crate::raster::fit_luma(tint, target, max).map(|value| value.round() as u16)
        } else {
            [gray.clamp(0., max).round() as u16; 3]
        }
    }
}
