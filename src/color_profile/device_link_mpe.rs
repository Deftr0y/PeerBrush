//! Bounded ICC v4 floating-point device-link stages for Color Lookup.
use super::{
    clut::{Clut, MAX_GRID, grid_position},
    device_link::MAX_BYTES,
    number,
};

const CHANNELS: &[u8; 8] = b"\0\0\0\0\0\x03\0\x03";
const CREATOR: &[u8; 4] = b"PBru";

pub(crate) enum Input<'a> {
    Domain([f64; 3], [f64; 3]),
    Shaper(&'a [f64]),
}

pub(crate) fn is_peerbrush_profile(profile: &[u8]) -> bool {
    profile.get(80..84) == Some(CREATOR)
}

pub(crate) fn compile(grid: &Clut, input: Input<'_>, swap: [bool; 2]) -> Option<Vec<u8>> {
    let input = match input {
        Input::Domain(mut minimum, mut maximum) => {
            if swap[0] {
                minimum.swap(0, 2);
                maximum.swap(0, 2);
            }
            if minimum == [0.; 3] && maximum == [1.; 3] {
                None
            } else {
                let mut coeffs = [0f32; 12];
                for i in 0..3 {
                    let scale = 1. / (maximum[i] - minimum[i]);
                    coeffs[i * 4] = scale as f32;
                    coeffs[9 + i] = (-minimum[i] * scale) as f32;
                }
                if !coeffs.into_iter().all(f32::is_finite) {
                    return None;
                }
                let mut matrix = stage_header(b"matf");
                matrix.extend(coeffs.into_iter().flat_map(f32::to_be_bytes));
                Some(matrix)
            }
        }
        Input::Shaper(samples) if samples.is_empty() => None,
        Input::Shaper(samples) => Some(shaper_stage(samples)),
    };
    let mut clut = stage_header(b"clut");
    clut.extend(grid.icc_grid(swap[0]).map(|size| size as u8));
    clut.extend([0; 13]);
    clut.extend(grid.icc_nodes(swap).flat_map(f32::to_be_bytes));
    let stages = input.into_iter().chain([clut]).collect::<Vec<_>>();
    let mut mpe = stage_header(b"mpet");
    mpe.extend((stages.len() as u32).to_be_bytes());
    write_directory(&mut mpe, stages.iter().map(Vec::as_slice));
    let mut profile = vec![0; 132];
    profile[8..12].copy_from_slice(&[4, 0x30, 0, 0]);
    profile[12..24].copy_from_slice(b"linkRGB RGB ");
    profile[24..36].copy_from_slice(&[0x07, 0xd0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]);
    profile[36..40].copy_from_slice(b"acsp");
    profile[68..80].copy_from_slice(&[0, 0, 0xf6, 0xd6, 0, 1, 0, 0, 0, 0, 0xd3, 0x2d]);
    profile[80..84].copy_from_slice(CREATOR);
    let description = mluc("PeerBrush Color Lookup");
    let copyright = mluc("Derived from the document's embedded lookup table");
    let tags = [
        (b"desc", &description),
        (b"cprt", &copyright),
        (b"D2B0", &mpe),
    ];
    profile[128..132].copy_from_slice(&(tags.len() as u32).to_be_bytes());
    let mut offset = 132 + tags.len() * 12;
    for (key, tag) in tags {
        profile.extend(key);
        profile.extend((offset as u32).to_be_bytes());
        profile.extend((tag.len() as u32).to_be_bytes());
        offset += tag.len().next_multiple_of(4);
    }
    for (_, tag) in tags {
        profile.extend(tag);
        profile.resize(profile.len().next_multiple_of(4), 0);
    }
    let length = profile.len();
    if length > MAX_BYTES {
        return None;
    }
    profile[..4].copy_from_slice(&(length as u32).to_be_bytes());
    Some(profile)
}

fn stage_header(signature: &[u8; 4]) -> Vec<u8> {
    [signature.as_slice(), CHANNELS].concat()
}

