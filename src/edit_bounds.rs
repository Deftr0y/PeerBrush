//! Raster edits use document coordinates, independently of transformed content bounds.
use crate::{engine::Document, raster::check_size};
use serde_json::Value;

pub(crate) fn covers_canvas(doc: &Document, target: &str) -> bool {
    doc.layers.iter().find(|l| l.id == target).is_some_and(|l| {
        l.x <= 0
            && l.y <= 0
            && l.x + l.pixels.width as i32 >= doc.width as i32
            && l.y + l.pixels.height as i32 >= doc.height as i32
    })
}

pub(crate) fn prepare(doc: &mut Document, c: &Value) -> Result<(), String> {
    let op = c["op"].as_str().unwrap_or("");
    if ![
        "paint",
        "smudge",
        "clone",
        "heal",
        "fill",
        "paint.fill",
        "shape",
        "gradient",
    ]
    .contains(&op)
    {
        return Ok(());
    }
    let Some(index) = doc
        .layers
        .iter()
        .position(|l| Some(l.id.as_str()) == c["layer"].as_str())
    else {
        return Ok(());
    };
    let layer = &doc.layers[index];
    if c["mask"] != true && (layer.kind != "paint" || layer.source.is_some()) {
        return Ok(());
    }
    let canvas = [0, 0, doc.width as i32, doc.height as i32];
    let mut area = if ["paint", "smudge", "clone", "heal"].contains(&op) {
        let (points, _) = crate::brush::points_from_command(c)?;
        let settings = crate::brush::Settings::from_command(c)?;
        if settings.opacity == 0.
            || settings.flow == 0.
            || (op == "paint" && crate::engine::color(c)[3] == 0)
        {
            return Ok(());
        }
        // Match the rasterizer's dab envelope, including its one-pixel edge.
        let radius = settings.radius + 1.;
        [
            (points.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min) - radius).floor() as i32,
            (points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min) - radius).floor() as i32,
            (points
                .iter()
                .map(|p| p[0])
                .fold(f32::NEG_INFINITY, f32::max)
                + radius)
                .ceil() as i32,
            (points
                .iter()
                .map(|p| p[1])
                .fold(f32::NEG_INFINITY, f32::max)
                + radius)
                .ceil() as i32,
        ]
    } else {
        c.get("rect")
            .and_then(crate::engine::rect)
            .or(doc.selection)
            .unwrap_or(canvas)
    };
    let clip = doc.selection.unwrap_or(canvas);
    area = [
        area[0].max(clip[0]).max(0),
        area[1].max(clip[1]).max(0),
        area[2].min(clip[2]).min(canvas[2]),
        area[3].min(clip[3]).min(canvas[3]),
    ];
    let old = [
        layer.x,
        layer.y,
        layer.x + layer.pixels.width as i32,
        layer.y + layer.pixels.height as i32,
    ];
    if area[0] >= area[2]
        || area[1] >= area[3]
        || (area[0] >= old[0] && area[1] >= old[1] && area[2] <= old[2] && area[3] <= old[3])
    {
        return Ok(());
    }
    // Expand once to a sparse document frame, retaining any existing off-canvas samples.
    let bounds = [
        old[0].min(0),
        old[1].min(0),
        old[2].max(canvas[2]),
        old[3].max(canvas[3]),
    ];
    let (w, h) = (
        (bounds[2] - bounds[0]) as u32,
        (bounds[3] - bounds[1]) as u32,
    );
    check_size(w, h)?;
    let (dx, dy) = (old[0] - bounds[0], old[1] - bounds[1]);
    let shift = |source: &crate::raster::Raster| {
        let mut pixels = crate::transform::copy_shift(source, w, h, dx, dy);
        pixels.retained = source.retained.clone();
        if let Some(original) = &mut pixels.retained {
            original.matrix =
                crate::retained::compose([1., 0., 0., 1., dx as f64, dy as f64], original.matrix);
        }
        pixels
    };
    let layer = &mut doc.layers[index];
    layer.pixels = shift(&layer.pixels);
    if let Some(mask) = &mut layer.mask {
        mask.cache_key = crate::engine::id();
        for step in &mut mask.steps {
            step.pixels = shift(&step.pixels);
        }
    }
    if let Some(source) = &mut layer.source {
        source.prepend([1., 0., 0., 1., dx as f32, dy as f32]);
    }
    for effect in &mut layer.effects {
        if effect.kind == "liquify" {
            crate::geometry::map_effect(
                &effect.kind,
                &mut effect.settings,
                [1., 1.],
                [dx, dy],
                true,
            )?;
        }
    }
    layer.x = bounds[0];
    layer.y = bounds[1];
    layer.effect_key = crate::engine::id();
    Ok(())
}
