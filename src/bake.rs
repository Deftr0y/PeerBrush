//! Explicit, atomic rasterization through the same native renderers as the canvas.
use crate::{
    engine::{id, Document, MaskStep},
    raster::Raster,
};
use serde_json::Value;

pub(crate) fn apply(doc: &mut Document, index: usize, c: &Value) -> Result<(), String> {
    let before = doc.clone();
    let target = before.layers[index].id.clone();
    if c["op"] == "effect.bake" {
        color(doc, index, c)?;
    } else {
        mask(doc, index, c)?;
    }
    crate::effects::invalidate(doc, std::slice::from_ref(c));
    // Check hidden/occluded targets as well as the current composite. Inexact
    // prefixes remain editable; the enclosing engine transaction rolls back.
    for channel in [false, true] {
        if channel && before.layers[index].mask.is_none() {
            continue;
        }
        let edge = doc.width.max(doc.height);
        let equal = if doc.bit_depth == 16 {
            crate::depth16::preview16(&before, None, edge, Some(&target), channel)?.2
                == crate::depth16::preview16(doc, None, edge, Some(&target), channel)?.2
        } else {
            before
                .preview_native8(None, edge, Some(&target), channel)?
                .2
                == doc.preview_native8(None, edge, Some(&target), channel)?.2
        };
        if !equal {
            return Err(inexact());
        }
    }
    if isolated(&before, &target)?.export_png()? != isolated(doc, &target)?.export_png()?
        || before.export_png()? != doc.export_png()?
    {
        return Err(inexact());
    }
    Ok(())
}
fn inexact() -> String {
    "This prefix cannot be baked without changing native samples. Bake a higher effect to include its dependencies, or keep the stack editable.".into()
}
fn isolated(doc: &Document, target: &str) -> Result<Document, String> {
    let mut out = doc.clone();
    let tree = crate::transform::tree_ids(doc, &[target.to_owned()]);
    for l in &mut out.layers {
        if !tree.contains(&l.id) {
            l.visible = false;
        }
        if l.id == target {
            l.visible = true;
            l.parent = None;
            l.clip_to = None;
            l.opacity = 1.;
            l.blend = "normal".into();
        }
    }
    out.filters.clear();
    crate::effects::invalidate(&mut out, &[serde_json::json!({"op":"filter.delete"})]);
    Ok(out)
}
fn color(doc: &mut Document, index: usize, c: &Value) -> Result<(), String> {
    let l = &doc.layers[index];
    if l.kind == "adjustment" {
        return Err("Adjustment effects depend on the backdrop; merge their affected layers explicitly instead".into());
    }
    let at = l
        .effects
        .iter()
        .position(|e| Some(e.id.as_str()) == c["effect"].as_str())
        .ok_or("Unknown effect")?;
    if !l.effects[..=at].iter().any(|e| e.enabled && e.weight > 0.) {
        doc.layers[index].effects.drain(..=at);
        return Ok(());
    }
    let tree = crate::transform::tree_ids(doc, &[l.id.clone()]);
    if doc.layers.iter().any(|l| tree.contains(&l.id) && l.locked) {
        return Err("Unlock the folder and all children before baking folder content".into());
    }
    let mut prefix = doc.clone();
    prefix.layers[index].effects.truncate(at + 1);
    prefix.layers[index].effect_key = id();
    let frame = crate::effects::canvas_frame(&prefix.layers[index]);
    let (w, h) = crate::effects::frame(&prefix, &prefix.layers[index]);
    if frame
        && l.mask.is_some()
        && (l.x != 0 || l.y != 0 || l.pixels.width != w || l.pixels.height != h)
    {
        return Err("Baking this expanding style would change the mask's frame. Keep it editable or explicitly merge the masked result.".into());
    }
    let pixels = if doc.bit_depth == 16 {
        let prepared = crate::depth16::prepare(&prefix)?;
        let image = prepared[index]
            .as_ref()
            .ok_or("Effect produced no native image")?;
        Raster::from_rgba16(w, h, &image.words)?
    } else {
        let masks = prefix
            .layers
            .iter()
            .map(|l| {
                l.mask
                    .as_ref()
                    .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
            })
            .collect::<Vec<_>>();
        let prepared = crate::effects::prepare(&prefix, &masks)?;
        let image = prepared[index].as_ref().ok_or("Effect produced no image")?;
        Raster::from_rgba(w, h, &image.bytes)?
    };
    let l = &mut doc.layers[index];
    l.effects.drain(..=at);
    l.pixels = pixels;
    l.source = None;
    l.kind = "paint".into();
    if frame {
        l.x = 0;
        l.y = 0;
    }
    let target = l.id.clone();
    doc.layers
        .retain(|l| l.id == target || !tree.contains(&l.id));
    Ok(())
}
fn mask(doc: &mut Document, index: usize, c: &Value) -> Result<(), String> {
    let l = &doc.layers[index];
    let m = l.mask.as_ref().ok_or("Layer has no mask")?;
    let at = m
        .steps
        .iter()
        .position(|s| Some(s.id.as_str()) == c["step"].as_str())
        .ok_or("Unknown mask step")?;
    if !m.steps[..=at].iter().any(|s| s.enabled && s.weight > 0.) {
        doc.layers[index].mask.as_mut().unwrap().steps.drain(..=at);
        return Ok(());
    }
    let quantized = m.needs_cache();
    let mut prefix = l.clone();
    prefix.mask.as_mut().unwrap().steps.truncate(at + 1);
    prefix.mask.as_mut().unwrap().cache_key = id();
    let (w, h) = (l.pixels.width, l.pixels.height);
    let mut pixels = Raster::new_depth(w, h, doc.bit_depth);
    if doc.bit_depth == 16 {
        let image = quantized
            .then(|| crate::depth16::mask::evaluate(prefix.mask.as_ref().unwrap(), w, h))
            .transpose()?;
        for y in 0..h {
            for x in 0..w {
                let value = (crate::depth16::mask_value_prepared(
                    &prefix,
                    x as i32,
                    y as i32,
                    image.as_ref(),
                    true,
                ) * 65535.)
                    .round() as u16;
                pixels.set16(x as i32, y as i32, [value, value, value, 65535]);
            }
        }
    } else {
        let image = quantized.then(|| crate::mask::evaluate(prefix.mask.as_ref().unwrap(), w, h));
        for y in 0..h {
            for x in 0..w {
                let value = (prefix.mask_value_prepared(x as i32, y as i32, image.as_ref(), true)
                    * 255.)
                    .round() as u8;
                pixels.set(x as i32, y as i32, [value, value, value, 255]);
            }
        }
    }
    let m = doc.layers[index].mask.as_mut().unwrap();
    m.steps.drain(..=at);
    m.steps.insert(
        0,
        MaskStep {
            id: id(),
            kind: "paint".into(),
            enabled: true,
            weight: 1.,
            value: 255.,
            pixels,
            settings: Value::Null,
        },
    );
    m.cache_key = id();
    Ok(())
}