fn mluc(text: &str) -> Vec<u8> {
    let mut bytes = b"mluc\0\0\0\0\0\0\0\x01\0\0\0\x0cenUS".to_vec();
    bytes.extend(((text.encode_utf16().count() * 2) as u32).to_be_bytes());
    bytes.extend(28u32.to_be_bytes());
    bytes.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
    bytes
}

fn write_directory<'a>(
    bytes: &mut Vec<u8>,
    elements: impl ExactSizeIterator<Item = &'a [u8]> + Clone,
) {
    let mut offset = bytes.len() + elements.len() * 8;
    for element in elements.clone() {
        bytes.extend((offset as u32).to_be_bytes());
        bytes.extend((element.len() as u32).to_be_bytes());
        offset += element.len();
    }
    for element in elements {
        bytes.extend(element);
    }
}

fn shaper_stage(samples: &[f64]) -> Vec<u8> {
    let mut curve = b"curf\0\0\0\0\0\x03\0\0".to_vec();
    curve.extend(0f32.to_be_bytes());
    curve.extend(1f32.to_be_bytes());
    constant_segment(&mut curve, samples[0]);
    curve.extend(b"samf\0\0\0\0");
    curve.extend(((samples.len() - 1) as u32).to_be_bytes());
    curve.extend(
        samples[1..]
            .iter()
            .flat_map(|&sample| (sample as f32).to_be_bytes()),
    );
    constant_segment(&mut curve, samples[samples.len() - 1]);
    let mut stage = stage_header(b"cvst");
    for _ in 0..3 {
        stage.extend(36u32.to_be_bytes());
        stage.extend((curve.len() as u32).to_be_bytes());
    }
    stage.extend(curve);
    stage
}

fn constant_segment(curve: &mut Vec<u8>, value: f64) {
    curve.extend(b"parf\0\0\0\0\0\0\0\0");
    curve.extend(
        [1f32, 0., 0., value as f32]
            .into_iter()
            .flat_map(f32::to_be_bytes),
    );
}

pub(super) struct Pipeline(Vec<Stage>);

enum Stage {
    Matrix([f64; 12]),
    Clut(Clut),
    Shaper([Box<[f64]>; 3]),
}

impl Pipeline {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 16 || &bytes[..4] != b"mpet" || &bytes[4..12] != CHANNELS {
            return Err("Invalid RGB DeviceLink mpet header".into());
        }
        let count = number(bytes, 12)?;
        if !(1..=4).contains(&count) {
            return Err("Unsupported RGB DeviceLink stage count".into());
        }
        let mut stages = Vec::with_capacity(count);
        let mut cluts = 0;
        for element in directory(bytes, 16, count)? {
            if element.len() < 12 || &element[4..12] != CHANNELS {
                return Err("Invalid RGB DeviceLink processing element".into());
            }
            let stage = match &element[..4] {
                b"matf" if element.len() == 60 => {
                    let matrix = floats(&element[12..])?;
                    Stage::Matrix(matrix.as_ref().try_into().unwrap())
                }
                b"clut" if element.len() >= 28 && element[15..28] == [0; 13] => {
                    let grid = [element[12], element[13], element[14]].map(usize::from);
                    if grid.iter().any(|size| !(2..=MAX_GRID).contains(size))
                        || element.len() != 28 + grid.into_iter().product::<usize>() * 12
                    {
                        return Err("Invalid RGB DeviceLink float CLUT dimensions".into());
                    }
                    cluts += 1;
                    Stage::Clut(Clut::new(grid, floats(&element[28..])?, false))
                }
                b"cvst" => {
                    let curves = directory(element, 12, 3)?;
                    let curves = curves
                        .into_iter()
                        .map(parse_shaper)
                        .collect::<Result<Vec<_>, _>>()?;
                    Stage::Shaper(curves.try_into().unwrap())
                }
                _ => return Err("Unsupported RGB DeviceLink processing element".into()),
            };
            stages.push(stage);
        }
        if cluts != 1 {
            return Err("RGB DeviceLink requires one float CLUT".into());
        }
        Ok(Self(stages))
    }

    pub(super) fn sample(&self, mut rgb: [f64; 3]) -> [f64; 3] {
        for stage in &self.0 {
            rgb = match stage {
                Stage::Matrix(matrix) => std::array::from_fn(|i| {
                    matrix[i * 3] * rgb[0]
                        + matrix[i * 3 + 1] * rgb[1]
                        + matrix[i * 3 + 2] * rgb[2]
                        + matrix[9 + i]
                }),
                Stage::Clut(clut) => clut.sample(rgb),
                Stage::Shaper(curves) => std::array::from_fn(|i| {
                    let (low, weight) = grid_position(rgb[i], curves[i].len());
                    curves[i][low] + (curves[i][low + 1] - curves[i][low]) * weight
                }),
            };
        }
        rgb
    }
}

