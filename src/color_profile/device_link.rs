//! RGB-to-RGB ICC DeviceLink. Its output is device RGB, with no PCS conversion.
use super::clut::{Clut, MAX_GRID, grid_position};
use super::device_link_mpe::Pipeline;
use super::{classic_lut, number};
use moxcms::ParametricCurve;

pub(crate) const MAX_BYTES: usize = 512 * 1024;

pub(crate) struct DeviceLink {
    input: [Curve; 3],
    clut: Option<Clut>,
    middle: [Curve; 3],
    matrix: Option<[f64; 12]>,
    output: [Curve; 3],
    float: Option<Pipeline>,
}

impl DeviceLink {
    pub(crate) fn parse(profile: &[u8]) -> Result<Self, String> {
        if !(132..=MAX_BYTES).contains(&profile.len())
            || number(profile, 0)? != profile.len()
            || !matches!(profile[8], 2 | 4)
            || &profile[12..24] != b"linkRGB RGB "
            || &profile[36..40] != b"acsp"
            || number(profile, 64)? > 3
        {
            return Err("Color Lookup requires an ICC v2/v4 RGB-to-RGB DeviceLink".into());
        }
        let count = number(profile, 128)?;
        let directory_end = 132 + count.min(129) * 12;
        if count > 128 || directory_end > profile.len() {
            return Err("Invalid DeviceLink tag directory".into());
        }
        let mut seen = std::collections::HashSet::new();
        let mut spans = Vec::with_capacity(count);
        let mut transform = None;
        let mut float = None;
        for entry in profile[132..directory_end].chunks_exact(12) {
            let key: [u8; 4] = entry[..4].try_into().unwrap();
            let start = number(entry, 4)?;
            let length = number(entry, 8)?;
            let data = start
                .checked_add(length)
                .and_then(|end| profile.get(start..end))
                .filter(|data| start >= directory_end && start % 4 == 0 && data.len() >= 8)
                .ok_or("Invalid DeviceLink tag bounds")?;
            if !seen.insert(key) {
                return Err("Duplicate ICC DeviceLink tag".into());
            }
            spans.push((start, start + length));
            match &key {
                b"A2B0" => transform = Some(data),
                b"D2B0" if profile[8] == 4 => float = Some(data),
                // These tags describe the link or its profile sequence. The
                // link already incorporates its adaptation and rendering intent.
                b"desc" | b"cprt" | b"pseq" | b"psid" | b"wtpt" | b"chad" | b"meta" => {}
                _ => return Err("Unsupported ICC DeviceLink tag".into()),
            }
        }
        spans.sort_unstable();
        spans.dedup();
        if spans.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err("Overlapping ICC DeviceLink tags".into());
        }
        if let Some(bytes) = float {
            if let Some(classic) = transform {
                Self::parse_classic(classic, profile[8])?;
            }
            return Ok(Self {
                input: identity(),
                clut: None,
                middle: identity(),
                matrix: None,
                output: identity(),
                float: Some(Pipeline::parse(bytes)?),
            });
        }
        let data = transform.ok_or("ICC DeviceLink has no supported forward transform")?;
        Self::parse_classic(data, profile[8])
    }

    fn parse_classic(data: &[u8], version: u8) -> Result<Self, String> {
        match &data[..4] {
            b"mft1" | b"mft2" => Self::classic(data),
            b"mAB " if version == 4 => Self::multistage(data),
            _ => Err("Unsupported ICC DeviceLink transform type".into()),
        }
    }

    fn classic(data: &[u8]) -> Result<Self, String> {
        let words = classic_lut(data)?;
        let coefficients = fixed_values::<9>(&data[12..48]);
        if coefficients != [1., 0., 0., 0., 1., 0., 0., 0., 1.] {
            return Err("RGB DeviceLink classic LUT matrix must be identity".into());
        }
        let grid = usize::from(data[10]);
        let (input, output, header, stride) = if words {
            (word(data, 48), word(data, 50), 52, 2)
        } else {
            (256, 256, 48, 1)
        };
        let data = &data[header..];
        let clut_start = input * 3 * stride;
        let output_start = clut_start + grid.pow(3) * 3 * stride;
        let curves = |data: &[u8], count| {
            let length = count * stride;
            std::array::from_fn(|axis| {
                Curve::Table(samples(&data[axis * length..][..length], stride))
            })
        };
        Ok(Self {
            input: curves(data, input),
            clut: Some(Clut::new(
                [grid; 3],
                samples(&data[clut_start..output_start], stride),
                false,
            )),
            middle: identity(),
            matrix: None,
            output: curves(&data[output_start..], output),
            float: None,
        })
    }

    fn multistage(data: &[u8]) -> Result<Self, String> {
        if data.len() < 32 || data[4..12] != [0, 0, 0, 0, 3, 3, 0, 0] {
            return Err("Invalid RGB DeviceLink mAB header".into());
        }
        let [b, matrix, m, clut, a] = std::array::from_fn(|i| number(data, 12 + i * 4).unwrap());
        if b == 0 || (a == 0) != (clut == 0) || (m == 0) != (matrix == 0) {
            return Err("Invalid DeviceLink mAB stage combination".into());
        }
        let mut offsets = [b, matrix, m, clut, a]
            .into_iter()
            .filter(|&offset| offset != 0)
            .collect::<Vec<_>>();
        offsets.sort_unstable();
        offsets.dedup();
        if offsets
            .iter()
            .any(|&offset| offset < 32 || offset % 4 != 0 || offset >= data.len())
            || data[32..offsets[0]].iter().any(|&byte| byte != 0)
            || (matrix != 0 && [a, b, m, clut].contains(&matrix))
            || (clut != 0 && [a, b, m].contains(&clut))
        {
            return Err("Invalid DeviceLink mAB stage offsets".into());
        }
        let stage = |offset| {
            let end = offsets
                .iter()
                .copied()
                .find(|&next| next > offset)
                .unwrap_or(data.len());
            &data[offset..end]
        };
        let curves = |offset| {
            if offset == 0 {
                Ok(identity())
            } else {
                parse_curves(stage(offset))
            }
        };
        let matrix = if matrix == 0 {
            None
        } else {
            let bytes = stage(matrix);
            padding(bytes, 48)?;
            Some(fixed_values::<12>(bytes))
        };
        let clut = if clut == 0 {
            None
        } else {
            Some(parse_clut(stage(clut))?)
        };
        Ok(Self {
            input: curves(a)?,
            clut,
            middle: curves(m)?,
            matrix,
            output: curves(b)?,
            float: None,
        })
    }

    pub(crate) fn sample(&self, rgb: [u16; 3], max: u16) -> [u16; 3] {
        let max = f64::from(max);
        if let Some(pipeline) = &self.float {
            return pipeline
                .sample(rgb.map(|value| f64::from(value) / max))
                .map(|value| (value.clamp(0., 1.) * max).round() as u16);
        }
        let mut rgb = std::array::from_fn(|i| self.input[i].sample(f64::from(rgb[i]) / max));
        if let Some(clut) = &self.clut {
            rgb = clut.sample(rgb);
        }
        if let Some(matrix) = self.matrix {
            rgb = std::array::from_fn(|i| self.middle[i].sample(rgb[i]));
            rgb = std::array::from_fn(|i| {
                matrix[i * 3] * rgb[0]
                    + matrix[i * 3 + 1] * rgb[1]
                    + matrix[i * 3 + 2] * rgb[2]
                    + matrix[9 + i]
            });
        }
        std::array::from_fn(|i| (self.output[i].sample(rgb[i]) * max).round() as u16)
    }
}

