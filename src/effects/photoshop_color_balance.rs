//! Photoshop Color Balance, calibrated as independent channel tone curves.
use serde_json::{Value, json};

pub(crate) const KIND: &str = "photoshop_color_balance";
pub(crate) const BANDS: [&str; 3] = ["shadows", "midtones", "highlights"];

pub(crate) struct Settings {
    pub bands: [[i16; 3]; 3],
    pub preserve_luminosity: bool,
}
impl Settings {
    pub(crate) fn new(settings: &Value) -> Result<Self, String> {
        let fields = settings.as_object().ok_or("Photoshop Color Balance settings must be an object")?;
        if fields.keys().any(|key| key != "preserve_luminosity" && !BANDS.contains(&key.as_str())) {
            return Err("Unknown Photoshop Color Balance setting".into());
        }
        let preserve_luminosity = fields.get("preserve_luminosity").map(|value| {
            value.as_bool().ok_or("Preserve Luminosity must be a boolean")
        }).transpose()?.unwrap_or(true);
        let mut bands = [[0; 3]; 3];
        for (key, band) in BANDS.into_iter().zip(&mut bands) {
            let Some(value) = fields.get(key) else { continue; };
            let values = value.as_array().filter(|values| values.len() == 3)
                .ok_or("Photoshop Color Balance bands require three axes")?;
            for (axis, value) in band.iter_mut().zip(values) {
                *axis = value.as_f64()
                    .filter(|value| (-100. ..=100.).contains(value) && value.fract() == 0.)
                    .ok_or("Photoshop Color Balance axes must be integers from -100 to 100")? as i16;
            }
        }
        Ok(Self { bands, preserve_luminosity })
    }

    pub(crate) fn value(&self) -> Value {
        let [shadows, midtones, highlights] = self.bands;
        json!({"shadows":shadows,"midtones":midtones,"highlights":highlights,
            "preserve_luminosity":self.preserve_luminosity})
    }

    pub(crate) fn prepare(&self) -> Prepared {
        let [shadows, midtones, highlights] = self.bands;
        let high_shadow = *shadows.iter().max().unwrap();
        let low_highlight = *highlights.iter().min().unwrap();
        let center = (midtones.iter().max().unwrap() + midtones.iter().min().unwrap()) / 2;
        Prepared(std::array::from_fn(|c| {
            let (black, white, shift) = if self.preserve_luminosity {
                (high_shadow - shadows[c], 255 - (highlights[c] - low_highlight), midtones[c] - center)
            } else {
                ((-shadows[c]).max(0), 255 - highlights[c].max(0), midtones[c] + shadows[c] / 2 + highlights[c] / 2)
            };
            let white = white.max(black + 1);
            let gamma = (100. * (f64::from(shift) / 100.).exp2()).round() / 100.;
            Channel {
                black: i32::from(black),
                span: i32::from(white - black),
                gamma: Gamma::new(gamma),
            }
        }))
    }
}

pub(crate) fn defaults() -> Value {
    Settings { bands: [[0; 3]; 3], preserve_luminosity: true }.value()
}

struct Gamma {
    inverse: f64,
    toe: f64,
    cubic: [f64; 3],
}
impl Gamma {
    fn new(gamma: f64) -> Self {
        let inverse = 1. / gamma;
        if gamma <= 1. {
            return Self { inverse, toe: 0., cubic: [0.; 3] };
        }
        let toe = (-gamma - 1. / (gamma - 1.)).exp2();
        let end = toe.powf(inverse);
        let start_tangent = toe * gamma.exp2();
        let end_tangent = end * inverse;
        // Hermite joins a finite shadow slope to the power curve in value and slope.
        Self { inverse, toe, cubic: [
            start_tangent, 3. * end - 2. * start_tangent - end_tangent,
            start_tangent + end_tangent - 2. * end,
        ] }
    }
    fn apply(&self, value: f64) -> f64 {
        if value >= self.toe {
            return value.powf(self.inverse);
        }
        let t = value / self.toe;
        let [linear, quadratic, cubic] = self.cubic;
        t * (linear + t * (quadratic + t * cubic))
    }
}

struct Channel {
    black: i32,
    span: i32,
    gamma: Gamma,
}
impl Channel {
    fn sample(&self, value: u16, max: u16) -> u16 {
        let step = i32::from(max) / 255;
        let span = self.span * step;
        let value = (i32::from(value) - self.black * step).clamp(0, span);
        let normalized = if max == 255 {
            // Photoshop rounds the Levels endpoint mapping before applying gamma.
            f64::from((255 * value + span / 2) / span) / 255.
        } else {
            f64::from(value) / f64::from(span)
        };
        (self.gamma.apply(normalized) * f64::from(max)).round() as u16
    }
}

pub(crate) struct Prepared([Channel; 3]);
impl Prepared {
    pub(crate) fn apply8(&self, rgba: &mut [u8]) {
        let tables: [[u8; 256]; 3] = std::array::from_fn(|c| {
            std::array::from_fn(|value| self.0[c].sample(value as u16, 255) as u8)
        });
        for pixel in rgba.chunks_exact_mut(4) {
            for (channel, table) in pixel[..3].iter_mut().zip(&tables) {
                *channel = table[usize::from(*channel)];
            }
        }
    }

    pub(crate) fn apply16(&self, rgba: &mut [u16]) {
        for pixel in rgba.chunks_exact_mut(4) {
            for (value, channel) in pixel[..3].iter_mut().zip(&self.0) {
                *value = channel.sample(*value, u16::MAX);
            }
        }
    }
}
