use super::*;

const MAX_TILES: usize = 4096;
pub(super) const MAX_TEXTURE_EDGE: usize = 8192;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Area {
    pub inner: [u32; 4],
    pub outer: [u32; 4],
}
#[derive(Clone, Copy)]
pub(super) struct Layout {
    pub size: [u32; 2],
    pub cell: [u32; 2],
    pub count: usize,
}
impl Layout {
    pub fn new(width: u32, height: u32, side: usize) -> Result<Self> {
        Self::with_limit(width, height, side, crate::render::native_preview::MAX_PREVIEW_PIXELS)
    }
    pub(super) fn streamed(width: u32, height: u32, side: usize) -> Result<Self> {
        Self::with_limit(width, height, side, 16000 * 16000)
    }
    fn with_limit(width: u32, height: u32, side: usize, pixels: u64) -> Result<Self> {
        if width == 0 || height == 0 || width > 16000 || height > 16000 || side == 0
            || u64::from(width) * u64::from(height) > pixels
        {
            return Err("Invalid native presentation dimensions or texture limit".into());
        }
        let side = side.min(MAX_TEXTURE_EDGE) as u32;
        let w = width.min(side);
        let h = height
            .min(side)
            .min((MAX_PIXELS / w as usize) as u32);
        let cw = if width > w {
            w.checked_sub(2)
                .filter(|n| *n > 0)
                .ok_or("Texture limit cannot hold native filtering neighbors")?
        } else {
            w
        };
        let ch = if height > h {
            h.checked_sub(2)
                .filter(|n| *n > 0)
                .ok_or("Texture limit cannot hold native filtering neighbors")?
        } else {
            h
        };
        let count = (width.div_ceil(cw) as usize)
            .checked_mul(height.div_ceil(ch) as usize)
            .filter(|n| *n <= MAX_TILES)
            .ok_or("Native presentation exceeds its tile index bound")?;
        Ok(Self {
            size: [width, height],
            cell: [cw, ch],
            count,
        })
    }
    pub fn areas(&self) -> impl Iterator<Item = Area> + '_ {
        (0..self.count).map(|index| self.area(index).unwrap())
    }
    pub(super) fn area(&self, index: usize) -> Option<Area> {
        if index >= self.count { return None; }
        let columns = self.size[0].div_ceil(self.cell[0]) as usize;
        let x = (index % columns) as u32 * self.cell[0];
        let y = (index / columns) as u32 * self.cell[1];
        let right = (x + self.cell[0]).min(self.size[0]);
        let bottom = (y + self.cell[1]).min(self.size[1]);
        Some(Area {
            inner: [x, y, right, bottom],
            outer: [x.saturating_sub(1), y.saturating_sub(1),
                (right + 1).min(self.size[0]), (bottom + 1).min(self.size[1])],
        })
    }
    fn index_bytes(&self) -> Result<usize> {
        self.count
            .checked_mul(std::mem::size_of::<Upload>() + std::mem::size_of::<TextureTile>()
                + std::mem::size_of::<usize>() + 512)
            .and_then(|n| n.checked_add(512))
            .ok_or_else(|| "Native presentation index overflow".into())
    }
    pub(super) fn retained_upload_bytes(&self) -> Result<usize> {
        self.areas().try_fold(self.index_bytes()?, |bytes, area| {
            let pixels = ((area.outer[2] - area.outer[0]) as usize)
                .checked_mul((area.outer[3] - area.outer[1]) as usize)
                .ok_or("Native upload footprint overflow")?;
            bytes.checked_add(retirement::image_bytes(pixels)?)
                .ok_or_else(|| "Native upload footprint overflow".into())
        })
    }
}
pub(super) trait Rows {
    fn retire_before(&mut self, _row: u32) {}
    fn row(&self, y: u32) -> Result<&[u8]>;
    fn columns(&self, y: u32, width: u32, columns: std::ops::Range<u32>) -> Result<&[u8]> {
        let row = self.row(y)?;
        if row.len() != width as usize * 4 {
            return Err("Native display row has an inconsistent footprint".into());
        }
        row.get(columns.start as usize * 4..columns.end as usize * 4)
            .ok_or_else(|| "Native upload lies outside its row".into())
    }
}
pub(super) struct OwnedImage {
    // Dropping an unpublished candidate destroys pixels before their ticket.
    pub image: Arc<egui::ColorImage>,
    pub ticket: retirement::Ticket,
}
impl OwnedImage {
    pub fn convert(
        size: [usize; 2],
        retirement: &Retirement,
        mut pixel: impl FnMut(usize) -> Result<[u8; 4]>,
    ) -> Result<Self> {
        Self::build(size, retirement, |pixels, count| {
            for index in 0..count {
                let [r, g, b, a] = pixel(index)?;
                pixels.push(egui::Color32::from_rgba_unmultiplied(r, g, b, a));
            }
            Ok(())
        })
    }

