//! Bounded object-effects inspection and conservative overlay import.
//! Descriptor recognition alone never authorizes editing; the caller verifies the native composite.
//! Binary layout: https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/
use super::Cursor;
use serde_json::{json, Map, Value};

struct Reader<'a> {
    cursor: Cursor<'a>,
    nodes: usize,
}
impl Reader<'_> {
    fn text(&mut self) -> Result<String, String> {
        let count = self.cursor.u32()? as usize;
        if count > 4096 {
            return Err("descriptor text exceeds 4096 code units".into());
        }
        let bytes = self.cursor.bytes(count * 2)?;
        let words = bytes
            .chunks_exact(2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&words).map_err(|_| "invalid descriptor Unicode".into())
    }
    fn id(&mut self) -> Result<String, String> {
        let count = self.cursor.u32()? as usize;
        let count = if count == 0 { 4 } else { count };
        if count > 128 {
            return Err("descriptor identifier exceeds 128 bytes".into());
        }
        let bytes = self.cursor.bytes(count)?;
        if !bytes.iter().all(|c| (32..=126).contains(c)) {
            return Err("invalid descriptor identifier".into());
        }
        Ok(std::str::from_utf8(bytes).unwrap().into())
    }
    fn number(&mut self) -> Result<f64, String> {
        let n = f64::from_be_bytes(self.cursor.bytes(8)?.try_into().unwrap());
        if !n.is_finite() {
            return Err("non-finite descriptor number".into());
        }
        Ok(n)
    }
    fn node(&mut self, depth: usize) -> Result<(), String> {
        self.nodes += 1;
        if depth > 32 || self.nodes > 4096 {
            return Err("descriptor exceeds its depth or item budget".into());
        }
        Ok(())
    }
    fn descriptor(&mut self, depth: usize) -> Result<Value, String> {
        self.node(depth)?;
        // Names and opaque data are validated but not exposed in diagnostics.
        self.text()?;
        let class = self.id()?;
        let count = self.cursor.u32()? as usize;
        if count > 256 {
            return Err("descriptor exceeds 256 properties".into());
        }
        let mut items = Map::new();
        for _ in 0..count {
            let key = self.id()?;
            if items.contains_key(&key) {
                return Err("duplicate descriptor property".into());
            }
            let value = self.value(depth + 1)?;
            items.insert(key, value);
        }
        Ok(json!({"class":class,"items":items}))
    }
    fn value(&mut self, depth: usize) -> Result<Value, String> {
        self.node(depth)?;
        let kind = self.cursor.bytes(4)?;
        let value = match kind {
            b"Objc" | b"GlbO" => self.descriptor(depth)?,
            b"VlLs" => {
                let count = self.cursor.u32()? as usize;
                if count > 256 {
                    return Err("descriptor list exceeds 256 items".into());
                }
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(self.value(depth + 1)?);
                }
                json!(values)
            }
            b"bool" => match self.cursor.u8()? {
                0 => json!(false),
                1 => json!(true),
                _ => return Err("invalid descriptor boolean".into()),
            },
            b"long" => json!(self.cursor.i32()?),
            b"comp" => json!(i64::from_be_bytes(
                self.cursor.bytes(8)?.try_into().unwrap()
            )),
            b"doub" => json!(self.number()?),
            b"UntF" => {
                let unit = self.cursor.bytes(4)?;
                if !unit.iter().all(|c| (32..=126).contains(c)) {
                    return Err("invalid descriptor unit".into());
                }
                json!([std::str::from_utf8(unit).unwrap(), self.number()?])
            }
            b"enum" => json!([self.id()?, self.id()?]),
            // Retained internally for bounded pattern-ID resolution; never exposed by inspect.
            b"TEXT" => json!(self.text()?),
            b"type" | b"GlbC" => {
                self.text()?;
                json!(self.id()?)
            }
            b"tdta" | b"alis" => {
                let count = self.cursor.u32()? as usize;
                self.cursor.bytes(count)?;
                json!({"bytes":count})
            }
            _ => return Err("unsupported descriptor value type".into()),
        };
        Ok(json!({"type":std::str::from_utf8(kind).unwrap(),"value":value}))
    }
}
fn boolean(value: &Value) -> Option<bool> {
    (value["type"] == "bool")
        .then(|| value["value"].as_bool())
        .flatten()
}
fn style_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "DrSh" | "dropShadowMulti" => "Drop Shadow",
        "IrSh" | "innerShadowMulti" => "Inner Shadow",
        "OrGl" => "Outer Glow",
        "IrGl" => "Inner Glow",
        "SoFi" | "solidFillMulti" => "Color Overlay",
        "GrFl" | "gradientFillMulti" => "Gradient Overlay",
        "patternFill" => "Pattern Overlay",
        "FrFX" | "frameFXMulti" => "Stroke",
        "ebbl" => "Bevel & Emboss",
        "ChFX" => "Satin",
        _ => return None,
    })
}
fn root(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > 1024 * 1024 {
        return Err("effects descriptor exceeds 1 MiB".into());
    }
    let mut reader = Reader {
        cursor: Cursor::new(bytes),
        nodes: 0,
    };
    if reader.cursor.u32()? != 0 || reader.cursor.u32()? != 16 {
        return Err("unsupported effects descriptor version".into());
    }
    let root = reader.descriptor(0)?;
    let remaining = &bytes[reader.cursor.pos..];
    if remaining.len() > 3 || remaining.iter().any(|v| *v != 0) {
        return Err("unrecognized effects descriptor trailer".into());
    }
    Ok(root)
}
/// Summary only: no native effect stack, editable IDs or decoded source pixels are created.
pub(super) fn inspect(bytes: &[u8]) -> Result<Vec<String>, String> {
    let root = root(bytes)?;
    let items = root["items"].as_object().unwrap();
    let mut summaries = vec![];
    let master = items.get("masterFXSwitch").and_then(boolean);
    if items.contains_key("masterFXSwitch") && master.is_none() {
        return Err("invalid style master switch".into());
    }
    summaries.push(format!(
        "Photoshop style master switch: {}",
        match master {
            Some(true) => "enabled",
            Some(false) => "disabled",
            None => "not specified",
        }
    ));
    for (key, value) in items {
        if matches!(key.as_str(), "Scl " | "masterFXSwitch") {
            continue;
        }
        if let Some(name) = style_name(key) {
            let entries = if value["type"] == "VlLs" {
                value["value"]
                    .as_array()
                    .ok_or("invalid effects list")?
                    .iter()
                    .collect::<Vec<_>>()
            } else {
                vec![value]
            };
            let mut enabled = 0;
            let mut disabled = 0;
            let mut unknown = 0;
            let mut present = 0;
            for entry in &entries {
                if !matches!(entry["type"].as_str(), Some("Objc" | "GlbO")) {
                    return Err("style entry must be a descriptor".into());
                }
                match boolean(&entry["value"]["items"]["enab"]) {
                    Some(true) => enabled += 1,
                    Some(false) => disabled += 1,
                    None => unknown += 1,
                }
                if boolean(&entry["value"]["items"]["present"]) == Some(true) {
                    present += 1;
                }
            }
            summaries.push(format!("Photoshop {name}: {} definitions; {enabled} enabled, {disabled} disabled, {unknown} unspecified; {present} marked present",entries.len()));
        } else {
            // Bounded printable keys only, never descriptor names, paths or blob contents.
            summaries.push(format!("Unrecognized Photoshop style property: {key}"));
        }
        if summaries.len() == 63 {
            summaries.push("Additional Photoshop style diagnostics omitted".into());
            break;
        }
    }
    Ok(summaries)
}

