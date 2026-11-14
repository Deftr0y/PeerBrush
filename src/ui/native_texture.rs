//! Bounded stock-egui uploads. Pixel charges follow the actual ColorImage owner,
//! including queued/cloned deltas after presenter/context/texture teardown.
use crate::{
    large_document::{Reservation, ResidentBudget},
    render::native_preview::{Frame, Request, StripePixels, Tag, TextureReuse},
};
use eframe::egui;
use std::sync::Arc;
#[path = "native_texture/retirement.rs"]
mod retirement;
#[path = "native_texture/tiles.rs"]
mod tiles;
mod stream;
mod upload;
pub(crate) use retirement::Retirement;
pub(super) fn thumbnail(
    image: crate::render::mask_thumbnail::Thumbnail,
    retirement: &Retirement,
    ctx: &egui::Context,
) -> Result<egui::TextureHandle> {
    if !image.pixels.uses_budget(retirement.pool()) {
        return Err("Thumbnail uses another admission pool".into());
    }
    let owned = tiles::OwnedImage::convert(
        [image.size[0] as usize, image.size[1] as usize],
        retirement,
        |index| {
            image
                .pixels
                .get(index * 4..index * 4 + 4)
                .ok_or_else(|| "Incomplete mask thumbnail".into())
                .map(|p| p.try_into().unwrap())
        },
    )?;
    Ok(owned.upload(ctx))
}
const MAX_PIXELS: usize = 128 * 1024;
pub(super) fn viewport_tile_cell(size: [u32; 2], side: usize) -> Option<[i32; 2]> {
    tiles::Layout::streamed(size[0], size[1], side).ok()
        .map(|layout| layout.cell.map(|value| value as i32))
}
type Result<T> = std::result::Result<T, String>;

/// Resident display bytes already exist as a full frame. Split uploads only
/// when the renderer cannot hold that frame in a single texture.
pub(super) struct RgbaTexture {
    size: [u32; 2],
    tiles: Vec<tiles::TextureTile>,
}

impl RgbaTexture {
    pub fn new(ctx: &egui::Context, name: &str, size: [u32; 2], bytes: &[u8]) -> Result<Self> {
        Self::validate(size, bytes)?;
        let [width, height] = size;
        let limit = ctx.input(|i| i.max_texture_side);
        let areas = if width as usize <= limit && height as usize <= limit {
            let bounds = [0, 0, width, height];
            vec![tiles::Area { inner: bounds, outer: bounds }]
        } else {
            tiles::Layout::new(width, height, limit)?.areas().collect()
        };
        let tiles = areas.into_iter().map(|area| tiles::TextureTile {
            area,
            texture: ctx.load_texture(name, Self::image(size, bytes, area.outer), egui::TextureOptions::LINEAR),
            digest: None,
            transparent: false,
        }).collect();
        Ok(Self { size, tiles })
    }

    pub fn update(&mut self, ctx: &egui::Context, name: &str, size: [u32; 2], bytes: &[u8]) -> Result<()> {
        let limit = ctx.input(|i| i.max_texture_side);
        if self.size != size || self.tiles.iter().any(|tile| tile.texture.size().iter().any(|&side| side > limit)) {
            *self = Self::new(ctx, name, size, bytes)?;
            return Ok(());
        }
        Self::validate(size, bytes)?;
        for tile in &mut self.tiles {
            tile.texture.set(Self::image(size, bytes, tile.area.outer), egui::TextureOptions::LINEAR);
        }
        Ok(())
    }

    fn validate([width, height]: [u32; 2], bytes: &[u8]) -> Result<()> {
        if width == 0 || height == 0
            || (width as usize).checked_mul(height as usize).and_then(|area| area.checked_mul(4)) != Some(bytes.len())
        {
            return Err("Incomplete RGBA preview frame".into());
        }
        Ok(())
    }

