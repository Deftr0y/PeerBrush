//! Natural point curves for the Photoshop RGB8 mapping.
use serde_json::Value;

pub(crate) const MAX_POINTS: usize = 16;
pub(crate) const PHOTOSHOP: &str = "photoshop";
pub(crate) const PHOTOSHOP_BYTE: &str = "photoshop_byte";

pub(crate) fn byte_composition(kind: &str, settings: &Value) -> bool {
    kind == "curves" && settings["composition"] == PHOTOSHOP_BYTE
}

pub(crate) fn channel_version(kind: &str, settings: &Value) -> Option<u8> {
    super::has_channel_curves(kind, settings).then(|| if byte_composition(kind, settings) { 2 } else { 1 })
}

pub(crate) fn grid_sample(value: &Value) -> Option<u16> {
    let value = value.as_f64()?;
    let sample = (value * 255.).round() as u16;
    (sample <= 255 && f64::from(sample) / 255. == value).then_some(sample)
}

pub(crate) fn uses_photoshop(kind: &str, settings: &Value) -> bool {
    kind == "curves" && (settings["interpolation"] == PHOTOSHOP
        || settings["channels"].as_array().is_some_and(|channels| {
            channels.iter().any(|channel| channel["interpolation"] == PHOTOSHOP)
        }))
}

pub(crate) struct Spline {
    points: [[f64; 2]; MAX_POINTS],
    second: [f64; MAX_POINTS],
    len: usize,
}

impl Spline {
    pub(crate) fn from_settings(settings: &Value) -> Option<Self> {
        if settings["interpolation"] != PHOTOSHOP {
            return None;
        }
        let source = settings["points"].as_array()?;
        if !(3..=MAX_POINTS).contains(&source.len()) {
            return None;
        }
        let mut points = [[0.; 2]; MAX_POINTS];
        let mut previous = -1.;
        for (target, point) in points.iter_mut().zip(source) {
            let x = point[0].as_f64()?;
            let y = point[1].as_f64()?;
            if !(0. ..=1.).contains(&x) || !(0. ..=1.).contains(&y) || x <= previous {
                return None;
            }
            *target = [x, y];
            previous = x;
        }
        let len = source.len();
        let mut second = [0.; MAX_POINTS];
        let mut rhs = [0.; MAX_POINTS];
        // Solve the tridiagonal spline system with zero endpoint curvature.
        for i in 1..len - 1 {
            let [left, low] = points[i - 1];
            let [x, y] = points[i];
            let [right, high] = points[i + 1];
            let ratio = (x - left) / (right - left);
            let pivot = 2. + ratio * second[i - 1];
            second[i] = (ratio - 1.) / pivot;
            rhs[i] = (6. * ((high - y) / (right - x) - (y - low) / (x - left)) / (right - left)
                - ratio * rhs[i - 1]) / pivot;
        }
        for i in (1..len - 1).rev() {
            second[i] = second[i] * second[i + 1] + rhs[i];
        }
        Some(Self { points, second, len })
    }

    pub(crate) fn value(&self, input: f64) -> f64 {
        let points = &self.points[..self.len];
        let right = points.partition_point(|point| point[0] < input);
        if right == 0 {
            return points[0][1];
        }
        if right == points.len() {
            return points[right - 1][1];
        }
        let [lo, y0] = points[right - 1];
        let [hi, y1] = points[right];
        let width = hi - lo;
        let a = (hi - input) / width;
        let b = (input - lo) / width;
        (a * y0 + b * y1 + ((a * a * a - a) * self.second[right - 1]
            + (b * b * b - b) * self.second[right]) * width * width / 6.).clamp(0., 1.)
    }
}

/// Prepare each natural spline once per lookup table or preview.
pub(crate) struct Prepared<'a> {
    settings: &'a Value,
    master: Option<Spline>,
    channels: [Option<Spline>; 3],
}

impl<'a> Prepared<'a> {
    pub(crate) fn new(kind: &str, settings: &'a Value) -> Option<Self> {
        (uses_photoshop(kind, settings) || byte_composition(kind, settings)).then(|| Self {
            settings,
            master: Spline::from_settings(settings),
            channels: std::array::from_fn(|channel| Spline::from_settings(&settings["channels"][channel])),
        })
    }

    pub(crate) fn value(&self, channel: usize, input: f64) -> f64 {
        let apply = |curve: &Option<Spline>, settings, value| {
            curve.as_ref().map_or_else(|| super::curve_value64(settings, value), |curve| curve.value(value))
        };
        let input = self.settings.get("channels").map_or(input, |channels| {
            apply(&self.channels[channel], &channels[channel], input)
        });
        apply(&self.master, self.settings, input)
    }

    pub(crate) fn sample8(&self, channel: usize, value: u8) -> u8 {
        if byte_composition("curves", self.settings) {
            let color = sample_byte(&self.channels[channel], &self.settings["channels"][channel], value);
            return sample_byte(&self.master, self.settings, color);
        }
        (self.value(channel, f64::from(value) / 255.) * 255.).round() as u8
    }
}

fn sample_byte(spline: &Option<Spline>, settings: &Value, value: u8) -> u8 {
    if let Some(spline) = spline {
        return (spline.value(f64::from(value) / 255.) * 255.).round_ties_even() as u8;
    }
    let points = &settings["points"];
    let [lo, hi, y0, y1] = [&points[0][0], &points[1][0], &points[0][1], &points[1][1]]
        .map(|value| u32::from(grid_sample(value).expect("validated Photoshop curve")));
    let width = hi - lo;
    let offset = u32::from(value).clamp(lo, hi) - lo;
    let numerator = (width - offset) * y0 + offset * y1;
    // Compare the exact integer remainder so half ties never depend on floating point.
    let quotient = numerator / width;
    let remainder = numerator % width * 2;
    (quotient + u32::from(remainder > width || remainder == width && quotient % 2 != 0)) as u8
}
