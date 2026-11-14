//! Shared hierarchy, locking and source-frame rules for resident/native merge.
pub(crate) struct Layer<'a> {
    pub id: &'a str,
    pub parent: Option<&'a str>,
    pub clip: Option<&'a str>,
    pub kind: &'a str,
    pub blend: &'a str,
    pub blend_if: bool,
    pub restricted_channels: bool,
    pub visible: bool,
    pub locked: bool,
}
macro_rules! layer {
    ($t:ty) => {
        impl<'a> From<&'a $t> for Layer<'a> {
            fn from(l: &'a $t) -> Self {
                Self {
                    id: &l.id,
                    parent: l.parent.as_deref(),
                    clip: l.clip_to.as_deref(),
                    kind: &l.kind,
                    blend: &l.blend,
                    blend_if: crate::psd::blend_ranges::bytes(&l.psd_metadata).is_some(),
                    restricted_channels: crate::psd::blend_channels::excluded(&l.psd_metadata) != 0,
                    visible: l.visible,
                    locked: l.locked,
                }
            }
        }
    };
}
layer!(crate::engine::Layer);
layer!(crate::engine::scene::Layer36);

pub(crate) fn resolve<'a>(
    count: usize,
    layer: impl Fn(usize) -> Layer<'a>,
    ids: &[String],
) -> Result<Vec<usize>, String> {
    if ids.is_empty() || ids.len() > 100 {
        return Err("Choose 1–100 layers to merge".into());
    }
    for id in ids {
        if !(0..count).any(|i| layer(i).id == id) {
            return Err("A selected layer no longer exists".into());
        }
    }
    let mut roots = Vec::new();
    for i in 0..count {
        let l = layer(i);
        if !ids.iter().any(|id| id == l.id) {
            continue;
        }
        let mut parent = l.parent;
        let mut contained = false;
        for _ in 0..=16 {
            let Some(id) = parent else { break };
            if ids.iter().any(|selected| selected == id) {
                contained = true;
                break;
            }
            parent = (0..count)
                .find(|&i| layer(i).id == id)
                .and_then(|i| layer(i).parent);
        }
        if !contained {
            roots.push(i);
        }
    }
    let first = *roots.first().ok_or("Invalid merge hierarchy")?;
    if roots.len() == 1 && layer(first).kind != "group" {
        roots.push(
            (first + 1..count)
                .find(|&i| layer(i).parent == layer(first).parent)
                .ok_or("There is no layer below this one to merge")?,
        );
    }
    let parent = layer(first).parent;
    if roots.iter().any(|&i| layer(i).parent != parent) {
        return Err("Choose layers in the same folder to merge".into());
    }
    if roots.iter().any(|&i| !layer(i).visible) {
        return Err("Show the selected layers before merging".into());
    }
    if roots.len() > 1 {
        let last = *roots.last().unwrap();
        if (first..=last)
            .filter(|&i| layer(i).parent == parent)
            .count()
            != roots.len()
        {
            return Err("Choose adjacent layers to preserve the canvas when merging".into());
        }
        if roots.iter().any(|&i| layer(i).blend != "normal") {
            return Err(
                "Put these layers in a folder and merge the folder to preserve their blend modes"
                    .into(),
            );
        }
    }
    Ok(roots)
}

