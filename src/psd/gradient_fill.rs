//! Exact standard Gradient Fill definitions, shared by both Photoshop writers.
use crate::engine::view::LayerView;
use serde_json::Value;
use std::io::{Cursor, Write};

pub(crate) fn standard(layer: LayerView<'_>, width: u32, height: u32) -> Option<&Value> {
    if layer.kind() != "fill" || layer.color()[3] != 255 || layer.source().is_some()
        || layer.origin() != [0, 0] || layer.pixels().width != width
        || layer.pixels().height != height || layer.effect_count() != 1
        || layer.blend() == "pass_through"
    { return None; }
    let effect = layer.effect(0)?;
    (effect.kind() == "gradient_overlay" && effect.enabled() && effect.weight() == 1.
        && representable(effect.settings())).then_some(effect.settings())
}

fn number(v: &Value, key: &str, default: f64) -> f64 {
    v.get(key).map_or(default, |n| n.as_f64().unwrap_or(f64::NAN))
}
fn exact_unit(value: f64, scale: f64, min: f64, max: f64, integer: bool) -> bool {
    value.is_finite() && (min..=max).contains(&value)
        && if integer { (value * scale).round() / scale == value } else { value * scale / scale == value }
}
pub(super) fn representable(v: &Value) -> bool {
    v["use_gradient_alpha"] == true && v["pixel_grid"] == true
        && number(v, "opacity", 0.75) == 1. && v["blend"].as_str().unwrap_or("normal") == "normal"
        && representable_fields(v)
}

pub(super) fn overlay_representable(v: &Value) -> bool {
    let pixel_grid = v["align_layer"] == true
        || matches!(v["style"].as_str(), Some("radial" | "angle" | "diamond"));
    v["use_gradient_alpha"] != true
        && v["pixel_grid"].as_bool().unwrap_or(false) == pixel_grid
        && exact_unit(number(v, "opacity", 0.75), 100., 0., 1., false)
        && representable_fields(v)
        && ["color_stops", "alpha_stops"].iter().all(|key| {
            v[key].as_array().and_then(|stops| stops.last())
                .is_none_or(|stop| number(stop, "midpoint", 0.5) == 0.5)
        })
}

fn representable_fields(v: &Value) -> bool {
    let Some(fields) = v.as_object() else { return false; };
    if fields.keys().any(|key| !matches!(key.as_str(), "opacity" | "blend" | "color" | "color2"
        | "angle" | "scale" | "style" | "reverse" | "color_stops" | "alpha_stops"
        | "offset_x" | "offset_y" | "pixel_grid" | "align_layer" | "smoothness" | "use_gradient_alpha"))
        || ["blend", "style"].iter().any(|key| v.get(key).is_some_and(|n| !n.is_string()))
        || number(v, "smoothness", 0.) != 0.
        || ![-180., -90., 0., 90., 180.].contains(&number(v, "angle", 90.))
        || !matches!(v["style"].as_str().unwrap_or("linear"), "linear" | "reflected" | "radial" | "angle" | "diamond")
        || ["reverse", "align_layer", "pixel_grid", "use_gradient_alpha"].iter().any(|key| v.get(key).is_some_and(|n| !n.is_boolean()))
        || !exact_unit(number(v, "scale", 1.), 100., 0.1, 10., false)
        || ["offset_x", "offset_y"].iter().any(|key| !exact_unit(number(v, key, 0.), 100., -1., 1., false))
    { return false; }
    for (key, alpha) in [("color_stops", false), ("alpha_stops", true)] {
        let Some(stops) = v[key].as_array() else { return false; };
        if alpha && stops.is_empty() { continue; }
        if !(2..=16).contains(&stops.len()) || stops[0]["position"] != 0.
            || stops.last().unwrap()["position"] != 1. { return false; }
        let mut previous = -1.;
        for stop in stops {
            let position = number(stop, "position", f64::NAN);
            if !exact_unit(position, 4096., 0., 1., true) || position <= previous
                || !exact_unit(number(stop, "midpoint", 0.5), 100., 0.01, 0.99, true)
                || stop.as_object().is_none_or(|s| s.keys().any(|key|
                    !matches!(key.as_str(), "position" | "midpoint") && key != if alpha { "opacity" } else { "color" }))
            { return false; }
            previous = position;
            if alpha {
                if !exact_unit(number(stop, "opacity", f64::NAN), 100., 0., 1., false) { return false; }
            } else if stop["color"].as_array().is_none_or(|rgb| rgb.len() != 3 || rgb.iter().any(|c|
                !exact_unit(c.as_f64().unwrap_or(f64::NAN), 255., 0., 1., false))) { return false; }
        }
    }
    true
}