fn word(data: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([data[at], data[at + 1]]))
}

fn samples(bytes: &[u8], stride: usize) -> Box<[f64]> {
    bytes
        .chunks_exact(stride)
        .map(|sample| {
            if stride == 1 {
                f64::from(sample[0]) / 255.
            } else {
                word(sample, 0) as f64 / 65535.
            }
        })
        .collect()
}

fn fixed_values<const N: usize>(bytes: &[u8]) -> [f64; N] {
    std::array::from_fn(|i| {
        f64::from(i32::from_be_bytes(bytes[i * 4..][..4].try_into().unwrap())) / 65536.
    })
}

fn padding(data: &[u8], end: usize) -> Result<(), String> {
    if data
        .get(end..)
        .is_none_or(|tail| tail.len() > 3 || tail.iter().any(|&byte| byte != 0))
    {
        return Err("Invalid ICC DeviceLink stage size or padding".into());
    }
    Ok(())
}

enum Curve {
    Identity,
    Table(Box<[f64]>),
    Gamma(f64),
    Parametric(ParametricCurve),
}

impl Curve {
    fn sample(&self, value: f64) -> f64 {
        let value = value.clamp(0., 1.);
        match self {
            Self::Identity => value,
            Self::Gamma(gamma) => value.powf(*gamma),
            Self::Parametric(curve) => {
                let value = if value < f64::from(curve.d) {
                    f64::from(curve.c) * value + f64::from(curve.f)
                } else {
                    (f64::from(curve.a) * value + f64::from(curve.b))
                        .max(0.)
                        .powf(f64::from(curve.g))
                        + f64::from(curve.e)
                };
                value.clamp(0., 1.)
            }
            Self::Table(table) => {
                let (low, fraction) = grid_position(value, table.len());
                table[low] + (table[low + 1] - table[low]) * fraction
            }
        }
    }
}

