//! Bounded RGB matrix/TRC and classic LUT ICC conversion. Source samples stay in their profile;
//! only display previews and an explicitly requested sRGB copy are converted.
use moxcms::{
    ColorProfile, DataColorSpace, Layout, ProfileClass, RenderingIntent, Transform16BitExecutor,
    Transform8BitExecutor, TransformOptions,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock},
};

pub const MAX_PROFILE_BYTES: usize = 4 * 1024 * 1024;
struct Transforms {
    bytes: Arc<Transform8BitExecutor>,
    words: Arc<Transform16BitExecutor>,
}
type Entry = (Vec<u8>, Result<Arc<Transforms>, String>);
static CACHE: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
pub fn srgb_profile() -> &'static [u8] {
    static PROFILE: OnceLock<Vec<u8>> = OnceLock::new();
    PROFILE.get_or_init(|| {
        let mut profile = ColorProfile::new_srgb();
        profile.cicp = None;
        let mut bytes = profile.encode().expect("Built-in sRGB ICC profile");
        // moxcms writes the current time even when the profile properties are
        // fixed. A standard output tag must be stable across app processes.
        bytes[24..36].copy_from_slice(&[0x07, 0xcc, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]);
        bytes
    })
}
pub fn is_srgb_profile(bytes: &[u8]) -> bool {
    let standard = srgb_profile();
    // Earlier PeerBrush saves used the same tag with an encoder timestamp.
    // Ignore only that cosmetic header field; matrices/TRCs and all other
    // header/tag bytes must still be identical to our standard output.
    bytes.len() == standard.len()
        && bytes[..24] == standard[..24]
        && bytes[36..] == standard[36..]
        && parse(bytes).is_ok()
}

fn number(bytes: &[u8], at: usize) -> Result<usize, String> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at + 4)
            .ok_or("Truncated ICC profile")?
            .try_into()
            .unwrap(),
    ) as usize)
}

// Validate dimensions before the CMS allocates any tables. Classic RGB LUTs
// have three input/output channels; larger grids and newer processing elements
// remain protected rather than being approximated as a matrix profile.
fn classic_lut(data: &[u8]) -> Result<bool, String> {
    let words = match data.get(..4) {
        Some(b"mft1") => false,
        Some(b"mft2") => true,
        _ => return Err("Only classic lut8/lut16 RGB ICC tables are supported".into()),
    };
    let header = if words { 52 } else { 48 };
    if data.len() < header
        || data[4..8] != [0; 4]
        || data[8..10] != [3, 3]
        || !(2..=33).contains(&data[10])
        || data[11] != 0
    {
        return Err("Invalid or excessive RGB ICC LUT dimensions".into());
    }
    let entries = |at| u16::from_be_bytes([data[at], data[at + 1]]) as usize;
    let (input, output) = if words {
        (entries(48), entries(50))
    } else {
        (256, 256)
    };
    if !(2..=4096).contains(&input) || !(2..=4096).contains(&output) {
        return Err("ICC LUT curves exceed the supported table size".into());
    }
    let size = header
        + (input * 3 + usize::from(data[10]).pow(3) * 3 + output * 3) * if words { 2 } else { 1 };
    if data.len() < size || data.len() > size + 3 || data[size..].iter().any(|v| *v != 0) {
        return Err("Invalid ICC LUT table length".into());
    }
    Ok(words)
}

