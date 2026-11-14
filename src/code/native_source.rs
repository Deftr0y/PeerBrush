//! Frozen procedural source reads. The worker owns real source admission and
//! one admitted input tile; no editable resident copy or display projection.
use super::*;
use crate::{
    edit_bounds::input::{InputTile, RasterInput},
    engine::{native::NativeDocument, scene::SceneSnapshot},
    psd::memory36::{Admitted, MetadataCharge},
    raster::TILE,
    workspace::{native_dispatch, source_runtime::SourceReadSession},
};

pub(super) struct NativeSource {
    tile: Option<InputTile>,
    document: NativeDocument,
    scene: Arc<SceneSnapshot>,
    session: SourceReadSession,
}
impl NativeSource {
    /// Call off Engine/workspace locks. The requested observation is exact
    /// native authority, including its nominal source generation.
    pub(super) fn capture(
        shared: &Shared,
        job: &Job,
        document: &NativeDocument,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        let scene = {
            let mut engine = shared.lock().unwrap();
            guard(&mut engine, job, document.into())?;
            engine.capture_scene(None)?.into_native()?
        };
        let scene = Arc::new(scene);
        let session = native_dispatch::begin_read_scene(shared, &scene)?;
        let source = Self {
            tile: None,
            document: document.clone(),
            scene,
            session,
        };
        source.check(shared, job, check)?;
        Ok(source)
    }
    pub(super) fn view(&self) -> DocumentView<'_> {
        (&self.document).into()
    }
    pub(super) fn budget(&self) -> &crate::large_document::ResidentBudget {
        self.document.budget()
    }
    pub(super) fn check(
        &self,
        shared: &Shared,
        job: &Job,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        {
            let mut engine = shared
                .try_lock()
                .map_err(|_| "Code source is busy; read deferred")?;
            if engine.project_id != self.scene.identity().project_id {
                return Err("Code source project changed; current work preserved".into());
            }
            guard(&mut engine, job, self.view())?;
        }
        let runtime = native_dispatch::runtime_for(shared)?;
        if !self.session.uses_runtime(&runtime) {
            return Err("Code source service changed; current work preserved".into());
        }
        self.session.check_scene(&self.scene)?;
        check()
    }
    /// Same raw paint-layer/document-coordinate contract as resident code:
    /// native8 returns 0..255 and native16 returns the original 0..65535 words.
    pub(super) fn pixel(
        &mut self,
        shared: &Shared,
        job: &Job,
        layer: &str,
        x: INT,
        y: INT,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<[u16; 4], String> {
        self.check(shared, job, check)?;
        let pixel = self.read_pixel(layer, x, y, check)?;
        self.check(shared, job, check)?;
        Ok(pixel)
    }
    /// Raw reads are private to a checked point or a checked patch row.
    fn read_pixel(
        &mut self,
        layer: &str,
        x: INT,
        y: INT,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<[u16; 4], String> {
        let doc = self.document.metadata();
        if x < 0 || y < 0 || x >= INT::from(doc.width()) || y >= INT::from(doc.height()) {
            return Err("Pixel coordinates must be inside the document".into());
        }
        let input = RasterInput::layer(&self.document, layer)?;
        if input.layer_metadata().kind() != "paint" || input.layer_metadata().source().is_some() {
            return Err("Native pixel access requires a paint layer".into());
        }
        let [ox, oy] = input.origin();
        let [width, height] = input.shape();
        let (lx, ly) = (x - INT::from(ox), y - INT::from(oy));
        // A translated source's exterior is explicit zero. It is never a
        // substitute for a missing, unreadable or rejected backing tile.
        let pixel = if lx < 0 || ly < 0 || lx >= INT::from(width) || ly >= INT::from(height) {
            [0; 4]
        } else {
            let (lx, ly) = (lx as u32, ly as u32);
            let key = (lx / TILE, ly / TILE);
            if self
                .tile
                .as_ref()
                .is_none_or(|tile| tile.binding() != input.binding() || tile.key() != key)
            {
                self.tile = None;
                self.tile = Some(input.prepare_tile(key, &mut || {
                    check()?;
                    self.session.check_scene(&self.scene)
                })?);
            }
            let tile = self.tile.as_ref().unwrap();
            let stride = usize::from(input.depth() / 8) * 4;
            let at = ((ly % TILE) as usize * TILE as usize + (lx % TILE) as usize) * stride;
            let bytes = tile
                .bytes()
                .get(at..at + stride)
                .ok_or("Code source tile is incomplete")?;
            std::array::from_fn(|channel| {
                if input.depth() == 16 {
                    u16::from_be_bytes([bytes[channel * 2], bytes[channel * 2 + 1]])
                } else {
                    u16::from(bytes[channel])
                }
            })
        };
        Ok(pixel)
    }
    pub(super) fn patch(
        &mut self,
        shared: &Shared,
        job: &Job,
        layer: &str,
        rect: [i32; 4],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Admitted<Vec<u16>>, String> {
        self.check(shared, job, check)?;
        let doc = self.document.metadata();
        if rect[0] < 0
            || rect[1] < 0
            || rect[0] >= rect[2]
            || rect[1] >= rect[3]
            || rect[2] > doc.width() as i32
            || rect[3] > doc.height() as i32
        {
            return Err("Pixel rectangle must be inside the document".into());
        }
        let count = (rect[2] - rect[0]) as usize * (rect[3] - rect[1]) as usize;
        if count > MAX_PIXELS {
            return Err("Code exceeds 262144 patch pixels".into());
        }
        let bytes = layer
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(count * 8 + 1024))
            .ok_or("Code pixel buffer admission overflow")?;
        let owner = MetadataCharge::reserve(self.document.budget(), bytes, check)?;
        contained(
            self.view(),
            &job.scopes,
            &[Scope {
                target: Some(layer.into()),
                rect: Some(rect),
            }],
        )?;
        let mut words = Vec::new();
        words
            .try_reserve_exact(count * 4)
            .map_err(|_| "Code pixel buffer allocation refused")?;
        if words.capacity() * std::mem::size_of::<u16>() + 1024 > owner.bytes() {
            return Err("Code pixel buffer capacity exceeds its admission".into());
        }
        let (origin, shape) = {
            let input = RasterInput::layer(&self.document, layer)?;
            if input.layer_metadata().kind() != "paint" || input.layer_metadata().source().is_some()
            {
                return Err("Native pixel access requires a paint layer".into());
            }
            (input.origin(), input.shape())
        };
        words.resize(count * 4, 0);
        let left = (INT::from(rect[0]) - INT::from(origin[0])).clamp(0, INT::from(shape[0])) as u32;
        let top = (INT::from(rect[1]) - INT::from(origin[1])).clamp(0, INT::from(shape[1])) as u32;
        let right =
            (INT::from(rect[2]) - INT::from(origin[0])).clamp(0, INT::from(shape[0])) as u32;
        let bottom =
            (INT::from(rect[3]) - INT::from(origin[1])).clamp(0, INT::from(shape[1])) as u32;
        // Read each physical tile once, keeping the one-tile working bound
        // even when a row crosses tile boundaries. Exterior remains known zero.
        for ty in top / TILE..bottom.div_ceil(TILE) {
            for tx in left / TILE..right.div_ceil(TILE) {
                for sy in top.max(ty * TILE)..bottom.min((ty + 1) * TILE) {
                    self.check(shared, job, check)?;
                    for sx in left.max(tx * TILE)..right.min((tx + 1) * TILE) {
                        let x = INT::from(sx) + INT::from(origin[0]);
                        let y = INT::from(sy) + INT::from(origin[1]);
                        let pixel = self.read_pixel(layer, x, y, check)?;
                        let at = ((y - INT::from(rect[1])) as usize * (rect[2] - rect[0]) as usize
                            + (x - INT::from(rect[0])) as usize)
                            * 4;
                        words[at..at + 4].copy_from_slice(&pixel);
                    }
                }
            }
        }
        self.check(shared, job, check)?;
        Ok(Admitted::from_parts(Arc::new(words), owner))
    }
}

#[cfg(test)]
pub(crate) mod tests;
