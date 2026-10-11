//! Bounded view-only object-effects inspection. Reading a descriptor never authorizes editing.
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
            b"TEXT" => {
                self.text()?;
                Value::Null
            }
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
        return Err("Color Overlay requires plain RGB channel numbers".into());
    }
    value["value"]
        .as_f64()
        .ok_or("Invalid Color Overlay number".into())
}
fn percent(value: &Value) -> Result<f64, String> {
    if value["type"] != "UntF" || value["value"][0] != "#Prc" {
        return Err("Color Overlay requires percentage units".into());
    }
    let n = value["value"][1]
        .as_f64()
        .ok_or("Invalid Color Overlay percentage")?;
    if !(0.0..=100.).contains(&n) {
        return Err("Color Overlay percentage is outside 0–100".into());
    }
    Ok(n / 100.)
}
/// Small explicit import subset. The caller must verify the entire saved composite.
pub(super) fn color_overlays(bytes: &[u8]) -> Result<Vec<crate::effects::Effect>, String> {
    let root = root(bytes)?;
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
        let mode = &entry["value"]["items"]["Md  "];
        if mode["type"] != "enum" || mode["value"][0] != "BlnM" {
            return Err("Invalid Photoshop Color Overlay blend enumeration".into());
        }
        let blend = match mode["value"][1].as_str() {
            Some("Nrml") => "normal",
            Some("Mltp") => "multiply",
            Some("Scrn") => "screen",
            Some("Ovrl") => "overlay",
            Some("Drkn") => "darken",
            Some("Lghn") => "lighten",
            _ => return Err("Unsupported Photoshop Color Overlay blend remains protected".into()),
        };
        let opacity = percent(&entry["value"]["items"]["Opct"])?;
        let color = &entry["value"]["items"]["Clr "];
        if !matches!(color["type"].as_str(), Some("Objc" | "GlbO"))
            || color["value"]["class"] != "RGBC"
        {
            return Err("Only RGB Photoshop Color Overlay colors can be imported".into());
        }
        let channels = color["value"]["items"]
            .as_object()
            .ok_or("Invalid RGB Color Overlay")?;
        if channels.len() != 3
            || channels
                .keys()
                .any(|k| !matches!(k.as_str(), "Rd  " | "Grn " | "Bl  "))
        {
            return Err("Unsupported Color Overlay RGB properties".into());
        }
        let mut rgb = vec![];
        for key in ["Rd  ", "Grn ", "Bl  "] {
            let n = plain_number(&channels[key])?;
            if !(0.0..=255.).contains(&n) {
                return Err("Color Overlay RGB channel is outside 0–255".into());
            }
            rgb.push(n / 255.);
        }
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
