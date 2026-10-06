use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

pub const TILE: u32 = 256;
pub const MAX_EDGE: u32 = 8192;
pub const MAX_PIXELS: u64 = 32 * 1024 * 1024;
pub type Pixel = [u8; 4];

pub fn check_size(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 || w > MAX_EDGE || h > MAX_EDGE || w as u64 * h as u64 > MAX_PIXELS {
        return Err("Image dimensions exceed the initial version's safety limits (8192 per edge, 32 megapixels).".into());
    }
    Ok(())
}

/// Copy-on-write sparse tiles: empty layers and history do not duplicate full canvases.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    #[serde(with = "tiles_serde")]
    pub tiles: BTreeMap<(u32, u32), Arc<Vec<u8>>>,
}

mod tiles_serde {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    pub fn serialize<S: serde::Serializer>(
        tiles: &BTreeMap<(u32, u32), Arc<Vec<u8>>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(tiles.len()))?;
        for (k, v) in tiles {
            seq.serialize_element(&(k.0, k.1, STANDARD.encode(v.as_slice())))?;
        }
        seq.end()
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<(u32, u32), Arc<Vec<u8>>>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum TileData {
            Compact(String),
            Legacy(Vec<u8>),
        }
        let items = Vec::<(u32, u32, TileData)>::deserialize(d)?;
        let mut out = BTreeMap::new();
        for (x, y, data) in items {
            let data = match data {
                TileData::Compact(s) => {
                    if s.len() > ((TILE * TILE * 4 + 2) / 3 * 4) as usize {
                        return Err(serde::de::Error::custom("Oversized tile"));
                    }
                    STANDARD.decode(s).map_err(serde::de::Error::custom)?
                }
                TileData::Legacy(bytes) => bytes,
            };
            if data.len() != (TILE * TILE * 4) as usize {
                return Err(serde::de::Error::custom("Invalid tile length"));
            }
            out.insert((x, y), Arc::new(data));
        }
        Ok(out)
    }
}