fn parse(bytes: &[u8]) -> Result<ColorProfile, String> {
    if bytes.len() > MAX_PROFILE_BYTES || bytes.len() < 132 {
        return Err("ICC profile exceeds the supported size or is truncated".into());
    }
    if number(bytes, 0)? != bytes.len() || &bytes[36..40] != b"acsp" {
        return Err("Invalid ICC profile header or size".into());
    }
    if !matches!(bytes[8], 2 | 4)
        || &bytes[16..20] != b"RGB "
        || !matches!(&bytes[20..24], b"XYZ " | b"Lab ")
    {
        return Err(
            "Only ICC v2/v4 RGB profiles with XYZ or Lab connection space are supported".into(),
        );
    }
    let count = number(bytes, 128)?;
    if count > 128 || 132 + count * 12 > bytes.len() {
        return Err("Invalid or excessive ICC tag table".into());
    }
    let mut tags = std::collections::HashSet::new();
    let mut forward = std::collections::HashMap::new();
    for tag in bytes[132..132 + count * 12].chunks_exact(12) {
        let signature: [u8; 4] = tag[..4].try_into().unwrap();
        if !tags.insert(signature) {
            return Err("Duplicate ICC tags are ambiguous".into());
        }
        let offset = number(tag, 4)?;
        let length = number(tag, 8)?;
        if offset < 132 + count * 12
            || length < 8
            || offset
                .checked_add(length)
                .is_none_or(|end| end > bytes.len())
        {
            return Err("Invalid ICC tag bounds".into());
        }
        if matches!(
            &signature,
            b"D2B0" | b"D2B1" | b"D2B2" | b"D2B3" | b"B2D0" | b"B2D1" | b"B2D2" | b"B2D3"
        ) {
            return Err("ICC multi-process color transforms are not supported".into());
        }
        if matches!(
            &signature,
            b"A2B0" | b"A2B1" | b"A2B2" | b"B2A0" | b"B2A1" | b"B2A2"
        ) {
            let words = classic_lut(&bytes[offset..offset + length])?;
            if !words && &bytes[20..24] == b"XYZ " {
                return Err("lut8 PCS XYZ has no standardized encoding".into());
            }
            if &signature[..3] == b"A2B" {
                forward.insert(signature, words);
            }
        }
        if &signature == b"cicp" {
            let data = &bytes[offset..offset + length];
            if length != 12
                || &data[..4] != b"cicp"
                || !matches!(data[9], 1 | 4 | 5 | 6 | 8 | 13 | 14 | 15)
            {
                return Err(
                    "HDR or unsupported CICP transfer characteristics are not supported".into(),
                );
            }
        }
    }
    let lut = !forward.is_empty();
    if !tags.contains(b"wtpt") {
        return Err("RGB ICC profiles require a white point".into());
    }
    if !lut
        && [
            *b"rXYZ", *b"gXYZ", *b"bXYZ", *b"rTRC", *b"gTRC", *b"bTRC", *b"wtpt",
        ]
        .iter()
        .any(|tag| !tags.contains(tag))
    {
        return Err(
            "RGB matrix profiles require three colorants, three TRCs and a white point".into(),
        );
    }
    let lut_words = if lut {
        Some(
            *forward
                .get(b"A2B1")
                .or_else(|| forward.get(b"A2B0"))
                .ok_or("RGB ICC LUT requires a colorimetric or perceptual forward table")?,
        )
    } else {
        None
    };
    let mut derived = std::borrow::Cow::Borrowed(bytes);
    if &bytes[20..24] == b"Lab " {
        if let Some(words) = lut_words {
            // lut16Type uses legacy PCS Lab encoding even in an ICC v4 file.
            // moxcms selects this normalization by version; adjust only its
            // derived input, never the stored profile or original samples.
            derived.to_mut()[8..12]
                .copy_from_slice(&if words { 0x02400000u32 } else { 0x04000000u32 }.to_be_bytes());
        }
    }
    let profile = ColorProfile::new_from_slice_with_options(
        &derived,
        moxcms::ParsingOptions {
            max_profile_size: MAX_PROFILE_BYTES + 1,
            max_allowed_clut_size: 33usize.pow(3),
            max_allowed_trc_size: 65536,
        },
    )
    .map_err(|e| format!("Invalid ICC profile: {e}"))?;
    if profile.color_space != DataColorSpace::Rgb
        || !matches!(profile.pcs, DataColorSpace::Xyz | DataColorSpace::Lab)
        || !matches!(
            profile.profile_class,
            ProfileClass::InputDevice
                | ProfileClass::DisplayDevice
                | ProfileClass::OutputDevice
                | ProfileClass::ColorSpace
        )
    {
        return Err(
            "Only RGB input, display, output or color-space profiles can be converted".into(),
        );
    }
    if lut {
        return Ok(profile);
    }
    if profile.pcs != DataColorSpace::Xyz || !profile.is_matrix_shaper() {
        return Err("RGB matrix/TRC profiles require XYZ connection space".into());
    }
    let (r, g, b) = (
        profile.red_colorant,
        profile.green_colorant,
        profile.blue_colorant,
    );
    let determinant = r.x * (g.y * b.z - g.z * b.y) - g.x * (r.y * b.z - r.z * b.y)
        + b.x * (r.y * g.z - r.z * g.y);
    if !determinant.is_finite() || determinant.abs() < 1e-8 {
        return Err("ICC colorant matrix is singular".into());
    }
    Ok(profile)
}

