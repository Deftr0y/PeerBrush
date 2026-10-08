use crate::{
    engine::{rect, Document},
    raster::Raster,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;
use std::io::{Cursor, Read};
pub fn apply(doc: &mut Document, c: &Value) -> Result<(), String> {
    if u64::from(doc.width) * u64::from(doc.height) > 16_000_000 {
        return Err("Selection import is limited to 16 megapixels".into());
    }
    let area = c
        .get("rect")
        .and_then(rect)
        .ok_or("Selection mask needs an explicit document rectangle")?;
    if area[0] < 0
        || area[1] < 0
        || area[2] > doc.width as i32
        || area[3] > doc.height as i32
        || area[0] >= area[2]
        || area[1] >= area[3]
    {
        return Err("Selection mask rectangle must have positive area inside the canvas".into());
    }
    let mode = c["mode"].as_str().unwrap_or("replace");
    if !["replace", "add", "subtract", "intersect"].contains(&mode) {
        return Err("Choose replace, add, subtract or intersect".into());
    }
    let channel = c["channel"].as_str().unwrap_or("luma");
    if !["luma", "alpha"].contains(&channel) {
        return Err("Selection mask channel must be luma or alpha".into());
    }
    let bytes = match (c["png"].as_str(), c["path"].as_str()) {
        (Some(s), None) if s.len() <= 90 * 1024 * 1024 => STANDARD
            .decode(s)
            .map_err(|_| "Invalid selection mask PNG encoding")?,
        (None, Some(path)) => {
            let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("Selection mask must be a regular PNG file".into());
            }
            let mut bytes = vec![];
            file.take(64 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            bytes
        }
        _ => return Err("Supply exactly one PNG path or base64 png".into()),
    };
    if bytes.len() > 64 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("Selection mask must be a bounded PNG image".into());
    }
    let reader = || {
        image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())
    };
    let (w, h) = reader()?.into_dimensions().map_err(|e| e.to_string())?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 16_000_000 {
        return Err("Selection mask image exceeds 16 megapixels".into());
    }
    let mut reader = reader()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?;
    let alpha;
    if channel == "alpha" && !image.color().has_alpha() {
        return Err("The selection PNG has no alpha channel".into());
    }
    let gray = if channel == "alpha" {
        alpha = image.to_rgba8();
        image::GrayImage::from_fn(w, h, |x, y| image::Luma([alpha.get_pixel(x, y)[3]]))
    } else {
        image.to_luma8()
    };
    let prior = super::current(doc);
    let mut mask = Raster::new(doc.width, doc.height);
    for y in 0..doc.height as i32 {
        for x in 0..doc.width as i32 {
            let new = if x >= area[0] && x < area[2] && y >= area[1] && y < area[3] {
                let sx = ((x - area[0]) as f32 + 0.5) * w as f32 / (area[2] - area[0]) as f32 - 0.5;
                let sy = ((y - area[1]) as f32 + 0.5) * h as f32 / (area[3] - area[1]) as f32 - 0.5;
                let ix = sx.floor() as i32;
                let iy = sy.floor() as i32;
                let fx = sx - ix as f32;
                let fy = sy - iy as f32;
                let at = |x: i32, y: i32| {
                    gray.get_pixel(
                        x.clamp(0, w as i32 - 1) as u32,
                        y.clamp(0, h as i32 - 1) as u32,
                    )[0] as f32
                        / 255.
                };
                at(ix, iy) * (1. - fx) * (1. - fy)
                    + at(ix + 1, iy) * fx * (1. - fy)
                    + at(ix, iy + 1) * (1. - fx) * fy
                    + at(ix + 1, iy + 1) * fx * fy
            } else {
                0.
            };
            let old = prior.as_ref().map_or(0., |m| m.value(x, y));
            let value = match mode {
                "add" => old.max(new),
                "subtract" => old * (1. - new),
                "intersect" => old.min(new),
                _ => new,
            };
            let value = (value * 255.).round() as u8;
            if value > 0 {
                mask.set(x, y, [value; 4]);
            }
        }
    }
    super::system::put(doc, mask)
}