    fn image([width, _]: [u32; 2], bytes: &[u8], [left, top, right, bottom]: [u32; 4]) -> egui::ColorImage {
        let size = [(right - left) as usize, (bottom - top) as usize];
        let stride = width as usize * 4;
        if left == 0 && right == width {
            return egui::ColorImage::from_rgba_unmultiplied(size, &bytes[top as usize * stride..bottom as usize * stride]);
        }
        let mut pixels = Vec::with_capacity(size[0] * size[1]);
        for y in top as usize..bottom as usize {
            let row = &bytes[y * stride + left as usize * 4..y * stride + right as usize * 4];
            pixels.extend(row.chunks_exact(4).map(|pixel| {
                egui::Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3])
            }));
        }
        egui::ColorImage { size, pixels }
    }

    pub fn patch(&mut self, bytes: &[u8], [left, top, right, bottom]: [u32; 4]) {
        for tile in &mut self.tiles {
            let [l, t, r, b] = tile.area.outer;
            let area = [left.max(l), top.max(t), right.min(r), bottom.min(b)];
            if area[0] < area[2] && area[1] < area[3] {
                tile.texture.set_partial(
                    [(area[0] - l) as usize, (area[1] - t) as usize],
                    Self::image(self.size, bytes, area),
                    egui::TextureOptions::LINEAR,
                );
            }
        }
    }

    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    pub fn paint(&self, painter: &egui::Painter, project: impl Fn([f32; 2]) -> egui::Pos2) {
        tiles::paint_tiles(&self.tiles, self.size, painter, project, egui::Color32::WHITE);
    }

    #[cfg(test)]
    pub fn id(&self) -> egui::TextureId {
        self.tiles[0].texture.id()
    }
}

struct FrameRows {
    stripes: crate::render::native_preview::StripeStream,
    width: u32,
}
impl tiles::Rows for FrameRows {
    fn retire_before(&mut self, row: u32) { self.stripes.retire_before(row); }
    fn row(&self, y: u32) -> Result<&[u8]> {
        let stripes = self.stripes.stripes();
        let index = stripes.partition_point(|stripe| stripe.rows()[1] <= y);
        let stripe = stripes
            .get(index)
            .ok_or("Native presentation row is outside its complete frame")?;
        if y < stripe.rows()[0] {
            return Err("Native presentation frame has a row gap".into());
        }
        let stride = self.width as usize * 4;
        let at = (y - stripe.rows()[0]) as usize * stride;
        stripe
            .bytes()
            .get(at..at + stride)
            .ok_or_else(|| "Native presentation frame row is incomplete".into())
    }
}
pub(crate) struct Prepared {
    outline: Arc<crate::engine::native::SelectionOutline>,
    tiles: upload::Candidate,
    tag: Tag,
    status: crate::color_profile::prepared::DisplayStatus,
}
impl crate::render::native_preview::DisplayOutput for Prepared {
    fn display_tag(&self) -> Tag { self.tag.clone() }
}
impl Prepared {
    pub(super) fn from_request(
        request: &impl crate::render::native_preview::DisplayRequest,
        retirement: &Retirement,
        side: usize,
        context: Option<&egui::Context>,
        reuse: Option<&Reuse>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        Self::render_request(request, retirement, side, context.map(|ctx| (ctx, reuse, true)), check)
    }

    pub(super) fn from_one_shot_request(
        request: &impl crate::render::native_preview::DisplayRequest,
        retirement: &Retirement,
        side: usize,
        context: &egui::Context,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        Self::render_request(request, retirement, side, Some((context, None, false)), check)
    }

    pub(super) fn from_canvas_request_reusing_tiles(
        request: &Request,
        retirement: &Retirement,
        side: usize,
        context: Option<&egui::Context>,
        reuse: Option<&Reuse>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        if let Some(uploads) = Self::try_reusing_tiles(request, retirement, side, context, reuse, check)? {
            return Ok(uploads);
        }
        Self::from_request(request, retirement, side, context, reuse, check)
    }

