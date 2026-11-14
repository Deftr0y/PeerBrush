//! Renderer backpressure for a complete, still-hidden texture candidate.
use super::*;

const BATCH: usize = 64;
pub(super) struct Batch<'a> {
    context: &'a egui::Context,
    handles: Vec<tiles::TextureTile>,
    fences: Vec<retirement::Fence>,
    limit: usize,
    pool: ResidentBudget,
    byte_limit: usize,
    pending_bytes: usize,
    // Each axis has at most three outer sizes: first, interior and last.
    transparent: [Option<usize>; 9],
}
impl<'a> Batch<'a> {
    pub(super) fn new(context: &'a egui::Context, count: usize, pool: &ResidentBudget) -> Result<Self> {
        // Both vectors fit the already-admitted tile/handle index allowance.
        let mut handles = Vec::new();
        handles.try_reserve_exact(count).map_err(|_| "Native texture handle allocation refused")?;
        let mut fences = Vec::new();
        let limit = count.min(BATCH);
        fences.try_reserve_exact(limit).map_err(|_| "Native upload batch allocation refused")?;
        // Leave most observed headroom for the next engine stripe. This is
        // batching policy; every image still obtains its actual reservation.
        let byte_limit = (pool.limit().saturating_sub(pool.used()) / 4)
            .min(BATCH * retirement::image_bytes(MAX_PIXELS)?).max(1);
        Ok(Self {
            context,
            handles,
            fences,
            limit,
            pool: pool.clone(),
            byte_limit,
            pending_bytes: 0,
            transparent: [None; 9],
        })
    }
    /// Retire queued owners before asking admission for the next converted tile.
    /// This is a headroom check, not a reservation; conversion still admits it.
    pub(super) fn before_conversion(&mut self, pixels: usize, check: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        check()?;
        let bytes = retirement::image_bytes(pixels)?;
        if !self.fences.is_empty() && (self.pending_bytes.saturating_add(bytes) > self.byte_limit
            || self.pool.limit().saturating_sub(self.pool.used()) < bytes)
        {
            self.flush(check)?;
        }
        Ok(())
    }
    fn check_handle_capacity(&self) -> Result<()> {
        if self.handles.len() >= self.handles.capacity() {
            return Err("Native texture handles exceed their admitted index".into());
        }
        Ok(())
    }
    pub(super) fn reuse(&mut self, tile: &tiles::TextureTile, area: tiles::Area) -> Result<()> {
        self.check_handle_capacity()?;
        let mut tile = tile.clone();
        tile.area = area;
        self.handles.push(tile);
        Ok(())
    }
    pub(super) fn reuse_transparent(&mut self, area: tiles::Area) -> Result<bool> {
        let Some(index) = self.transparent.iter().flatten().copied().find(|&index| {
            self.handles[index].area.texture_size() == area.texture_size()
        }) else {
            return Ok(false);
        };
        self.check_handle_capacity()?;
        let mut tile = self.handles[index].clone();
        tile.area = area;
        self.handles.push(tile);
        Ok(true)
    }
    pub(super) fn remember_transparent(&mut self) {
        if let Some(slot) = self.transparent.iter_mut().find(|slot| slot.is_none()) {
            *slot = self.handles.len().checked_sub(1);
        }
    }
    pub(super) fn push(
        &mut self,
        upload: tiles::Upload,
        digest: Option<[u8; 32]>,
        transparent: bool,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        check()?;
        self.check_handle_capacity()?;
        let limit = self.context.input(|input| input.max_texture_side);
        if upload.pixels.image.size.iter().any(|side| *side > limit) {
            return Err("Native texture limit changed during upload".into());
        }
        let bytes = retirement::image_bytes(upload.pixels.image.pixels.len())?;
        if !self.fences.is_empty() && self.pending_bytes.saturating_add(bytes) > self.byte_limit {
            self.flush(check)?;
        }
        let (texture, fence) = upload.pixels.upload_tracked(self.context);
        self.handles.push(tiles::TextureTile { area: upload.area, texture, digest, transparent });
        self.fences.push(fence);
        self.pending_bytes += bytes;
        self.context.request_repaint();
        if self.fences.len() == self.limit || self.pending_bytes >= self.byte_limit
            || self.pool.limit().saturating_sub(self.pool.used()) < retirement::image_bytes(MAX_PIXELS)?
        {
            self.flush(check)?;
        }
        Ok(())
    }
    fn flush(&mut self, check: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        if self.fences.is_empty() { return check(); }
        self.context.request_repaint();
        for fence in self.fences.drain(..) { fence.wait(check)?; }
        self.pending_bytes = 0;
        check()
    }
    pub(super) fn finish(mut self, check: &mut dyn FnMut() -> Result<()>) -> Result<Vec<tiles::TextureTile>> {
        self.flush(check)?;
        Ok(self.handles)
    }
}

pub(super) enum Candidate {
    Pixels(tiles::Tiles),
    Uploaded { textures: tiles::Textures, context: egui::Context },
}
impl Candidate {
    pub(super) fn publish(self, ctx: &egui::Context, check: &mut dyn FnMut() -> Result<()>) -> Result<tiles::Textures> {
        check()?;
        match self {
            Self::Pixels(pixels) => pixels.publish(ctx, check),
            Self::Uploaded { textures, context } => {
                if !Arc::ptr_eq(&ctx.tex_manager(), &context.tex_manager()) {
                    return Err("Native texture candidate belongs to another display context".into());
                }
                let limit = ctx.input(|input| input.max_texture_side);
                if textures.tiles.iter().any(|tile| tile.texture.size().iter().any(|side| *side > limit)) {
                    return Err("Native texture limit changed before presentation".into());
                }
                check()?;
                Ok(textures)
            }
        }
    }
    #[cfg(test)]
    pub(super) fn pixels(&self) -> &tiles::Tiles {
        match self { Self::Pixels(pixels) => pixels, Self::Uploaded { .. } => panic!("Expected CPU candidate") }
    }
}
