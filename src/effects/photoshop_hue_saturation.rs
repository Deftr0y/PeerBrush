//! Photoshop's Hue/Saturation transfer on byte and native-word RGB.
//! Adapted from Patchy's MIT-licensed calibration. See hue_saturation_tables.rs.
use super::hue_saturation_tables::{
    COLORIZE_HUE, COLORIZE_SATURATION, DEEP_SATURATION, MASTER_SATURATION,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const KIND: &str = "photoshop_hue_saturation";
pub(crate) const RANGES: [[i16; 4]; 6] = [
    [315, 345, 15, 45],
    [15, 45, 75, 105],
    [75, 105, 135, 165],
    [135, 165, 195, 225],
    [195, 225, 255, 285],
    [255, 285, 315, 345],
];
const TURN: f64 = 1530.;
const STEPS_PER_DEGREE: f64 = TURN / 360.;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Band {
    pub range: [i16; 4],
    pub change: [i16; 3],
}

#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub master: [i16; 3],
    pub colorize: bool,
    pub colorization: [i16; 3],
    pub bands: [Band; 6],
    pub trailer: Vec<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            master: [0; 3],
            colorize: false,
            colorization: [0, 25, 0],
            bands: RANGES.map(|range| Band {
                range,
                change: [0; 3],
            }),
            trailer: (0i16..6)
                .flat_map(|band| [band * 60, 100, 50])
                .flat_map(i16::to_be_bytes)
                .collect(),
            preset: None,
        }
    }
}