    fn build(
        size: [usize; 2],
        retirement: &Retirement,
        fill: impl FnOnce(&mut Vec<egui::Color32>, usize) -> Result<()>,
    ) -> Result<Self> {
        let count = size[0]
            .checked_mul(size[1])
            .filter(|n| *n > 0 && *n <= MAX_PIXELS)
            .ok_or("Native upload dimensions exceed their bounded conversion")?;
        let ticket = retirement.admit(count)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(count)
            .map_err(|_| "Native display pixel allocation refused")?;
        fill(&mut pixels, count)?;
        if pixels.len() != count {
            return Err("Native upload conversion returned an incomplete image".into());
        }
        Ok(Self {
            image: Arc::new(egui::ColorImage { size, pixels }),
            ticket,
        })
    }
    pub fn upload(self, ctx: &egui::Context) -> egui::TextureHandle {
        self.upload_tracked(ctx).0
    }
    pub(super) fn upload_tracked(self, ctx: &egui::Context) -> (egui::TextureHandle, retirement::Fence) {
        let Self { image, ticket } = self;
        let fence = ticket.track(&image);
        let texture = ctx.load_texture(
            "native-preview",
            egui::ImageData::Color(image),
            egui::TextureOptions::LINEAR,
        );
        (texture, fence)
    }
}
pub(super) struct Upload {
    pub area: Area,
    pub pixels: OwnedImage,
}
impl Upload {
    pub(super) fn prepare(
        area: Area,
        width: u32,
        rows: &impl Rows,
        retirement: &Retirement,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        let [left, top, right, bottom] = area.outer;
        let size = [(right - left) as usize, (bottom - top) as usize];
        let columns = left..right;
        let pixels = OwnedImage::build(size, retirement, |pixels, _| {
            for y in top..bottom {
                check()?;
                let row = rows.columns(y, width, columns.clone())?;
                pixels.extend(row.chunks_exact(4).map(|p| {
                    egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])
                }));
            }
            Ok(())
        })?;
        Ok(Self { area, pixels })
    }
}
pub(super) struct Tiles {
    pub uploads: Vec<Upload>,
    size: [u32; 2],
    charge: Reservation,
}
impl Tiles {
    pub(super) fn with_textures(self, tiles: Vec<TextureTile>) -> Result<Textures> {
        let count = tiles.iter().filter(|tile| tile.digest.is_some()).count();
        let mut by_content = Vec::new();
        by_content.try_reserve_exact(count).map_err(|_| "Native texture lookup allocation refused")?;
        if by_content.capacity() > count {
            return Err("Native texture lookup exceeds admission".into());
        }
        by_content.extend(tiles.iter().enumerate().filter_map(|(i, tile)| tile.digest.map(|_| i)));
        by_content.sort_unstable_by_key(|&i| tiles[i].content_key());
        Ok(Textures {
            tiles,
            by_content,
            size: self.size,
            _charge: self.charge,
        })
    }
    pub(super) fn allocate(layout: &Layout, retirement: &Retirement) -> Result<Self> {
        let bytes = layout.index_bytes()?;
        let charge = retirement
            .pool()
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut uploads = Vec::new();
        uploads
            .try_reserve_exact(layout.count)
            .map_err(|_| "Native presentation index allocation refused")?;
        Ok(Self { uploads, size: layout.size, charge })
    }
    pub fn prepare(
        width: u32,
        height: u32,
        side: usize,
        rows: &mut impl Rows,
        retirement: &Retirement,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        let layout = Layout::new(width, height, side)?;
        let mut candidate = Self::allocate(&layout, retirement)?;
        for area in layout.areas() {
            check()?;
            // Areas are ordered in horizontal bands. Preserve the current
            // band's top filtering neighbor until every tile has copied it.
            rows.retire_before(area.outer[1]);
            candidate.uploads.push(Upload::prepare(area, width, rows, retirement, check)?);
        }
        check()?;
        rows.retire_before(height);
        Ok(candidate)
    }
    pub fn publish(
        self,
        ctx: &egui::Context,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Textures> {
        check()?;
        let limit = ctx.input(|i| i.max_texture_side);
        if self
            .uploads
            .iter()
            .any(|u| u.pixels.image.size.iter().any(|v| *v > limit))
        {
            return Err("Native texture limit changed before presentation".into());
        }
        let mut tiles = Vec::new();
        tiles
            .try_reserve_exact(self.uploads.len())
            .map_err(|_| "Native texture handle allocation refused")?;
        check()?;
        // Every conversion and fallible admission is complete. Renderer applies
        // all set deltas before drawing this complete replacement texture set.
        for upload in self.uploads {
            let texture = upload.pixels.upload(ctx);
            tiles.push(TextureTile {
                area: upload.area,
                texture,
                digest: None,
                transparent: false,
            });
        }
        check()?;
        Ok(Textures {
            tiles,
            by_content: Vec::new(),
            size: self.size,
            _charge: self.charge,
        })
    }
}
#[derive(Clone)]
pub(super) struct TextureTile {
    pub area: Area,
    pub texture: egui::TextureHandle,
    pub digest: Option<[u8; 32]>,
    // True only when the content scan also checked the filtering halo.
    pub transparent: bool,
}
impl TextureTile {
    fn content_key(&self) -> Option<([u32; 2], [u8; 32])> {
        self.digest.map(|digest| (self.area.texture_size(), digest))
    }
}
impl Area {
    pub(super) fn texture_size(self) -> [u32; 2] {
        [self.outer[2] - self.outer[0], self.outer[3] - self.outer[1]]
    }
}
pub(super) struct Textures {
    pub tiles: Vec<TextureTile>,
    by_content: Vec<usize>,
    pub size: [u32; 2],
    _charge: Reservation,
}
impl Textures {
    pub(super) fn at_same_position(&self, area: Area, offset: [i32; 2]) -> Option<usize> {
        let first = self.tiles.first()?;
        let [_, _, cw, ch] = first.area.inner;
        let x = u32::try_from(area.inner[0] as i32 + offset[0]).ok()?;
        let y = u32::try_from(area.inner[1] as i32 + offset[1]).ok()?;
        if x >= self.size[0] || y >= self.size[1] || x % cw != 0 || y % ch != 0 {
            return None;
        }
        let index = (y / ch * self.size[0].div_ceil(cw) + x / cw) as usize;
        let old = self.tiles.get(index)?;
        area.outer.iter().zip(old.area.outer).enumerate()
            .all(|(i, (&new, old))| new as i32 + offset[i % 2] == old as i32)
            .then_some(index)
    }
    pub(super) fn reusable(&self, index: usize, area: Area, digest: [u8; 32]) -> Option<&TextureTile> {
        let key = Some((area.texture_size(), digest));
        if let Some(tile) = self.tiles.get(index).filter(|tile| tile.content_key() == key) {
            return Some(tile);
        }
        let slot = self.by_content
            .binary_search_by_key(&key, |&i| self.tiles[i].content_key())
            .ok()?;
        Some(&self.tiles[self.by_content[slot]])
    }
    pub(super) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self._charge.uses_budget(pool)
    }
    #[cfg(test)]
    pub(super) fn ids(&self) -> impl Iterator<Item = egui::TextureId> + '_ {
        self.tiles.iter().map(|tile| tile.texture.id())
    }
    pub fn paint_with(
        &self,
        painter: &egui::Painter,
        project: impl Fn([f32; 2]) -> egui::Pos2,
        tint: egui::Color32,
    ) {
        paint_tiles(&self.tiles, self.size, painter, project, tint);
    }
}

