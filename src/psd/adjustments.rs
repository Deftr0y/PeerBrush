//! Standard Photoshop adjustment definitions shared by both save paths.
use crate::{effects::{self, black_white, channel_mixer, curves, exposure, gradient_map, legacy_brightness, modern_brightness, photo_filter, photoshop_color_balance, photoshop_hue_saturation, selective_color, vibrance}, engine::view::LayerView};
use serde_json::{Value, json};
pub(super) const LEGACY_BRIGHTNESS_FORMAT: u32 = 78;
pub(super) const MODERN_BRIGHTNESS_FORMAT: u32 = 79;
pub(super) const EXPOSURE_FORMAT: u32 = 80;
pub(super) const CHANNEL_MIXER_FORMAT: u32 = 81;
pub(super) const BLACK_WHITE_FORMAT: u32 = 82;
pub(super) const SELECTIVE_COLOR_FORMAT: u32 = 83;
pub(super) const VIBRANCE_FORMAT: u32 = 84;
pub(super) const PHOTO_FILTER_FORMAT: u32 = 85;
pub(super) const GRADIENT_MAP_FORMAT: u32 = 86;
pub(super) const COLOR_BALANCE_FORMAT: u32 = 87;
pub(super) const HUE_SATURATION_FORMAT: u32 = 88;
pub(super) const TONE_PRESET_FORMAT: u32 = 89;
pub(super) const COLOR_LOOKUP_FORMAT: u32 = 90;
pub(super) const CUBE_LOOKUP_FORMAT: u32 = 91;
pub(super) const TABLE_LOOKUP_FORMAT: u32 = 92;
pub(super) const COMPILED_LOOKUP_FORMAT: u32 = 93;
pub(super) const LOOK_LOOKUP_FORMAT: u32 = 94;
pub(super) const MAX_COLOR_LOOKUP_RECORD: usize =
    effects::color_lookup::MAX_SOURCE_BYTES + 4 * effects::color_lookup::MAX_NAME + 512;
pub(super) const MAX_ADJUSTMENT_DESCRIPTOR_BYTES: usize = 384 + effects::MAX_ADJUSTMENT_PRESET_NAME * 2;
#[cfg(test)]
#[path = "levels_tests.rs"]
mod levels_tests;

pub(crate) fn standard_solid(layer: LayerView<'_>, width: u32, height: u32) -> Option<crate::raster::Pixel> {
    let color = layer.color();
    (layer.kind() == "fill"
        && color[3] == 255
        && layer.effect_count() == 0
        && layer.origin() == [0, 0]
        && layer.pixels().width == width
        && layer.pixels().height == height)
        .then_some(color)
}

const SOLID_HEADER: &[u8] = b"\0\0\0\x10\0\0\0\0\0\0\0\0null\0\0\0\x01\0\0\0\0Clr Objc";
const RGB_HEADER: &[u8] = b"\0\0\0\0\0\0\0\0RGBC\0\0\0\x03";
pub(super) const RGB_DESCRIPTOR_BYTES: usize = RGB_HEADER.len() + 3 * 20;
const SOLID_BYTES: usize = SOLID_HEADER.len() + RGB_DESCRIPTOR_BYTES;

pub(super) fn solid_descriptor(color: crate::raster::Pixel) -> [u8; SOLID_BYTES] {
    let mut descriptor = [0; SOLID_BYTES];
    descriptor[..SOLID_HEADER.len()].copy_from_slice(SOLID_HEADER);
    let [r, g, b, _] = color;
    descriptor[SOLID_HEADER.len()..].copy_from_slice(&rgb_descriptor([r, g, b].map(f64::from)));
    descriptor
}

pub(super) fn rgb_descriptor(color: [f64; 3]) -> [u8; RGB_DESCRIPTOR_BYTES] {
    let mut descriptor = [0; RGB_DESCRIPTOR_BYTES];
    descriptor[..RGB_HEADER.len()].copy_from_slice(RGB_HEADER);
    for (item, (key, value)) in descriptor[RGB_HEADER.len()..]
        .chunks_exact_mut(20)
        .zip([b"Rd  ", b"Grn ", b"Bl  "].into_iter().zip(color))
    {
        item[4..8].copy_from_slice(key);
        item[8..12].copy_from_slice(b"doub");
        item[12..].copy_from_slice(&value.to_be_bytes());
    }
    descriptor
}

pub(crate) fn standard_invert(layer: LayerView<'_>, width: u32, height: u32, clipped: bool) -> bool {
    standard_layer(layer, width, height, clipped)
        && layer.effect(0).is_some_and(|effect| {
            effect.kind() == "invert"
                && effect.enabled()
                && effect.weight() == 1.
                && effect.settings().as_object().is_some_and(|s| s.is_empty())
        })
}

fn standard_layer(layer: LayerView<'_>, width: u32, height: u32, clipped: bool) -> bool {
    layer.kind() == "adjustment"
        && layer.blend() != "pass_through"
        && super::scene_save36::blend(layer.blend()).is_ok()
        && (clipped || layer.clip_to().is_none())
        && layer.source().is_none()
        && layer.origin() == [0, 0]
        && layer.pixels().width == width
        && layer.pixels().height == height
        && layer.effect_count() == 1
}

pub(crate) fn standard_levels(layer: LayerView<'_>, width: u32, height: u32, clipped: bool) -> Option<[u8; 292]> {
    if !standard_layer(layer, width, height, clipped) { return None; }
    let effect = layer.effect(0)?;
    if effect.kind() != "levels" || !effect.enabled() || effect.weight() != 1. { return None; }
    levels_descriptor(effect.settings())
}

pub(crate) enum ToneRecord {
    Levels([u8; 292], Option<Vec<u8>>),
    Curves(Vec<u8>, Option<Vec<u8>>),
    Posterize([u8; 4]),
    Threshold([u8; 4]),
    LegacyBrightness([u8; 8]),
    ModernBrightness(Vec<u8>),
    Exposure([u8; 16], Option<Vec<u8>>),
    ChannelMixer([u8; 44], Option<Vec<u8>>),
    BlackWhite(Vec<u8>),
    SelectiveColor([u8; 84]),
    Vibrance([u8; 60], usize),
    PhotoFilter([u8; 20]),
    GradientMap(Vec<u8>),
    ColorBalance([u8; 20]),
    HueSaturation([u8; 136], usize, Option<Vec<u8>>),
    ColorLookup(Vec<u8>),
}

