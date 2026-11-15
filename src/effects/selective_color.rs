//! Photoshop Selective Color on encoded RGB channels.
use serde_json::{Value, json};

pub(crate) const KIND: &str = "selective_color";
pub(crate) const RANGES: [&str; 9] = [
    "reds", "yellows", "greens", "cyans", "blues", "magentas", "whites", "neutrals", "blacks",
];

pub(crate) struct Settings {
    pub absolute: bool,
    pub ranges: [[i16; 4]; 9],
}
impl Settings {
    pub(crate) fn new(value: &Value) -> Result<Self, String> {
        let fields = value.as_object().ok_or("Selective Color settings must be an object")?;
        if fields.keys().any(|key| key != "method" && !RANGES.contains(&key.as_str())) {
            return Err("Unknown Selective Color setting".into());
        }
        let absolute = match fields.get("method").map(Value::as_str) {
            None | Some(Some("relative")) => false,
            Some(Some("absolute")) => true,
            _ => return Err("Selective Color method must be relative or absolute".into()),
        };
        let mut ranges = [[0; 4]; 9];
        for (name, row) in RANGES.into_iter().zip(&mut ranges) {
            let Some(value) = fields.get(name) else { continue; };
            let values = value.as_array().filter(|values| values.len() == row.len())
                .ok_or("Selective Color ranges require cyan, magenta, yellow and black")?;
            for (field, value) in row.iter_mut().zip(values) {
                *field = value.as_f64()
                    .filter(|value| (-100. ..=100.).contains(value) && value.fract() == 0.)
                    .ok_or("Selective Color corrections must be integers from -100 to 100")? as i16;
            }
        }
        Ok(Self { absolute, ranges })
    }

    pub(crate) fn value(&self) -> Value {
        let mut value = json!({"method": if self.absolute { "absolute" } else { "relative" }});
        for (name, row) in RANGES.into_iter().zip(self.ranges) {
            value[name] = json!(row);
        }
        value
    }

    pub(crate) fn prepare(&self) -> Prepared {
        let mut corrections = [(0, [0; 3]); 9];
        let mut count = 0;
        for (index, [c, m, y, black]) in self.ranges.into_iter().enumerate() {
            let shifts = [c, m, y].map(|ink| {
                let ink = i64::from(ink);
                -(100 + ink) * i64::from(black) - ink * 100
            });
            if shifts != [0; 3] {
                corrections[count] = (index, shifts);
                count += 1;
            }
        }
        Prepared { absolute: self.absolute, corrections, count }
    }
}

pub(crate) fn defaults() -> Value {
    Settings { absolute: false, ranges: [[0; 4]; 9] }.value()
}

pub(crate) struct Prepared {
    absolute: bool,
    corrections: [(usize, [i64; 3]); 9],
    count: usize,
}
impl Prepared {
    pub(crate) fn sample(&self, sample: [u16; 3], max: u16) -> [u16; 3] {
        if self.count == 0 {
            return sample;
        }
        let input = sample.map(i64::from);
        let [r, g, b] = input;
        let max = i64::from(max);
        let high = r.max(g).max(b);
        let low = r.min(g).min(b);
        let middle = r + g + b - high - low;
        let primary = high - middle;
        let secondary = middle - low;
        let weights = [
            if r == high { primary } else { 0 },
            if b == low { secondary } else { 0 },
            if g == high { primary } else { 0 },
            if r == low { secondary } else { 0 },
            if b == high { primary } else { 0 },
            if g == low { secondary } else { 0 },
            (2 * low - max).max(0),
            max - ((2 * high - max).abs() + (2 * low - max).abs()) / 2,
            (max - 2 * high).max(0),
        ];
        // Each range is weighted against the original RGB, with rounding once at native depth.
        let unit = 100 * 100;
        let denominator = max * unit;
        let mut output = input.map(|value| value * denominator);
        for &(range, shifts) in &self.corrections[..self.count] {
            let weight = weights[range];
            if weight == 0 {
                continue;
            }
            for (index, ink_shift) in shifts.into_iter().enumerate() {
                let value = input[index];
                let room = max - value;
                let scale = if self.absolute { max } else { room };
                let shift = (ink_shift * scale).clamp(-value * unit, room * unit);
                output[index] += shift * weight;
            }
        }
        output.map(|value| ((value + denominator / 2) / denominator).clamp(0, max) as u16)
    }
}