fn plain_number(value: &Value) -> Result<f64, String> {
    if !matches!(value["type"].as_str(), Some("doub" | "long")) {
        return Err("Photoshop style requires plain channel/coordinate numbers".into());
    }
    value["value"]
        .as_f64()
        .ok_or("Invalid Photoshop style number".into())
}
fn percent(value: &Value) -> Result<f64, String> {
    if value["type"] != "UntF" || value["value"][0] != "#Prc" {
        return Err("Photoshop style requires percentage units".into());
    }
    let n = value["value"][1]
        .as_f64()
        .ok_or("Invalid Photoshop style percentage")?;
    if !(0.0..=100.).contains(&n) {
        return Err("Photoshop style percentage is outside 0–100".into());
    }
    Ok(n / 100.)
}
fn bounded_percent(value: &Value, min: f64, max: f64) -> Result<f64, String> {
    if value["type"] != "UntF" || value["value"][0] != "#Prc" {
        return Err("Photoshop gradient placement requires percentage units".into());
    }
    value["value"][1]
        .as_f64()
        .filter(|n| n.is_finite() && (min..=max).contains(n))
        .map(|n| n / 100.)
        .ok_or_else(|| format!("Photoshop gradient percentage must be {min}–{max}"))
}
fn overlay_blend(value: &Value) -> Result<&'static str, String> {
    if value["type"] != "enum" || value["value"][0] != "BlnM" {
        return Err("Invalid Photoshop overlay blend enumeration".into());
    }
    match value["value"][1].as_str() {
        Some("Nrml") => Ok("normal"),
        Some("Mltp") => Ok("multiply"),
        Some("Scrn") => Ok("screen"),
        Some("Ovrl") => Ok("overlay"),
        Some("Drkn") => Ok("darken"),
        Some("Lghn") => Ok("lighten"),
        Some("Dfrn") => Ok("difference"),
        Some("Xclu") => Ok("exclusion"),
        Some("CBrn") => Ok("color_burn"),
        Some("linearBurn") => Ok("linear_burn"),
        Some("CDdg") => Ok("color_dodge"),
        Some("linearDodge") => Ok("linear_dodge"),
        Some("SftL") => Ok("soft_light"),
        Some("HrdL") => Ok("hard_light"),
        Some("Sbtr") => Ok("subtract"),
        Some("divide") => Ok("divide"),
        _ => Err("Unsupported Photoshop overlay blend remains protected".into()),
    }
}
// RGBC has either byte-scaled channel fields or normalized float fields.
// Accept one complete spelling only; never clamp HDR values or mix scales.
// Reference: psd-tools/composite/paint.py::_get_color and terminology.py.
fn rgb_color(value: &Value) -> Result<Vec<f64>, String> {
    if value["value"]["class"] == "HSBC" {
        let channels = descriptor(value, "HSBC")?;
        if channels.len() != 3
            || ["H   ", "Strt", "Brgh"]
                .iter()
                .any(|key| !channels.contains_key(*key))
        {
            return Err(
                "Photoshop HSB colors require exactly hue, saturation and brightness".into(),
            );
        }
        let component = |key: &str, unit: &str, max: f64| -> Result<f64, String> {
            let value = field(channels, key);
            let number = if value["type"] == "UntF" {
                if value["value"][0] != unit {
                    return Err("Invalid Photoshop HSB component unit".into());
                }
                value["value"][1]
                    .as_f64()
                    .ok_or("Invalid Photoshop HSB component")?
            } else {
                plain_number(value)?
            };
            if !number.is_finite() || !(0.0..=max).contains(&number) {
                return Err("Photoshop HSB component exceeds its supported range".into());
            }
            Ok(number)
        };
        let hue = component("H   ", "#Ang", 360.)?.rem_euclid(360.) / 60.;
        let saturation = component("Strt", "#Prc", 100.)? / 100.;
        let brightness = component("Brgh", "#Prc", 100.)? / 100.;
        let fraction = hue - hue.floor();
        let p = brightness * (1. - saturation);
        let q = brightness * (1. - saturation * fraction);
        let t = brightness * (1. - saturation * (1. - fraction));
        return Ok(match hue as u8 {
            0 => vec![brightness, t, p],
            1 => vec![q, brightness, p],
            2 => vec![p, brightness, t],
            3 => vec![p, q, brightness],
            4 => vec![t, p, brightness],
            _ => vec![brightness, p, q],
        });
    }
    if value["value"]["class"] == "Grsc" {
        let channels = descriptor(value, "Grsc")?;
        if channels.len() != 1 || !channels.contains_key("Gry ") {
            return Err("Photoshop grayscale colors require one plain gray channel".into());
        }
        let black = plain_number(field(channels, "Gry "))?;
        if !(0.0..=100.).contains(&black) {
            return Err("Photoshop grayscale black percentage must be within 0–100".into());
        }
        // Grsc stores percent black: zero is white and 100 is black.
        // Preserve fractional source values in normalized RGB, without bytes.
        return Ok(vec![(100. - black) / 100.; 3]);
    }
    let channels = descriptor(value, "RGBC")?;
    let byte_keys = ["Rd  ", "Grn ", "Bl  "];
    let float_keys = ["redFloat", "greenFloat", "blueFloat"];
    let (keys, scale) =
        if channels.len() == 3 && byte_keys.iter().all(|key| channels.contains_key(*key)) {
            (byte_keys, 255.)
        } else if channels.len() == 3 && float_keys.iter().all(|key| channels.contains_key(*key)) {
            (float_keys, 1.)
        } else {
            return Err("Photoshop RGB colors require one complete plain channel spelling".into());
        };
    keys.into_iter()
        .map(|key| {
            let channel = plain_number(field(channels, key))?;
            if !(0.0..=scale).contains(&channel) {
                return Err(format!("Photoshop RGB channels must be within 0–{scale}"));
            }
            Ok(channel / scale)
        })
        .collect()
}
pub(super) fn native_styles(
    bytes: &[u8],
    patterns: &[super::patterns::Source],
    depth: u16,
) -> Result<(Vec<crate::effects::Effect>, Option<usize>), String> {
    let parsed = root(bytes)?;
    if !matches!(parsed["class"].as_str(), Some("null" | "Lefx")) {
        return Err("Unsupported Photoshop effects class".into());
    }
    let items = parsed["items"]
        .as_object()
        .ok_or("Invalid effects object")?;
    if items.keys().any(|key| {
        !matches!(
            key.as_str(),
            "masterFXSwitch"
                | "Scl "
                | "SoFi"
                | "solidFillMulti"
                | "patternFill"
                | "GrFl"
                | "gradientFillMulti"
                | "IrSh"
                | "innerShadowMulti"
                | "DrSh"
                | "dropShadowMulti"
        )
    }) {
        return Err("Unsupported Photoshop style in overlay stack".into());
    }
    let mut effects = vec![];
    let mut pattern_index = None;
    // Photoshop overlays run Color, Pattern, Gradient. Keep the same execution
    // order in the native stack (the UI displays the last effect at the top).
    // Reference: psd-tools/composite/composite.py::_OVERLAY_DRAWS.
    // Preserve multi-overlay entries in file order, including disabled entries.
    // Never try alternative orders merely to match the saved appearance.
    for keys in [
        &["SoFi", "solidFillMulti"][..],
        &["patternFill"][..],
        &["GrFl", "gradientFillMulti"][..],
    ] {
        let present = keys
            .iter()
            .filter(|key| items.contains_key(**key))
            .collect::<Vec<_>>();
        if present.is_empty() {
            continue;
        }
        if present.len() != 1 {
            return Err("Ambiguous Photoshop single/multiple overlay aliases".into());
        }
        let key = *present[0];
        let source = field(items, key);
        let entries = if source["type"] == "VlLs" {
            let entries = source["value"]
                .as_array()
                .filter(|v| (1..=16).contains(&v.len()))
                .ok_or("Photoshop overlay list requires 1–16 definitions")?;
            if key == "patternFill" || entries.len() > 1 && !key.ends_with("Multi") {
                return Err(
                    "Multiple overlays require an explicit Photoshop multi-effect key".into(),
                );
            }
            entries.iter().collect::<Vec<_>>()
        } else {
            vec![source]
        };
        if effects.len() + entries.len() > 32 {
            return Err("Photoshop overlay stack exceeds the shared 32-effect limit".into());
        }
        for entry in entries {
            let mut isolated = parsed.clone();
            let isolated_items = isolated["items"].as_object_mut().unwrap();
            isolated_items.retain(|key, _| matches!(key.as_str(), "masterFXSwitch" | "Scl "));
            isolated_items.insert(keys[0].into(), entry.clone());
            match keys[0] {
                "SoFi" => effects.extend(color_overlay_root(&isolated)?),
                "patternFill" => {
                    let (effect, index) = pattern_overlay(&isolated, patterns, depth)?;
                    effects.push(effect);
                    pattern_index = Some(index);
                }
                _ => effects.extend(gradient_overlay(&isolated)?),
            }
        }
    }
    // Overlays preserve alpha. A hard Inner Shadow can therefore cast from
    // that same original alpha after the overlay colors have been applied.
    // Apply the single Normal Drop Shadow last: its behind compositing keeps
    // it beneath the styled image, and its grown alpha cannot feed an inner
    // shadow or become overlay coverage. The caller still requires binary
    // source alpha, Normal layer blend, no masks/clipping and an exact composite.
    // Reference: Krita kis_layer_style_projection_plane.cpp (overlay inner
    // shadow and stylesBefore drop shadow). Multiple casts per family remain protected.
    for keys in [["IrSh", "innerShadowMulti"], ["DrSh", "dropShadowMulti"]] {
        let present = keys
            .iter()
            .filter(|key| items.contains_key(**key))
            .collect::<Vec<_>>();
        if present.is_empty() {
            continue;
        }
        if present.len() != 1 {
            return Err("Ambiguous Photoshop single/multiple shadow aliases".into());
        }
        if effects.len() == 32 {
            return Err("Photoshop style stack exceeds the shared 32-effect limit".into());
        }
        let mut isolated = parsed.clone();
        isolated["items"].as_object_mut().unwrap().retain(|key, _| {
            matches!(key.as_str(), "masterFXSwitch" | "Scl ") || key == *present[0]
        });
        effects.push(hard_shadow(&isolated)?);
    }
    if effects.is_empty() {
        return Err("No supported Photoshop overlay definitions".into());
    }
    Ok((effects, pattern_index))
}
// Shadow fields follow psd-tools/api/effects.py and Krita's ASL serializer.
// Validate each isolated shadow definition. The native stack builder keeps
// casts bound to original alpha; independent exterior backdrop blends and
// multiple casts in one family still require separate source/backdrop handling.
fn hard_shadow(root: &Value) -> Result<crate::effects::Effect, String> {
    let items = root["items"].as_object().ok_or("Invalid effects object")?;
    let shadow_keys = ["DrSh", "dropShadowMulti", "IrSh", "innerShadowMulti"];
    let present = shadow_keys
        .into_iter()
        .filter(|key| items.contains_key(*key))
        .collect::<Vec<_>>();
    if items.keys().any(|key| {
        !matches!(
            key.as_str(),
            "masterFXSwitch" | "Scl " | "DrSh" | "dropShadowMulti" | "IrSh" | "innerShadowMulti"
        )
    }) || present.len() != 1
        || items.get("Scl ").is_some_and(|v| percent(v) != Ok(1.))
    {
        return Err(
            "Unsupported Photoshop style stack: only a single unscaled hard shadow without other styles can be imported".into(),
        );
    }
    let master = boolean(field(items, "masterFXSwitch")).ok_or("Missing style master switch")?;
    let source_key = present[0];
    let (key, kind) = if matches!(source_key, "DrSh" | "dropShadowMulti") {
        ("DrSh", "drop_shadow")
    } else {
        ("IrSh", "inner_shadow")
    };
    let source = field(items, source_key);
    let source = if source_key.ends_with("Multi") {
        if source["type"] != "VlLs" {
            return Err("Photoshop shadow multi-effect keys require an explicit list".into());
        }
        let entries = source["value"]
            .as_array()
            .filter(|entries| entries.len() == 1)
            .ok_or("Only a single Photoshop shadow definition can be imported")?;
        &entries[0]
    } else {
        source
    };
    let fields = descriptor(source, key)?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "enab"
                | "present"
                | "showInDialog"
                | "Md  "
                | "Opct"
                | "Clr "
                | "uglg"
                | "lagl"
                | "Dstn"
                | "Ckmt"
                | "blur"
                | "Nose"
                | "AntA"
                | "TrnS"
        ) && !(kind == "drop_shadow" && key == "layerConceals")
    }) {
        return Err("Unsupported Photoshop hard shadow property".into());
    }
    let required_bool =
        |key: &str| boolean(field(fields, key)).ok_or("Missing or invalid shadow boolean");
    let enabled = required_bool("enab")?;
    let present = required_bool("present")?;
    if fields
        .get("showInDialog")
        .is_some_and(|v| boolean(v).is_none())
        || required_bool("uglg")?
        || required_bool("AntA")?
        || kind == "drop_shadow" && !required_bool("layerConceals")?
    {
        return Err("Global-light, antialiased or non-knockout shadows remain protected".into());
    }
    let unit = |key: &str, unit: &str| -> Result<f64, String> {
        let value = field(fields, key);
        if value["type"] != "UntF" || value["value"][0] != unit {
            return Err("Invalid Photoshop shadow unit".into());
        }
        value["value"][1]
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or("Invalid Photoshop shadow number".into())
    };
    let angle = unit("lagl", "#Ang")?;
    let distance = unit("Dstn", "#Pxl")?;
    if !(-180. ..=180.).contains(&angle)
        || angle.fract() != 0.
        || !(0.0..=128.).contains(&distance)
        || distance.fract() != 0.
        || unit("blur", "#Pxl")? != 0.
        || unit("Ckmt", "#Pxl")? != 0.
        || percent(field(fields, "Nose"))? != 0.
    {
        return Err("Only integer-angle, integer-distance shadows with zero size, choke and noise can be imported".into());
    }
    let contour = descriptor(field(fields, "TrnS"), "ShpC")?;
    if contour
        .keys()
        .any(|key| !matches!(key.as_str(), "Nm  " | "Crv "))
        || contour.get("Nm  ").is_some_and(|v| v["type"] != "TEXT")
        || field(contour, "Crv ")["type"] != "VlLs"
    {
        return Err("Hard shadow requires an explicit linear contour".into());
    }
    let points = field(contour, "Crv ")["value"]
        .as_array()
        .filter(|v| v.len() == 2)
        .ok_or("Hard shadow requires two contour endpoints")?;
    for (point, expected) in points.iter().zip([0., 255.]) {
        let point = descriptor(point, "CrPt")?;
        if point.len() != 2
            || plain_number(field(point, "Hrzn"))? != expected
            || plain_number(field(point, "Vrtc"))? != expected
        {
            return Err("Nonlinear or malformed Photoshop shadow contour remains protected".into());
        }
    }
    let color = rgb_color(field(fields, "Clr "))?;
    let blend = overlay_blend(field(fields, "Md  "))?;
    if kind == "drop_shadow" && blend != "normal" {
        return Err("Independent Photoshop Drop Shadow backdrop blends remain protected".into());
    }
    // Photoshop's angle locates the light; native angle locates displacement.
    let cardinal = [-180., -90., 0., 90., 180.].contains(&angle);
    let angle = (angle + 360.).rem_euclid(360.) - 180.;
    let mut settings = json!({"opacity":percent(field(fields,"Opct"))?,"color":color,"angle":angle,"distance":distance,"size":0.,"spread":0.,"contour":"linear"});
    if !cardinal {
        settings["pixel_snap"] = json!(true);
    }
    if kind == "inner_shadow" {
        settings["blend"] = json!(blend);
    }
    crate::effects::validate(kind, &settings)?;
    Ok(crate::effects::Effect {
        id: crate::engine::id(),
        kind: kind.into(),
        enabled: master && enabled && present,
        weight: 1.,
        settings,
    })
}
fn pattern_overlay(
    root: &Value,
    patterns: &[super::patterns::Source],
    depth: u16,
) -> Result<(crate::effects::Effect, usize), String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    if !matches!(root["class"].as_str(), Some("null" | "Lefx")) {
        return Err("Unsupported Photoshop effects class".into());
    }
    let items = root["items"].as_object().ok_or("Invalid effects object")?;
    if items
        .keys()
        .any(|k| !matches!(k.as_str(), "masterFXSwitch" | "Scl " | "patternFill"))
        || items.get("Scl ").is_some_and(|v| percent(v) != Ok(1.))
    {
        return Err("Only a single unscaled plain Pattern Overlay can be imported".into());
    }
    let master = boolean(field(items, "masterFXSwitch")).ok_or("Missing style master switch")?;
    let fields = descriptor(field(items, "patternFill"), "patternFill")?;
    if fields.keys().any(|k| {
        !matches!(
            k.as_str(),
            "enab"
                | "present"
                | "showInDialog"
                | "Md  "
                | "Opct"
                | "Ptrn"
                | "Angl"
                | "Scl "
                | "Algn"
                | "Lnkd"
                | "phase"
        )
    }) {
        return Err("Unsupported Pattern Overlay property".into());
    }
    let enabled = boolean(field(fields, "enab")).ok_or("Missing pattern enabled state")?;
    let present = boolean(field(fields, "present")).ok_or("Missing pattern present state")?;
    let blend = overlay_blend(field(fields, "Md  "))?;
    if fields
        .get("showInDialog")
        .is_some_and(|v| boolean(v).is_none())
        // Unrotated pattern definitions may omit Angle. It defaults to zero
        // in the primary effect reader; an explicit value must remain strict.
        || fields.get("Angl").is_some_and(|angle| {
            angle["type"] != "UntF" || angle["value"] != json!(["#Ang", 0.])
        })
        || percent(field(fields, "Scl "))? != 1.
        || boolean(field(fields, "Algn")) != Some(false)
        || fields
            .get("Lnkd")
            .is_some_and(|v| boolean(v) != Some(false))
    {
        return Err("Only unrotated, unscaled, document-aligned patterns can be imported".into());
    }
    let phase = descriptor(field(fields, "phase"), "Pnt ")?;
    if phase.len() != 2
        || plain_number(field(phase, "Hrzn"))? != 0.
        || plain_number(field(phase, "Vrtc"))? != 0.
    {
        return Err("Offset Photoshop patterns remain protected".into());
    }
    let pattern = descriptor(field(fields, "Ptrn"), "Ptrn")?;
    if pattern
        .keys()
        .any(|k| !matches!(k.as_str(), "Nm  " | "Idnt"))
        || pattern.get("Nm  ").is_some_and(|v| v["type"] != "TEXT")
        || field(pattern, "Idnt")["type"] != "TEXT"
    {
        return Err("Unsupported pattern identity fields".into());
    }
    let identity = field(pattern, "Idnt")["value"]
        .as_str()
        .ok_or("Invalid pattern identity")?;
    let identity = identity.strip_suffix('\0').unwrap_or(identity);
    let matches = patterns
        .iter()
        .enumerate()
        .filter(|(_, p)| p.pattern_id == identity)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err("Missing or ambiguous embedded pattern identity".into());
    }
    let (index, source) = matches[0];
    // Pattern channels carry their own precision. An 8-bit tile is exactly
    // representable in a 16-bit document; retain its PNG8 source rather than
    // projecting the document or pretending the tile contains native low bits.
    if !(source.bit_depth == depth || source.bit_depth == 8 && depth == 16) {
        return Err("Pattern precision requires protected interpretation".into());
    }
    // The asset rectangle is its storage frame, not document placement. The
    // decoder checks matching full channel rectangles and extracts local rows.
    // Retain those tile samples without applying the storage origin as phase.
    let png = if source.bit_depth == 16 {
        crate::raster::png16(source.width, source.height, &source.pixels.rgba16())?
    } else {
        crate::raster::png(source.width, source.height, &source.pixels.rgba())?
    };
    let settings = json!({"opacity":percent(field(fields, "Opct"))?,"blend":blend,"pattern":"bitmap","bitmap_png":STANDARD.encode(png),"bitmap_scale":1.,"bitmap_angle":0.,"phase_x":0.,"phase_y":0.,"sampling":"nearest"});
    crate::effects::validate("pattern_overlay", &settings)?;
    Ok((
        crate::effects::Effect {
            id: crate::engine::id(),
            kind: "pattern_overlay".into(),
            enabled: master && enabled && present,
            weight: 1.,
            settings,
        },
        index,
    ))
}
fn descriptor<'a>(value: &'a Value, class: &str) -> Result<&'a Map<String, Value>, String> {
    if !matches!(value["type"].as_str(), Some("Objc" | "GlbO")) || value["value"]["class"] != class
    {
        return Err("Unsupported Photoshop style descriptor class".into());
    }
    value["value"]["items"]
        .as_object()
        .ok_or("Invalid Photoshop style properties".into())
}
fn field<'a>(fields: &'a Map<String, Value>, key: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    fields.get(key).unwrap_or(&NULL)
}
fn gradient_overlay(root: &Value) -> Result<Vec<crate::effects::Effect>, String> {
    if !matches!(root["class"].as_str(), Some("null" | "Lefx")) {
        return Err("Unsupported Photoshop effects class".into());
    }
    let items = root["items"].as_object().ok_or("Invalid effects object")?;
    if items.keys().any(|k| {
        !matches!(
            k.as_str(),
            "masterFXSwitch" | "Scl " | "GrFl" | "gradientFillMulti"
        )
    }) || items.contains_key("GrFl") && items.contains_key("gradientFillMulti")
    {
        return Err("Only a single unambiguous plain Gradient Overlay can be imported".into());
    }
    let master = items
        .get("masterFXSwitch")
        .and_then(boolean)
        .ok_or("Missing style master switch")?;
    if items.get("Scl ").is_some_and(|v| percent(v) != Ok(1.)) {
        return Err("Scaled Photoshop styles remain protected".into());
    }
    let value = items
        .get("GrFl")
        .or_else(|| items.get("gradientFillMulti"))
        .ok_or("Missing Gradient Overlay")?;
    let value = if value["type"] == "VlLs" {
        let list = value["value"]
            .as_array()
            .filter(|v| v.len() == 1)
            .ok_or("Multiple Photoshop Gradient Overlay ordering remains protected")?;
        &list[0]
    } else {
        value
    };
    let fields = descriptor(value, "GrFl")?;
    if fields.keys().any(|k| {
        !matches!(
            k.as_str(),
            "enab"
                | "present"
                | "showInDialog"
                | "Md  "
                | "Opct"
                | "Grad"
                | "Angl"
                | "Type"
                | "Rvrs"
                | "Dthr"
                | "Algn"
                | "Scl "
                | "Ofst"
        )
    }) {
        return Err("Unsupported Photoshop gradient property/interpolation method".into());
    }
    let required_bool = |key: &str| {
        fields
            .get(key)
            .and_then(boolean)
            .ok_or_else(|| format!("Invalid Gradient Overlay {key}"))
    };
    let enabled = required_bool("enab")?;
    let present = required_bool("present")?;
    if fields
        .get("showInDialog")
        .is_some_and(|v| boolean(v).is_none())
    {
        return Err("Invalid Gradient Overlay dialog flag".into());
    }
    if required_bool("Dthr")? || required_bool("Algn")? {
        return Err("Dithered or layer-aligned Photoshop gradients remain protected".into());
    }
    let blend = overlay_blend(field(fields, "Md  "))?;
    if field(fields, "Type")["type"] != "enum" || field(fields, "Type")["value"][0] != "GrdT" {
        return Err("Invalid Photoshop Gradient Overlay shape enumeration".into());
    }
    let style = match field(fields, "Type")["value"][1].as_str() {
        Some("Lnr ") => "linear",
        Some("Rflc") => "reflected",
        _ => return Err("Unsupported Photoshop Gradient Overlay shape".into()),
    };
    let angle = field(fields, "Angl");
    let degrees = angle["value"][1]
        .as_f64()
        .ok_or("Missing Photoshop gradient angle")?;
    if angle["type"] != "UntF"
        || angle["value"][0] != "#Ang"
        || ![-180., -90., 0., 90., 180.].contains(&degrees)
    {
        return Err("Only cardinal-axis Photoshop gradients can be imported".into());
    }
    let scale = bounded_percent(field(fields, "Scl "), 10., 1000.)?;
    let offset = if let Some(offset) = fields.get("Ofst") {
        let offset = descriptor(offset, "Pnt ")?;
        if offset.len() != 2 {
            return Err("Gradient offset requires exactly two percentage coordinates".into());
        }
        Some((
            bounded_percent(field(offset, "Hrzn"), -100., 100.)?,
            bounded_percent(field(offset, "Vrtc"), -100., 100.)?,
        ))
    } else {
        None
    };
    let gradient = descriptor(field(fields, "Grad"), "Grdn")?;
    if gradient
        .keys()
        .any(|k| !matches!(k.as_str(), "Nm  " | "GrdF" | "Intr" | "Clrs" | "Trns"))
        || gradient.get("Nm  ").is_some_and(|v| v["type"] != "TEXT")
        || field(gradient, "GrdF")["type"] != "enum"
        || field(gradient, "GrdF")["value"] != json!(["GrdF", "CstS"])
        || plain_number(field(gradient, "Intr"))? != 0.
    {
        return Err(
            "Noise, smooth or unsupported Photoshop gradient definitions remain protected".into(),
        );
    }
    let mut settings = json!({"opacity":percent(field(fields, "Opct"))? ,"blend":blend,"angle":degrees,"scale":scale,"style":style,"reverse":required_bool("Rvrs")?});
    if let Some((x, y)) = offset {
        settings["offset_x"] = json!(x);
        settings["offset_y"] = json!(y);
    }
    for (input, output, class, alpha) in [
        ("Clrs", "color_stops", "Clrt", false),
        ("Trns", "alpha_stops", "TrnS", true),
    ] {
        // An omitted transparency list means opaque coverage. Explicit empty
        // or malformed lists remain invalid, rather than silently defaulting.
        if alpha && !gradient.contains_key(input) {
            settings[output] = json!([]);
            continue;
        }
        let list = field(gradient, input);
        if list["type"] != "VlLs" {
            return Err("Gradient stops require descriptor lists".into());
        }
        let list = list["value"]
            .as_array()
            .filter(|s| (1..=16).contains(&s.len()))
            .ok_or("Gradient requires 1–16 source stops")?;
        let mut stops = vec![];
        let mut previous = -1.;
        for entry in list {
            let stop = descriptor(entry, class)?;
            if stop.keys().any(|k| {
                !matches!(k.as_str(), "Lctn" | "Mdpn")
                    && if alpha {
                        k != "Opct"
                    } else {
                        k != "Clr " && k != "Type"
                    }
            }) || field(stop, "Lctn")["type"] != "long"
                || field(stop, "Mdpn")["type"] != "long"
                || !(1. ..=99.).contains(&plain_number(field(stop, "Mdpn"))?)
            {
                return Err("Unsupported Photoshop gradient stop fields/midpoint".into());
            }
            // ASL midpoints are integer percentages of the following segment.
            // Krita's ASL parser creates INTERP_LINEAR segments with this ratio;
            // the shared native gradient sampler uses the same piecewise rule.
            let position = plain_number(field(stop, "Lctn"))? / 4096.;
            if !(0.0..=1.).contains(&position) || position <= previous {
                return Err("Photoshop gradient stops must strictly increase within 0–4096".into());
            }
            previous = position;
            let mut result = json!({
                "position":position,
                "midpoint":plain_number(field(stop, "Mdpn"))?/100.
            });
            if alpha {
                result["opacity"] = json!(percent(field(stop, "Opct"))?);
            } else {
                if field(stop, "Type")["type"] != "enum"
                    || field(stop, "Type")["value"] != json!(["Clry", "UsrS"])
                {
                    return Err(
                        "Foreground/background Photoshop gradient colors remain protected".into(),
                    );
                }
                result["color"] = json!(rgb_color(field(stop, "Clr "))?);
            }
            stops.push(result);
        }
        // Photoshop extends the nearest stop outside its range, including a
        // one-stop constant ramp. Native controls require explicit endpoints;
        // retain every original stop and add only equivalent constant edges.
        if stops[0]["position"] != 0. {
            let mut edge = stops[0].clone();
            edge["position"] = json!(0.);
            edge["midpoint"] = json!(0.5);
            stops.insert(0, edge);
        }
        if stops.last().unwrap()["position"] != 1. {
            let mut edge = stops.last().unwrap().clone();
            edge["position"] = json!(1.);
            edge["midpoint"] = json!(0.5);
            stops.push(edge);
        }
        if stops.len() > 16 {
            return Err("Photoshop gradient endpoints exceed the shared 16-stop limit".into());
        }
        settings[output] = json!(stops);
    }
    crate::effects::validate("gradient_overlay", &settings)?;
    Ok(vec![crate::effects::Effect {
        id: crate::engine::id(),
        kind: "gradient_overlay".into(),
        enabled: master && enabled && present,
        weight: 1.,
        settings,
    }])
}
/// Small explicit import subset. The caller must verify the entire saved composite.
fn color_overlay_root(root: &Value) -> Result<Vec<crate::effects::Effect>, String> {
    if !matches!(root["class"].as_str(), Some("null" | "Lefx")) {
        return Err("Unsupported Photoshop effects class".into());
    }
    let items = root["items"].as_object().ok_or("Invalid effects object")?;
    let master = items
        .get("masterFXSwitch")
        .and_then(boolean)
        .ok_or("Missing style master switch")?;
    if let Some(scale) = items.get("Scl ") {
        if percent(scale)? != 1. {
            return Err("Scaled Photoshop styles remain protected".into());
        }
    }
    if items.keys().any(|k| {
        !matches!(
            k.as_str(),
            "masterFXSwitch" | "Scl " | "SoFi" | "solidFillMulti"
        )
    }) || items.contains_key("SoFi") && items.contains_key("solidFillMulti")
    {
        return Err("Only unambiguous plain Color Overlay definitions can be imported".into());
    }
    let source = items
        .get("SoFi")
        .or_else(|| items.get("solidFillMulti"))
        .ok_or("No Color Overlay definitions")?;
    let entries = if source["type"] == "VlLs" {
        source["value"]
            .as_array()
            .ok_or("Invalid Color Overlay list")?
            .iter()
            .collect::<Vec<_>>()
    } else {
        vec![source]
    };
    if entries.is_empty() || entries.len() > 32 {
        return Err("Color Overlay requires 1–32 definitions".into());
    }
    // Photoshop multi-effect ordering is not guessed. Preserve a single definition only.
    if entries.len() != 1 {
        return Err("Multiple Photoshop Color Overlay ordering remains protected".into());
    }
    let mut effects = vec![];
    for entry in entries {
        if !matches!(entry["type"].as_str(), Some("Objc" | "GlbO"))
            || entry["value"]["class"] != "SoFi"
        {
            return Err("Invalid Color Overlay descriptor class".into());
        }
        let fields = entry["value"]["items"]
            .as_object()
            .ok_or("Invalid Color Overlay properties")?;
        if fields.keys().any(|k| {
            !matches!(
                k.as_str(),
                "enab" | "present" | "showInDialog" | "Md  " | "Opct" | "Clr "
            )
        }) {
            return Err("Unsupported Color Overlay property".into());
        }
        let enabled = fields
            .get("enab")
            .and_then(boolean)
            .ok_or("Missing Color Overlay enabled state")?;
        let present = fields
            .get("present")
            .and_then(boolean)
            .ok_or("Missing Color Overlay present state")?;
        if fields
            .get("showInDialog")
            .is_some_and(|v| boolean(v).is_none())
        {
            return Err("Invalid Color Overlay dialog state".into());
        }
        let blend = overlay_blend(field(fields, "Md  "))?;
        let opacity = percent(&entry["value"]["items"]["Opct"])?;
        let rgb = rgb_color(field(fields, "Clr "))?;
        let settings = json!({"opacity":opacity,"blend":blend,"color":rgb});
        crate::effects::validate("color_overlay", &settings)?;
        effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "color_overlay".into(),
            enabled: master && enabled && present,
            weight: 1.,
            settings,
        });
    }
    Ok(effects)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(out: &mut Vec<u8>, name: &str) {
        out.extend(
            (if name.len() == 4 {
                0
            } else {
                name.len() as u32
            })
            .to_be_bytes(),
        );
        out.extend(name.as_bytes());
    }
    fn descriptor(class: &str, items: Vec<(&str, &[u8; 4], Vec<u8>)>) -> Vec<u8> {
        let mut out = 1u32.to_be_bytes().to_vec();
        out.extend([0, 0]);
        id(&mut out, class);
        out.extend((items.len() as u32).to_be_bytes());
        for (key, kind, data) in items {
            id(&mut out, key);
            out.extend(kind);
            out.extend(data);
        }
        out
    }
    fn effect(enabled: bool) -> Vec<u8> {
        descriptor(
            "DrSh",
            vec![
                ("enab", b"bool", vec![u8::from(enabled)]),
                ("present", b"bool", vec![1]),
                ("showInDialog", b"bool", vec![1]),
            ],
        )
    }
    fn payload(master: bool) -> Vec<u8> {
        let mut list = 2u32.to_be_bytes().to_vec();
        for enabled in [true, false] {
            list.extend(b"Objc");
            list.extend(effect(enabled));
        }
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(descriptor(
            "null",
            vec![
                ("masterFXSwitch", b"bool", vec![u8::from(master)]),
                ("dropShadowMulti", b"VlLs", list),
                (
                    "SoFi",
                    b"Objc",
                    descriptor("SoFi", vec![("enab", b"bool", vec![0])]),
                ),
            ],
        ));
        out
    }
    #[test]
    fn descriptors_report_multiple_and_disabled_definitions_without_creating_effects() {
        for master in [true, false] {
            let bytes = payload(master);
            let report = inspect(&bytes).unwrap();
            assert!(report.iter().any(|v|v=="Photoshop Drop Shadow: 2 definitions; 1 enabled, 1 disabled, 0 unspecified; 2 marked present"));
            assert!(report
                .iter()
                .any(|v| v.contains("Color Overlay: 1 definitions; 0 enabled, 1 disabled")));
            assert_eq!(
                report[0],
                format!(
                    "Photoshop style master switch: {}",
                    if master { "enabled" } else { "disabled" }
                )
            );
            for padding in 0..=3 {
                let mut padded = bytes.clone();
                padded.extend(vec![0; padding]);
                assert_eq!(inspect(&padded).unwrap(), report);
            }
        }
    }
    #[test]
    fn truncated_or_malformed_descriptors_are_bounded_errors_not_panics() {
        let original = payload(true);
        for end in 0..original.len() {
            assert!(inspect(&original[..end]).is_err(), "{end}");
        }
        let mut invalid = original.clone();
        invalid[4..8].copy_from_slice(&17u32.to_be_bytes());
        assert!(inspect(&invalid).is_err());
        let mut invalid = original.clone();
        invalid.push(1);
        assert!(inspect(&invalid).is_err());
        let mut invalid = original.clone();
        invalid.extend([0; 4]);
        assert!(inspect(&invalid).is_err());
        let mut invalid = original.clone();
        invalid[12..14].copy_from_slice(&0xd800u16.to_be_bytes());
        assert!(inspect(&invalid).unwrap_err().contains("Unicode"));
        for value in [2, 255] {
            let mut invalid = original.clone();
            let at = invalid.windows(4).position(|v| v == b"bool").unwrap() + 4;
            invalid[at] = value;
            assert!(inspect(&invalid).unwrap_err().contains("boolean"));
        }
        let mut invalid = original.clone();
        let at = invalid.windows(4).position(|v| v == b"bool").unwrap();
        invalid[at..at + 4].copy_from_slice(b"obj ");
        assert!(inspect(&invalid).unwrap_err().contains("value type"));
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(descriptor(
            "null",
            vec![("same", b"bool", vec![0]), ("same", b"bool", vec![1])],
        ));
        assert!(inspect(&out).unwrap_err().contains("duplicate"));
    }
    #[test]
    fn unknown_properties_and_opaque_content_stay_protected_and_do_not_expose_values() {
        let mut raw = 7u32.to_be_bytes().to_vec();
        raw.extend(b"private");
        let mut units = b"#Prc".to_vec();
        units.extend(80f64.to_be_bytes());
        let mut text = 7u32.to_be_bytes().to_vec();
        for c in "private".encode_utf16() {
            text.extend(c.to_be_bytes());
        }
        let mut enum_value = vec![];
        id(&mut enum_value, "BlnM");
        id(&mut enum_value, "Nrml");
        let values = descriptor(
            "FutureStyle",
            vec![
                ("data", b"tdta", raw.clone()),
                ("alias", b"alis", raw),
                ("TEXT", b"TEXT", text),
                ("large", b"comp", (-7i64).to_be_bytes().to_vec()),
                ("unit", b"UntF", units),
                ("enum", b"enum", enum_value),
                ("number", b"doub", 2.5f64.to_be_bytes().to_vec()),
                ("integer", b"long", (-3i32).to_be_bytes().to_vec()),
                ("global", b"GlbO", effect(true)),
            ],
        );
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(descriptor("null", vec![("FutureStyle", b"Objc", values)]));
        let report = inspect(&out).unwrap().join("\n");
        assert!(report.contains("Unrecognized Photoshop style property: FutureStyle"));
        assert!(!report.contains("private"));
        let at = out.windows(4).position(|v| v == b"doub").unwrap() + 4;
        out[at..at + 8].copy_from_slice(&f64::NAN.to_be_bytes());
        assert!(inspect(&out).unwrap_err().contains("non-finite"));
    }
    #[test]
    fn descriptor_byte_depth_item_and_identifier_budgets_reject_before_unbounded_allocation() {
        assert!(inspect(&vec![0; 1024 * 1024 + 1])
            .unwrap_err()
            .contains("1 MiB"));
        let mut nested = descriptor("null", vec![]);
        for _ in 0..34 {
            nested = descriptor("null", vec![("nest", b"Objc", nested)]);
        }
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(nested);
        assert!(inspect(&out).unwrap_err().contains("budget"));
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(4097u32.to_be_bytes());
        assert!(inspect(&out).unwrap_err().contains("code units"));
        let mut list = 257u32.to_be_bytes().to_vec();
        list.extend([0; 4]);
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(descriptor("null", vec![("list", b"VlLs", list)]));
        assert!(inspect(&out).unwrap_err().contains("256 items"));
        let mut invalid = payload(true);
        let at = invalid
            .windows(14)
            .position(|v| v == b"masterFXSwitch")
            .unwrap();
        invalid[at..at + 1].copy_from_slice(&[0]);
        assert!(inspect(&invalid).unwrap_err().contains("identifier"));
        let names = (0..17).map(|i| format!("list-{i}")).collect::<Vec<_>>();
        let mut list = 256u32.to_be_bytes().to_vec();
        for _ in 0..256 {
            list.extend(b"bool");
            list.push(1);
        }
        let items = names
            .iter()
            .map(|n| (n.as_str(), b"VlLs", list.clone()))
            .collect();
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(descriptor("null", items));
        assert!(inspect(&out).unwrap_err().contains("item budget"));
    }

    fn replace_style(
        bytes: &[u8],
        depth: u16,
        layout: super::super::Layout,
        payload: &[u8],
    ) -> Vec<u8> {
        use super::super::{tag, Layout};
        let mut r = Cursor::new(bytes);
        r.bytes(26).unwrap();
        r.block().unwrap();
        r.block().unwrap();
        let section_length = r.pos;
        let section = layout.block(&mut r).unwrap();
        let section_start = r.pos - section.len();
        let mut info = Cursor::new(section);
        let (length_at, start) = if depth == 8 {
            let data = layout.block(&mut info).unwrap();
            (section_start, section_start + info.pos - data.len())
        } else {
            layout.block(&mut info).unwrap();
            info.block().unwrap();
            assert_eq!(info.bytes(4).unwrap(), b"8BIM");
            assert_eq!(info.bytes(4).unwrap(), b"Lr16");
            let length = section_start + info.pos;
            let data = layout.tag_block(&mut info, b"Lr16").unwrap();
            (length, section_start + info.pos - data.len())
        };
        let mut layer = Cursor::new(&bytes[start..]);
        assert_eq!(layer.i16().unwrap(), -1);
        layer.bytes(16).unwrap();
        let channels = layer.u16().unwrap();
        for _ in 0..channels {
            layer.i16().unwrap();
            layer.length(layout.large()).unwrap();
        }
        layer.bytes(12).unwrap();
        let extra_length = start + layer.pos;
        let extra = layer.block().unwrap();
        let extra_start = start + layer.pos - extra.len();
        let at = extra.windows(8).position(|v| v == b"8BIMfxrp").unwrap() + extra_start;
        let old_len = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
        let mut replacement = vec![];
        tag(&mut replacement, b"lmfx", payload);
        let change = replacement.len() as i64 - (12 + old_len + (old_len % 2)) as i64;
        let mut result = bytes.to_vec();
        result.splice(at..at + 12 + old_len + (old_len % 2), replacement);
        let mut update = |at: usize, wide: bool| {
            let size = if wide { 8 } else { 4 };
            let original = u64::from_be_bytes({
                let mut v = [0; 8];
                v[8 - size..].copy_from_slice(&bytes[at..at + size]);
                v
            });
            let value = (original as i64 + change) as u64;
            result[at..at + size].copy_from_slice(&value.to_be_bytes()[8 - size..]);
        };
        update(extra_length, false);
        update(length_at, layout == Layout::Psb);
        update(section_length, layout == Layout::Psb);
        result
    }
    #[test]
    fn modern_styles_keep_psd_psb_native_composites_and_source_reports_protected() {
        use crate::{
            engine::{Document, Engine},
            psd, server,
        };
        for depth in [8, 16] {
            for layout in [super::super::Layout::Psd, super::super::Layout::Psb] {
                let mut doc = Document::new_depth(3, 2, depth).unwrap();
                doc.layers[0]
                    .pixels
                    .set16(1, 1, [12347, 23459, 34571, 65535]);
                doc.layers[0].psd_metadata = vec![psd::LayerMetadata {
                    key: "fxrp".into(),
                    data: vec![0; 16],
                }];
                let baseline = doc.export_png().unwrap();
                let encoded = if layout == super::super::Layout::Psd {
                    psd::encode(&doc)
                } else {
                    psd::encode_psb(&doc)
                }
                .unwrap();
                for malformed in [false, true] {
                    let mut descriptor = payload(true);
                    descriptor.resize((descriptor.len() + 3) & !3, 0);
                    if malformed {
                        descriptor[4..8].copy_from_slice(&99u32.to_be_bytes());
                    }
                    let original = replace_style(&encoded, depth, layout, &descriptor);
                    let protected = psd::decode(&original).unwrap();
                    assert!(protected.read_only);
                    assert_eq!(protected.bit_depth, depth);
                    assert!(protected.layers[0].effects.is_empty());
                    assert_eq!(protected.export_png().unwrap(), baseline);
                    assert!(protected.source_structure[0].tags.contains(&"lmfx".into()));
                    assert!(
                        protected.source_structure[0]
                            .warnings
                            .iter()
                            .any(|v| v.contains(if malformed {
                                "Cannot inspect"
                            } else {
                                "Drop Shadow: 2 definitions"
                            })),
                        "{:?}",
                        protected.warnings
                    );
                    assert!(psd::encode(&protected).is_err());
                    let mut engine = Engine::new();
                    engine.doc = protected;
                    let layer = engine.doc.layers[0].id.clone();
                    assert!(engine
                        .edit(
                            "human",
                            &[json!({"op":"layer.delete","layer":layer})],
                            None,
                            None,
                            "Refuse protected edits"
                        )
                        .is_err());
                    let shared = std::sync::Arc::new(std::sync::Mutex::new(engine));
                    let id = shared.lock().unwrap().doc.id.clone();
                    let report = server::dispatch(
                        &shared,
                        "document",
                        &json!({"action":"save_report","document_id":id,"expected_revision":0}),
                    )
                    .unwrap();
                    assert_eq!(report["save_allowed"], false);
                    assert_eq!(shared.lock().unwrap().doc.export_png().unwrap(), baseline);
                    assert!(shared.lock().unwrap().undo.is_empty());
                    assert_eq!(
                        psd::decode(&original).unwrap().export_png().unwrap(),
                        baseline
                    );
                }
            }
        }
    }
}