fn directory(bytes: &[u8], start: usize, count: usize) -> Result<Vec<&[u8]>, String> {
    let end = start + count * 8;
    let entries = bytes
        .get(start..end)
        .ok_or("Truncated ICC processing element directory")?;
    let mut spans = Vec::with_capacity(count);
    let mut elements = Vec::with_capacity(count);
    for entry in entries.chunks_exact(8) {
        let offset = number(entry, 0)?;
        let size = number(entry, 4)?;
        let finish = offset
            .checked_add(size)
            .ok_or("ICC processing element offset overflow")?;
        if offset < end || offset % 4 != 0 || finish > bytes.len() {
            return Err("Invalid ICC processing element bounds".into());
        }
        spans.push((offset, finish));
        elements.push(&bytes[offset..finish]);
    }
    spans.sort_unstable();
    spans.dedup();
    let mut used = end;
    for (offset, finish) in spans {
        if offset < used || !padding(&bytes[used..offset]) {
            return Err("Overlapping or unrecognized ICC processing element data".into());
        }
        used = finish;
    }
    if !padding(&bytes[used..]) {
        return Err("Unrecognized ICC processing element trailer".into());
    }
    Ok(elements)
}

fn padding(data: &[u8]) -> bool {
    data.len() <= 3 && data.iter().all(|&value| value == 0)
}

fn floats(data: &[u8]) -> Result<Box<[f64]>, String> {
    data.chunks_exact(4)
        .map(|value| {
            let value = f32::from_be_bytes(value.try_into().unwrap());
            value
                .is_finite()
                .then_some(f64::from(value))
                .ok_or_else(|| "Non-finite ICC processing value".into())
        })
        .collect()
}

fn parse_shaper(bytes: &[u8]) -> Result<Box<[f64]>, String> {
    if bytes.len() < 20 || &bytes[..20] != b"curf\0\0\0\0\0\x03\0\0\0\0\0\0\x3f\x80\0\0" {
        return Err("Unsupported ICC DeviceLink shaper curve".into());
    }
    let constant = |segment: &[u8]| -> Result<f64, String> {
        if segment.len() != 28
            || &segment[..24] != b"parf\0\0\0\0\0\0\0\0\x3f\x80\0\0\0\0\0\0\0\0\0\0"
        {
            return Err("Unsupported ICC shaper endpoint".into());
        }
        Ok(floats(&segment[24..])?[0])
    };
    let head = bytes.get(20..48).ok_or("Truncated ICC shaper endpoint")?;
    if bytes.get(48..56) != Some(b"samf\0\0\0\0") {
        return Err("Unsupported ICC DeviceLink shaper samples".into());
    }
    let count = number(bytes, 56)?;
    if !(1..=4095).contains(&count) || bytes.len() != 88 + count * 4 {
        return Err("Invalid ICC DeviceLink shaper size".into());
    }
    let end = 60 + count * 4;
    let mut samples = Vec::with_capacity(count + 1);
    samples.push(constant(head)?);
    samples.extend(floats(&bytes[60..end])?.iter());
    if constant(&bytes[end..])? != samples[count] {
        return Err("Discontinuous ICC shaper endpoint".into());
    }
    Ok(samples.into_boxed_slice())
}
