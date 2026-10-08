use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

pub const TILE: u32 = 256;
pub const MAX_EDGE: u32 = 8192;
pub const MAX_PIXELS: u64 = 32 * 1024 * 1024;
pub type Pixel = [u8; 4];
pub type Pixel16 = [u16; 4];
pub fn default_depth() -> u16 {
    8
}
#[inline]
pub fn project16(value: u16) -> u8 {
    ((value as u32 + 128) / 257) as u8
}

pub fn check_size(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 || w > MAX_EDGE || h > MAX_EDGE || w as u64 * h as u64 > MAX_PIXELS {
        return Err("Image dimensions exceed the initial version's safety limits (8192 per edge, 32 megapixels).".into());
    }
    Ok(())
}

/// Copy-on-write sparse tiles: empty layers and history do not duplicate full canvases.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Raster {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained: Option<Box<crate::retained::Original>>,
    pub width: u32,
    pub height: u32,
    #[serde(default = "default_depth")]
    pub depth: u16,
    #[serde(
        default,
        with = "samples16_serde",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub samples16: BTreeMap<(u32, u32), Arc<Vec<u16>>>,
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

mod samples16_serde {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    pub fn serialize<S: serde::Serializer>(
        tiles: &BTreeMap<(u32, u32), Arc<Vec<u16>>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(tiles.len()))?;
        for (&(x, y), words) in tiles {
            let bytes: Vec<u8> = words.iter().flat_map(|v| v.to_be_bytes()).collect();
            seq.serialize_element(&(x, y, STANDARD.encode(&bytes)))?;
        }
        seq.end()
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<(u32, u32), Arc<Vec<u16>>>, D::Error> {
        let entries = Vec::<(u32, u32, String)>::deserialize(d)?;
        let mut tiles = BTreeMap::new();
        for (x, y, data) in entries {
            if data.len() > ((TILE * TILE * 8 + 2) / 3 * 4) as usize {
                return Err(serde::de::Error::custom("Oversized16-bit tile"));
            }
            let bytes = STANDARD.decode(&data).map_err(serde::de::Error::custom)?;
            if bytes.len() != (TILE * TILE * 8) as usize {
                return Err(serde::de::Error::custom("Invalid16-bit tile length"));
            }
            let words = bytes
                .chunks_exact(2)
                .map(|v| u16::from_be_bytes([v[0], v[1]]))
                .collect();
            if tiles.insert((x, y), Arc::new(words)).is_some() {
                return Err(serde::de::Error::custom("Duplicate16-bit tile"));
            }
        }
        Ok(tiles)
    }
}