pub(super) fn is_tone_tag(key: &[u8]) -> bool {
    matches!(key, b"levl" | b"curv" | b"post" | b"thrs" | b"brit" | b"CgEd" | b"expA" | b"mixr" | b"blwh" | b"selc" | b"vibA" | b"phfl" | b"grdm" | b"blnc" | b"hue2" | b"hue " | b"clrL")
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CurveRecords {
    Off,
    FullRange,
    Shifted,
    Photoshop,
    Combined,
}

impl CurveRecords {
    fn current(depth: u16) -> Self {
        if depth == 8 { Self::Combined } else { Self::Shifted }
    }

    fn for_source_format(format: u32, depth: u16) -> Self {
        match format {
            59.. => Self::current(depth),
            58 if depth == 8 => Self::Photoshop,
            54.. => Self::Shifted,
            52..=53 => Self::FullRange,
            _ => Self::Off,
        }
    }

    pub(super) fn from_settings(settings: &serde_json::Value) -> Self {
        if curves::byte_composition("curves", settings) {
            Self::Combined
        } else if curves::uses_photoshop("curves", settings) {
            Self::Photoshop
        } else if shifted_curve(settings) {
            Self::Shifted
        } else {
            Self::FullRange
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ToneRecords {
    pub(super) levels: bool,
    pub(super) combined_levels: bool,
    pub(super) curves: CurveRecords,
    pub(super) posterize_depth: Option<u16>,
    pub(super) threshold: bool,
    pub(super) legacy_brightness: bool,
    pub(super) modern_brightness: bool,
    pub(super) exposure: bool,
    pub(super) channel_mixer: bool,
    pub(super) black_white: bool,
    pub(super) selective_color: bool,
    pub(super) vibrance: bool,
    pub(super) photo_filter: bool,
    pub(super) gradient_map: bool,
    pub(super) color_balance: bool,
    pub(super) hue_saturation: bool,
    pub(super) tone_presets: bool,
    pub(super) color_lookup: bool,
    pub(super) cube_lookup: bool,
}

impl ToneRecords {
    pub(crate) fn current(depth: u16) -> Self {
        Self { levels: true, combined_levels: depth == 8, curves: CurveRecords::current(depth),
            posterize_depth: Some(depth), threshold: true, legacy_brightness: depth == 8,
            modern_brightness: depth == 8, exposure: depth == 8, channel_mixer: depth == 8,
            black_white: depth == 8, selective_color: depth == 8, vibrance: depth == 8,
            photo_filter: depth == 8, gradient_map: depth == 8, color_balance: depth == 8,
            hue_saturation: depth == 8, tone_presets: true, color_lookup: true, cube_lookup: true }
    }

    pub(super) fn for_source_format(format: u32, depth: u16) -> Self {
        Self {
            levels: format >= 48,
            combined_levels: format >= 61 && depth == 8,
            curves: CurveRecords::for_source_format(format, depth),
            posterize_depth: (format >= 56 || format >= 55 && depth == 8).then_some(depth),
            threshold: format >= 57,
            legacy_brightness: format >= LEGACY_BRIGHTNESS_FORMAT && depth == 8,
            modern_brightness: format >= MODERN_BRIGHTNESS_FORMAT && depth == 8,
            exposure: format >= EXPOSURE_FORMAT && depth == 8,
            channel_mixer: format >= CHANNEL_MIXER_FORMAT && depth == 8,
            black_white: format >= BLACK_WHITE_FORMAT && depth == 8,
            selective_color: format >= SELECTIVE_COLOR_FORMAT && depth == 8,
            vibrance: format >= VIBRANCE_FORMAT && depth == 8,
            photo_filter: format >= PHOTO_FILTER_FORMAT && depth == 8,
            gradient_map: format >= GRADIENT_MAP_FORMAT && depth == 8,
            color_balance: format >= COLOR_BALANCE_FORMAT && depth == 8,
            hue_saturation: format >= HUE_SATURATION_FORMAT && depth == 8,
            tone_presets: format >= TONE_PRESET_FORMAT,
            color_lookup: format >= COLOR_LOOKUP_FORMAT,
            cube_lookup: format >= CUBE_LOOKUP_FORMAT,
        }
    }
}

impl ToneRecord {
    pub(super) fn parts(&self) -> ([u8; 4], &[u8]) {
        match self {
            Self::Levels(bytes, _) => (*b"levl", bytes),
            Self::Curves(bytes, _) => (*b"curv", bytes),
            Self::Posterize(bytes) => (*b"post", bytes),
            Self::Threshold(bytes) => (*b"thrs", bytes),
            Self::LegacyBrightness(bytes) => (*b"brit", bytes),
            Self::ModernBrightness(bytes) => (*b"CgEd", bytes),
            Self::Exposure(bytes, _) => (*b"expA", bytes),
            Self::ChannelMixer(bytes, _) => (*b"mixr", bytes),
            Self::BlackWhite(bytes) => (*b"blwh", bytes),
            Self::SelectiveColor(bytes) => (*b"selc", bytes),
            Self::Vibrance(bytes, len) => (*b"vibA", &bytes[..*len]),
            Self::PhotoFilter(bytes) => (*b"phfl", bytes),
            Self::GradientMap(bytes) => (*b"grdm", bytes),
            Self::ColorBalance(bytes) => (*b"blnc", bytes),
            Self::HueSaturation(bytes, len, _) => (*b"hue2", &bytes[..*len]),
            Self::ColorLookup(bytes) => (*b"clrL", bytes),
        }
    }
    pub(super) fn companion(&self) -> Option<([u8; 4], &[u8])> {
        match self {
            Self::ModernBrightness(_) => Some((*b"brit", &[0; 8])),
            Self::Levels(_, Some(preset)) | Self::Curves(_, Some(preset))
            | Self::Exposure(_, Some(preset)) | Self::ChannelMixer(_, Some(preset))
            | Self::HueSaturation(_, _, Some(preset)) => Some((*b"CgEd", preset)),
            _ => None,
        }
    }
    pub(super) fn valid(&self) -> bool {
        match self {
            Self::Levels(bytes, preset) => levels_settings(bytes).is_ok()
                && valid_preset(preset.as_deref(), PresetKind::Generic),
            Self::Curves(bytes, preset) => curves_settings(bytes).is_ok()
                && valid_preset(preset.as_deref(), PresetKind::Curves),
            Self::Posterize(bytes) => posterize_settings(bytes).is_ok(),
            Self::Threshold(bytes) => threshold_settings(bytes).is_ok(),
            Self::LegacyBrightness(bytes) => brightness_record(bytes)
                .and_then(|settings| legacy_brightness::Mapping::new(&settings)).is_ok(),
            Self::ModernBrightness(bytes) => matches!(tone_descriptor(bytes),
                Ok(ToneDescriptor::Brightness(modern_brightness::KIND, _))),
            Self::Exposure(bytes, preset) => exposure_settings(bytes).is_ok()
                && valid_preset(preset.as_deref(), PresetKind::Generic),
            Self::ChannelMixer(bytes, preset) => channel_mixer_settings(bytes).is_ok()
                && valid_preset(preset.as_deref(), PresetKind::Mixer),
            Self::BlackWhite(bytes) => black_white_settings(bytes).is_ok(),
            Self::SelectiveColor(bytes) => selective_color_settings(bytes).is_ok(),
            Self::Vibrance(bytes, len) => bytes.get(..*len).is_some_and(|bytes| vibrance_settings(bytes).is_ok()),
            Self::PhotoFilter(bytes) => photo_filter_settings(bytes).is_ok(),
            Self::GradientMap(bytes) => super::gradient_map::settings(bytes).is_ok(),
            Self::ColorBalance(bytes) => color_balance_settings(bytes).is_ok(),
            Self::HueSaturation(bytes, len, preset) => bytes.get(..*len)
                .is_some_and(|bytes| hue_saturation_settings(bytes).is_ok())
                && valid_preset(preset.as_deref(), PresetKind::Generic),
            Self::ColorLookup(bytes) => color_lookup_settings(bytes).is_ok(),
        }
    }
}

pub(crate) fn standard_tone(
    layer: LayerView<'_>,
    width: u32,
    height: u32,
    clipped: bool,
    records: ToneRecords,
) -> Option<ToneRecord> {
    if !standard_layer(layer, width, height, clipped) {
        return None;
    }
    let effect = layer.effect(0)?;
    if !effect.enabled() || effect.weight() != 1. {
        return None;
    }
    let preset = |kind: PresetKind| {
        let settings = effects::adjustment_preset(effect.settings()).ok()?;
        Some(settings.map(|settings| write_adjustment_preset(kind, settings)))
    };
    if effects::supports_tone_preset(effect.kind()) && effect.settings().get("preset").is_some()
        && !records.tone_presets
    {
        return None;
    }
    match effect.kind() {
        "levels" if records.levels && (records.combined_levels
            || !crate::effects::levels::byte_composition("levels", effect.settings())) => {
            Some(ToneRecord::Levels(levels_descriptor(effect.settings())?, preset(PresetKind::Generic)?))
        }
        "curves" if records.curves >= CurveRecords::from_settings(effect.settings()) => {
            Some(ToneRecord::Curves(curves_descriptor(effect.settings())?, preset(PresetKind::Curves)?))
        }
        "posterize" => {
            posterize_descriptor(effect.settings(), records.posterize_depth?).map(ToneRecord::Posterize)
        }
        "threshold" if records.threshold => {
            let level = crate::effects::threshold::level(effect.settings()).ok()?;
            Some(ToneRecord::Threshold([0, level, 0, 0]))
        }
        legacy_brightness::KIND if records.legacy_brightness => {
            let mapping = legacy_brightness::Mapping::new(effect.settings()).ok()?;
            let mut bytes = [0; 8];
            bytes[..2].copy_from_slice(&mapping.brightness.to_be_bytes());
            bytes[2..4].copy_from_slice(&mapping.contrast.to_be_bytes());
            bytes[4..6].copy_from_slice(&mapping.mean.to_be_bytes());
            Some(ToneRecord::LegacyBrightness(bytes))
        }
        modern_brightness::KIND if records.modern_brightness => {
            let mapping = modern_brightness::parameters(effect.settings()).ok()?;
            Some(ToneRecord::ModernBrightness(write_brightness_descriptor(mapping)))
        }
        exposure::KIND if records.exposure => {
            let mut bytes = [0; 16];
            bytes[1] = 1;
            for (field, value) in bytes[2..14].chunks_exact_mut(4).zip(exposure::parameters(effect.settings()).ok()?) {
                field.copy_from_slice(&value.to_be_bytes());
            }
            Some(ToneRecord::Exposure(bytes, preset(PresetKind::Generic)?))
        }
        channel_mixer::KIND if records.channel_mixer => {
            let mixer = channel_mixer::Mixer::new(effect.settings()).ok()?;
            Some(ToneRecord::ChannelMixer(channel_mixer_descriptor(&mixer), preset(PresetKind::Mixer)?))
        }
        black_white::KIND if records.black_white => {
            let settings = black_white::Settings::new(effect.settings()).ok()?;
            let preset = effects::adjustment_preset(effect.settings()).ok()?.unwrap_or((0, "\0"));
            Some(ToneRecord::BlackWhite(black_white_descriptor(&settings, preset)))
        }
        selective_color::KIND if records.selective_color => {
            let settings = selective_color::Settings::new(effect.settings()).ok()?;
            let mut bytes = [0; 84];
            bytes[1] = 1;
            bytes[3] = u8::from(settings.absolute);
            for (field, value) in bytes[12..].chunks_exact_mut(2).zip(settings.ranges.into_iter().flatten()) {
                field.copy_from_slice(&value.to_be_bytes());
            }
            Some(ToneRecord::SelectiveColor(bytes))
        }
        vibrance::KIND if records.vibrance => {
            let parameters = vibrance::parameters(effect.settings()).ok()?;
            Some(vibrance_descriptor(parameters))
        }
        photo_filter::KIND if records.photo_filter => {
            let settings = photo_filter::Settings::new(effect.settings()).ok()?;
            Some(ToneRecord::PhotoFilter(photo_filter_descriptor(settings)))
        }
        gradient_map::KIND if records.gradient_map => {
            let settings = gradient_map::Settings::new(effect.settings()).ok()?;
            Some(ToneRecord::GradientMap(super::gradient_map::descriptor(&settings)))
        }
        photoshop_color_balance::KIND if records.color_balance => {
            let settings = photoshop_color_balance::Settings::new(effect.settings()).ok()?;
            let mut bytes = [0; 20];
            for (wire, value) in bytes[..18].chunks_exact_mut(2).zip(settings.bands.into_iter().flatten()) {
                wire.copy_from_slice(&value.to_be_bytes());
            }
            bytes[18] = u8::from(settings.preserve_luminosity);
            Some(ToneRecord::ColorBalance(bytes))
        }
        photoshop_hue_saturation::KIND if records.hue_saturation => {
            let settings = photoshop_hue_saturation::Settings::new(effect.settings()).ok()?;
            let (bytes, len) = hue_saturation_record(&settings);
            Some(ToneRecord::HueSaturation(bytes, len, preset(PresetKind::Generic)?))
        }
        effects::color_lookup::KIND if records.color_lookup
            && (records.cube_lookup || !effects::color_lookup::uses_table(effect.settings())) =>
            color_lookup_record(effect.settings()).map(ToneRecord::ColorLookup),
        _ => None,
    }
}

fn color_lookup_record(settings: &Value) -> Option<Vec<u8>> {
    use effects::color_lookup;
    let source = color_lookup::prepare(settings).ok()??;
    let name = color_lookup::text(settings, "name").ok()?;
    let file_name = color_lookup::text(settings, "file_name").ok()?;
    let data_order = color_lookup::text(settings, "data_order").ok()?;
    let table_order = color_lookup::text(settings, "table_order").ok()?;
    let cube = !source.cube.is_empty();
    let compiled = settings["embed_profile"] == true;
    let profile = if compiled { source.compiled_profile()? } else { &source.profile };
    let has_profile = !profile.is_empty();
    let count = 3u32 + u32::from(has_profile) + 2 * u32::from(cube)
        + [file_name, data_order, table_order].iter().filter(|value| !value.is_empty()).count() as u32;
    let mut bytes = Vec::with_capacity(profile.len() + source.cube.len()
        + 2 * (name.encode_utf16().count() + file_name.encode_utf16().count()) + 512);
    bytes.extend(b"\0\x01\0\0\0\x10\0\0\0\0\0\0\0\0null");
    bytes.extend(count.to_be_bytes());
    lookup_enum(&mut bytes, "lookupType", "colorLookupType", if cube { "3DLUT" } else { "deviceLinkProfile" });
    descriptor_item(&mut bytes, "Nm  ", b"TEXT");
    write_descriptor_text(&mut bytes, name);
    descriptor_item(&mut bytes, "Dthr", b"bool");
    bytes.push(0);
    if has_profile { lookup_blob(&mut bytes, "profile", profile); }
    if cube {
        let format = match source.format {
            color_lookup::LutFormat::Cube => "LUTFormatCUBE",
            color_lookup::LutFormat::ThreeDl => "LUTFormat3DL",
            color_lookup::LutFormat::Look => "LUTFormatLOOK",
        };
        lookup_enum(&mut bytes, "LUTFormat", "LUTFormatType", format);
        lookup_blob(&mut bytes, "LUT3DFileData", &source.cube);
        if !file_name.is_empty() {
            descriptor_item(&mut bytes, "LUT3DFileName", b"TEXT");
            write_descriptor_text(&mut bytes, file_name);
        }
        for (field, order) in [("dataOrder", data_order), ("tableOrder", table_order)] {
            if !order.is_empty() {
                lookup_enum(&mut bytes, field, "colorLookupOrder", if order == "bgr" { "bgrOrder" } else { "rgbOrder" });
            }
        }
    }
    // Photoshop's receiving path can require these bytes inside the declared
    // clrL payload. The outer tag's alignment byte does not count.
    if compiled { bytes.extend([0, 0]); }
    Some(bytes)
}

fn lookup_enum(bytes: &mut Vec<u8>, key: &str, class: &str, value: &str) {
    descriptor_item(bytes, key, b"enum");
    for id in [class, value] {
        bytes.extend((id.len() as u32).to_be_bytes());
        bytes.extend(id.as_bytes());
    }
}

fn lookup_blob(bytes: &mut Vec<u8>, key: &str, data: &[u8]) {
    descriptor_item(bytes, key, b"tdta");
    bytes.extend((data.len() as u32).to_be_bytes());
    bytes.extend(data);
}

pub(super) fn color_lookup_settings(payload: &[u8]) -> Result<Value, String> {
    use effects::color_lookup::{self, LutFormat};
    if payload.len() > MAX_COLOR_LOOKUP_RECORD || !payload.starts_with(&[0, 1]) {
        return Err("Unsupported Photoshop Color Lookup record version or size".into());
    }
    let body = &payload[2..];
    let root = super::styles::descriptor_blob_offsets(body, MAX_COLOR_LOOKUP_RECORD)?;
    let items = root["items"].as_object().unwrap();
    let is_enum = |key: &str, class, variant| {
        root["items"][key]["type"] == "enum" && root["items"][key]["value"] == json!([class, variant])
    };
    let cube = is_enum("lookupType", "colorLookupType", "3DLUT");
    let link = is_enum("lookupType", "colorLookupType", "deviceLinkProfile");
    if !matches!(root["class"].as_str(), Some("null" | "colorLookup"))
        || (!link && !cube)
        || items.keys().any(|key| {
            !matches!(key.as_str(), "lookupType" | "Nm  " | "Dthr" | "profile")
                && !(cube && matches!(key.as_str(), "LUTFormat" | "LUT3DFileData" | "LUT3DFileName" | "dataOrder" | "tableOrder"))
        })
        || items.get("Dthr").is_some_and(|item| item["type"] != "bool" || item["value"] != false)
    {
        return Err("Unsupported Photoshop Color Lookup mode, fields or dither".into());
    }
    let text = |key: &str| items.get(key).map(|item| {
        (item["type"] == "TEXT").then(|| item["value"].as_str()).flatten()
            .ok_or("Invalid Photoshop Color Lookup filename")
    }).transpose().map(|value| value.unwrap_or(""));
    let blob = |key: &str| -> Result<&[u8], String> {
        let Some(item) = items.get(key) else { return Ok(&[]); };
        if item["type"] != "tdta" { return Err("Invalid Photoshop Color Lookup source type".into()); }
        let offset = item["value"]["offset"].as_u64().and_then(|n| usize::try_from(n).ok())
            .ok_or("Invalid Photoshop Color Lookup source offset")?;
        let size = item["value"]["bytes"].as_u64().and_then(|n| usize::try_from(n).ok())
            .ok_or("Invalid Photoshop Color Lookup source length")?;
        offset.checked_add(size).and_then(|end| body.get(offset..end)).filter(|source| !source.is_empty())
            .ok_or_else(|| "Empty or truncated Photoshop Color Lookup source".into())
    };
    let profile = blob("profile")?;
    let name = text("Nm  ")?;
    if link { return color_lookup::from_profile(profile, name); }
    let format = if is_enum("LUTFormat", "LUTFormatType", "LUTFormatCUBE") {
        LutFormat::Cube
    } else if is_enum("LUTFormat", "LUTFormatType", "LUTFormat3DL") {
        LutFormat::ThreeDl
    } else if is_enum("LUTFormat", "LUTFormatType", "LUTFormatLOOK") {
        LutFormat::Look
    } else {
        return Err("Photoshop Color Lookup requires an embedded .cube, .3dl or .look table".into());
    };
    let mut settings = color_lookup::from_table(format, blob("LUT3DFileData")?, profile, name, text("LUT3DFileName")?)?;
    for (field, key) in [("dataOrder", "data_order"), ("tableOrder", "table_order")] {
        if items.contains_key(field) {
            let value = if is_enum(field, "colorLookupOrder", "rgbOrder") { "rgb" }
                else if is_enum(field, "colorLookupOrder", "bgrOrder") { "bgr" }
                else {
                return Err("Unsupported Photoshop Color Lookup channel order".into());
                };
            settings[key] = json!(value);
        }
    }
    if crate::color_profile::device_link_mpe::is_peerbrush_profile(profile) {
        let mut generated = settings.clone();
        generated["profile"] = json!("");
        generated["embed_profile"] = json!(true);
        if color_lookup::prepare(&generated)?.and_then(|source| {
            source.compiled_profile().map(|compiled| compiled == profile)
        }) == Some(true)
        {
            return Ok(generated);
        }
    }
    Ok(settings)
}

fn hue_saturation_record(settings: &photoshop_hue_saturation::Settings) -> ([u8; 136], usize) {
    let mut bytes = [0; 136];
    bytes[..4].copy_from_slice(&[0, 2, u8::from(settings.colorize), 0]);
    let values = settings.colorization.into_iter().chain(settings.master)
        .chain(settings.bands.iter().flat_map(|band| band.range.into_iter().chain(band.change)));
    for (field, value) in bytes[4..100].chunks_exact_mut(2).zip(values) {
        field.copy_from_slice(&value.to_be_bytes());
    }
    let len = 100 + settings.trailer.len();
    bytes[100..len].copy_from_slice(&settings.trailer);
    (bytes, len)
}

pub(super) fn hue_saturation_settings(payload: &[u8]) -> Result<Value, String> {
    if !matches!(payload.len(), 100 | 136) || !matches!(payload[..4], [0, 2, 0 | 1, 0]) {
        return Err("Invalid or unsupported hue2 record".into());
    }
    let mut words = payload[4..100].chunks_exact(2).map(|word| i16::from_be_bytes([word[0], word[1]]));
    let mut triple = || std::array::from_fn(|_| words.next().unwrap());
    let colorization = triple();
    let master = triple();
    let bands = std::array::from_fn(|_| photoshop_hue_saturation::Band {
        range: std::array::from_fn(|_| words.next().unwrap()),
        change: std::array::from_fn(|_| words.next().unwrap()),
    });
    let settings = photoshop_hue_saturation::Settings {
        master, colorize: payload[2] == 1, colorization, bands, trailer: payload[100..].to_vec(), preset: None,
    };
    settings.validate()?;
    Ok(settings.value())
}

pub(super) fn color_balance_settings(payload: &[u8]) -> Result<Value, String> {
    if !matches!(payload.len(), 19 | 20) || payload[18] > 1
        || payload[19..].iter().any(|&value| value != 0)
    {
        return Err("Invalid Photoshop Color Balance length, luminosity flag or padding".into());
    }
    let mut settings = photoshop_color_balance::Settings { bands: [[0; 3]; 3], preserve_luminosity: payload[18] == 1 };
    for (axis, wire) in settings.bands.iter_mut().flatten().zip(payload[..18].chunks_exact(2)) {
        *axis = i16::from_be_bytes([wire[0], wire[1]]);
        if !(-100..=100).contains(axis) {
            return Err("Photoshop Color Balance axis exceeds -100 to 100".into());
        }
    }
    Ok(settings.value())
}

fn photo_filter_descriptor(settings: photo_filter::Settings) -> [u8; 20] {
    use photo_filter::Color;
    let mut bytes = [0; 20];
    let control = match settings.color {
        Color::Xyz(xyz) => {
            bytes[1] = 3;
            for (wire, value) in bytes[2..14].chunks_exact_mut(4).zip(xyz) {
                wire.copy_from_slice(&value.to_be_bytes());
            }
            14
        }
        Color::Lab(_) | Color::Rgb(_) => {
            bytes[1] = 2;
            let channels = match settings.color {
                Color::Lab(lab) => {
                    bytes[3] = 7;
                    lab.map(|value| value as u16)
                }
                Color::Rgb(rgb) => rgb,
                Color::Xyz(_) => unreachable!(),
            };
            for (wire, value) in bytes[4..10].chunks_exact_mut(2).zip(channels) {
                wire.copy_from_slice(&value.to_be_bytes());
            }
            12
        }
    };
    bytes[control..control + 4].copy_from_slice(&u32::from(settings.density).to_be_bytes());
    bytes[control + 4] = u8::from(settings.preserve_luminosity);
    bytes
}

pub(super) fn photo_filter_settings(payload: &[u8]) -> Result<Value, String> {
    use photo_filter::Color;
    let (color, control) = match (payload.get(..2), payload.len()) {
        (Some([0, 2]), 17..=20) if payload[10..12] == [0; 2] => {
            let channels = std::array::from_fn(|index| {
                let start = 4 + index * 2;
                u16::from_be_bytes([payload[start], payload[start + 1]])
            });
            let color = match payload[2..4] {
                [0, 0] => Color::Rgb(channels),
                [0, 7] => Color::Lab(channels.map(|value| value as i16)),
                _ => return Err("Unsupported Photo Filter color space".into()),
            };
            (color, 12)
        }
        (Some([0, 3]), 19..=20) => {
            let xyz = std::array::from_fn(|index| {
                let start = 2 + index * 4;
                u32::from_be_bytes(payload[start..start + 4].try_into().unwrap())
            });
            (Color::Xyz(xyz), 14)
        }
        _ => return Err("Unsupported Photo Filter version, color record or length".into()),
    };
    let density = u32::from_be_bytes(payload[control..control + 4].try_into().unwrap());
    let preserve = payload[control + 4];
    if density > 100 || preserve > 1 || payload[control + 5..].iter().any(|&byte| byte != 0) {
        return Err("Invalid Photo Filter density, luminosity flag or padding".into());
    }
    let settings = photo_filter::Settings { color, density: density as u8, preserve_luminosity: preserve == 1 }.value();
    photo_filter::Settings::new(&settings)?;
    Ok(settings)
}

fn vibrance_descriptor(values: [i32; 2]) -> ToneRecord {
    let header = b"\0\0\0\x10\0\0\0\x01\0\0\0\0\0\0null\0\0\0\0";
    let mut bytes = [0; 60];
    let mut end = header.len();
    bytes[..end].copy_from_slice(header);
    for (field, value) in [b"\0\0\0\x08vibrancelong".as_slice(), b"\0\0\0\0Strtlong"].into_iter().zip(values) {
        if value != 0 {
            bytes[header.len() - 1] += 1;
            bytes[end..end + field.len()].copy_from_slice(field);
            end += field.len();
            bytes[end..end + 4].copy_from_slice(&value.to_be_bytes());
            end += 4;
        }
    }
    ToneRecord::Vibrance(bytes, end.next_multiple_of(4))
}

pub(super) fn vibrance_settings(payload: &[u8]) -> Result<Value, String> {
    let root = super::styles::descriptor_block(payload)?;
    let items = root["items"].as_object().unwrap();
    if items.contains_key("temperature") || items.contains_key("tint")
        || items.get("useLegacy").is_some_and(|flag| flag["type"] == "bool" && flag["value"] == false)
    {
        return Err("Photoshop Color and Vibrance uses the newer rendering model".into());
    }
    if root["class"] != "null" || items.keys().any(|key| !matches!(key.as_str(), "vibrance" | "Strt" | "useLegacy"))
        || items.get("useLegacy").is_some_and(|flag| flag["type"] != "bool" || flag["value"] != true)
    {
        return Err("Unsupported Legacy Vibrance descriptor".into());
    }
    let mut settings = [0; 2];
    for (value, key) in settings.iter_mut().zip(["vibrance", "Strt"]) {
        if let Some(item) = items.get(key) {
            *value = item["value"].as_i64().filter(|amount| item["type"] == "long" && (-100..=100).contains(amount))
                .ok_or("Invalid Legacy Vibrance slider value")? as i32;
        }
    }
    let [vibrance, saturation] = settings;
    Ok(json!({"vibrance":vibrance,"saturation":saturation}))
}

pub(super) fn selective_color_settings(payload: &[u8]) -> Result<Value, String> {
    if payload.len() != 84 || payload[..3] != [0, 1, 0] || payload[3] > 1 || payload[4..12] != [0; 8] {
        return Err("Invalid Selective Color version, method, length or reserved record".into());
    }
    let mut settings = selective_color::Settings { absolute: payload[3] == 1, ranges: [[0; 4]; 9] };
    for (field, wire) in settings.ranges.iter_mut().flatten().zip(payload[12..].chunks_exact(2)) {
        *field = i16::from_be_bytes([wire[0], wire[1]]);
        if !(-100..=100).contains(field) {
            return Err("Selective Color correction exceeds -100 to 100".into());
        }
    }
    Ok(settings.value())
}

fn channel_mixer_descriptor(mixer: &channel_mixer::Mixer) -> [u8; 44] {
    let mut bytes = [0; 44];
    bytes[1] = 1;
    bytes[3] = u8::from(mixer.monochrome);
    let rows = if mixer.monochrome { &mixer.rows[3..] } else { &mixer.rows[..3] };
    for (wire, &[red, green, blue, constant]) in bytes[4..].chunks_exact_mut(10).zip(rows) {
        for (field, value) in wire.chunks_exact_mut(2).zip([red, green, blue, 0, constant]) {
            field.copy_from_slice(&value.to_be_bytes());
        }
    }
    bytes
}

pub(super) fn channel_mixer_settings(payload: &[u8]) -> Result<Value, String> {
    if payload.len() != 44 || payload[..3] != [0, 1, 0] || payload[3] > 1 {
        return Err("Unsupported Channel Mixer version, mode or record length".into());
    }
    let monochrome = payload[3] == 1;
    let active = if monochrome { 1 } else { 3 };
    let (wire_rows, reserved) = payload[4..].split_at(active * 10);
    if reserved.iter().any(|&byte| byte != 0) {
        return Err("Unsupported data in unused Channel Mixer rows".into());
    }
    let mut rows = channel_mixer::DEFAULT_ROWS;
    let active_rows = if monochrome { &mut rows[3..] } else { &mut rows[..3] };
    for (row, wire) in active_rows.iter_mut().zip(wire_rows.chunks_exact(10)) {
        if wire[6..8] != [0, 0] {
            return Err("Unsupported non-RGB Channel Mixer coefficient".into());
        }
        for (value, position) in row.iter_mut().zip([0, 2, 4, 8]) {
            *value = i16::from_be_bytes([wire[position], wire[position + 1]]);
        }
    }
    let settings = channel_mixer::Mixer { monochrome, rows }.settings();
    channel_mixer::Mixer::new(&settings)?;
    Ok(settings)
}

pub(super) fn exposure_settings(payload: &[u8]) -> Result<Value, String> {
    if !matches!(payload.len(), 14 | 16) || payload[..2] != [0, 1]
        || payload[14..].iter().any(|&value| value != 0)
    {
        return Err("Invalid Exposure version, record length or padding".into());
    }
    let [exposure, offset, gamma] = std::array::from_fn(|index| {
        let start = 2 + 4 * index;
        f32::from_be_bytes(payload[start..start + 4].try_into().unwrap())
    });
    if ![exposure, offset, gamma].iter().all(|value| value.is_finite()) {
        return Err("Non-finite Exposure parameter".into());
    }
    let settings = json!({"exposure":exposure,"offset":offset,"gamma":gamma});
    exposure::parameters(&settings)?;
    Ok(settings)
}

pub(super) fn brightness_record(payload: &[u8]) -> Result<Value, String> {
    let ([b1, b2, c1, c2, m1, m2, 0] | [b1, b2, c1, c2, m1, m2, 0, 0]) = payload else {
        return Err("Invalid brit record or unsupported Lab-only mode".into());
    };
    Ok(json!({
        "brightness": i16::from_be_bytes([*b1, *b2]),
        "contrast": i16::from_be_bytes([*c1, *c2]),
        "mean": u16::from_be_bytes([*m1, *m2]),
    }))
}

pub(super) enum ToneDescriptor {
    Brightness(&'static str, Value),
    Preset(PresetKind, Value),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PresetKind {
    Generic,
    Curves,
    Mixer,
}

impl PresetKind {
    fn keys(self) -> (&'static str, &'static str) {
        match self {
            Self::Generic => ("presetKind", "presetFileName"),
            Self::Curves => ("curvesPresetKind", "curvesPresetFileName"),
            Self::Mixer => ("mixerPresetKind", "mixerPresetFileName"),
        }
    }

    pub(super) fn applies_to(self, adjustment: &str) -> bool {
        match self {
            Self::Generic => matches!(adjustment, "levels" | exposure::KIND | photoshop_hue_saturation::KIND),
            Self::Curves => adjustment == "curves",
            Self::Mixer => adjustment == channel_mixer::KIND,
        }
    }
}

fn valid_preset(bytes: Option<&[u8]>, kind: PresetKind) -> bool {
    bytes.is_none_or(|bytes| matches!(tone_descriptor(bytes), Ok(ToneDescriptor::Preset(actual, _)) if actual == kind))
}

pub(super) fn tone_descriptor(payload: &[u8]) -> Result<ToneDescriptor, String> {
    if payload.len() > MAX_ADJUSTMENT_DESCRIPTOR_BYTES {
        return Err("Adjustment descriptor exceeds its size limit".into());
    }
    let root = super::styles::descriptor_block(payload)?;
    let items = root["items"].as_object().unwrap();
    let long = |key: &str| items.get(key).filter(|item| item["type"] == "long")
        .and_then(|item| item["value"].as_i64());
    if root["class"] != "null" || long("Vrsn") != Some(1) {
        return Err("Unsupported adjustment descriptor class or version".into());
    }
    for preset_kind in [PresetKind::Generic, PresetKind::Curves, PresetKind::Mixer] {
        let (kind_key, name_key) = preset_kind.keys();
        if !items.contains_key(kind_key) && !items.contains_key(name_key) { continue; }
        let kind = long(kind_key).ok_or("Invalid adjustment preset kind")?;
        let name = items.get(name_key).filter(|item| item["type"] == "TEXT")
            .and_then(|item| item["value"].as_str()).ok_or("Invalid adjustment preset name")?;
        if items.len() != 3 {
            return Err("Unsupported adjustment preset fields".into());
        }
        let preset = json!({"preset":{"kind":kind,"name":name}});
        effects::adjustment_preset(&preset)?;
        return Ok(ToneDescriptor::Preset(preset_kind, preset["preset"].clone()));
    }
    let flag = |key: &str| items.get(key).filter(|item| item["type"] == "bool")
        .and_then(|item| item["value"].as_bool());
    if items.keys().any(|key| !matches!(key.as_str(),
            "Vrsn" | "Brgh" | "Cntr" | "means" | "Lab " | "useLegacy" | "Auto"))
        || (items.contains_key("Lab ") && flag("Lab ") != Some(false))
        || (items.contains_key("Auto") && flag("Auto") != Some(false))
    {
        return Err("Unsupported Brightness/Contrast descriptor, preset or mode".into());
    }
    let legacy = flag("useLegacy").ok_or("Missing or invalid Brightness/Contrast mode")?;
    let settings = json!({
        "brightness": long("Brgh").ok_or("Missing or invalid descriptor brightness")?,
        "contrast": long("Cntr").ok_or("Missing or invalid descriptor contrast")?,
        "mean": long("means").ok_or("Missing or invalid descriptor mean")?,
    });
    let kind = if legacy {
        legacy_brightness::Mapping::new(&settings)?;
        legacy_brightness::KIND
    } else {
        modern_brightness::parameters(&settings)?;
        modern_brightness::KIND
    };
    Ok(ToneDescriptor::Brightness(kind, settings))
}

pub(super) fn brightness_definition(
    record: Option<Value>,
    descriptor: Option<(&'static str, Value)>,
) -> Result<Option<(&'static str, Value)>, String> {
    if let Some((kind, settings)) = descriptor {
        // Photoshop writes a zeroed fallback; other writers repeat the settings.
        if record.is_some_and(|record| record != settings
            && record != json!({"brightness":0,"contrast":0,"mean":0}))
        {
            return Err("Conflicting brit and CgEd Brightness/Contrast settings".into());
        }
        return Ok(Some((kind, settings)));
    }
    record.map(|settings| {
        legacy_brightness::Mapping::new(&settings)?;
        Ok((legacy_brightness::KIND, settings))
    }).transpose()
}

fn write_brightness_descriptor(mapping: legacy_brightness::Mapping) -> Vec<u8> {
    let mut bytes = b"\0\0\0\x10\0\0\0\0\0\0\0\0null\0\0\0\x07".to_vec();
    for (key, value) in [
        (b"Vrsn", 1),
        (b"Brgh", i32::from(mapping.brightness)),
        (b"Cntr", i32::from(mapping.contrast)),
    ] {
        bytes.extend(0u32.to_be_bytes());
        bytes.extend(key);
        bytes.extend(b"long");
        bytes.extend(value.to_be_bytes());
    }
    bytes.extend(b"\0\0\0\x05meanslong");
    bytes.extend(i32::from(mapping.mean).to_be_bytes());
    bytes.extend(b"\0\0\0\0Lab bool\0\0\0\0\x09useLegacybool\0\0\0\0\0Autobool\0");
    bytes
}

fn descriptor_item(bytes: &mut Vec<u8>, key: &str, kind: &[u8; 4]) {
    let length = if key.len() == 4 { 0 } else { key.len() as u32 };
    bytes.extend(length.to_be_bytes());
    bytes.extend(key.as_bytes());
    bytes.extend(kind);
}

fn write_preset_items(bytes: &mut Vec<u8>, kind_key: &str, name_key: &str, (kind, name): (i32, &str)) {
    descriptor_item(bytes, kind_key, b"long");
    bytes.extend(kind.to_be_bytes());
    descriptor_item(bytes, name_key, b"TEXT");
    write_descriptor_text(bytes, name);
}

fn write_descriptor_text(bytes: &mut Vec<u8>, name: &str) {
    bytes.extend((name.encode_utf16().count() as u32).to_be_bytes());
    for unit in name.encode_utf16() {
        bytes.extend(unit.to_be_bytes());
    }
}

const PRESET_HEADER: &[u8] = b"\0\0\0\x10\0\0\0\0\0\0\0\0null\0\0\0\x03\0\0\0\0Vrsnlong\0\0\0\x01";

fn write_adjustment_preset(preset_kind: PresetKind, (kind, name): (i32, &str)) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(100 + name.encode_utf16().count() * 2);
    bytes.extend(PRESET_HEADER);
    let (kind_key, name_key) = preset_kind.keys();
    write_preset_items(&mut bytes, kind_key, name_key, (kind, name));
    bytes
}

const BLACK_WHITE_KEYS: [&str; 6] = ["Rd  ", "Yllw", "Grn ", "Cyn ", "Bl  ", "Mgnt"];
const TINT_KEYS: [&str; 3] = ["Rd  ", "Grn ", "Bl  "];

fn black_white_descriptor(settings: &black_white::Settings, preset: (i32, &str)) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(384 + preset.1.encode_utf16().count() * 2);
    bytes.extend(b"\0\0\0\x10\0\0\0\0\0\0\0\0null\0\0\0\x0a");
    for (key, weight) in BLACK_WHITE_KEYS.into_iter().zip(settings.weights) {
        descriptor_item(&mut bytes, key, b"long");
        bytes.extend(weight.to_be_bytes());
    }
    descriptor_item(&mut bytes, "useTint", b"bool");
    bytes.push(u8::from(settings.use_tint));
    descriptor_item(&mut bytes, "tintColor", b"Objc");
    bytes.extend(b"\0\0\0\0\0\0\0\0RGBC\0\0\0\x03");
    for (key, channel) in TINT_KEYS.into_iter().zip(settings.tint) {
        descriptor_item(&mut bytes, key, b"doub");
        bytes.extend(channel.to_be_bytes());
    }
    write_preset_items(&mut bytes, "bwPresetKind", "blackAndWhitePresetFileName", preset);
    bytes
}

pub(super) fn black_white_settings(payload: &[u8]) -> Result<Value, String> {
    if payload.len() > MAX_ADJUSTMENT_DESCRIPTOR_BYTES {
        return Err("Black & White descriptor exceeds its size limit".into());
    }
    let root = super::styles::descriptor_block(payload)?;
    let items = root["items"].as_object().unwrap();
    if root["class"] != "null" || items.keys().any(|key| !BLACK_WHITE_KEYS.contains(&key.as_str())
        && !matches!(key.as_str(), "useTint" | "tintColor" | "bwPresetKind" | "blackAndWhitePresetFileName"))
    {
        return Err("Unsupported Black & White descriptor fields".into());
    }
    let field = |key: &str, kind: &str| items.get(key).filter(|item| item["type"] == kind)
        .map(|item| &item["value"]);
    let mut settings = black_white::defaults();
    for (key, wire) in black_white::COLORS.into_iter().zip(BLACK_WHITE_KEYS) {
        let weight = field(wire, "long").and_then(Value::as_i64).ok_or("Missing or invalid Black & White weight")?;
        settings[key] = json!(weight);
    }
    let tint = field("useTint", "bool").and_then(Value::as_bool).ok_or("Missing or invalid Black & White tint switch")?;
    settings["use_tint"] = json!(tint);
    if items.contains_key("tintColor") || tint {
        let object = field("tintColor", "Objc").filter(|object| object["class"] == "RGBC")
            .ok_or("Unsupported Black & White tint color space")?;
        let channels = object["items"].as_object().filter(|channels| channels.len() == 3)
            .ok_or("Invalid Black & White tint channels")?;
        let mut rgb = [0.; 3];
        for (value, key) in rgb.iter_mut().zip(TINT_KEYS) {
            *value = channels.get(key).filter(|item| item["type"] == "doub")
                .and_then(|item| item["value"].as_f64()).ok_or("Invalid Black & White tint channel")?;
        }
        settings["tint_color"] = json!(rgb);
    }
    if items.contains_key("bwPresetKind") || items.contains_key("blackAndWhitePresetFileName") {
        let kind = field("bwPresetKind", "long").and_then(Value::as_i64).ok_or("Invalid Black & White preset kind")?;
        let name = field("blackAndWhitePresetFileName", "TEXT").and_then(Value::as_str)
            .ok_or("Invalid Black & White preset name")?;
        settings["preset"] = json!({"kind":kind,"name":name});
    }
    black_white::Settings::new(&settings)?;
    Ok(settings)
}

fn padded_word(payload: &[u8]) -> Option<u16> {
    match payload {
        [high, low] | [high, low, 0, 0] => Some(u16::from_be_bytes([*high, *low])),
        _ => None,
    }
}

pub(super) fn threshold_settings(payload: &[u8]) -> Result<serde_json::Value, String> {
    let level = padded_word(payload).ok_or("Invalid Threshold record length or reserved bytes")?;
    let settings = serde_json::json!({"level":level});
    crate::effects::threshold::level(&settings)?;
    Ok(settings)
}

pub(super) fn posterize_settings(payload: &[u8]) -> Result<serde_json::Value, String> {
    let levels = padded_word(payload).ok_or("Invalid Posterize record length or reserved bytes")?;
    let settings = serde_json::json!({"levels": levels,
        "mapping": crate::effects::posterize::PHOTOSHOP_NATIVE});
    crate::effects::posterize::Quantizer::new(&settings)?;
    Ok(settings)
}

fn posterize_descriptor(settings: &serde_json::Value, depth: u16) -> Option<[u8; 4]> {
    if settings.as_object()?.keys().any(|key| !matches!(key.as_str(), "mapping" | "levels")) {
        return None;
    }
    let quantizer = crate::effects::posterize::Quantizer::new(settings).ok()?;
    let [high, low] = quantizer.levels.to_be_bytes();
    quantizer.standard_at_depth(depth).then_some([high, low, 0, 0])
}

const IDENTITY_CURVE: [[u16; 2]; 2] = [[0, 0], [255, 255]];
pub(super) const MAX_CURVES_RECORD: usize = (17 + 4 * (6 + 8 * curves::MAX_POINTS)).next_multiple_of(4);

pub(super) fn shifted_curve(settings: &serde_json::Value) -> bool {
    let shifted = |curve: &serde_json::Value| {
        curve["points"].as_array().is_some_and(|points| {
            points.first().is_some_and(|point| point[0] != 0.)
                || points.last().is_some_and(|point| point[0] != 1.)
        })
    };
    shifted(settings) || settings["channels"].as_array().is_some_and(|channels| channels.iter().any(shifted))
}

pub(super) fn curves_settings(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let mut reader = super::Cursor::new(bytes);
    let invalid = "Unsupported Photoshop Curves layout or channel settings";
    if reader.u8()? != 0 {
        return Err(invalid.into());
    }
    let version = reader.u16()?;
    let channels = match (version, reader.u32()?) {
        (1, mask @ 1..=15) => mask,
        (4, count @ 1..=4) => (1 << count) - 1,
        _ => return Err(invalid.into()),
    };
    let read_points = |reader: &mut super::Cursor<'_>| -> Result<Vec<[u16; 2]>, String> {
        let count = usize::from(reader.u16()?);
        if !(2..=curves::MAX_POINTS).contains(&count) {
            return Err(invalid.into());
        }
        let mut points = Vec::<[u16; 2]>::with_capacity(count);
        for _ in 0..count {
            let y = reader.u16()?;
            let x = reader.u16()?;
            if x > 255 || y > 255 || points.last().is_some_and(|point| x <= point[0]) {
                return Err(invalid.into());
            }
            points.push([x, y]);
        }
        Ok(points)
    };
    let mut points = [const { None }; 4];
    for (channel, point) in points.iter_mut().enumerate() {
        if channels & (1 << channel) != 0 {
            *point = Some(read_points(&mut reader)?);
        }
    }
    if version == 1 && bytes[reader.pos..].starts_with(b"Crv ") {
        reader.bytes(4)?;
        if !matches!(reader.u16()?, 3 | 4) {
            return Err(invalid.into());
        }
        let count = reader.u32()?;
        if count == 0 || count > channels.count_ones() {
            return Err(invalid.into());
        }
        let mut seen = 0;
        for _ in 0..count {
            let channel = usize::from(reader.u16()?);
            if channel >= points.len() || seen & (1 << channel) != 0
                || points[channel].as_deref() != Some(read_points(&mut reader)?.as_slice())
            {
                return Err("Conflicting Photoshop Curves extension".into());
            }
            seen |= 1 << channel;
        }
    }
    let tail = &bytes[reader.pos..];
    if tail.len() > 3 || tail.iter().any(|&v| v != 0) {
        return Err(invalid.into());
    }
    let [master, red, green, blue] = points;
    let master = master.as_deref().unwrap_or(&IDENTITY_CURVE);
    let colors = [red, green, blue];
    let record = |points: &[[u16; 2]]| serde_json::json!({
        "interpolation": if points.len() == 2 { "linear" } else { curves::PHOTOSHOP },
        "points": points.iter().map(|point| point.map(|value| f64::from(value) / 255.)).collect::<Vec<_>>(),
    });
    let mut settings = record(master);
    if colors.iter().flatten().any(|color| color != &IDENTITY_CURVE)
        || master == IDENTITY_CURVE && colors.iter().any(Option::is_some)
    {
        settings["channels"] = serde_json::json!(colors.map(|color| record(color.as_deref().unwrap_or(&IDENTITY_CURVE))));
        if master != IDENTITY_CURVE {
            settings["composition"] = serde_json::json!(curves::PHOTOSHOP_BYTE);
        }
    }
    Ok(settings)
}

fn curve_points(settings: &serde_json::Value) -> Option<Vec<[u16; 2]>> {
    let points = settings["points"].as_array()?;
    match (settings["interpolation"].as_str()?, points.len()) {
        ("linear", 2) | (curves::PHOTOSHOP, 3..=curves::MAX_POINTS) => points.iter()
            .map(|point| Some([curves::grid_sample(&point[0])?, curves::grid_sample(&point[1])?]))
            .collect(),
        _ => None,
    }
}

fn curves_descriptor(settings: &serde_json::Value) -> Option<Vec<u8>> {
    crate::effects::validate("curves", settings).ok()?;
    if settings.as_object()?.keys().any(|key| !matches!(key.as_str(), "interpolation" | "points" | "channels" | "composition" | "preset")) {
        return None;
    }
    let mut points = vec![curve_points(settings)?];
    if let Some(channels) = settings["channels"].as_array() {
        for channel in channels {
            points.push(curve_points(channel)?);
        }
        let combined = points[0] != IDENTITY_CURVE && points[1..].iter().any(|color| color != &IDENTITY_CURVE);
        if combined != curves::byte_composition("curves", settings)
            || !combined && points[0] != IDENTITY_CURVE
        {
            return None;
        }
    }
    let count = points.len();
    let mut out = Vec::with_capacity((17 + points.iter().map(|points| 6 + 8 * points.len()).sum::<usize>()).next_multiple_of(4));
    out.extend_from_slice(b"\0\0\x01");
    super::u32b(&mut out, (1 << count) - 1);
    let write_points = |out: &mut Vec<u8>, points: &[[u16; 2]]| {
        super::u16b(out, points.len() as u16);
        for &[x, y] in points {
            super::u16b(out, y);
            super::u16b(out, x);
        }
    };
    for values in &points {
        write_points(&mut out, values);
    }
    out.extend_from_slice(b"Crv \0\x04");
    super::u32b(&mut out, count as u32);
    for (channel, values) in points.iter().enumerate() {
        super::u16b(&mut out, channel as u16);
        write_points(&mut out, values);
    }
    out.resize(out.len().next_multiple_of(4), 0);
    Some(out)
}

const IDENTITY_LEVELS: [u16; 5] = [0, 255, 0, 255, 100];

fn levels_record_settings([black, white, low, high, gamma]: [u16; 5]) -> Result<serde_json::Value, String> {
    if black > 253 || !(2..=255).contains(&white) || black >= white
        || low > 255 || high > 255 || !(10..=999).contains(&gamma) {
        return Err("Invalid Levels channel values".into());
    }
    Ok(serde_json::json!({"black":f64::from(black)/255.,"white":f64::from(white)/255.,
        "output_black":f64::from(low)/255.,"output_white":f64::from(high)/255.,"gamma":f64::from(gamma)/100.,
        "input_mapping":"exact"}))
}

/// Photoshop's version-2 Levels records; unsupported channel settings must not
/// disappear merely because the current merged pixels happen to match.
pub(super) fn levels_settings(payload: &[u8]) -> Result<serde_json::Value, String> {
    if payload.len() < 292 || payload[..2] != [0, 2] {
        return Err("Invalid Levels version or record count".into());
    }
    let record = |bytes: &[u8]| std::array::from_fn::<_, 5, _>(|i| {
        u16::from_be_bytes([bytes[i * 2], bytes[i * 2 + 1]])
    });
    let master = record(&payload[2..12]);
    let mut settings = levels_record_settings(master)?;
    let colors = [record(&payload[12..22]), record(&payload[22..32]), record(&payload[32..42])];
    if colors.iter().any(|values| *values != IDENTITY_LEVELS) {
        if master != IDENTITY_LEVELS {
            if master[4] != 100 || colors.iter().any(|values| values[4] != 100) {
                return Err("Combined nonlinear Photoshop Levels requires calibration".into());
            }
            settings["composition"] = serde_json::json!(crate::effects::levels::PHOTOSHOP_BYTE);
        }
        let channels = colors.into_iter().map(levels_record_settings).collect::<Result<Vec<_>, _>>()?;
        settings["channels"] = serde_json::json!(channels);
    }
    for (i, bytes) in payload[42..292].chunks_exact(10).enumerate() {
        let values = record(bytes);
        if values != IDENTITY_LEVELS && !(i + 4 >= 27 && values == [0; 5]) {
            return Err("Additional channel Levels adjustments are not supported".into());
        }
    }
    if payload.len() != 292 {
        if payload.len() < 300 || &payload[292..298] != b"Lvls\0\x03" {
            return Err("Invalid extended Levels header".into());
        }
        let count = usize::from(u16::from_be_bytes([payload[298], payload[299]]));
        let extra = count.checked_sub(29).ok_or("Invalid extended Levels record count")?;
        let end = 300 + extra * 10;
        let padding = payload.get(end..).ok_or("Invalid extended Levels record count")?;
        // Photoshop can include the four-byte alignment in the tag length.
        if !padding.is_empty() && (payload.len() != end.next_multiple_of(4) || padding != [0, 0]) {
            return Err("Invalid extended Levels padding".into());
        }
        if payload[300..end].chunks_exact(10).any(|bytes| record(bytes) != IDENTITY_LEVELS) {
            return Err("Extended channel Levels adjustments are not yet supported".into());
        }
    }
    crate::effects::validate("levels", &settings)?;
    Ok(settings)
}

fn levels_record(settings: &serde_json::Value, exact: bool) -> Option<[u16; 5]> {
    let mut master = IDENTITY_LEVELS;
    for (i, key, scale, default) in [(0, "black", 255., 0.), (1, "white", 255., 1.),
        (2, "output_black", 255., 0.), (3, "output_white", 255., 1.), (4, "gamma", 100., 1.)] {
        let value = settings[key].as_f64().unwrap_or(default);
        let integer = (value * scale).round();
        if integer / scale != value { return None; }
        master[i] = integer as u16;
    }
    if master[1] - master[0] < 3 && !exact {
        return None;
    }
    levels_record_settings(master).ok()?;
    Some(master)
}

pub(super) fn levels_descriptor(settings: &serde_json::Value) -> Option<[u8; 292]> {
    crate::effects::validate("levels", settings).ok()?;
    if settings.as_object()?.keys().any(|key| !["black", "white", "gamma", "output_black", "output_white", "input_mapping", "channels", "composition", "preset"].contains(&key.as_str())) {
        return None;
    }
    let mut records = [IDENTITY_LEVELS; 4];
    records[0] = levels_record(settings, crate::effects::levels::exact(settings))?;
    if let Some(channels) = settings.get("channels") {
        for (record, channel) in records[1..].iter_mut().zip(channels.as_array()?) {
            *record = levels_record(channel, true)?;
        }
    }
    let mut payload = [0; 292];
    payload[1] = 2;
    for (i, bytes) in payload[2..].chunks_exact_mut(10).enumerate() {
        let values = if i < 4 { records[i] } else if i < 27 { IDENTITY_LEVELS } else { [0; 5] };
        for (target, value) in bytes.chunks_exact_mut(2).zip(values) {
            target.copy_from_slice(&value.to_be_bytes());
        }
    }
    let decoded = levels_settings(&payload).ok()?;
    if crate::effects::levels::byte_composition("levels", settings) {
        if records.iter().any(|record| record[4] != 100) { return None; }
    } else if crate::effects::levels::byte_composition("levels", &decoded) {
        return None;
    }
    Some(payload)
}

#[cfg(test)]
pub(in crate::psd) mod tests {
    use super::super::*;
    use serde_json::json;

    #[test]
    fn look_descriptor_preserves_raw_xml_profile_and_order_fields() {
        use crate::effects::color_lookup::{self, LutFormat, tests::look_fixture};
        let look = look_fixture();
        let mut settings = color_lookup::from_table(LutFormat::Look, look.as_bytes(), &[], "Lookup", "Grade.look").unwrap();
        settings["data_order"] = json!("bgr");
        settings["table_order"] = json!("rgb");
        let record = super::color_lookup_record(&settings).unwrap();
        assert!(record.windows(13).any(|bytes| bytes == b"LUTFormatLOOK"));
        assert!(record.windows(look.len()).any(|bytes| bytes == look.as_bytes()));
        let prepared = color_lookup::prepare(&settings).unwrap().unwrap();
        let profile = prepared.compiled_profile().unwrap();
        assert!(record.windows(profile.len()).any(|bytes| bytes == profile));
        assert_eq!(super::color_lookup_settings(&record).unwrap(), settings);
        let mut invalid = record.clone();
        let mask = record.windows(7).position(|bytes| bytes == b"<mask/>").unwrap();
        invalid[mask..mask + 7].copy_from_slice(b"<oops/>");
        assert!(super::color_lookup_settings(&invalid).is_err());
    }

    #[test]
    fn cube_lookup_round_trips_both_sources_and_native_precision() {
        use crate::effects::color_lookup::{self, tests::CUBE};
        let link = crate::color_profile::device_link::tests::channel_link(true);
        for profile in [&[][..], link.as_slice()] {
            let mut settings = color_lookup::from_cube(CUBE, profile, "Grade", "Épreuve.cube\0").unwrap();
            settings["data_order"] = json!("rgb");
            for depth in [8, 16] {
                let mut doc = Document::new_depth(2, 1, depth).unwrap();
                let original = if depth == 8 { [113, 51, 249, 255].map(|value| value * 257) }
                    else { [12345, 23456, 34567, 65535] };
                doc.layers[0].pixels.set16(0, 0, original);
                doc.layers[0].pixels.set16(1, 0, original);
                crate::adjustment::add(&mut doc, &json!({"kind":color_lookup::KIND,"settings":settings})).unwrap();
                let expected = if profile.is_empty() { [original[2], original[0], original[1], original[3]] }
                    else { [65535 - original[1], original[0], original[2], original[3]] };
                let rendered = doc.export_png().unwrap();
                if depth == 16 {
                    assert_eq!(&crate::depth16::render(&doc).unwrap().words[..4], &expected);
                } else {
                    assert_eq!(&doc.preview(None, 2, None, false).unwrap().2[..4], &expected.map(|word| (word / 257) as u8));
                }
                for layout in [Layout::Psd, Layout::Psb] {
                    let bytes = encode_layout(&doc, layout).unwrap();
                    let index = structural::index(
                        &mut std::io::Cursor::new(&bytes), structural::Limits::current(), || Ok(()),
                    ).unwrap();
                    let tag = index.layers.iter().flat_map(|layer| &layer.tags)
                        .find(|tag| tag.key == *b"clrL").unwrap();
                    let span = tag.data.offset as usize..(tag.data.offset + tag.data.bytes) as usize;
                    let payload = &bytes[span.clone()];
                    assert!(payload.windows(CUBE.len()).any(|value| value == CUBE));
                    let prepared = color_lookup::prepare(&settings).unwrap().unwrap();
                    let embedded_profile = if profile.is_empty() {
                        assert!(payload.ends_with(&[0, 0]));
                        prepared.compiled_profile().unwrap()
                    } else { profile };
                    let profile_at = payload.windows(embedded_profile.len()).position(|value| value == embedded_profile).unwrap();
                    let mut cursor = Cursor::new(&bytes);
                    cursor.bytes(26).unwrap();
                    cursor.block().unwrap();
                    let start = cursor.pos;
                    cursor.block().unwrap();
                    let removed = cursor.pos - start - 4;
                    let mut plain = bytes[..start].to_vec();
                    u32b(&mut plain, 0);
                    plain.extend_from_slice(&bytes[cursor.pos..]);
                    for saved in [&bytes, &plain] {
                        let reopened = decode(saved).unwrap();
                        assert!(!reopened.read_only, "{:?}", reopened.warnings);
                        assert_eq!(*reopened.layers[0].effects[0].settings, settings);
                        assert_eq!(reopened.layers[1].pixels.get16(0, 0), original);
                        assert_eq!(reopened.export_png().unwrap(), rendered);
                    }
                    let order = payload.windows(8).position(|bytes| bytes == b"rgbOrder").unwrap();
                    let start = span.start - removed + order;
                    let mut reordered = plain.clone();
                    reordered[start..start + 8].copy_from_slice(b"bgrOrder");
                    let changed = decode(&reordered).unwrap();
                    assert!(!changed.read_only, "{:?}", changed.warnings);
                    let changed_settings = &changed.layers[0].effects[0].settings;
                    assert_eq!(changed_settings["data_order"], "bgr");
                    assert_eq!(changed_settings["embed_profile"], false);
                    assert!(!changed_settings["profile"].as_str().unwrap().is_empty());
                    assert_eq!(changed.export_png().unwrap(), rendered);
                    if profile.is_empty() {
                        let clut = embedded_profile.windows(4).position(|bytes| bytes == b"clut").unwrap();
                        let node = span.start - removed + profile_at + clut + 28;
                        plain[node..node + 4].copy_from_slice(&1f32.to_be_bytes());
                        assert!(decode(&plain).unwrap().read_only);
                    }
                }
            }
        }
    }

    #[test]
    fn rgb_device_link_adjustments_bind_standard_profile_and_saved_composite() {
        use crate::effects::color_lookup;
        let profile = crate::color_profile::device_link::tests::channel_link(true);
        let settings = color_lookup::from_profile(&profile, "Épreuve\0").unwrap();
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            let original = if depth == 8 { [113, 51, 249, 255].map(|value| value * 257) }
                else { [12345, 23456, 34567, 65535] };
            doc.layers[0].pixels.set16(0, 0, original);
            doc.layers[0].pixels.set16(1, 0, original);
            crate::adjustment::add(&mut doc, &json!({"kind":color_lookup::KIND,"settings":settings})).unwrap();
            let expected = [65535 - original[1], original[0], original[2], original[3]];
            let rendered = doc.export_png().unwrap();
            if depth == 16 {
                assert_eq!(&crate::depth16::render(&doc).unwrap().words[..4], &expected);
            } else {
                assert_eq!(&doc.preview(None, 2, None, false).unwrap().2[..4], &expected.map(|word| (word / 257) as u8));
            }
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let index = structural::index(
                    &mut std::io::Cursor::new(&bytes), structural::Limits::current(), || Ok(()),
                ).unwrap();
                let tag = index.layers.iter().flat_map(|layer| &layer.tags)
                    .find(|tag| tag.key == *b"clrL").unwrap();
                let span = tag.data.offset as usize..(tag.data.offset + tag.data.bytes) as usize;
                assert!(bytes[span.clone()].windows(profile.len()).any(|value| value == profile));
                let reopened = decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(*reopened.layers[0].effects[0].settings, settings);
                assert_eq!(reopened.layers[1].pixels.get16(0, 0), original);
                assert_eq!(reopened.export_png().unwrap(), rendered);

                let mut cursor = Cursor::new(&bytes);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let start = cursor.pos;
                cursor.block().unwrap();
                let removed = cursor.pos - start - 4;
                let mut plain = bytes[..start].to_vec();
                u32b(&mut plain, 0);
                plain.extend_from_slice(&bytes[cursor.pos..]);
                let standard = decode(&plain).unwrap();
                assert!(!standard.read_only, "{:?}", standard.warnings);
                assert_eq!(*standard.layers[0].effects[0].settings, settings);
                assert_eq!(standard.export_png().unwrap(), rendered);
                let dither = bytes[span.clone()].windows(9).position(|bytes| bytes == b"Dthrbool\0").unwrap();
                let profile_start = bytes[span.clone()].windows(profile.len()).position(|bytes| bytes == profile).unwrap();
                let clut = span.start - removed + profile_start + 144 + 68 + 20;
                let mut mismatched = plain.clone();
                mismatched[clut..clut + 2].fill(0);
                assert!(decode(&mismatched).unwrap().read_only);
                plain[span.start - removed + dither + 8] = 1;
                assert!(decode(&plain).unwrap().read_only);
            }
        }
    }

    #[test]
    fn multiply_invert_import_requires_the_saved_composite_and_reopens_editably() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0].pixels.set16(0, 0, [65535, 0, 0, 65535]);
            doc.layers[0].pixels.set16(1, 0, [0, 65535, 0, 65535]);
            crate::adjustment::add(&mut doc, &json!({"kind":"invert"})).unwrap();
            for layout in [Layout::Psd, Layout::Psb] {
                let mut bytes = encode_layout(&doc, layout).unwrap();
                let index = structural::index(
                    &mut std::io::Cursor::new(&bytes), structural::Limits::current(), || Ok(()),
                ).unwrap();
                let adjustment = index.layers.iter().find(|layer| {
                    layer.tags.iter().any(|tag| tag.key == *b"nvrt")
                }).unwrap();
                let extra = adjustment.extra.offset as usize;
                assert_eq!(&bytes[extra - 12..extra - 8], b"norm");
                bytes[extra - 12..extra - 8].copy_from_slice(b"mul ");
                let mut file = Cursor::new(&bytes);
                file.bytes(26).unwrap();
                file.block().unwrap();
                let resource_length = file.pos;
                file.block().unwrap();
                let mut standard = bytes[..resource_length].to_vec();
                u32b(&mut standard, 0);
                standard.extend_from_slice(&bytes[file.pos..index.composite.span.offset as usize]);
                for matches in [true, false] {
                    let mut input = standard.clone();
                    u16b(&mut input, 0);
                    // A primary color multiplied by its inverse is opaque black.
                    for channel in 0..index.header.channels {
                        for _ in 0..2 {
                            let value = if channel == 3 || !matches { 65535 } else { 0 };
                            if depth == 16 { u16b(&mut input, value); }
                            else { input.push((value / 257) as u8); }
                        }
                    }
                    for restored in [decode(&input).unwrap(), decode_reader(
                        &mut std::io::Cursor::new(&input), &Default::default(),
                    ).unwrap()] {
                        assert_eq!(restored.read_only, !matches, "{:?}", restored.warnings);
                        if matches {
                            assert_eq!(restored.layers[0].kind, "adjustment");
                            assert_eq!(restored.layers[0].blend, "multiply");
                            assert_eq!(restored.layers[0].effects[0].kind, "invert");
                            assert!(!standard_plan(&restored).composite);
                            let saved = encode_layout(&restored, layout).unwrap();
                            let saved_index = structural::index(
                                &mut std::io::Cursor::new(&saved), structural::Limits::current(), || Ok(()),
                            ).unwrap();
                            let invert = saved_index.layers.iter().find(|layer| {
                                layer.tags.iter().any(|tag| tag.key == *b"nvrt")
                            }).unwrap();
                            assert_eq!(invert.blend_mode, *b"mul ");
                            let reopened = decode(&saved).unwrap();
                            assert!(!reopened.read_only, "{:?}", reopened.warnings);
                            assert_eq!(reopened.layers[0].blend, "multiply");
                            assert_eq!(reopened.layers[0].effects[0].kind, "invert");
                            assert_eq!(reopened.layers[1].pixels.get16(0, 0), [65535, 0, 0, 65535]);
                            let downgraded = replace_sources(&saved, layout, &restored, 43);
                            assert!(decode(&downgraded).unwrap().read_only);
                            let mut baked = Document::new_depth(2, 1, depth).unwrap();
                            for x in 0..2 {
                                baked.layers[0].pixels.set16(x, 0, [0, 0, 0, 65535]);
                            }
                            let legacy = replace_sources(&encode_layout(&baked, layout).unwrap(), layout, &restored, 43);
                            let legacy = decode(&legacy).unwrap();
                            assert!(!legacy.read_only, "{:?}", legacy.warnings);
                            assert_eq!(legacy.layers[0].blend, "multiply");
                            assert_eq!(legacy.layers[1].pixels.get16(0, 0), [65535, 0, 0, 65535]);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn empty_adjustment_channels_validate_their_compression_and_payload() {
        for depth in [8, 16] {
            for layout in [Layout::Psd, Layout::Psb] {
                let mut raster = Raster::new_depth(0, 0, depth);
                for bytes in [&[][..], &[0, 0], &[0, 1]] {
                    stream_layer_channel(bytes, &mut raster, 0, true, layout, &Default::default())
                        .unwrap();
                }
                for bytes in [&[0][..], &[0, 4], &[0, 0, 1], &[0, 1, 1], &[0, 2]] {
                    assert!(stream_layer_channel(
                        bytes,
                        &mut raster,
                        0,
                        true,
                        layout,
                        &Default::default()
                    )
                    .is_err());
                }
                let mut zip = ZlibEncoder::new(Vec::new(), Compression::fast());
                zip.write_all(&[]).unwrap();
                let empty_zip = [vec![0, 2], zip.finish().unwrap()].concat();
                stream_layer_channel(
                    &empty_zip,
                    &mut raster,
                    0,
                    true,
                    layout,
                    &Default::default(),
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn standard_clipped_invert_requires_an_exact_composite_and_keeps_its_raster_base() {
        let expected = [[65535, 0, 65535, 65535], [0, 0, 65535, 65535]];
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            let base = &mut doc.layers[0];
            base.name = "Clipping base".into();
            base.pixels = Raster::new_depth(1, 1, depth);
            base.pixels.set16(0, 0, [0, 65535, 0, 65535]);
            let mut backdrop = Layer::new("Backdrop", "paint", 2, 1);
            backdrop.pixels = Raster::new_depth(2, 1, depth);
            for x in 0..2 {
                backdrop.pixels.set16(x, 0, expected[1]);
            }
            doc.layers.push(backdrop);
            crate::adjustment::add(&mut doc, &json!({"kind":"invert"})).unwrap();
            for layout in [Layout::Psd, Layout::Psb] {
                let mut bytes = encode_layout(&doc, layout).unwrap();
                let index = structural::index(
                    &mut std::io::Cursor::new(&bytes),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                let invert = index
                    .layers
                    .iter()
                    .find(|layer| layer.tags.iter().any(|tag| tag.key == *b"nvrt"))
                    .unwrap();
                let extra = invert.extra.offset as usize;
                assert_eq!(&bytes[extra - 8..extra - 4], &[255, 0, 0x18, 0]);
                bytes[extra - 7] = 1;
                let mut file = Cursor::new(&bytes);
                file.bytes(26).unwrap();
                file.block().unwrap();
                let resource_length = file.pos;
                file.block().unwrap();
                let mut standard = bytes[..resource_length].to_vec();
                u32b(&mut standard, 0);
                standard.extend_from_slice(&bytes[file.pos..index.composite.span.offset as usize]);
                for matches in [true, false] {
                    let mut input = standard.clone();
                    u16b(&mut input, 0); // Raw planar merged data, independent of the renderer.
                    let mut pixels = expected;
                    if !matches {
                        pixels[1] = [65535, 65535, 0, 65535];
                    }
                    for c in 0..usize::from(index.header.channels) {
                        for pixel in pixels {
                            if depth == 8 {
                                input.push((pixel[c] / 257) as u8);
                            } else {
                                u16b(&mut input, pixel[c]);
                            }
                        }
                    }
                    for restored in [
                        decode(&input).unwrap(),
                        decode_reader(&mut std::io::Cursor::new(&input), &Default::default())
                            .unwrap(),
                    ] {
                        assert_eq!(!restored.read_only, matches, "{:?}", restored.warnings);
                        if matches {
                            assert_eq!(restored.layers[0].kind, "adjustment");
                            assert_eq!(restored.layers[0].effects[0].kind, "invert");
                            assert_eq!(restored.layers[1].name, "Clipping base");
                            assert_eq!(
                                restored.layers[0].clip_to.as_deref(),
                                Some(restored.layers[1].id.as_str())
                            );
                            assert_eq!(
                                restored.layers[1].pixels.get16(0, 0),
                                [0, 65535, 0, 65535]
                            );
                            let mut current = encode_layout(&restored, layout).unwrap();
                            let index = structural::index(
                                &mut std::io::Cursor::new(&current),
                                structural::Limits::current(),
                                || Ok(()),
                            )
                            .unwrap();
                            let invert = index.layers.iter().find(|layer| {
                                layer.tags.iter().any(|tag| tag.key == *b"nvrt")
                            }).unwrap();
                            assert_eq!(invert.clipping, 1);
                            let grouped = index.layers.iter().filter(|layer| layer.clipping == 0)
                                .flat_map(|layer| &layer.tags)
                                .find(|tag| tag.key == *b"clbl").unwrap();
                            assert!(!decode(&current).unwrap().read_only);
                            let mut changed = current.clone();
                            changed[grouped.data.offset as usize] = 0;
                            assert!(decode(&replace_sources(&changed, layout, &restored, 40))
                                .unwrap().read_only);
                            current[invert.extra.offset as usize - 7] = 0;
                            let forged = replace_sources(&current, layout, &restored, 40);
                            assert!(decode(&forged).unwrap().read_only);

                            let mut baked = Document::new_depth(2, 1, depth).unwrap();
                            for (x, pixel) in pixels.into_iter().enumerate() {
                                baked.layers[0].pixels.set16(x as i32, 0, pixel);
                            }
                            let old = replace_sources(
                                &encode_layout(&baked, layout).unwrap(), layout, &restored,
                                if depth == 16 { 5 } else { 4 },
                            );
                            let old = decode(&old).unwrap();
                            assert!(!old.read_only, "{:?}", old.warnings);
                            assert_eq!(old.layers[0].clip_to, restored.layers[0].clip_to);
                        } else {
                            assert!(restored.layers.iter().all(|l| l.kind != "adjustment"));
                            assert_eq!(restored.layers[0].pixels.get16(1, 0), pixels[1]);
                        }
                    }
                }
            }
        }
    }

    pub(in crate::psd) fn replace_sources(
        bytes: &[u8],
        layout: Layout,
        doc: &Document,
        format: u32,
    ) -> Vec<u8> {
        let mut file = Cursor::new(bytes);
        file.bytes(26).unwrap();
        file.block().unwrap();
        let start = file.pos;
        file.block().unwrap();
        let end = file.pos;
        let section = layout.block(&mut file).unwrap();
        let source = Embedded {
            format,
            standard_hash: hash(section) ^ hash(&bytes[file.pos..]),
            document: doc.clone(),
        };
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::fast());
        serde_json::to_writer(&mut zlib, &source).unwrap();
        let payload = [b"PBR1".as_slice(), &zlib.finish().unwrap()].concat();
        let mut resource = b"8BIM\x0f\xa0\x0cPeerBrush.v1\0".to_vec();
        block(&mut resource, &payload);
        if payload.len() % 2 != 0 {
            resource.push(0);
        }
        let mut changed = bytes[..start].to_vec();
        block(&mut changed, &resource);
        changed.extend(&bytes[end..]);
        changed
    }

    #[test]
    fn old_baked_sources_restore_but_new_sources_require_the_standard_invert_definition() {
        for depth in [8, 16] {
            let mut engine = crate::engine::Engine::new();
            engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(2, 1, depth).unwrap();
            engine.resident_state_mut().expect("resident fixture").document.layers[0]
                .pixels
                .set16(0, 0, [12347, 23459, 34571, 65535]);
            engine
                .edit(
                    "human",
                    &[json!({"op":"adjustment.add","kind":"invert"})],
                    None,
                    None,
                    "Invert",
                )
                .unwrap();
            let doc = &engine.resident_state().expect("resident fixture").document;
            let mut baked = Document::new_depth(2, 1, depth).unwrap();
            baked.layers[0].pixels = if depth == 16 {
                Raster::from_rgba16(2, 1, &crate::depth16::render(doc).unwrap().words).unwrap()
            } else {
                Raster::from_rgba(2, 1, &doc.preview_native8(None, 2, None, false).unwrap().2)
                    .unwrap()
            };
            for layout in [Layout::Psd, Layout::Psb] {
                let baked = encode_layout(&baked, layout).unwrap();
                let old = replace_sources(&baked, layout, doc, if depth == 16 { 5 } else { 4 });
                for restored in [
                    decode(&old).unwrap(),
                    decode_reader(&mut std::io::Cursor::new(&old), &Default::default()).unwrap(),
                ] {
                    assert!(!restored.read_only, "{:?}", restored.warnings);
                    assert_eq!(
                        serde_json::to_value(&restored).unwrap(),
                        serde_json::to_value(doc).unwrap()
                    );
                }
                assert!(
                    decode(&replace_sources(&baked, layout, doc, 35))
                        .unwrap()
                        .read_only
                );
                let current = encode_layout(doc, layout).unwrap();
                assert!(
                    decode(&replace_sources(&current, layout, doc, 34))
                        .unwrap()
                        .read_only
                );
                // A recomputed checksum cannot replace the standard adjustment
                // with an unrelated tag while keeping the editable definition.
                let mut changed = current.clone();
                let at = changed.windows(8).position(|v| v == b"8BIMnvrt").unwrap();
                changed[at + 4..at + 8].copy_from_slice(b"fake");
                assert!(
                    decode(&replace_sources(&changed, layout, doc, 35))
                        .unwrap()
                        .read_only
                );
            }
        }
    }
}