impl Raster {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            tiles: BTreeMap::new(),
        }
    }
    pub fn get(&self, x: i32, y: i32) -> Pixel {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return [0; 4];
        }
        let (x, y) = (x as u32, y as u32);
        self.tiles
            .get(&(x / TILE, y / TILE))
            .map(|t| {
                let p = (((y % TILE) * TILE + x % TILE) * 4) as usize;
                [t[p], t[p + 1], t[p + 2], t[p + 3]]
            })
            .unwrap_or([0; 4])
    }
    pub fn set(&mut self, x: i32, y: i32, color: Pixel) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let (x, y) = (x as u32, y as u32);
        if color == [0; 4] && !self.tiles.contains_key(&(x / TILE, y / TILE)) {
            return;
        }
        let tile = self
            .tiles
            .entry((x / TILE, y / TILE))
            .or_insert_with(|| Arc::new(vec![0; (TILE * TILE * 4) as usize]));
        let p = (((y % TILE) * TILE + x % TILE) * 4) as usize;
        Arc::make_mut(tile)[p..p + 4].copy_from_slice(&color);
    }
    pub fn from_rgba(w: u32, h: u32, bytes: &[u8]) -> Result<Self, String> {
        check_size(w, h)?;
        if bytes.len() != w as usize * h as usize * 4 {
            return Err("Wrong pixel data length".into());
        }
        let mut r = Self::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let p = ((y * w + x) * 4) as usize;
                r.set(
                    x as i32,
                    y as i32,
                    [bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]],
                );
            }
        }
        Ok(r)
    }
    pub fn rgba(&self) -> Vec<u8> {
        let mut out = vec![0; self.width as usize * self.height as usize * 4];
        for y in 0..self.height {
            for x in 0..self.width {
                let p = ((y * self.width + x) * 4) as usize;
                out[p..p + 4].copy_from_slice(&self.get(x as i32, y as i32));
            }
        }
        out
    }
    pub fn bytes(&self) -> usize {
        self.tiles.len() * (TILE * TILE * 4) as usize
    }
    /// Interpolate premultiplied color to keep transparent edges clean.
    pub fn sample(&self, x: f32, y: f32) -> Pixel {
        let ix = x.floor() as i32;
        let iy = y.floor() as i32;
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let mut out = [0.0f32; 4];
        for (dx, dy, weight) in [
            (0, 0, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let p = self.get(ix + dx, iy + dy);
            let alpha = p[3] as f32 / 255.0;
            for c in 0..3 {
                out[c] += p[c] as f32 * alpha * weight;
            }
            out[3] += alpha * weight;
        }
        if out[3] <= 0.00001 {
            return [0; 4];
        }
        [
            (out[0] / out[3]).round() as u8,
            (out[1] / out[3]).round() as u8,
            (out[2] / out[3]).round() as u8,
            (out[3] * 255.0).round() as u8,
        ]
    }
    pub fn content_bounds(&self) -> Option<[i32; 4]> {
        let mut bounds = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for (&(tx, ty), tile) in &self.tiles {
            for (i, p) in tile.chunks_exact(4).enumerate() {
                if p[3] == 0 {
                    continue;
                }
                let x = tx * TILE + i as u32 % TILE;
                let y = ty * TILE + i as u32 / TILE;
                if x >= self.width || y >= self.height {
                    continue;
                }
                bounds = [
                    bounds[0].min(x as i32),
                    bounds[1].min(y as i32),
                    bounds[2].max(x as i32 + 1),
                    bounds[3].max(y as i32 + 1),
                ];
            }
        }
        (bounds[0] != i32::MAX).then_some(bounds)
    }
    pub fn paint_dot(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        color: Pixel,
        soft: bool,
        erase: bool,
        clip: Option<[i32; 4]>,
    ) {
        let radius = radius.clamp(0.5, 512.0);
        let rect = clip.unwrap_or([0, 0, self.width as i32, self.height as i32]);
        for y in ((cy - radius).floor() as i32).max(rect[1])
            ..=((cy + radius).ceil() as i32).min(rect[3] - 1)
        {
            for x in ((cx - radius).floor() as i32).max(rect[0])
                ..=((cx + radius).ceil() as i32).min(rect[2] - 1)
            {
                let d =
                    ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt() / radius;
                if d > 1.0 {
                    continue;
                }
                let coverage = if soft {
                    (1.0 - d).min(0.5) * 2.0
                } else {
                    ((1.0 - d) * radius).clamp(0.0, 1.0)
                };
                let mut src = color;
                src[3] = (src[3] as f32 * coverage) as u8;
                let old = self.get(x, y);
                self.set(
                    x,
                    y,
                    if erase {
                        [
                            old[0],
                            old[1],
                            old[2],
                            (old[3] as f32 * (1.0 - src[3] as f32 / 255.0)) as u8,
                        ]
                    } else {
                        blend(old, src, 1.0, "normal")
                    },
                );
            }
        }
    }
}

pub fn blend(dst: Pixel, src: Pixel, opacity: f32, mode: &str) -> Pixel {
    let sa = src[3] as f32 / 255.0 * opacity;
    let da = dst[3] as f32 / 255.0;
    let a = sa + da * (1.0 - sa);
    if a <= 0.0 {
        return [0; 4];
    }
    let mut out = [0, 0, 0, (a * 255.0).round() as u8];
    for c in 0..3 {
        let s = src[c] as f32 / 255.0;
        let d = dst[c] as f32 / 255.0;
        let b = match mode {
            "multiply" => s * d,
            "screen" => 1.0 - (1.0 - s) * (1.0 - d),
            "overlay" => {
                if d < 0.5 {
                    2.0 * s * d
                } else {
                    1.0 - 2.0 * (1.0 - s) * (1.0 - d)
                }
            }
            "darken" => s.min(d),
            "lighten" => s.max(d),
            _ => s,
        };
        out[c] = (((1.0 - sa) * da * d + sa * ((1.0 - da) * s + da * b)) / a * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    out
}

pub fn png(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(rgba, w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(out)
}
