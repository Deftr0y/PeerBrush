//! Shared bake eligibility, prefix and frame policy.
use crate::engine::view::DocumentView;
use serde_json::Value;

pub(crate) struct Plan {
    pub index: usize,
    pub mask: bool,
    pub at: usize,
    pub active: bool,
    pub quantized: bool,
    pub canvas_frame: bool,
    pub size: [u32; 2],
    pub origin: [i32; 2],
    pub tree: Vec<usize>,
}
pub(crate) fn tree(doc: DocumentView<'_>, index: usize) -> Vec<usize> {
    let mut tree = vec![index];
    loop {
        let count = tree.len();
        for (i, l) in doc.layers().enumerate() {
            if !tree.contains(&i)
                && l.parent()
                    .is_some_and(|p| tree.iter().any(|&j| doc.layer(j).unwrap().id() == p))
            {
                tree.push(i);
            }
        }
        if count == tree.len() {
            return tree;
        }
    }
}
impl Plan {
    pub(crate) fn new(doc: DocumentView<'_>, index: usize, c: &Value) -> Result<Self, String> {
        if doc.read_only() {
            return Err("This PSD is read-only".into());
        }
        let l = doc.layer(index).ok_or("Layer no longer exists")?;
        let mut parent = Some(l.id());
        for _ in 0..=16 {
            let Some(id) = parent else { break };
            let layer = doc.find_layer(id).ok_or("Invalid parent")?;
            if layer.locked() {
                return Err("Unlock the layer and its containing folders before baking".into());
            }
            parent = layer.parent();
        }
        if parent.is_some() {
            return Err("Group nesting limit exceeded".into());
        }
        let mask = c["op"] == "mask.step.bake";
        if !mask && c["op"] != "effect.bake" {
            return Err("Unknown bake command".into());
        }
        let (at, active, quantized, canvas_frame) = if mask {
            let m = l.mask().ok_or("Layer has no mask")?;
            let at = (0..m.step_count())
                .find(|&i| Some(m.step(i).unwrap().id()) == c["step"].as_str())
                .ok_or("Unknown mask step")?;
            let active = (0..=at).any(|i| {
                let s = m.step(i).unwrap();
                s.enabled() && s.weight() > 0.
            });
            let quantized = (0..m.step_count()).any(|i| {
                let s = m.step(i).unwrap();
                crate::mask::needs_cache_step(s.kind(), s.enabled(), s.weight(), s.value())
            });
            (at, active, quantized, false)
        } else {
            if l.kind() == "adjustment" {
                return Err("Adjustment effects depend on the backdrop; merge their affected layers explicitly instead".into());
            }
            let at = (0..l.effect_count())
                .find(|&i| Some(l.effect(i).unwrap().id()) == c["effect"].as_str())
                .ok_or("Unknown effect")?;
            let active = (0..=at).any(|i| {
                let e = l.effect(i).unwrap();
                e.enabled() && e.weight() > 0.
            });
            let frame = l.kind() == "group"
                || (0..=at).any(|i| {
                    let e = l.effect(i).unwrap();
                    e.enabled() && e.weight() > 0. && crate::effects::styles::contains(e.kind())
                });
            (at, active, false, frame)
        };
        let pixels = l.pixels();
        let size = if canvas_frame {
            [doc.width(), doc.height()]
        } else {
            [pixels.width, pixels.height]
        };
        let tree = tree(doc, index);
        if active && !mask {
            if tree.iter().any(|&i| doc.layer(i).unwrap().locked()) {
                return Err(
                    "Unlock the folder and all children before baking folder content".into(),
                );
            }
            if canvas_frame
                && l.mask().is_some()
                && (l.origin() != [0, 0] || [pixels.width, pixels.height] != size)
            {
                return Err("Baking this expanding style would change the mask's frame. Keep it editable or explicitly merge the masked result.".into());
            }
        }
        Ok(Self {
            index,
            mask,
            at,
            active,
            quantized,
            canvas_frame,
            size,
            origin: if canvas_frame { [0, 0] } else { l.origin() },
            tree,
        })
    }
}