// At most 16 RGB and 16 opacity stops. Fixed storage avoids an unadmitted
// descriptor allocation while native save planning measures or emits records.
pub(super) const MAX_DESCRIPTOR_BYTES: usize = 8192;
pub(super) struct Descriptor { bytes: [u8; MAX_DESCRIPTOR_BYTES], len: usize }
impl Descriptor { pub(super) fn bytes(&self) -> &[u8] { &self.bytes[..self.len] } }
pub(super) fn descriptor(v: &Value) -> Result<Descriptor, String> {
    if !representable(v) { return Err("Gradient Fill is outside the exact standard subset".into()); }
    let mut out = Descriptor { bytes: [0; MAX_DESCRIPTOR_BYTES], len: 0 };
    let mut w = Cursor::new(out.bytes.as_mut_slice());
    write(&mut w, &16u32.to_be_bytes())?;
    head(&mut w, b"null", 8)?;
    write_fields(&mut w, v)?;
    out.len = w.position() as usize;
    Ok(out)
}

/// Eight descriptor fields shared by Gradient Fill and Gradient Overlay.
pub(super) fn write_fields(w: &mut impl Write, v: &Value) -> Result<(), String> {
    entry(w, b"Grad", b"Objc")?;
    let colors = v["color_stops"].as_array().unwrap();
    let alphas = v["alpha_stops"].as_array().unwrap();
    head(w, b"Grdn", if alphas.is_empty() { 3 } else { 4 })?;
    enumeration(w, b"GrdF", b"GrdF", b"CstS")?;
    long(w, b"Intr", 0)?;
    for (key, class, stops, alpha) in [(b"Clrs", b"Clrt", colors, false), (b"Trns", b"TrnS", alphas, true)] {
        if alpha && stops.is_empty() { continue; }
        entry(w, key, b"VlLs")?;
        write(w, &(stops.len() as u32).to_be_bytes())?;
        for (index, stop) in stops.iter().enumerate() {
            write(w, b"Objc")?;
            head(w, class, if alpha { 3 } else { 4 })?;
            long(w, b"Lctn", (number(stop, "position", 0.) * 4096.) as i32)?;
            let midpoint = if index == 0 { 0.5 } else { number(&stops[index - 1], "midpoint", 0.5) };
            long(w, b"Mdpn", (midpoint * 100.).round() as i32)?;
            if alpha { unit(w, b"Opct", b"#Prc", number(stop, "opacity", 1.) * 100.)?; }
            else {
                enumeration(w, b"Type", b"Clry", b"UsrS")?;
                entry(w, b"Clr ", b"Objc")?;
                head(w, b"RGBC", 3)?;
                for (channel, value) in [b"Rd  ", b"Grn ", b"Bl  "].into_iter().zip(stop["color"].as_array().unwrap()) {
                    entry(w, channel, b"doub")?;
                    write(w, &(value.as_f64().unwrap() * 255.).to_be_bytes())?;
                }
            }
        }
    }
    unit(w, b"Angl", b"#Ang", number(v, "angle", 90.))?;
    let shape = match v["style"].as_str().unwrap_or("linear") {
        "reflected" => b"Rflc", "radial" => b"Rdl ", "angle" => b"Angl", "diamond" => b"Dmnd", _ => b"Lnr "
    };
    enumeration(w, b"Type", b"GrdT", shape)?;
    for (key, value) in [(b"Rvrs", v["reverse"] == true), (b"Dthr", false), (b"Algn", v["align_layer"] == true)] {
        entry(w, key, b"bool")?;
        write(w, &[u8::from(value)])?;
    }
    unit(w, b"Scl ", b"#Prc", number(v, "scale", 1.) * 100.)?;
    entry(w, b"Ofst", b"Objc")?;
    head(w, b"Pnt ", 2)?;
    unit(w, b"Hrzn", b"#Prc", number(v, "offset_x", 0.) * 100.)?;
    unit(w, b"Vrtc", b"#Prc", number(v, "offset_y", 0.) * 100.)?;
    Ok(())
}
fn write(w: &mut impl Write, bytes: &[u8]) -> Result<(), String> { w.write_all(bytes).map_err(|e| e.to_string()) }
fn id(w: &mut impl Write, key: &[u8; 4]) -> Result<(), String> { write(w, &[0; 4])?; write(w, key) }
fn head(w: &mut impl Write, class: &[u8; 4], count: u32) -> Result<(), String> {
    write(w, &[0; 4])?; id(w, class)?; write(w, &count.to_be_bytes())
}
fn entry(w: &mut impl Write, key: &[u8; 4], kind: &[u8; 4]) -> Result<(), String> { id(w, key)?; write(w, kind) }
fn long(w: &mut impl Write, key: &[u8; 4], value: i32) -> Result<(), String> { entry(w, key, b"long")?; write(w, &value.to_be_bytes()) }
fn unit(w: &mut impl Write, key: &[u8; 4], kind: &[u8; 4], value: f64) -> Result<(), String> {
    entry(w, key, b"UntF")?; write(w, kind)?; write(w, &value.to_be_bytes())
}
fn enumeration(w: &mut impl Write, key: &[u8; 4], class: &[u8; 4], value: &[u8; 4]) -> Result<(), String> {
    entry(w, key, b"enum")?; id(w, class)?; id(w, value)
}

