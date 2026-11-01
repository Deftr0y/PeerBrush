use super::*;

const MAX_TILES: usize = 4096;
#[derive(Clone, Copy, Debug)]
pub(super) struct Area {
    pub inner: [u32; 4],
    pub outer: [u32; 4],
}
pub(super) struct Layout {
    pub size: [u32; 2],
    pub cell: [u32; 2],
    pub count: usize,
}
impl Layout {
    pub fn new(width: u32, height: u32, side: usize) -> Result<Self> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 || side == 0 {
            return Err("Invalid native presentation dimensions or texture limit".into());
        }
        let w = width.min(side.min(8192) as u32);
        let h = height
            .min(side.min(8192) as u32)
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
        (0..self.size[1])
            .step_by(self.cell[1] as usize)
            .flat_map(move |y| {
                (0..self.size[0])
                    .step_by(self.cell[0] as usize)
                    .map(move |x| {
                        let right = (x + self.cell[0]).min(self.size[0]);
                        let bottom = (y + self.cell[1]).min(self.size[1]);
                        Area {
                            inner: [x, y, right, bottom],
                            outer: [
                                x.saturating_sub(1),
                                y.saturating_sub(1),
                                (right + 1).min(self.size[0]),
                                (bottom + 1).min(self.size[1]),
                            ],
                        }
                    })
            })
    }
}
pub(super) trait Rows {
    fn row(&self, y: u32) -> Result<&[u8]>;
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
        let count = size[0]
            .checked_mul(size[1])
            .filter(|n| *n > 0 && *n <= MAX_PIXELS)
            .ok_or("Native upload dimensions exceed their bounded conversion")?;
        let ticket = retirement.admit(count)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(count)
            .map_err(|_| "Native display pixel allocation refused")?;
        for index in 0..count {
            let p = pixel(index)?;
            pixels.push(egui::Color32::from_rgba_unmultiplied(
                p[0], p[1], p[2], p[3],
            ));
        }
        Ok(Self {
            image: Arc::new(egui::ColorImage { size, pixels }),
            ticket,
        })
    }
    pub fn upload(self, ctx: &egui::Context) -> egui::TextureHandle {
        let Self { image, ticket } = self;
        ticket.track(&image);
        ctx.load_texture(
            "native-preview",
            egui::ImageData::Color(image),
            egui::TextureOptions::LINEAR,
        )
    }
}
pub(super) struct Upload {
    pub area: Area,
    pub pixels: OwnedImage,
}
pub(super) struct Tiles {
    pub uploads: Vec<Upload>,
    size: [u32; 2],
    charge: Reservation,
}
impl Tiles {
    pub fn prepare(
        width: u32,
        height: u32,
        side: usize,
        rows: &impl Rows,
        retirement: &Retirement,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        let layout = Layout::new(width, height, side)?;
        let bytes = layout
            .count
            .checked_mul(std::mem::size_of::<Upload>() + std::mem::size_of::<TextureTile>() + 512)
            .and_then(|n| n.checked_add(512))
            .ok_or("Native presentation index overflow")?;
        let charge = retirement
            .pool()
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut uploads = Vec::new();
        uploads
            .try_reserve_exact(layout.count)
            .map_err(|_| "Native presentation index allocation refused")?;
        for area in layout.areas() {
            check()?;
            let size = [
                (area.outer[2] - area.outer[0]) as usize,
                (area.outer[3] - area.outer[1]) as usize,
            ];
            let mut current_y = None;
            let mut row: &[u8] = &[];
            let pixels = OwnedImage::convert(size, retirement, |index| {
                let y = area.outer[1] + (index / size[0]) as u32;
                if current_y != Some(y) {
                    check()?;
                    row = rows.row(y)?;
                    if row.len() != width as usize * 4 {
                        return Err("Native display row has an inconsistent footprint".into());
                    }
                    current_y = Some(y);
                }
                let x = area.outer[0] as usize + index % size[0];
                let at = x * 4;
                Ok([row[at], row[at + 1], row[at + 2], row[at + 3]])
            })?;
            uploads.push(Upload { area, pixels });
        }
        check()?;
        Ok(Self {
            uploads,
            size: [width, height],
            charge,
        })
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
            });
        }
        check()?;
        Ok(Textures {
            tiles,
            size: self.size,
            _charge: self.charge,
        })
    }
}
pub(super) struct TextureTile {
    pub area: Area,
    pub texture: egui::TextureHandle,
}
pub(super) struct Textures {
    pub tiles: Vec<TextureTile>,
    size: [u32; 2],
    _charge: Reservation,
}
impl Textures {
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
        for tile in &self.tiles {
            let a = tile.area;
            let width = (a.outer[2] - a.outer[0]) as f32;
            let height = (a.outer[3] - a.outer[1]) as f32;
            let points = [
                [a.inner[0], a.inner[1]],
                [a.inner[2], a.inner[1]],
                [a.inner[2], a.inner[3]],
                [a.inner[0], a.inner[3]],
            ];
            let mut mesh = egui::epaint::Mesh::with_texture(tile.texture.id());
            for [x, y] in points {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: project([
                        x as f32 / self.size[0] as f32,
                        y as f32 / self.size[1] as f32,
                    ]),
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
}
