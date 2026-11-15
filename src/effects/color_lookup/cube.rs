//! IRIDAS/Adobe RGB .cube with red-fastest lattice ordering.
use crate::color_profile::clut::{Clut, MAX_GRID};
use crate::color_profile::device_link_mpe::{self, Input};

pub(super) struct Cube {
    grid: Clut,
    minimum: [f64; 3],
    maximum: [f64; 3],
}

impl Cube {
    pub(super) fn compile_profile(&self, swap: [bool; 2]) -> Option<Vec<u8>> {
        device_link_mpe::compile(&self.grid, Input::Domain(self.minimum, self.maximum), swap)
    }
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, String> {
        let source = std::str::from_utf8(bytes).map_err(|_| "A .cube table must be UTF-8 text")?;
        let (mut size, mut minimum, mut maximum) = (None, None, None);
        let mut title = false;
        let mut values = Vec::new();
        for raw in source.trim_start_matches('\u{feff}').lines() {
            let line = raw.trim();
            if let Some(text) = line
                .strip_prefix("TITLE")
                .filter(|text| text.starts_with(char::is_whitespace))
            {
                let (_, tail) = text
                    .trim_start()
                    .strip_prefix('"')
                    .and_then(|text| text.split_once('"'))
                    .ok_or("Invalid .cube title")?;
                if title
                    || !values.is_empty()
                    || !tail.trim().is_empty() && !tail.trim_start().starts_with('#')
                {
                    return Err("Invalid or duplicate .cube title".into());
                }
                title = true;
                continue;
            }
            let line = line.split('#').next().unwrap_or_default();
            let mut words = line.split_whitespace();
            let Some(key) = words.next() else {
                continue;
            };
            match key {
                "LUT_3D_SIZE" if size.is_none() && values.is_empty() => {
                    let grid = words
                        .next()
                        .and_then(|word| word.parse::<usize>().ok())
                        .filter(|grid| (2..=MAX_GRID).contains(grid) && words.next().is_none())
                        .ok_or("A .cube table requires a 3D grid from 2 to 33")?;
                    size = Some(grid);
                    values.reserve_exact(grid.pow(3) * 3);
                }
                "DOMAIN_MIN" if minimum.is_none() && values.is_empty() => {
                    minimum = Some(triple(words)?)
                }
                "DOMAIN_MAX" if maximum.is_none() && values.is_empty() => {
                    maximum = Some(triple(words)?)
                }
                "LUT_3D_INPUT_RANGE"
                    if minimum.is_none() && maximum.is_none() && values.is_empty() =>
                {
                    let low = number(words.next())?;
                    let high = number(words.next())?;
                    if words.next().is_some() {
                        return Err("Invalid .cube input range".into());
                    }
                    minimum = Some([low; 3]);
                    maximum = Some([high; 3]);
                }
                _ => {
                    let grid = size.ok_or("A .cube table needs LUT_3D_SIZE before its rows")?;
                    if values.len() >= grid.pow(3) * 3 {
                        return Err("Unexpected data after the .cube table".into());
                    }
                    values.extend(triple(std::iter::once(key).chain(words))?);
                }
            }
        }
        let grid = size.ok_or("Missing .cube grid size")?;
        let minimum = minimum.unwrap_or([0.; 3]);
        let maximum = maximum.unwrap_or([1.; 3]);
        if values.len() != grid.pow(3) * 3 || (0..3).any(|i| minimum[i] >= maximum[i]) {
            return Err("Invalid .cube domain or row count".into());
        }
        Ok(Self {
            grid: Clut::new([grid; 3], values.into_boxed_slice(), true),
            minimum,
            maximum,
        })
    }

    pub(super) fn sample(&self, rgb: [u16; 3], max: u16) -> [u16; 3] {
        let max = f64::from(max);
        let position = std::array::from_fn(|i| {
            (f64::from(rgb[i]) / max - self.minimum[i]) / (self.maximum[i] - self.minimum[i])
        });
        self.grid
            .sample(position)
            .map(|value| (value.clamp(0., 1.) * max).round() as u16)
    }
}

fn number(word: Option<&str>) -> Result<f64, String> {
    word.and_then(|word| word.parse::<f64>().ok())
        .filter(|value| (-1_000_000. ..=1_000_000.).contains(value))
        .ok_or_else(|| "Invalid or unsupported .cube value".into())
}

fn triple<'a>(mut words: impl Iterator<Item = &'a str>) -> Result<[f64; 3], String> {
    let rgb = [
        number(words.next())?,
        number(words.next())?,
        number(words.next())?,
    ];
    if words.next().is_some() {
        return Err("A .cube RGB row must contain three values".into());
    }
    Ok(rgb)
}
