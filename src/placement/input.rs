//! Shared placement validation, insertion order and document-coordinate frames.
use serde_json::Value;

pub(crate) struct Layer<'a> {
    pub id: &'a str,
    pub parent: Option<&'a str>,
    pub kind: &'a str,
    pub locked: bool,
    pub source: bool,
}
pub(crate) struct Plan {
    pub target: Option<usize>,
    pub parent: Option<String>,
    pub index: usize,
    pub replace: bool,
}
impl Plan {
    pub fn new<'a>(
        c: &Value,
        count: usize,
        layer: impl Fn(usize) -> Layer<'a>,
    ) -> Result<Self, String> {
        let new_layer = c.get("new_layer").map_or(Ok(true), |v| {
            v.as_bool().ok_or("new_layer must be true or false")
        })?;
        let unlocked = |id: &str| -> Result<usize, String> {
            let target = (0..count)
                .find(|&i| layer(i).id == id)
                .ok_or("Unknown image destination")?;
            let mut current = Some(target);
            for _ in 0..=16 {
                let Some(index) = current else {
                    return Ok(target);
                };
                let node = layer(index);
                if node.locked {
                    return Err("Unlock the image destination first".into());
                }
                current = node
                    .parent
                    .map(|id| {
                        (0..count)
                            .find(|&i| layer(i).id == id)
                            .ok_or("Unknown image destination")
                    })
                    .transpose()?;
            }
            if current.is_some() {
                Err("Group nesting limit exceeded".into())
            } else {
                Ok(target)
            }
        };
        let context = c
            .get("layer")
            .map(|v| unlocked(v.as_str().ok_or("layer must be a layer or folder ID")?))
            .transpose()?;
        let parent = if new_layer {
            if let Some(value) = c.get("parent") {
                if value.is_null() {
                    None
                } else {
                    let id = value.as_str().ok_or("Parent must be a folder ID or null")?;
                    if layer(unlocked(id)?).kind != "group" {
                        return Err("Parent must be a folder".into());
                    }
                    Some(id)
                }
            } else {
                context.and_then(|i| {
                    let node = layer(i);
                    if node.kind == "group" {
                        Some(node.id)
                    } else {
                        node.parent
                    }
                })
            }
        } else {
            if c.get("parent").is_some() {
                return Err("parent is only used for new layers".into());
            }
            let i = context.ok_or("Choose the current layer with layer and new_layer:false")?;
            let node = layer(i);
            if node.source {
                return Err(
                    "Rasterize the editable text/vector layer before placing pixels into it".into(),
                );
            }
            if node.kind == "group" {
                return Err("Place into a paint layer or create a new child layer".into());
            }
            None
        };
        if new_layer && count >= 100 {
            return Err("Initial version supports up to 100 layers".into());
        }
        let replace = match c.get("mode").and_then(Value::as_str).unwrap_or("over") {
            "over" => false,
            "replace" => true,
            _ => return Err("Image mode must be over or replace".into()),
        };
        let index = context
            .filter(|&i| layer(i).parent == parent && layer(i).kind != "group")
            .unwrap_or_else(|| {
                parent
                    .and_then(|id| (0..count).find(|&i| layer(i).id == id))
                    .map_or(0, |i| i + 1)
            });
        Ok(Self {
            target: (!new_layer).then_some(context).flatten(),
            parent: parent.map(str::to_owned),
            index,
            replace,
        })
    }
}

pub(crate) fn bounds(c: &Value, source: [u32; 2], limits: crate::raster::CanvasLimits) -> Result<[i32; 4], String> {
    let area = if let Some(value) = c.get("rect") {
        crate::engine::rect(value).ok_or("Invalid destination rectangle")?
    } else {
        let coordinate = |key: &str| -> Result<i32, String> {
            c.get(key).map_or(Ok(0), |v| {
                v.as_i64()
                    .filter(|n| (-100000..=100000).contains(n))
                    .map(|n| n as i32)
                    .ok_or_else(|| format!("Invalid image {key}"))
            })
        };
        let (x, y) = (coordinate("x")?, coordinate("y")?);
        [
            x,
            y,
            x.checked_add(source[0] as i32)
                .ok_or("Image bounds overflow")?,
            y.checked_add(source[1] as i32)
                .ok_or("Image bounds overflow")?,
        ]
    };
    shape(area, limits)?;
    Ok(area)
}
pub(crate) fn shape(area: [i32; 4], limits: crate::raster::CanvasLimits) -> Result<[u32; 2], String> {
    let width = u32::try_from(i64::from(area[2]) - i64::from(area[0]))
        .map_err(|_| "Invalid image width")?;
    let height = u32::try_from(i64::from(area[3]) - i64::from(area[1]))
        .map_err(|_| "Invalid image height")?;
    limits.check(width, height)?;
    Ok([width, height])
}
pub(crate) fn frame(origin: [i32; 2], size: [u32; 2], area: [i32; 4], limits: crate::raster::CanvasLimits) -> Result<[i32; 4], String> {
    let right = i64::from(origin[0]) + i64::from(size[0]);
    let bottom = i64::from(origin[1]) + i64::from(size[1]);
    let frame = [
        origin[0].min(area[0]),
        origin[1].min(area[1]),
        i32::try_from(right.max(i64::from(area[2]))).map_err(|_| "Image frame overflow")?,
        i32::try_from(bottom.max(i64::from(area[3]))).map_err(|_| "Image frame overflow")?,
    ];
    shape(frame, limits)?;
    Ok(frame)
}

pub(crate) fn coordinate(value: u32, source: u32, destination: u32) -> f32 {
    if source == destination {
        return value as f32;
    }
    ((value as f32 + 0.5) * source as f32 / destination as f32 - 0.5).clamp(0., source as f32 - 1.)
}
