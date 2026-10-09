//! Bounded RGB matrix/TRC ICC conversion. Source samples stay in their profile;
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

fn parse(bytes: &[u8]) -> Result<ColorProfile, String> {
    if bytes.len() > MAX_PROFILE_BYTES || bytes.len() < 132 {
        return Err("ICC profile exceeds the supported size or is truncated".into());
    }
    if number(bytes, 0)? != bytes.len() || &bytes[36..40] != b"acsp" {
        return Err("Invalid ICC profile header or size".into());
    }
    if !matches!(bytes[8], 2 | 4) || &bytes[16..20] != b"RGB " || &bytes[20..24] != b"XYZ " {
        return Err(
            "Only ICC v2/v4 RGB matrix profiles with XYZ connection space are supported".into(),
        );
    }
    let count = number(bytes, 128)?;
    if count > 128 || 132 + count * 12 > bytes.len() {
        return Err("Invalid or excessive ICC tag table".into());
    }
    let mut tags = std::collections::HashSet::new();
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
            b"A2B0"
                | b"A2B1"
                | b"A2B2"
                | b"B2A0"
                | b"B2A1"
                | b"B2A2"
                | b"D2B0"
                | b"D2B1"
                | b"D2B2"
                | b"D2B3"
                | b"B2D0"
                | b"B2D1"
                | b"B2D2"
                | b"B2D3"
        ) {
            return Err("LUT color profiles are not supported".into());
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
    if [
        *b"rXYZ", *b"gXYZ", *b"bXYZ", *b"rTRC", *b"gTRC", *b"bTRC", *b"wtpt",
    ]
    .iter()
    .any(|tag| !tags.contains(tag))
    {
        return Err(
            "RGB matrix profiles require three colorants, three TRCs and a white point".into(),
        );
    }
    let profile = ColorProfile::new_from_slice_with_options(
        bytes,
        moxcms::ParsingOptions {
            max_profile_size: MAX_PROFILE_BYTES + 1,
            max_allowed_clut_size: 0,
            max_allowed_trc_size: 65536,
        },
    )
    .map_err(|e| format!("Invalid ICC profile: {e}"))?;
    if profile.color_space != DataColorSpace::Rgb
        || profile.pcs != DataColorSpace::Xyz
        || !matches!(
            profile.profile_class,
            ProfileClass::InputDevice
                | ProfileClass::DisplayDevice
                | ProfileClass::OutputDevice
                | ProfileClass::ColorSpace
        )
        || !profile.is_matrix_shaper()
    {
        return Err("Only RGB matrix/TRC color profiles can be converted".into());
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
            rendering_intent: RenderingIntent::RelativeColorimetric,
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
