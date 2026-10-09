use crate::{
    engine::{id, Document, Mask, MaskStep},
    raster::Raster,
};
use serde_json::Value;

pub fn apply(doc: &mut Document, index: usize, c: &Value) -> Result<(), String> {
    let layer = &doc.layers[index];
    let w = layer.pixels.width;
    let h = layer.pixels.height;
    let depth = layer.pixels.depth;
    if u64::from(w) * u64::from(h) > 16_000_000 {
        return Err("Mask refinement is limited to 16 megapixels".into());
    }
    if layer.mask.as_ref().is_some_and(|m| m.steps.len() >= 32) {
        return Err("Mask stack limit: 32 steps".into());
    }
    let x = layer.x;
    let y = layer.y;
    let native = if depth == 16 {
        crate::depth16::mask_image(doc, index)?
    } else {
        None
    };
    let prepared = layer.mask.as_ref().and_then(|m| m.prepare(w, h));
    let mut input = Raster::new_depth(w, h, depth);
    let selected = if c["op"] == "mask.from_selection" {
        Some(super::current(doc).ok_or("Select an area before creating its mask")?)
    } else {
        None
    };
    if selected.is_none() && layer.mask.is_none() {
        return Err("Add a mask before refining it".into());
    }
    let mode = c["mode"].as_str().unwrap_or("replace");
    if !["replace", "add", "subtract", "intersect"].contains(&mode) {
        return Err("Mask mode must be replace, add, subtract or intersect".into());
    }
    for yy in 0..h as i32 {
        for xx in 0..w as i32 {
            let old = native
                .as_ref()
                .map(|m| m.get(xx, yy)[0])
                .unwrap_or_else(|| {
                    (layer.mask_value_prepared(xx, yy, prepared.as_deref(), true) * 65535.).round()
                        as u16
                });
            let value = if let Some(selection) = &selected {
                let new = (selection.value(xx + x, yy + y) * 65535.).round() as u16;
                match mode {
                    "add" => old.max(new),
                    "subtract" => ((old as u64 * (65535 - new) as u64 + 32767) / 65535) as u16,
                    "intersect" => old.min(new),
                    _ => new,
                }
            } else {
                old
            };
            if value > 0 {
                input.set16(xx, yy, [value; 4]);
            }
        }
    }
    if selected.is_none() {
        let guide = if c["edge"].as_f64().unwrap_or(0.) > 0. {
            let region = [x, y, x + w as i32, y + h as i32];
            Some(
                doc.preview(Some(region), w.max(h), Some(&layer.id), false)?
                    .2,
            )
        } else {
            None
        };
        input = super::refine::run(&input, guide.as_deref(), c)?;
    }
    // Opaque grayscale replaces the evaluated mask; original editable steps remain
    // underneath for history/inspection. Alpha must stay opaque even where gray is zero.
    for yy in 0..h as i32 {
        for xx in 0..w as i32 {
            let v = input.get16(xx, yy)[0];
            input.set16(xx, yy, [v, v, v, 65535]);
        }
    }
    let layer = &mut doc.layers[index];
    let mask = layer.mask.get_or_insert_with(|| Mask {
        enabled: true,
        steps: vec![],
        cache_key: id(),
    });
    mask.steps.push(MaskStep {
        weight: 1.0,
        id: id(),
        kind: "paint".into(),
        enabled: true,
        value: 0.,
        pixels: input,
        settings: Value::Null,
    });
    mask.cache_key = id();
    Ok(())
}