    pub(super) fn try_reusing_tiles(
        request: &Request,
        retirement: &Retirement,
        side: usize,
        context: Option<&egui::Context>,
        reuse: Option<&Reuse>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Option<Self>> {
        let (Some(context), Some(reuse)) = (context, reuse) else { return Ok(None); };
        let Some(previous) = reuse.textures_for(context, request.budget()) else { return Ok(None); };
        check()?;
        if !request.budget().same_pool(retirement.pool()) {
            return Err("Native upload request uses another admission pool".into());
        }
        let Some(plan) = request.texture_reuse(&reuse.tag)? else { return Ok(None); };
        let key = request.key();
        let stream = stream::Stream::for_upload(key.width, key.height, side, retirement, Some(previous), true)?;
        let Some(positions) = stream.position_reuse(reuse.tag.key().rect, key.rect, plan.damage()) else {
            return Ok(None);
        };
        Self::finish_request(request, stream, Some(context), Some((plan, positions)), check).map(Some)
    }

    fn render_request(
        request: &impl crate::render::native_preview::DisplayRequest,
        retirement: &Retirement,
        side: usize,
        display: Option<(&egui::Context, Option<&Reuse>, bool)>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        if !request.budget().same_pool(retirement.pool()) {
            return Err("Native upload request uses another admission pool".into());
        }
        let context = display.map(|(context, _, _)| context);
        let stream = if let Some((context, reuse, track_content)) = display {
            let previous = reuse.and_then(|reuse| reuse.textures_for(context, request.budget()));
            stream::Stream::for_upload(
                request.key().width, request.key().height, side, retirement,
                previous, track_content,
            )?
        } else {
            stream::Stream::new(request.key().width, request.key().height, side, retirement)?
        };
        Self::finish_request(request, stream, context, None, check)
    }

    fn finish_request(
        request: &impl crate::render::native_preview::DisplayRequest,
        mut stream: stream::Stream<'_>,
        context: Option<&egui::Context>,
        reuse: Option<(TextureReuse<'_>, stream::PanTiles<'_>)>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        request.check()?;
        let mut batch = context.map(|ctx| upload::Batch::new(ctx, stream.tile_count(), request.budget())).transpose()?;
        // Rows and renderer waits use admitted display data. The renderer
        // checks backing ownership during source reads and stripe handoff.
        let mut live = || {
            request.check_pinned_work()?;
            check()?;
            request.check_pinned_work()
        };
        let checked = std::cell::RefCell::new(&mut live);
        let mut check = || (checked.borrow_mut())();
        let positions = reuse.as_ref().map(|(_, positions)| positions);
        let mut consume = |rows, bytes| {
            stream.push(rows, bytes, batch.as_mut(), positions, &mut || (checked.borrow_mut())())
        };
        let complete = match &reuse {
            Some((plan, positions)) => plan.render_into(positions, &mut check, &mut consume),
            None => request.render_into(&mut check, &mut |rows, bytes| consume(rows, Some(StripePixels::Full(bytes)))),
        }?;
        drop(reuse);
        if !complete.tag.same_allocation(&request.tag()) {
            return Err("Native upload completion differs from its captured request".into());
        }
        let tiles = if let (Some(batch), Some(context)) = (batch, context) {
            let handles = batch.finish(&mut || (checked.borrow_mut())())?;
            upload::Candidate::Uploaded {
                textures: stream.finish_uploaded(handles, &mut || (checked.borrow_mut())())?, context: context.clone(),
            }
        } else {
            upload::Candidate::Pixels(stream.finish(&mut || (checked.borrow_mut())())?)
        };
        request.check()?;
        Ok(Self { outline: complete.outline, tiles, tag: complete.tag, status: complete.display_status })
    }
    pub(crate) fn retained_upload_bytes(frame: &Frame, side: usize, streamed: bool) -> Result<usize> {
        let layout = tiles::Layout::new(frame.key().width, frame.key().height, side)?;
        let bytes = layout.retained_upload_bytes()?;
        let stripe_index = if streamed { stream::stripe_index_bytes(&layout) } else { 0 };
        bytes.checked_add(stripe_index).ok_or_else(|| "Native upload footprint overflow".into())
    }
    /// Run off UI. Consumed stripes release after their last filtering neighbor;
    /// live owners remain charged. No source IO,
    /// layer metadata, profile executor or session is retained in this output.
    #[cfg(test)]
    pub(crate) fn from_frame(
        frame: Frame,
        retirement: &Retirement,
        max_texture_side: usize,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        Self::from_frame_with_display(frame, retirement, max_texture_side, None, check)
    }
    pub(super) fn from_frame_with_display(
        mut frame: Frame,
        retirement: &Retirement,
        max_texture_side: usize,
        display: Option<(&egui::Context, Option<&Reuse>)>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        if !frame.budget().same_pool(retirement.pool()) {
            return Err("Native upload frame uses another admission pool".into());
        }
        if frame
            .stripes()
            .iter()
            .any(|s| !s.bytes().uses_budget(retirement.pool()))
        {
            return Err("Native upload frame uses another admission pool".into());
        }
        let mut next = 0;
        for stripe in frame.stripes() {
            if stripe.rows()[0] != next
                || stripe.rows()[1] <= next
                || stripe.bytes().len()
                    != (stripe.rows()[1] - next) as usize * frame.key().width as usize * 4
            {
                return Err("Native display stripe coverage is inconsistent".into());
            }
            next = stripe.rows()[1];
        }
        if next != frame.key().height {
            return Err("Native display frame is incomplete".into());
        }
        let width = frame.key().width;
        let height = frame.key().height;
        let tag = frame.tag();
        let status = frame.display_status();
        let outline = frame.take_outline()?;
        let mut stripes = frame.into_stripes();
        let tiles = if let Some((context, reuse)) = display {
            let budget = retirement.pool();
            let previous = reuse.and_then(|reuse| reuse.textures_for(context, budget));
            let mut stream = stream::Stream::for_upload(width, height, max_texture_side, retirement, previous, true)?;
            let mut batch = upload::Batch::new(context, stream.tile_count(), budget)?;
            while let Some((rows, bytes)) = stripes.take_next() {
                stream.push(rows, Some(StripePixels::Full(bytes)), Some(&mut batch), None, check)?;
            }
            drop(stripes);
            let handles = batch.finish(check)?;
            upload::Candidate::Uploaded {
                textures: stream.finish_uploaded(handles, check)?,
                context: context.clone(),
            }
        } else {
            let mut rows = FrameRows { stripes, width };
            upload::Candidate::Pixels(tiles::Tiles::prepare(
                width, height, max_texture_side, &mut rows, retirement, check,
            )?)
        };
        Ok(Self {
            outline,
            tiles,
            tag,
            status,
        })
    }
    pub(crate) fn tag(&self) -> &Tag {
        &self.tag
    }
    pub(crate) fn profile_warning(&self) -> Option<&'static str> {
        self.status.warning()
    }
    pub(super) fn publish(
        self,
        ctx: &egui::Context,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Canvas> {
        let textures = self.tiles.publish(ctx, check)?;
        Ok(Canvas {
            outline: self.outline,
            reuse: Reuse { textures: Arc::new(textures), context: ctx.clone(), tag: self.tag },
            status: self.status,
        })
    }
}
#[derive(Clone)]
pub(super) struct Reuse {
    textures: Arc<tiles::Textures>,
    context: egui::Context,
    tag: Tag,
}
impl Reuse {
    pub(super) fn tag(&self) -> &Tag { &self.tag }
    fn textures_for(&self, context: &egui::Context, budget: &ResidentBudget) -> Option<&tiles::Textures> {
        (Arc::ptr_eq(&context.tex_manager(), &self.context.tex_manager())
            && self.textures.uses_budget(budget)).then_some(self.textures.as_ref())
    }
}
pub(crate) struct Canvas {
    outline: Arc<crate::engine::native::SelectionOutline>,
    reuse: Reuse,
    status: crate::color_profile::prepared::DisplayStatus,
}
impl Canvas {
    pub(super) fn reuse(&self) -> Reuse {
        self.reuse.clone()
    }
    pub(crate) fn tag(&self) -> &Tag {
        &self.reuse.tag
    }
    pub(crate) fn outline(&self) -> &crate::engine::native::SelectionOutline {
        &self.outline
    }
    #[cfg(test)]
    pub(crate) fn texture_ids(&self) -> impl Iterator<Item = egui::TextureId> + '_ {
        self.reuse.textures.ids()
    }
    pub(crate) fn key(&self) -> &crate::render::native_preview::Key {
        self.tag().key()
    }
    pub(super) fn reusable_tile_cell(&self) -> Option<[i32; 2]> {
        let first = self.reuse.textures.tiles.first()?;
        first.digest?;
        let [left, top, right, bottom] = first.area.inner;
        Some([(right - left) as i32, (bottom - top) as i32])
    }
    pub(crate) fn profile_warning(&self) -> Option<&'static str> {
        self.status.warning()
    }
    pub(crate) fn paint_with(
        &self,
        painter: &egui::Painter,
        project: impl Fn([f32; 2]) -> egui::Pos2,
        tint: egui::Color32,
    ) {
        let rect = self.key().rect;
        self.paint_normalized(
            painter,
            |uv| {
                project([
                    rect[0] as f32 + (rect[2] - rect[0]) as f32 * uv[0],
                    rect[1] as f32 + (rect[3] - rect[1]) as f32 * uv[1],
                ])
            },
            tint,
        )
    }
    pub(super) fn paint_normalized(
        &self,
        painter: &egui::Painter,
        project: impl Fn([f32; 2]) -> egui::Pos2,
        tint: egui::Color32,
    ) {
        self.reuse.textures.paint_with(painter, project, tint);
    }
}
#[derive(Default)]
pub(crate) struct Presenter {
    current: Option<Canvas>,
}
impl Presenter {
    /// The caller rechecks actual current source/epoch before supplying this tag.
    /// A refusal leaves the preceding complete canvas untouched.
    pub(crate) fn present(
        &mut self,
        prepared: Prepared,
        current: &Tag,
        ctx: &egui::Context,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        check()?;
        if prepared.tag.key() != current.key() {
            return Err("Native display upload belongs to a stale request".into());
        }
        self.current = Some(prepared.publish(ctx, check)?);
        Ok(())
    }
    pub(crate) fn current(&self) -> Option<&Canvas> {
        self.current.as_ref()
    }
    pub(crate) fn clear(&mut self) {
        self.current = None;
    }
}
#[cfg(test)]
#[path = "native_texture/tests.rs"]
mod tests;
