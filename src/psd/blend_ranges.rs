//! Photoshop layer-record blending ranges, kept separate from tagged blocks.
use super::LayerMetadata;

pub(crate) const TAG: [u8; 4] = *b"@rng";
pub(crate) const KEY: &str = "@rng";
pub(crate) const DEFAULT: [u8; 8] = [0, 0, 255, 255, 0, 0, 255, 255];
pub(super) const INACTIVE_EFFECTS_FORMAT: u32 = 75;

#[derive(Clone, Copy)]
pub(crate) struct Ranges([[u8; 8]; 4]);

pub(crate) fn parse(bytes: &[u8], channels: u16) -> Result<&[u8], &'static str> {
    if bytes.len() % 8 != 0 || bytes.len() > (usize::from(channels) + 1) * 8 {
        return Err("Invalid Photoshop Blend If range count");
    }
    let mut end = 0;
    for (index, pair) in bytes.chunks_exact(8).enumerate() {
        if pair == DEFAULT {
            continue;
        }
        if index >= 4 || pair.chunks_exact(4).any(|range| !range.is_sorted()) {
            return Err("Unsupported Photoshop Blend If channel or endpoints");
        }
        end = (index + 1) * 8;
    }
    Ok(&bytes[..end])
}

pub(crate) fn bytes(metadata: &[LayerMetadata]) -> Option<&[u8]> {
    metadata
        .iter()
        .find(|entry| entry.key == KEY)
        .map(|entry| entry.data.as_slice())
}

pub(crate) fn value(metadata: &[LayerMetadata]) -> Option<[[u8; 8]; 4]> {
    bytes(metadata).map(pairs)
}

pub(crate) fn pairs(bytes: &[u8]) -> [[u8; 8]; 4] {
    let mut pairs = [DEFAULT; 4];
    for (pair, stored) in pairs.iter_mut().zip(bytes.chunks_exact(8)) {
        pair.copy_from_slice(stored);
    }
    pairs
}

pub(crate) fn ranges(metadata: &[LayerMetadata]) -> Option<Ranges> {
    value(metadata).map(Ranges)
}

pub(super) fn source_format(doc: &crate::engine::Document, layer: &crate::engine::Layer) -> u32 {
    if bytes(&layer.psd_metadata).is_none() {
        return 0;
    }
    if doc
        .layers
        .iter()
        .any(|member| member.clip_to.as_deref() == Some(&layer.id))
    {
        return 69;
    }
    if layer.clip_to.is_some() {
        return 68;
    }
    match layer.kind.as_str() {
        "group" if layer.blend == "pass_through" => 67,
        "adjustment" => 66,
        "group" => 65,
        "paint" if !layer.effects.is_empty() => INACTIVE_EFFECTS_FORMAT,
        _ => 64,
    }
}

pub(crate) fn update(
    metadata: &mut Vec<LayerMetadata>,
    command: &serde_json::Value,
) -> Result<(), String> {
    let Some(input) = command.get("blend_if") else {
        return Ok(());
    };
    let storage;
    let payload = if input.is_null() {
        &[][..]
    } else {
        let rows = input
            .as_array()
            .filter(|rows| (1..=4).contains(&rows.len()))
            .ok_or("Blend If requires one to four Gray/R/G/B pairs, or null to reset")?;
        let mut pairs = [DEFAULT; 4];
        for (pair, row) in pairs.iter_mut().zip(rows) {
            let invalid = "Each Blend If pair requires eight byte endpoints";
            let row = row
                .as_array()
                .filter(|row| row.len() == pair.len())
                .ok_or(invalid)?;
            for (endpoint, value) in pair.iter_mut().zip(row) {
                *endpoint = value
                    .as_u64()
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or(invalid)?;
            }
        }
        storage = pairs;
        parse(storage.as_flattened(), 3)?
    };
    if bytes(metadata).unwrap_or_default() == payload {
        return Ok(());
    }
    metadata.retain(|entry| entry.key != KEY);
    if !payload.is_empty() {
        metadata.push(LayerMetadata {
            key: KEY.into(),
            data: payload.to_vec(),
        });
    }
    super::validate_metadata(metadata)
}

pub(crate) fn validate_layer(
    metadata: &[LayerMetadata],
    kind: &str,
    blend: &str,
    effects: impl Iterator<Item = bool>,
    clipped: bool,
    unsupported_base: bool,
) -> Result<(), String> {
    if bytes(metadata).is_some() && !supported(kind, blend, effects, clipped, unsupported_base) {
        return Err("Blend If requires an adjustment, unstyled paint, Solid Color or folder, or unclipped paint with disabled effects. Clear the ranges before enabling an effect on that paint layer. Clipping bases must be grouped paint or Solid Color without effects or special partial Fill".into());
    }
    Ok(())
}

