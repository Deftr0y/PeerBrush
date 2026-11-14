//! Exact document-coordinate sampling for the foreground and mask swatches.
use crate::{
    engine::{scene::SceneSnapshot, Document},
    raster::{Pixel, Pixel16},
    workspace::source_runtime::SourceReadSession,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Color {
    Byte(Pixel),
    Word(Pixel16),
}
impl Color {
    pub(crate) fn display(self) -> Pixel {
        match self {
            Self::Byte(value) => value,
            Self::Word(value) => crate::depth16::display_pixel(value),
        }
    }
    pub(crate) fn words(self) -> Option<Pixel16> {
        match self {
            Self::Byte(_) => None,
            Self::Word(value) => Some(value),
        }
    }
}

pub(crate) fn point(size: [u32; 2], point: [f32; 2]) -> Result<[i32; 2], String> {
    if point.iter().any(|v| !v.is_finite()) {
        return Err("Sample point must be finite".into());
    }
    let point = point.map(|v| v.floor());
    if point
        .iter()
        .zip(size)
        .any(|(v, bound)| *v < 0. || f64::from(*v) >= f64::from(bound) || *v >= i32::MAX as f32)
    {
        return Err("Sample point is outside the document".into());
    }
    Ok(point.map(|v| v as i32))
}
fn rect(size: [u32; 2], [x, y]: [i32; 2]) -> Result<[i32; 4], String> {
    if x < 0 || y < 0 || x as u32 >= size[0] || y as u32 >= size[1] {
        return Err("Sample point is outside the document".into());
    }
    Ok([
        x,
        y,
        x.checked_add(1).ok_or("Sample coordinate overflow")?,
        y.checked_add(1).ok_or("Sample coordinate overflow")?,
    ])
}
#[cfg(test)]
pub(crate) fn resident(
    doc: &Document, point: [i32; 2], mask_target: Option<&str>,
) -> Result<Pixel, String> {
    resident_color(doc, point, mask_target, &|| Ok(())).map(Color::display)
}

pub(crate) fn resident_color(
    doc: &Document,
    point: [i32; 2],
    mask_target: Option<&str>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Color, String> {
    check()?;
    let rect = rect([doc.width, doc.height], point)?;
    if let Some(id) = mask_target {
        let layer = doc
            .layers
            .iter()
            .find(|layer| layer.id == id)
            .ok_or("Sample mask layer no longer exists")?;
        let x = point[0]
            .checked_sub(layer.x)
            .ok_or("Sample mask coordinate overflow")?;
        let y = point[1]
            .checked_sub(layer.y)
            .ok_or("Sample mask coordinate overflow")?;
        // Share the renderer's mask evaluator without preparing unrelated Color effects.
        if doc.bit_depth == 16 {
            crate::depth16::validate_budget(doc)?;
            let prepared = crate::depth16::mask::prepare_checked(layer, check)?;
            let value = crate::depth16::mask::raw_word(layer, x, y, prepared.as_deref());
            check()?;
            return Ok(Color::Word([value, value, value, u16::MAX]));
        }
        let value = (layer.mask_value_raw(x, y) * 255.).round() as u8;
        return Ok(Color::Byte([value, value, value, 255]));
    }
    if doc.bit_depth == 16 {
        let source = crate::depth16::render_crop_checked(doc, rect, check)?;
        let pixel = source.get(0, 0);
        let pixel = if let Some(profile) = doc.icc_profile.as_deref()
            .and_then(|bytes| crate::color_profile::transforms(bytes).ok())
        {
            let mut converted = [0; 4];
            profile.convert16_into(&pixel, &mut converted)?;
            converted
        } else {
            pixel
        };
        check()?;
        return Ok(Color::Word(pixel));
    }
    let (width, height, bytes, area) = doc.preview_checked(Some(rect), 1, None, false, check)?;
    if width != 1 || height != 1 || area != rect {
        return Err("Pixel sample renderer returned another document region".into());
    }
    bytes
        .as_slice()
        .try_into()
        .map(Color::Byte)
        .map_err(|_| "Pixel sample renderer returned incomplete RGBA".into())
}
#[cfg(test)]
pub(crate) fn native(
    scene: Arc<SceneSnapshot>,
    session: impl Into<SourceReadSession>,
    point: [i32; 2],
    mask_target: Option<&str>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Pixel, String> {
    native_color(scene, session, point, mask_target, check).map(Color::display)
}

pub(crate) fn native_color(
    scene: Arc<SceneSnapshot>,
    session: impl Into<SourceReadSession>,
    point: [i32; 2],
    mask_target: Option<&str>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Color, String> {
    check()?;
    let doc = scene.metadata();
    let area = rect([doc.width(), doc.height()], point)?;
    let native_words = doc.depth() == 16;
    if mask_target.is_some_and(|id| !doc.layers().any(|layer| layer.id() == id)) {
        return Err("Sample mask layer no longer exists".into());
    }
    let request = super::native_preview::Request::capture(
        scene,
        session,
        Some(area),
        1,
        mask_target,
        mask_target.is_some(),
    )?;
    if native_words {
        return request.sample16(check).map(Color::Word);
    }
    let frame = request.render(check)?;
    if frame.key().rect != area
        || frame.key().width != 1
        || frame.key().height != 1
        || frame.stripes().len() != 1
    {
        return Err("Pixel sample renderer returned another document region".into());
    }
    let pixel: Pixel = frame.stripes()[0].bytes()[..]
        .try_into()
        .map_err(|_| "Pixel sample renderer returned incomplete RGBA")?;
    check()?;
    Ok(Color::Byte(pixel))
}

#[cfg(test)]
#[path = "sample_tests.rs"]
mod tests;