fn identity() -> [Curve; 3] {
    [Curve::Identity, Curve::Identity, Curve::Identity]
}

fn parse_curves(mut data: &[u8]) -> Result<[Curve; 3], String> {
    let mut curves = identity();
    for curve in &mut curves {
        if data.len() < 12 || data[4..8] != [0; 4] {
            return Err("Invalid ICC DeviceLink curve header".into());
        }
        let end;
        *curve = match &data[..4] {
            b"curv" => {
                let count = number(data, 8)?;
                if count > 4096 || 12 + count * 2 > data.len() {
                    return Err("Invalid ICC DeviceLink curve size".into());
                }
                end = 12 + count * 2;
                match count {
                    0 => Curve::Identity,
                    1 if word(data, 12) != 0 => Curve::Gamma(word(data, 12) as f64 / 256.),
                    1 => return Err("Invalid ICC DeviceLink curve gamma".into()),
                    _ => Curve::Table(samples(&data[12..end], 2)),
                }
            }
            b"para" if data[10..12] == [0; 2] => {
                let count = *[1, 3, 4, 5, 7]
                    .get(word(data, 8))
                    .ok_or("Unsupported ICC DeviceLink parametric curve")?;
                end = 12 + count * 4;
                if end > data.len() {
                    return Err("Truncated ICC DeviceLink parametric curve".into());
                }
                let params = data[12..end]
                    .chunks_exact(4)
                    .map(|value| fixed_values::<1>(value)[0] as f32)
                    .collect::<Vec<_>>();
                let curve = ParametricCurve::new(&params)
                    .ok_or("Invalid ICC DeviceLink parametric curve")?;
                if curve.g <= 0.
                    || curve.a <= 0.
                    || (curve.d <= 1. && curve.a * curve.d.max(0.) + curve.b < -1e-6)
                    || !curve.eval(0.).is_finite()
                    || !curve.eval(1.).is_finite()
                {
                    return Err("Invalid ICC DeviceLink parametric curve coefficients".into());
                }
                Curve::Parametric(curve)
            }
            _ => return Err("Unsupported ICC DeviceLink curve type".into()),
        };
        let next = end.next_multiple_of(4).min(data.len());
        padding(&data[..next], end)?;
        data = &data[next..];
    }
    padding(data, 0)?;
    Ok(curves)
}