impl Settings {
    pub(crate) fn new(settings: &Value) -> Result<Self, String> {
        if !settings.is_object() {
            return Err("Photoshop Hue/Saturation settings must be an object".into());
        }
        super::adjustment_preset(settings)?;
        let settings = Self::deserialize(settings)
            .map_err(|error| format!("Invalid Photoshop Hue/Saturation settings: {error}"))?;
        settings.validate()?;
        Ok(settings)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        let valid_change = |[hue, saturation, lightness]: [i16; 3]| {
            (-180..=180).contains(&hue)
                && (-100..=100).contains(&saturation)
                && (-100..=100).contains(&lightness)
        };
        let [hue, saturation, lightness] = self.colorization;
        if !valid_change(self.master)
            || !(-180..=359).contains(&hue)
            || !(0..=100).contains(&saturation)
            || !(-100..=100).contains(&lightness)
            || !matches!(self.trailer.len(), 0 | 36)
            || self
                .bands
                .iter()
                .any(|band| !valid_change(band.change) || !valid_range(band.range))
        {
            return Err(
                "Photoshop Hue/Saturation values, range order or trailer are unsupported".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn value(&self) -> Value {
        serde_json::to_value(self).expect("Hue/Saturation settings are serializable")
    }

    pub(crate) fn prepare(&self, depth: u16) -> Prepared {
        let byte = depth == 8;
        let saturation = if byte {
            &MASTER_SATURATION
        } else {
            &DEEP_SATURATION
        };
        let rotation = |hue| wheel_rotation(hue, byte);
        let [hue, sat, light] = self.master;
        let active_bands = self
            .bands
            .iter()
            .copied()
            .filter(|band| !self.colorize && band.change != [0; 3] && band.widths() != [0; 3])
            .map(|band| ActiveBand {
                start: f64::from(band.range[0]),
                widths: band.widths().map(f64::from),
                rotation: f64::from(band.change[0]) * STEPS_PER_DEGREE,
                saturation: saturation[(band.change[1] + 100) as usize] - 1.,
                light: f64::from(band.change[2]),
            })
            .collect::<Vec<_>>();
        let lightness = if self.colorize {
            self.colorization[2]
        } else {
            light
        };
        let target = if lightness > 0 { 255. } else { 0. };
        let magnitude = i32::from(lightness.abs());
        let fraction = if byte {
            f64::from(magnitude * 255 / 100) / 255.
        } else {
            f64::from(magnitude) / 100.
        };
        Prepared {
            byte,
            identity: !self.colorize && self.master == [0; 3] && active_bands.is_empty(),
            lightness: fraction,
            target,
            ramp: std::array::from_fn(|value| {
                (value as f64 + (target - value as f64) * fraction).round()
            }),
            rotation: rotation(f64::from(hue) * STEPS_PER_DEGREE),
            saturation: saturation[(sat + 100) as usize],
            bands: active_bands,
            colorize: self.colorize.then(|| {
                let [hue, sat, _] = self.colorization;
                let hue = hue.rem_euclid(360) as usize;
                (
                    hue / 60,
                    f64::from(COLORIZE_HUE[hue]),
                    COLORIZE_SATURATION[sat as usize],
                )
            }),
        }
    }
}

pub(crate) fn valid_range(range: [i16; 4]) -> bool {
    range.into_iter().all(|stop| (0..=360).contains(&stop))
        && (Band {
            range,
            change: [0; 3],
        })
        .widths()
        .into_iter()
        .sum::<i16>()
            <= 360
}

impl Band {
    pub(crate) fn widths(self) -> [i16; 3] {
        let [a, b, c, d] = self.range;
        // Widen before subtraction so malformed settings can be rejected safely.
        [(a, b), (b, c), (c, d)].map(|(a, b)| (i32::from(b) - i32::from(a)).rem_euclid(360) as i16)
    }
}

struct ActiveBand {
    start: f64,
    widths: [f64; 3],
    rotation: f64,
    saturation: f64,
    light: f64,
}

impl ActiveBand {
    fn weight(&self, hue: f64) -> f64 {
        let position = (hue - self.start).rem_euclid(360.);
        let [rise, plateau, fall] = self.widths;
        if position < rise {
            position / rise
        } else if position <= rise + plateau {
            1.
        } else if position < rise + plateau + fall {
            (rise + plateau + fall - position) / fall
        } else {
            0.
        }
    }
}

pub(crate) struct Prepared {
    byte: bool,
    identity: bool,
    lightness: f64,
    target: f64,
    ramp: [f64; 256],
    rotation: f64,
    saturation: f64,
    bands: Vec<ActiveBand>,
    colorize: Option<(usize, f64, f64)>,
}

impl Prepared {
    pub(crate) fn apply8(&self, rgba: &mut [u8]) {
        if self.identity {
            return;
        }
        for pixel in rgba.chunks_exact_mut(4) {
            let rgb = self.sample([pixel[0], pixel[1], pixel[2]].map(f64::from));
            for (channel, value) in pixel[..3].iter_mut().zip(rgb) {
                *channel = value as u8;
            }
        }
    }

    pub(crate) fn apply16(&self, rgba: &mut [u16]) {
        if self.identity {
            return;
        }
        for pixel in rgba.chunks_exact_mut(4) {
            let rgb =
                self.sample([pixel[0], pixel[1], pixel[2]].map(|value| f64::from(value) / 257.));
            for (channel, value) in pixel[..3].iter_mut().zip(rgb) {
                *channel = (value * 257.).round() as u16;
            }
        }
    }

    pub(crate) fn preserves_black(&self) -> bool {
        self.sample([0.; 3]) == [0.; 3]
    }

    fn lightness(&self, value: f64) -> f64 {
        if self.byte {
            self.ramp[value as usize]
        } else {
            value + (self.target - value) * self.lightness
        }
    }

    fn light(&self, low: f64, high: f64) -> f64 {
        let light = (low + high) * 0.5;
        if self.byte { light.floor() } else { light }
    }

    fn sample(&self, mut rgb: [f64; 3]) -> [f64; 3] {
        let (low, high) = bounds(rgb);
        if let Some((sector, mix, saturation)) = self.colorize {
            let light = self.lightness(self.light(low, high));
            return self.reconstruct(light, light.min(255. - light) * saturation, sector, mix);
        }

        let mut saturation = 1.;
        let mut rotation = self.rotation;
        if !self.bands.is_empty() && low != high {
            let hue = wheel(rgb, low, high);
            let mut band_light = 0.;
            for band in &self.bands {
                let weight = band.weight(hue / STEPS_PER_DEGREE);
                band_light += weight * band.light;
                rotation += wheel_rotation(weight * band.rotation, self.byte);
                saturation += weight * band.saturation;
            }
            if band_light != 0. {
                let mut min = low;
                let mut max = high;
                let collapse = (high - low) * band_light.abs().min(100.) / 100.;
                if band_light > 0. {
                    min += collapse;
                } else {
                    max -= collapse;
                }
                if self.byte {
                    min = min.round();
                    max = max.round();
                }
                let (sector, mix) = sector(hue);
                rgb = self.reconstruct(self.light(min, max), (max - min) * 0.5, sector, mix);
            }
        }
        let lit = rgb.map(|value| self.lightness(value));
        let (low, high) = bounds(lit);
        let ratio = saturation.max(0.) * self.saturation;
        if low == high || (rotation == 0. && ratio == 1.) {
            return lit;
        }
        let light = self.light(low, high);
        let half = (high - low) * 0.5;
        let limit = half.max(light.min(255. - light));
        let adjusted = (half * ratio).min(limit);
        let (sector, mix) = sector(wheel(lit, low, high) + rotation);
        self.reconstruct(light, adjusted, sector, mix)
    }

    fn reconstruct(&self, light: f64, half: f64, sector: usize, mix: f64) -> [f64; 3] {
        // The byte path rounds the high side and truncates the low side.
        let (above, below) = if self.byte {
            (half.round(), half.floor())
        } else {
            (half, half)
        };
        let high = (light + above).min(255.);
        let low = (light - below).max(0.);
        let middle = (high - low) * mix / 255.;
        let mid = low + if self.byte { middle.round() } else { middle };
        match sector {
            0 => [high, mid, low],
            1 => [mid, high, low],
            2 => [low, high, mid],
            3 => [low, mid, high],
            4 => [mid, low, high],
            _ => [high, low, mid],
        }
    }
}

fn bounds([red, green, blue]: [f64; 3]) -> (f64, f64) {
    (red.min(green).min(blue), red.max(green).max(blue))
}

fn wheel([red, green, blue]: [f64; 3], low: f64, high: f64) -> f64 {
    let ramp = |value| 255. * (value - low) / (high - low);
    if red == high && blue == low {
        ramp(green)
    } else if green == high && blue == low {
        510. - ramp(red)
    } else if green == high && red == low {
        510. + ramp(blue)
    } else if blue == high && red == low {
        1020. - ramp(green)
    } else if blue == high && green == low {
        1020. + ramp(red)
    } else {
        TURN - ramp(blue)
    }
}

fn wheel_rotation(steps: f64, byte: bool) -> f64 {
    if byte { (steps + 0.5).floor() } else { steps }
}

fn sector(position: f64) -> (usize, f64) {
    let position = position.rem_euclid(TURN);
    let sector = ((position / 255.) as usize).min(5);
    let offset = position - sector as f64 * 255.;
    (
        sector,
        if sector % 2 == 0 {
            offset
        } else {
            255. - offset
        },
    )
}