pub(super) fn paint_tiles(
    tiles: &[TextureTile],
    size: [u32; 2],
    painter: &egui::Painter,
    project: impl Fn([f32; 2]) -> egui::Pos2,
    tint: egui::Color32,
) {
    let clip = painter.clip_rect();
    if !clip.is_positive() {
        return;
    }
    // Canvas and thumbnail projections are affine. A fitted image needs no
    // per-tile bounds checks, and a wholly hidden image needs no tile work.
    let clip_tiles = if tiles.len() > 1 {
        let corners = [[0., 0.], [1., 0.], [1., 1.], [0., 1.]].map(&project);
        let image = egui::Rect::from_points(&corners);
        if !clip.intersects(image) {
            return;
        }
        !clip.contains_rect(image)
    } else {
        true
    };
    for tile in tiles {
        if tile.transparent {
            continue;
        }
        let a = tile.area;
        let points = [
            [a.inner[0], a.inner[1]],
            [a.inner[2], a.inner[1]],
            [a.inner[2], a.inner[3]],
            [a.inner[0], a.inner[3]],
        ];
        let positions = points.map(|[x, y]| {
            project([x as f32 / size[0] as f32, y as f32 / size[1] as f32])
        });
        if clip_tiles && !clip.intersects(egui::Rect::from_points(&positions)) {
            continue;
        }
        let width = (a.outer[2] - a.outer[0]) as f32;
        let height = (a.outer[3] - a.outer[1]) as f32;
        let mut mesh = egui::epaint::Mesh::with_texture(tile.texture.id());
        for ([x, y], pos) in points.into_iter().zip(positions) {
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv: egui::pos2(
                    (x - a.outer[0]) as f32 / width,
                    (y - a.outer[1]) as f32 / height,
                ),
                color: tint,
            });
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        painter.add(egui::Shape::mesh(mesh));
    }
}