pub(crate) fn supported(
    kind: &str,
    blend: &str,
    mut effects: impl Iterator<Item = bool>,
    clipped: bool,
    unsupported_base: bool,
) -> bool {
    !unsupported_base
        && (kind == "group" || blend != "pass_through")
        && match kind {
            "adjustment" => true,
            "paint" if !clipped => effects.all(|enabled| !enabled),
            "paint" | "fill" | "group" => effects.next().is_none(),
            _ => false,
        }
}

pub(crate) fn supports_clip_base(
    kind: &str,
    blend: &str,
    has_effects: bool,
    grouped: bool,
    fill: u8,
) -> bool {
    super::clipping::supports_advanced_base(kind, has_effects, grouped)
        && crate::raster::fill_blend::SpecialFill::new(blend, fill).is_none()
}

pub(crate) fn validate_clipped_member(
    metadata: &[LayerMetadata],
    kind: &str,
    base_kind: &str,
    base_has_effects: bool,
    grouped: bool,
) -> Result<(), String> {
    if bytes(metadata).is_some()
        && !super::clipping::supports_advanced_member(kind, base_kind, base_has_effects, grouped)
    {
        return Err("Clipped Blend If requires a paint, Solid Color or adjustment layer over a grouped paint or Solid Color base without effects".into());
    }
    Ok(())
}

impl Ranges {
    pub(crate) fn coverage(self, backdrop: [u16; 4], source: [u16; 4], max: u16) -> f64 {
        self.coverage_scaled(backdrop, source, max, 1.)
    }

    pub(crate) fn coverage_scaled(
        self,
        backdrop: [u16; 4],
        source: [u16; 4],
        max: u16,
        backdrop_scale: f64,
    ) -> f64 {
        let source_gate = self.gate(source, max, 0);
        let alpha = f64::from(backdrop[3]) / f64::from(max) * backdrop_scale;
        source_gate * (1. - alpha + alpha * self.gate(backdrop, max, 4))
    }

    fn gate(self, pixel: [u16; 4], max: u16, offset: usize) -> f64 {
        let [r, g, b, _] = pixel.map(|value| f64::from(value) * 255. / f64::from(max));
        let [red, green, blue, _] = pixel.map(u64::from);
        let scale = u64::from(max) * 1000;
        let gray = (((299 * red + 590 * green + 111 * blue) * 255 + scale / 2) / scale) as f64;
        self.0
            .iter()
            .zip([gray, r, g, b])
            .filter(|(pair, _)| **pair != DEFAULT)
            .map(|(pair, value)| {
                let [black_min, black_max, white_min, white_max] =
                    std::array::from_fn(|i| f64::from(pair[offset + i]));
                let rising = |v, start, end| {
                    if v < start {
                        0.
                    } else if v >= end {
                        1.
                    } else {
                        (v - start + 1.) / (end - start + 1.)
                    }
                };
                rising(value, black_min, black_max)
                    * rising(255. - value, 255. - white_max, 255. - white_min)
            })
            .product()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_gates_multiply_and_transparent_backdrop_does_not_gate() {
        let ranges = Ranges([
            [0, 0, 200, 201, 0, 0, 100, 100],
            [201, 202, 255, 255, 0, 0, 255, 255],
            DEFAULT,
            DEFAULT,
        ]);
        let src = [201, 201, 201, 255];
        assert_eq!(ranges.coverage([0; 4], src, 255), 0.25);
        assert_eq!(ranges.coverage([150, 150, 150, 255], src, 255), 0.);
        let partial = ranges.coverage([150, 150, 150, 128], src, 255);
        assert!((partial - 0.25 * 127. / 255.).abs() < 1e-12);
        let deep = ranges.coverage([0; 4], src.map(|sample| sample * 257), 65535);
        assert_eq!(deep, 0.25);
    }

    #[test]
    fn only_neutral_non_color_pairs_can_be_ignored() {
        let mut pairs = [DEFAULT; 6];
        pairs[1][0] = 50;
        assert!(parse(pairs.as_flattened(), 5).is_err());
        pairs[1][1] = 50;
        assert_eq!(
            parse(pairs.as_flattened(), 5).unwrap(),
            pairs[..2].as_flattened()
        );
        pairs[4][4..6].fill(64);
        assert!(parse(pairs.as_flattened(), 5).is_err());
    }
}