fn transforms(bytes: &[u8]) -> Result<Arc<Transforms>, String> {
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err("ICC profile exceeds the 4 MiB limit".into());
    }
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    if let Some(index) = cache.iter().position(|(key, _)| key == bytes) {
        let entry = cache.remove(index).unwrap();
        let result = entry.1.clone();
        cache.push_back(entry);
        return result;
    }
    let result = (|| {
        let source = parse(bytes)?;
        let target = ColorProfile::new_srgb();
        let options = TransformOptions {
            rendering_intent: if source.lut_a_to_b_colorimetric.is_none()
                && source.lut_a_to_b_perceptual.is_some()
            {
                RenderingIntent::Perceptual
            } else {
                RenderingIntent::RelativeColorimetric
            },
            prefer_fixed_point: false,
            allow_use_cicp_transfer: false,
            ..Default::default()
        };
        Ok(Arc::new(Transforms {
            bytes: source
                .create_transform_8bit(Layout::Rgba, &target, Layout::Rgba, options)
                .map_err(|e| e.to_string())?,
            words: source
                .create_transform_16bit(Layout::Rgba, &target, Layout::Rgba, options)
                .map_err(|e| e.to_string())?,
        }))
    })();
    // Presentation cache only. Full byte keys avoid collisions; four entries
    // bound profile storage to 16 MiB and keep tables out of history/recovery.
    if cache.len() == 4 {
        cache.pop_front();
    }
    cache.push_back((bytes.to_vec(), result.clone()));
    result
}

pub fn supported(bytes: &[u8]) -> Result<(), String> {
    transforms(bytes).map(|_| ())
}
pub fn convert8(profile: &[u8], pixels: &mut [u8]) -> Result<(), String> {
    if pixels.len() % 4 != 0 {
        return Err("Invalid RGBA sample length".into());
    }
    let transform = transforms(profile)?;
    let mut output = vec![0; pixels.len()];
    transform
        .bytes
        .transform(pixels, &mut output)
        .map_err(|e| e.to_string())?;
    for (source, target) in pixels.chunks_exact(4).zip(output.chunks_exact_mut(4)) {
        target[3] = source[3];
    }
    pixels.copy_from_slice(&output);
    Ok(())
}
pub fn convert16(profile: &[u8], pixels: &mut [u16]) -> Result<(), String> {
    if pixels.len() % 4 != 0 {
        return Err("Invalid RGBA sample length".into());
    }
    let transform = transforms(profile)?;
    let mut output = vec![0; pixels.len()];
    transform
        .words
        .transform(pixels, &mut output)
        .map_err(|e| e.to_string())?;
    for (source, target) in pixels.chunks_exact(4).zip(output.chunks_exact_mut(4)) {
        target[3] = source[3];
    }
    pixels.copy_from_slice(&output);
    Ok(())
}
pub fn summary(profile: Option<&[u8]>) -> serde_json::Value {
    match profile {
        None => {
            serde_json::json!({"source":"sRGB", "preview":"sRGB", "conversion_available":false})
        }
        Some(bytes) => match supported(bytes) {
            Ok(()) => {
                serde_json::json!({"source":"embedded RGB ICC", "preview":"sRGB", "conversion_available":true})
            }
            Err(reason) => {
                serde_json::json!({"source":"embedded ICC", "preview":"unmanaged", "conversion_available":false, "reason":reason})
            }
        },
    }
}