fn parse_clut(data: &[u8]) -> Result<Clut, String> {
    if data.len() < 20
        || data[3..16] != [0; 13]
        || !matches!(data[16], 1 | 2)
        || data[17..20] != [0; 3]
    {
        return Err("Invalid ICC DeviceLink CLUT header".into());
    }
    let grid = [data[0], data[1], data[2]].map(usize::from);
    if grid.iter().any(|&size| !(2..=MAX_GRID).contains(&size)) {
        return Err("ICC DeviceLink CLUT dimensions exceed the supported grid".into());
    }
    let stride = usize::from(data[16]);
    let end = 20 + grid.into_iter().product::<usize>() * 3 * stride;
    padding(data, end)?;
    Ok(Clut::new(grid, samples(&data[20..end], stride), false))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn channel_link(modern: bool) -> Vec<u8> {
        let cube = (0..=1u16)
            .flat_map(|r| {
                (0..=1u16).flat_map(move |g| {
                    (0..=1u16).flat_map(move |b| [1 - g, r, b].map(|value| value * 65535))
                })
            })
            .flat_map(u16::to_be_bytes);
        let data = if modern {
            let mut data = b"mAB \0\0\0\0\x03\x03\0\0".to_vec();
            for offset in [32u32, 0, 0, 68, 32] {
                data.extend(offset.to_be_bytes());
            }
            for _ in 0..3 {
                data.extend(b"curv\0\0\0\0\0\0\0\0");
            }
            let mut grid = [0; 20];
            grid[..3].copy_from_slice(&[2; 3]);
            grid[16] = 2;
            data.extend(grid);
            data.extend(cube);
            data
        } else {
            let mut data = b"mft2\0\0\0\0\x03\x03\x02\0".to_vec();
            for value in [65536i32, 0, 0, 0, 65536, 0, 0, 0, 65536] {
                data.extend(value.to_be_bytes());
            }
            data.extend([0, 2, 0, 2]);
            for _ in 0..3 {
                data.extend([0, 0, 255, 255]);
            }
            data.extend(cube);
            for _ in 0..3 {
                data.extend([0, 0, 255, 255]);
            }
            data
        };
        let mut profile = vec![0; 144];
        profile[..4].copy_from_slice(&((144 + data.len()) as u32).to_be_bytes());
        profile[8] = if modern { 4 } else { 2 };
        profile[12..24].copy_from_slice(b"linkRGB RGB ");
        profile[36..40].copy_from_slice(b"acsp");
        profile[128..132].copy_from_slice(&1u32.to_be_bytes());
        profile[132..136].copy_from_slice(b"A2B0");
        profile[136..140].copy_from_slice(&144u32.to_be_bytes());
        profile[140..144].copy_from_slice(&(data.len() as u32).to_be_bytes());
        profile.extend(data);
        profile
    }

    #[test]
    fn classic_and_v4_rgb_device_links_keep_native_words_and_validate_stages() {
        for modern in [false, true] {
            let bytes = channel_link(modern);
            let link = DeviceLink::parse(&bytes).unwrap();
            for rgb in [
                [12345, 23456, 34567],
                [65535, 123, 0],
                [0, 65535, 65535],
                [731, 731, 731],
            ] {
                assert_eq!(link.sample(rgb, 65535), [65535 - rgb[1], rgb[0], rgb[2]]);
            }
            assert_eq!(link.sample([239, 101, 17], 255), [154, 239, 17]);
            let mut wrong_space = bytes.clone();
            wrong_space[20..24].copy_from_slice(b"Lab ");
            assert!(DeviceLink::parse(&wrong_space).is_err());
            let mut malformed = bytes.clone();
            if modern {
                malformed[144 + 24..144 + 28].copy_from_slice(&32u32.to_be_bytes());
            } else {
                malformed[144 + 10] = 34;
            }
            assert!(DeviceLink::parse(&malformed).is_err());
            assert!(DeviceLink::parse(&bytes[..bytes.len() - 1]).is_err());
        }
    }
}
