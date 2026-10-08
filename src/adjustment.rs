use crate::{
    effects,
    engine::{id, Document, Layer, Mask, MaskStep},
    raster::Raster,
};
use serde_json::{json, Value};

pub fn add(doc: &mut Document, c: &Value) -> Result<String, String> {
    let kind = c["kind"].as_str().unwrap_or("color_balance");
    if ![
        "color_balance",
        "hsl",
        "levels",
        "curves",
        "adjust",
        "invert",
        "grayscale",
    ]
    .contains(&kind)
    {
        return Err("Choose a color or tone adjustment".into());
    }
    let settings = c
        .get("settings")
        .cloned()
        .unwrap_or_else(|| effects::defaults(kind));
    let settings = effects::normalized(kind, &settings)?;
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
    adjustment.effects.push(effects::Effect {
        id: crate::engine::id(),
        kind: kind.into(),
        enabled: true,
        settings,
    });
    let at = if let Some(id) = target {
        let at = draft
            .layers
            .iter()
            .position(|l| l.id == id)
            .ok_or("Choose an existing layer or folder")?;
        let base = &draft.layers[at];
        let mut ancestor = Some(base.id.as_str());
        while let Some(id) = ancestor {
            let l = draft
                .layers
                .iter()
                .find(|l| l.id == id)
                .ok_or("Missing parent")?;
            if l.locked {
                return Err("Unlock the target and its folders first".into());
            }
            ancestor = l.parent.as_deref();
        }
        if base.kind == "group" {
            adjustment.parent = Some(base.id.clone());
            at + 1
        } else {
            adjustment.parent = base.parent.clone();
            adjustment.clip_to = Some(base.clip_to.clone().unwrap_or_else(|| base.id.clone()));
            at
        }
    } else {
        0
    };
    let adjustment_id = adjustment.id.clone();
    if draft.selection.is_some() {
        adjustment.mask = Some(Mask {
            cache_key: id(),
            enabled: true,
            steps: vec![
                MaskStep {
                    id: id(),
                    kind: "fill".into(),
                    enabled: true,
                    value: 0.0,
                    pixels: Raster::new(doc.width, doc.height),
                    settings: Value::Null,
                },
                MaskStep {
                    id: id(),
                    kind: "paint".into(),
                    enabled: true,
                    value: 255.0,
                    pixels: Raster::new(doc.width, doc.height),
                    settings: Value::Null,
                },
            ],
        });
    }
    draft.layers.insert(at, adjustment);
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