#[cfg(test)]
pub(in crate::psd) mod tests {
    use super::*;
    use serde_json::json;
    pub(in crate::psd) fn settings() -> Value {
        json!({"opacity":1.,"blend":"normal","use_gradient_alpha":true,
            "pixel_grid":true,"align_layer":true,"angle":0.,"scale":1.,"style":"linear","reverse":false,
            "color_stops":[{"position":0.,"color":[0.,0.,0.],"midpoint":0.29},
                {"position":1.,"color":[1.,0.5,0.25],"midpoint":0.5}],
            "alpha_stops":[{"position":0.,"opacity":0.25,"midpoint":0.73},
                {"position":1.,"opacity":1.,"midpoint":0.5}]})
    }
    #[test]
    fn descriptor_keeps_exact_stops_midpoints_and_geometry_and_refuses_approximations() {
        for shape in ["linear", "reflected", "radial", "angle", "diamond"] {
            for angle in [-180., -90., 0., 90., 180.] {
                let mut value = settings();
                value["style"] = json!(shape);
                value["angle"] = json!(angle);
                let restored = super::super::styles::gradient_fill(descriptor(&value).unwrap().bytes()).unwrap();
                for key in value.as_object().unwrap().keys() {
                    assert_eq!(restored.settings[key], value[key], "{key}");
                }
            }
        }
        for (key, bad) in [("smoothness", json!(0.1)), ("pixel_grid", json!(false)),
            ("opacity", json!(0.5)), ("unknown", json!(true)), ("angle", json!(12.))] {
            let mut value = settings(); value[key] = bad;
            assert!(descriptor(&value).is_err(), "{key}");
        }
        let mut value = settings();
        value["color_stops"][1]["position"] = json!(0.12345);
        assert!(descriptor(&value).is_err());
        let mut value = settings();
        for key in ["color_stops", "alpha_stops"] {
            value[key] = json!((0..16).map(|index| {
                let mut stop = value[key][usize::from(index == 15)].clone();
                stop["position"] = json!(if index == 15 { 1. } else { index as f64 / 16. });
                stop
            }).collect::<Vec<_>>());
        }
        let payload = descriptor(&value).unwrap();
        assert!(payload.bytes().len() < 8192);
        let restored = super::super::styles::gradient_fill(payload.bytes()).unwrap();
        assert_eq!(restored.settings["color_stops"], value["color_stops"]);
        assert_eq!(restored.settings["alpha_stops"], value["alpha_stops"]);
    }

    #[test]
    fn earlier_raster_only_gradient_sources_restore_without_new_standard_tags() {
        use crate::{engine::Document, raster::Raster};
        use super::super::{decode, encode_layout, Layout};
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0].kind = "fill".into();
            doc.layers[0].color = [255; 4];
            doc.layers[0].effects.push(crate::effects::Effect {
                id: crate::engine::id(), kind: "gradient_overlay".into(), enabled: true, weight: 1.,
                settings: settings().into(),
            });
            let mut baked = Document::new_depth(2, 1, depth).unwrap();
            baked.layers[0].pixels = if depth == 16 {
                Raster::from_rgba16(2, 1, &crate::depth16::render(&doc).unwrap().words).unwrap()
            } else {
                Raster::from_rgba(2, 1, &doc.preview_native8(None, 2, None, false).unwrap().2).unwrap()
            };
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&baked, layout).unwrap();
                let old = super::super::adjustments::tests::replace_sources(&bytes, layout, &doc, 45);
                let restored = decode(&old).unwrap();
                assert!(!restored.read_only, "{:?}", restored.warnings);
                assert_eq!(restored.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
                assert_eq!(restored.export_png().unwrap(), doc.export_png().unwrap());
                let forged = super::super::adjustments::tests::replace_sources(&bytes, layout, &doc, 46);
                assert!(decode(&forged).unwrap().read_only);
            }
        }
    }
}
