//! Integer Flame and Lustre lookup tables, with blue-fastest rows.
use crate::color_profile::clut::{Clut, MAX_GRID};
use crate::color_profile::device_link_mpe::{self, Input};

pub(super) struct ThreeDl {
    grid: Clut,
    shaper: Box<[f64]>,
}

impl ThreeDl {
    pub(super) fn compile_profile(&self, swap: [bool; 2]) -> Option<Vec<u8>> {
        device_link_mpe::compile(&self.grid, Input::Shaper(&self.shaper), swap)
    }
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "A .3dl table must be UTF-8 text")?;
        let mut rows = Vec::new();
        let mut shaper = Vec::new();
        let (mut mesh_header, mut footer, mut gamma) = (false, false, false);
        let mut mesh = None;
        for raw in text.trim_start_matches('\u{feff}').lines() {
            let mut words = raw.split('#').next().unwrap_or("").split_whitespace();
            let Some(first) = words.next() else {
                continue;
            };
            match first {
                "3DMESH"
                    if !mesh_header
                        && mesh.is_none()
                        && shaper.is_empty()
                        && rows.is_empty()
                        && words.next().is_none() =>
                {
                    mesh_header = true;
                }
                "Mesh" if mesh_header && mesh.is_none() && shaper.is_empty() && rows.is_empty() => {
                    let input = code(words.next())?;
                    let output = code(words.next())?;
                    if !(1..=5).contains(&input)
                        || !matches!(output, 8 | 10 | 12 | 14 | 16)
                        || words.next().is_some()
                    {
                        return Err("Unsupported .3dl Mesh dimensions or output depth".into());
                    }
                    mesh = Some(((1usize << input) + 1, (1u32 << output) - 1));
                }
                "LUT8"
                    if mesh.is_some() && !rows.is_empty() && !footer && words.next().is_none() =>
                {
                    footer = true
                }
                "gamma"
                    if footer
                        && !gamma
                        && words.next().and_then(|word| word.parse::<f64>().ok()) == Some(1.)
                        && words.next().is_none() =>
                {
                    gamma = true
                }
                _ if !footer => {
                    let mut line = std::iter::once(first).chain(words);
                    let rgb = [code(line.next())?, code(line.next())?, code(line.next())?];
                    if let Some(fourth) = line.next() {
                        if !shaper.is_empty() || !rows.is_empty() {
                            return Err("Unexpected .3dl shaper row".into());
                        }
                        shaper.extend(rgb);
                        shaper.push(code(Some(fourth))?);
                        for word in line {
                            if shaper.len() == 4096 {
                                return Err(".3dl shaper exceeds 4096 entries".into());
                            }
                            shaper.push(code(Some(word))?);
                        }
                    } else {
                        if rows.len() >= MAX_GRID.pow(3) * 3 {
                            return Err(".3dl grid exceeds 33 points per axis".into());
                        }
                        rows.extend(rgb);
                    }
                }
                _ => return Err("Unsupported .3dl footer".into()),
            }
        }
        if mesh_header && mesh.is_none() {
            return Err("Missing .3dl Mesh record".into());
        }
        let edge = (2..=MAX_GRID)
            .find(|edge| edge.pow(3) * 3 == rows.len())
            .ok_or("Incomplete or oversized .3dl grid")?;
        let largest = rows.iter().copied().max().unwrap();
        let scale = if let Some((declared_edge, scale)) = mesh {
            if edge != declared_edge || u32::from(largest) > scale {
                return Err(".3dl data disagrees with the Mesh record".into());
            }
            f64::from(scale)
        } else {
            inferred_scale(largest)?
        };
        let shaper = if let Some(largest) = shaper.iter().copied().max() {
            let scale = inferred_scale(largest)?;
            let step = scale / (shaper.len() - 1) as f64;
            if shaper
                .iter()
                .enumerate()
                .all(|(i, &value)| (f64::from(value) - i as f64 * step).abs() < 2.)
            {
                Box::default()
            } else {
                shaper
                    .into_iter()
                    .map(|value| f64::from(value) / scale)
                    .collect()
            }
        } else {
            Box::default()
        };
        let values = rows
            .into_iter()
            .map(|value| f64::from(value) / scale)
            .collect();
        Ok(Self {
            grid: Clut::new([edge; 3], values, false),
            shaper,
        })
    }

    pub(super) fn sample(&self, rgb: [u16; 3], max: u16) -> [u16; 3] {
        let max = f64::from(max);
        let position = rgb.map(|value| {
            let value = f64::from(value) / max;
            if self.shaper.is_empty() {
                return value;
            }
            let p = value * (self.shaper.len() - 1) as f64;
            let low = (p as usize).min(self.shaper.len() - 2);
            self.shaper[low] + (self.shaper[low + 1] - self.shaper[low]) * (p - low as f64)
        });
        self.grid
            .sample(position)
            .map(|value| (value.clamp(0., 1.) * max).round() as u16)
    }
}

fn code(word: Option<&str>) -> Result<u16, String> {
    word.and_then(|word| word.parse().ok())
        .ok_or_else(|| "Invalid .3dl integer code".into())
}

fn inferred_scale(largest: u16) -> Result<f64, String> {
    // Headerless integer tables use the code-depth inference documented by OpenColorIO.
    match largest {
        0..=127 => Err("Headerless .3dl code depth is ambiguous".into()),
        128..=511 => Ok(255.),
        512..=2047 => Ok(1023.),
        2048..=8191 => Ok(4095.),
        _ => Ok(65535.),
    }
}