pub(crate) struct Plan {
    pub roots: Vec<usize>,
    pub removed: Vec<usize>,
    pub insertion: usize,
    pub folder_only: bool,
}
impl Plan {
    pub fn new<'a>(
        count: usize,
        layer: impl Fn(usize) -> Layer<'a>,
        ids: &[String],
    ) -> Result<Self, String> {
        let roots = resolve(count, &layer, ids)?;
        if roots.iter().any(|&i| {
            let root = layer(i);
            crate::compositor::passes_backdrop(root.blend, root.blend_if)
        }) {
            return Err(
                "Choose an isolated folder blend before merging a pass-through folder".into(),
            );
        }
        let mut removed = roots.clone();
        loop {
            let before = removed.len();
            for i in 0..count {
                if !removed.contains(&i)
                    && layer(i)
                        .parent
                        .is_some_and(|p| removed.iter().any(|&j| layer(j).id == p))
                {
                    removed.push(i);
                }
            }
            if before == removed.len() {
                break;
            }
        }
        for &i in &removed {
            if layer(i).locked {
                return Err("Unlock the selected layers and their contents before merging".into());
            }
            let mut parent = layer(i).parent;
            for _ in 0..=16 {
                let Some(id) = parent else { break };
                let at = (0..count)
                    .find(|&i| layer(i).id == id)
                    .ok_or("Invalid layer folder")?;
                let p = layer(at);
                if p.locked {
                    return Err("Unlock the containing folder before merging".into());
                }
                parent = p.parent;
            }
            if parent.is_some() {
                return Err("Group nesting limit exceeded".into());
            }
        }
        for i in 0..count {
            if let Some(base) = layer(i).clip {
                if removed.contains(&i) != removed.iter().any(|&i| layer(i).id == base) {
                    return Err(
                        "Merge the complete clipping group, or its containing folder".into(),
                    );
                }
            }
        }
        if roots
            .iter()
            .any(|&i| layer(i).kind == "adjustment" && layer(i).clip.is_none())
        {
            return Err(
                "Put a shared adjustment and its artwork in a folder, then merge the folder".into(),
            );
        }
        if roots.iter().any(|&i| layer(i).blend_if) {
            return Err("Put Blend If layers and their backdrop in an isolated folder before merging".into());
        }
        if roots.iter().any(|&i| layer(i).restricted_channels) {
            return Err("Put layers with restricted blend channels and their backdrop in an isolated folder before merging".into());
        }
        let insertion = (0..roots[0]).filter(|i| !removed.contains(i)).count();
        let folder_only = roots.len() == 1 && layer(roots[0]).kind == "group";
        Ok(Self {
            roots,
            removed,
            insertion,
            folder_only,
        })
    }
}

pub(crate) fn bounds<'a>(
    canvas: [u32; 2],
    origin: [i32; 2],
    size: [u32; 2],
    kind: &str,
    effects: impl Iterator<Item = (&'a str, bool, f32)> + Clone,
    content: impl FnOnce() -> Result<Option<[i32; 4]>, String>,
) -> Result<Option<[i64; 4]>, String> {
    let area = if effects.clone().any(|(kind, enabled, weight)| {
        enabled && weight > 0. && crate::effects::styles::contains(kind)
    }) {
        Some([
            -origin[0],
            -origin[1],
            canvas[0] as i32 - origin[0],
            canvas[1] as i32 - origin[1],
        ])
    } else if kind == "group" {
        effects.clone().any(|(_, enabled, _)| enabled).then_some([
            0,
            0,
            canvas[0] as i32,
            canvas[1] as i32,
        ])
    } else if kind == "fill"
        || kind == "adjustment"
        || effects
            .clone()
            .any(|(kind, enabled, _)| enabled && ["blur", "bloom", "liquify"].contains(&kind))
    {
        Some([0, 0, size[0] as i32, size[1] as i32])
    } else {
        content()?
    };
    Ok(area.map(|a| std::array::from_fn(|i| i64::from(a[i]) + i64::from(origin[i % 2]))))
}
pub(crate) fn union(bounds: Option<[i64; 4]>, area: [i64; 4]) -> Option<[i64; 4]> {
    Some(bounds.map_or(area, |b| {
        [
            area[0].min(b[0]),
            area[1].min(b[1]),
            area[2].max(b[2]),
            area[3].max(b[3]),
        ]
    }))
}
pub(crate) fn frame(
    bounds: Option<[i64; 4]>,
    limits: crate::raster::CanvasLimits,
) -> Result<([i32; 4], [u32; 2]), String> {
    let b = bounds.unwrap_or([0, 0, 1, 1]);
    if b.iter().any(|v| i32::try_from(*v).is_err()) {
        return Err("Merged layer coordinates exceed the supported range".into());
    }
    let w = u32::try_from(b[2] - b[0]).map_err(|_| "Merged layer is too wide")?;
    let h = u32::try_from(b[3] - b[1]).map_err(|_| "Merged layer is too tall")?;
    limits.check(w, h)?;
    Ok((b.map(|v| v as i32), [w, h]))
}
