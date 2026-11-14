use crate::{
    effects,
    engine::{id, Document, Layer, Mask, MaskStep},
    raster::Raster,
};
use serde_json::{json, Value};

pub(crate) fn definition(c: &Value) -> Result<(&str, Value), String> {
    let kind = c["kind"].as_str().unwrap_or("color_balance");
    if ![
        "color_balance",
        "hsl",
        "levels",
        "curves",
        "adjust",
        "invert",
        "grayscale",
        "posterize",
        "threshold",
        effects::legacy_brightness::KIND,
        effects::modern_brightness::KIND,
        effects::exposure::KIND,
        effects::channel_mixer::KIND,
        effects::black_white::KIND,
        effects::selective_color::KIND,
        effects::vibrance::KIND,
        effects::photo_filter::KIND,
        effects::gradient_map::KIND,
        effects::photoshop_color_balance::KIND,
        effects::photoshop_hue_saturation::KIND,
        effects::color_lookup::KIND,
    ]
    .contains(&kind)
    {
        return Err("Choose a color or tone adjustment".into());
    }
    let defaults;
    let settings = if let Some(settings) = c.get("settings") {
        settings
    } else {
        defaults = effects::defaults(kind);
        &defaults
    };
    Ok((kind, effects::normalized(kind, settings)?))
}

pub(crate) struct Placement {
    pub at: usize,
    pub parent: Option<String>,
    pub clip_to: Option<String>,
}
pub(crate) fn placement(
    doc: crate::engine::view::DocumentView<'_>,
    target: Option<&str>,
) -> Result<Placement, String> {
    let Some(id) = target else {
        return Ok(Placement {
            at: 0,
            parent: None,
            clip_to: None,
        });
    };
    let at = doc
        .layers()
        .position(|l| l.id() == id)
        .ok_or("Choose an existing layer or folder")?;
    let base = doc.layer(at).unwrap();
    let mut ancestor = Some(id);
    for _ in 0..=16 {
        let Some(id) = ancestor else {
            break;
        };
        let layer = doc.find_layer(id).ok_or("Missing parent")?;
        if layer.locked() {
            return Err("Unlock the target and its folders first".into());
        }
        ancestor = layer.parent();
    }
    if ancestor.is_some() {
        return Err("Group nesting limit exceeded".into());
    }
    Ok(if base.kind() == "group" {
        Placement {
            at: at + 1,
            parent: Some(base.id().into()),
            clip_to: None,
        }
    } else {
        Placement {
            at,
            parent: base.parent().map(String::from),
            clip_to: Some(base.clip_to().unwrap_or(base.id()).into()),
        }
    })
}

pub fn add(doc: &mut Document, c: &Value) -> Result<String, String> {
    let (kind, settings) = definition(c)?;
    let ids = crate::grouping::requested(c)?;
    let mut draft = doc.clone();
    let target = if ids.len() > 1 {
        Some(crate::grouping::create(&mut draft, &ids, "Color group")?)
    } else {
        ids.first().cloned()
    };
    if draft.layers.len() >= 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let mut adjustment = Layer::new(
        c["name"].as_str().unwrap_or(kind),
        "adjustment",
        doc.width,
        doc.height,
    );
    adjustment.pixels = Raster::new_depth(doc.width, doc.height, doc.bit_depth);
    adjustment.effects.push(effects::Effect {
        weight: 1.0,
        id: crate::engine::id(),
        kind: kind.into(),
        enabled: true,
        settings: settings.into(),
    });
    let placement = placement((&draft).into(), target.as_deref())?;
    adjustment.parent = placement.parent;
    adjustment.clip_to = placement.clip_to;
    let adjustment_id = adjustment.id.clone();
    if draft.selection.is_some() {
        adjustment.mask = Some(Mask {
            linked: true,
            cache_key: id(),
            enabled: true,
            steps: vec![
                MaskStep {
                    weight: 1.0,
                    id: id(),
                    kind: "fill".into(),
                    enabled: true,
                    value: 0.0,
                    pixels: Raster::new_depth(doc.width, doc.height, doc.bit_depth),
                    settings: Value::Null,
                },
                MaskStep {
                    weight: 1.0,
                    id: id(),
                    kind: "paint".into(),
                    enabled: true,
                    value: 255.0,
                    pixels: Raster::new_depth(doc.width, doc.height, doc.bit_depth),
                    settings: Value::Null,
                },
            ],
        });
    }
    draft.layers.insert(placement.at, adjustment);
    if draft.selection.is_some() {
        crate::fill::apply(
            &mut draft,
            &json!({"op":"paint.fill","layer":adjustment_id,"mask":true,"color":[255,255,255,255],"rect":[0,0,doc.width,doc.height]}),
        )?;
    }
    if let Some(coverage) = crate::selection::current(doc) {
        if let Some(mask) = draft
            .layers
            .iter_mut()
            .find(|l| l.id == adjustment_id)
            .and_then(|l| l.mask.as_mut())
        {
            for step in mask.steps.iter_mut().filter(|s| s.kind == "paint") {
                let empty =
                    Raster::new_depth(step.pixels.width, step.pixels.height, step.pixels.depth);
                crate::selection::restrict_raster(&mut step.pixels, &empty, &coverage, [0, 0]);
            }
        }
    }
    draft.ensure_depth();
    crate::psd::validate(&draft)?;
    *doc = draft;
    Ok(adjustment_id)
}