impl Raster {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            retained: None,
            width,
            height,
            depth: 8,
            samples16: BTreeMap::new(),
            tiles: BTreeMap::new(),
        }
    }
    pub fn new_depth(width: u32, height: u32, depth: u16) -> Self {
        assert!(
            matches!(depth, 8 | 16),
            "Supported raster depths are8 and16"
        );
        let mut r = Self::new(width, height);
        r.depth = depth;
        r
    }
    pub fn is16(&self) -> bool {
        self.depth == 16
    }
    /// Promote explicit8-bit source samples without any full-canvas allocation.
    pub fn convert_depth(&mut self, depth: u16) {
        if depth == 16 {
            self.promote16();
            return;
        }
        if self.depth == 8 {
            return;
        }
        let mut out = Raster::new(self.width, self.height);
        for (&key, tile) in &self.samples16 {
            out.tiles
                .insert(key, Arc::new(tile.iter().copied().map(project16).collect()));
        }
        out.retained = self.retained.take();
        if let Some(s) = &mut out.retained {
            s.pixels.convert_depth(8);
        }
        *self = out;
    }
    pub fn promote16(&mut self) {
        if let Some(s) = &mut self.retained {
            s.pixels.promote16();
        }
        if self.is16() {
            return;
        }
        self.samples16 = std::mem::take(&mut self.tiles)
            .into_iter()
            .map(|(k, t)| (k, Arc::new(t.iter().map(|&v| v as u16 * 257).collect())))
            .collect();
        self.depth = 16;
    }
    pub fn get16(&self, x: i32, y: i32) -> Pixel16 {
        if !self.is16() {
            return self.get(x, y).map(|v| v as u16 * 257);
        }
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return [0; 4];
        }
        let (x, y) = (x as u32, y as u32);
        self.samples16
            .get(&(x / TILE, y / TILE))
            .map(|t| {
                let p = (((y % TILE) * TILE + x % TILE) * 4) as usize;
                [t[p], t[p + 1], t[p + 2], t[p + 3]]
            })
            .unwrap_or([0; 4])
    }
    pub fn set16(&mut self, x: i32, y: i32, color: Pixel16) {
        if !self.is16() {
            self.set(x, y, color.map(project16));
            return;
        }
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let (x, y) = (x as u32, y as u32);
        if color == [0; 4] && !self.samples16.contains_key(&(x / TILE, y / TILE)) {
            return;
        }
        if self.retained.is_some() && self.get16(x as i32, y as i32) == color {
            return;
        }
        self.retained = None;
        let tile = self
            .samples16
            .entry((x / TILE, y / TILE))
            .or_insert_with(|| Arc::new(vec![0; (TILE * TILE * 4) as usize]));
        let at = (((y % TILE) * TILE + x % TILE) * 4) as usize;
        Arc::make_mut(tile)[at..at + 4].copy_from_slice(&color);
    }
    pub fn rgba16(&self) -> Vec<u16> {
        if !self.is16() {
            return self.rgba().iter().map(|&v| v as u16 * 257).collect();
        }
        let mut out = vec![0; self.width as usize * self.height as usize * 4];
        let stride = self.width as usize * 4;
        let tile_stride = TILE as usize * 4;
        for (&(tx, ty), tile) in &self.samples16 {
            if tx >= self.width.div_ceil(TILE) || ty >= self.height.div_ceil(TILE) {
                continue;
            }
            let left = tx * TILE;
            let top = ty * TILE;
            let count = (self.width - left).min(TILE) as usize * 4;
            for row in 0..(self.height - top).min(TILE) as usize {
                let target = (top as usize + row) * stride + left as usize * 4;
                let source = row * tile_stride;
                out[target..target + count].copy_from_slice(&tile[source..source + count]);
            }
        }
        out
    }
    pub fn from_rgba16(w: u32, h: u32, words: &[u16]) -> Result<Self, String> {
        check_size(w, h)?;
        if words.len() != w as usize * h as usize * 4 {
            return Err("Wrong16-bit pixel data length".into());
        }
        let mut r = Self::new_depth(w, h, 16);
        let stride = w as usize * 4;
        let tile_stride = TILE as usize * 4;
        for ty in 0..h.div_ceil(TILE) {
            let top = ty * TILE;
            let rows = (h - top).min(TILE) as usize;
            for tx in 0..w.div_ceil(TILE) {
                let left = tx * TILE;
                let count = (w - left).min(TILE) as usize * 4;
                let first = top as usize * stride + left as usize * 4;
                if !(0..rows).any(|row| {
                    words[first + row * stride..first + row * stride + count]
                        .iter()
                        .any(|&v| v != 0)
                }) {
                    continue;
                }
                let mut tile = vec![0; (TILE * TILE * 4) as usize];
                for row in 0..rows {
                    tile[row * tile_stride..row * tile_stride + count].copy_from_slice(
                        &words[first + row * stride..first + row * stride + count],
                    );
                }
                r.samples16.insert((tx, ty), Arc::new(tile));
            }
        }
        Ok(r)
    }
    /// Bilinear premultiplied resampling keeps source precision until final storage.
    pub fn sample16(&self, x: f32, y: f32) -> Pixel16 {
        if x.fract() == 0.0 && y.fract() == 0.0 {
            return self.get16(x as i32, y as i32);
        }
        let ix = x.floor() as i32;
        let iy = y.floor() as i32;
        let fx = (x - x.floor()) as f64;
        let fy = (y - y.floor()) as f64;
        let mut sum = [0.0f64; 4];
        for (dx, dy, w) in [
            (0, 0, (1. - fx) * (1. - fy)),
            (1, 0, fx * (1. - fy)),
            (0, 1, (1. - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let p = self.get16(ix + dx, iy + dy);
            let a = p[3] as f64 / 65535.;
            for c in 0..3 {
                sum[c] += p[c] as f64 * a * w;
            }
            sum[3] += a * w;
        }
        if sum[3] <= 0. {
            return [0; 4];
        }
        [
            (sum[0] / sum[3]).round().clamp(0., 65535.) as u16,
            (sum[1] / sum[3]).round().clamp(0., 65535.) as u16,
            (sum[2] / sum[3]).round().clamp(0., 65535.) as u16,
            (sum[3] * 65535.).round().clamp(0., 65535.) as u16,
        ]
    }
    /// Allocated support envelope, clipped to raster bounds without scanning pixels.
    pub fn tile_bounds(&self) -> Option<[i32; 4]> {
        let keys: Box<dyn Iterator<Item = &(u32, u32)> + '_> = if self.is16() {
            Box::new(self.samples16.keys())
        } else {
            Box::new(self.tiles.keys())
        };
        let mut b = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for &(tx, ty) in keys {
            if tx >= self.width.div_ceil(TILE) || ty >= self.height.div_ceil(TILE) {
                continue;
            }
            let x = tx * TILE;
            let y = ty * TILE;
            b = [
                b[0].min(x as i32),
                b[1].min(y as i32),
                b[2].max((x + TILE).min(self.width) as i32),
                b[3].max((y + TILE).min(self.height) as i32),
            ];
        }
        (b[0] != i32::MAX).then_some(b)
    }
    pub fn validate_layout(&self) -> Result<(), String> {
        if let Some(original) = &self.retained {
            original.validate(self.depth)?;
        }
        if !matches!(self.depth, 8 | 16)
            || (self.is16() && !self.tiles.is_empty())
            || (!self.is16() && !self.samples16.is_empty())
        {
            return Err("Raster storage does not match its bit depth".into());
        }
        if self
            .tiles
            .values()
            .any(|v| v.len() != (TILE * TILE * 4) as usize)
            || self
                .samples16
                .values()
                .any(|v| v.len() != (TILE * TILE * 4) as usize)
        {
            return Err("Invalid raster tile length".into());
        }
        let valid =
            |&(x, y): &(u32, u32)| x < self.width.div_ceil(TILE) && y < self.height.div_ceil(TILE);
        if self.tiles.keys().any(|k| !valid(k)) || self.samples16.keys().any(|k| !valid(k)) {
            return Err("Raster tile lies outside its image".into());
        }
        Ok(())
    }

    pub fn get(&self, x: i32, y: i32) -> Pixel {
        if self.is16() {
            return self.get16(x, y).map(project16);
        }
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
        if self.is16() {
            self.set16(x, y, color.map(|v| v as u16 * 257));
            return;
        }
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let (x, y) = (x as u32, y as u32);
        if color == [0; 4] && !self.tiles.contains_key(&(x / TILE, y / TILE)) {
            return;
        }
        if self.retained.is_some() && self.get(x as i32, y as i32) == color {
            return;
        }
        self.retained = None;
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
        let stride = w as usize * 4;
        let tile_stride = TILE as usize * 4;
        for ty in 0..h.div_ceil(TILE) {
            let top = ty * TILE;
            let height = (h - top).min(TILE) as usize;
            for tx in 0..w.div_ceil(TILE) {
                let left = tx * TILE;
                let row_bytes = (w - left).min(TILE) as usize * 4;
                let source = top as usize * stride + left as usize * 4;
                // Alpha alone is insufficient: transparent pixels may retain editable RGB.
                let nonzero = (0..height).any(|row| {
                    bytes[source + row * stride..source + row * stride + row_bytes]
                        .iter()
                        .any(|&value| value != 0)
                });
                if !nonzero {
                    continue;
                }
                let mut tile = vec![0; (TILE * TILE * 4) as usize];
                for row in 0..height {
                    tile[row * tile_stride..row * tile_stride + row_bytes].copy_from_slice(
                        &bytes[source + row * stride..source + row * stride + row_bytes],
                    );
                }
                r.tiles.insert((tx, ty), Arc::new(tile));
            }
        }
        Ok(r)
    }
    pub fn rgba(&self) -> Vec<u8> {
        if self.is16() {
            let mut out = vec![0; self.width as usize * self.height as usize * 4];
            for (&(tx, ty), tile) in &self.samples16 {
                if tx >= self.width.div_ceil(TILE) || ty >= self.height.div_ceil(TILE) {
                    continue;
                }
                let left = tx * TILE;
                let top = ty * TILE;
                let count = (self.width - left).min(TILE) as usize * 4;
                for row in 0..(self.height - top).min(TILE) as usize {
                    let target = ((top as usize + row) * self.width as usize + left as usize) * 4;
                    let source = row * TILE as usize * 4;
                    for (dst, &value) in out[target..target + count]
                        .iter_mut()
                        .zip(&tile[source..source + count])
                    {
                        *dst = project16(value);
                    }
                }
            }
            return out;
        }
        let mut out = vec![0; self.width as usize * self.height as usize * 4];
        let columns = self.width.div_ceil(TILE);
        let rows = self.height.div_ceil(TILE);
        let stride = self.width as usize * 4;
        let tile_stride = TILE as usize * 4;
        for (&(tx, ty), tile) in &self.tiles {
            // Ignore serialized tiles outside the raster before multiplying coordinates.
            if tx >= columns || ty >= rows {
                continue;
            }
            let left = tx * TILE;
            let top = ty * TILE;
            let row_bytes = (self.width - left).min(TILE) as usize * 4;
            let height = (self.height - top).min(TILE) as usize;
            for row in 0..height {
                let source = row * tile_stride;
                let target = (top as usize + row) * stride + left as usize * 4;
                out[target..target + row_bytes].copy_from_slice(&tile[source..source + row_bytes]);
            }
        }
        out
    }
    pub fn bytes(&self) -> usize {
        self.tiles.len() * (TILE * TILE * 4) as usize
            + self.samples16.len() * (TILE * TILE * 8) as usize
    }
    /// Budget both the current projection and its supplementary original source.
    pub fn stored_bytes(&self) -> usize {
        self.bytes()
            + self
                .retained
                .as_ref()
                .map(|s| s.pixels.bytes())
                .unwrap_or(0)
    }
    /// Interpolate premultiplied color to keep transparent edges clean.
    pub fn sample(&self, x: f32, y: f32) -> Pixel {
        if x.fract() == 0.0 && y.fract() == 0.0 {
            return self.get(x as i32, y as i32);
        }
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
        self.data_bounds(false)
    }
    /// Editable support includes hidden RGB; a whole-source transform must not crop it away.
    pub fn source_bounds(&self) -> Option<[i32; 4]> {
        self.data_bounds(true)
    }
    fn data_bounds(&self, hidden: bool) -> Option<[i32; 4]> {
        if self.is16() {
            let mut bounds = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
            for (&(tx, ty), tile) in &self.samples16 {
                if tx >= self.width.div_ceil(TILE) || ty >= self.height.div_ceil(TILE) {
                    continue;
                }
                for (i, p) in tile.chunks_exact(4).enumerate() {
                    if p[3] == 0 && (!hidden || p[..3].iter().all(|v| *v == 0)) {
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
            return (bounds[0] != i32::MAX).then_some(bounds);
        }
        let mut bounds = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for (&(tx, ty), tile) in &self.tiles {
            if tx >= self.width.div_ceil(TILE) || ty >= self.height.div_ceil(TILE) {
                continue;
            }
            for (i, p) in tile.chunks_exact(4).enumerate() {
                if p[3] == 0 && (!hidden || p[..3].iter().all(|v| *v == 0)) {
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
    pub fn paint_dot16(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        color: Pixel16,
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
                src[3] = (src[3] as f64 * coverage as f64).round() as u16;
                let old = self.get16(x, y);
                let pixel = if erase {
                    [
                        old[0],
                        old[1],
                        old[2],
                        (old[3] as f64 * (1.0 - src[3] as f64 / 65535.0)).round() as u16,
                    ]
                } else {
                    blend16(old, src, 1.0, "normal")
                };
                self.set16(x, y, pixel);
            }
        }
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
        if self.is16() {
            self.paint_dot16(
                cx,
                cy,
                radius,
                color.map(|v| v as u16 * 257),
                soft,
                erase,
                clip,
            );
            return;
        }
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

pub const BLENDS: &[(&str, &str)] = &[
    ("normal", "Normal"),
    ("darken", "Darken"),
    ("multiply", "Multiply"),
    ("color_burn", "Color burn"),
    ("linear_burn", "Linear burn"),
    ("lighten", "Lighten"),
    ("screen", "Screen"),
    ("color_dodge", "Color dodge"),
    ("linear_dodge", "Linear dodge / Add"),
    ("overlay", "Overlay"),
    ("soft_light", "Soft light"),
    ("hard_light", "Hard light"),
    ("difference", "Difference"),
    ("exclusion", "Exclusion"),
    ("subtract", "Subtract"),
    ("divide", "Divide"),
];
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
            "linear_dodge" => (s + d).min(1.),
            "linear_burn" => (s + d - 1.).max(0.),
            "color_dodge" => {
                if d == 0. {
                    0.
                } else if s >= 1. {
                    1.
                } else {
                    (d / (1. - s)).min(1.)
                }
            }
            "color_burn" => {
                if d >= 1. {
                    1.
                } else if s <= 0. {
                    0.
                } else {
                    1. - ((1. - d) / s).min(1.)
                }
            }
            "hard_light" => {
                if s < 0.5 {
                    2. * s * d
                } else {
                    1. - 2. * (1. - s) * (1. - d)
                }
            }
            "soft_light" => {
                if s <= 0.5 {
                    d - (1. - 2. * s) * d * (1. - d)
                } else {
                    let g = if d <= 0.25 {
                        ((16. * d - 12.) * d + 4.) * d
                    } else {
                        d.sqrt()
                    };
                    d + (2. * s - 1.) * (g - d)
                }
            }
            "difference" => (d - s).abs(),
            "exclusion" => d + s - 2. * d * s,
            "subtract" => (d - s).max(0.),
            "divide" => {
                if d == 0. {
                    0.
                } else if s == 0. {
                    1.
                } else {
                    (d / s).min(1.)
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

/// Normalized high-depth blending. Quantize once after the full source-over operation.
pub fn blend16(dst: Pixel16, src: Pixel16, opacity: f64, mode: &str) -> Pixel16 {
    let sa = src[3] as f64 / 65535. * opacity;
    let da = dst[3] as f64 / 65535.;
    let a = sa + da * (1. - sa);
    if a <= 0. {
        return [0; 4];
    }
    let mut out = [0, 0, 0, (a * 65535.).round().clamp(0., 65535.) as u16];
    for c in 0..3 {
        let s = src[c] as f64 / 65535.;
        let d = dst[c] as f64 / 65535.;
        let b = match mode {
            "multiply" => s * d,
            "screen" => 1. - (1. - s) * (1. - d),
            "overlay" => {
                if d < 0.5 {
                    2. * s * d
                } else {
                    1. - 2. * (1. - s) * (1. - d)
                }
            }
            "linear_dodge" => (s + d).min(1.),
            "linear_burn" => (s + d - 1.).max(0.),
            "color_dodge" => {
                if d == 0. {
                    0.
                } else if s >= 1. {
                    1.
                } else {
                    (d / (1. - s)).min(1.)
                }
            }
            "color_burn" => {
                if d >= 1. {
                    1.
                } else if s <= 0. {
                    0.
                } else {
                    1. - ((1. - d) / s).min(1.)
                }
            }
            "hard_light" => {
                if s < 0.5 {
                    2. * s * d
                } else {
                    1. - 2. * (1. - s) * (1. - d)
                }
            }
            "soft_light" => {
                if s <= 0.5 {
                    d - (1. - 2. * s) * d * (1. - d)
                } else {
                    let g = if d <= 0.25 {
                        ((16. * d - 12.) * d + 4.) * d
                    } else {
                        d.sqrt()
                    };
                    d + (2. * s - 1.) * (g - d)
                }
            }
            "difference" => (d - s).abs(),
            "exclusion" => d + s - 2. * d * s,
            "subtract" => (d - s).max(0.),
            "divide" => {
                if d == 0. {
                    0.
                } else if s == 0. {
                    1.
                } else {
                    (d / s).min(1.)
                }
            }
            "darken" => s.min(d),
            "lighten" => s.max(d),
            _ => s,
        };
        out[c] = (((1. - sa) * da * d + sa * ((1. - da) * s + da * b)) / a * 65535.)
            .round()
            .clamp(0., 65535.) as u16;
    }
    out
}
pub fn png16(w: u32, h: u32, rgba: &[u16]) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    check_size(w, h)?;
    if rgba.len() != w as usize * h as usize * 4 {
        return Err("Wrong16-bit PNG pixel length".into());
    }
    // image's encoder expects native-endian words and writes PNG's big-endian samples.
    let bytes: Vec<u8> = rgba.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let mut out = vec![];
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&bytes, w, h, image::ExtendedColorType::Rgba16)
        .map_err(|e| e.to_string())?;
    Ok(out)
}
