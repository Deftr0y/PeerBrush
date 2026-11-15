//! Captured, source-backed preview rows. Display bytes own their admission;
//! completed frames retain no source handle or profile operation.
use crate::{
    color_profile::prepared::{profile_binding, ChargedWords, DisplayPlan, DisplayStatus},
    compositor::native::{
        prepare_filters_with_checks, LayerWindow, Native16, Native8, Plan, Stored, Topology,
    },
    engine::{
        native::{worker::RasterDamage, DocumentKey, EffectMetadata, LayerMetadata, NativeDocument},
        scene::{SceneIdentity, SceneSnapshot},
    },
    large_document::{PinnedBytes, Reservation, ResidentBudget, ScratchBytes},
    raster::backed::{Region, MAX_REGION_PIXELS},
    workspace::source_runtime::{ReadChecks, SourceReadSession},
};
use std::{ops::Range, sync::Arc};
type Result<T> = std::result::Result<T, String>;
pub(crate) const MAX_PREVIEW_PIXELS: u64 = 8192 * 8192;
pub(crate) const MAX_CANVAS_PIXELS: u64 = 16000 * 16000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LayerThumbnailKey {
    frame: [u32; 2],
    depth: u16,
    origin: [i32; 2],
    pixels: crate::raster::SurfaceKey,
    fill: Option<crate::raster::Pixel>,
    profile: Option<[u8; 32]>,
}

/// A raw target preview of a paint or fill layer without active effects
/// samples only its own pixels or fill color, before masks and composition.
pub(crate) fn layer_thumbnail_key(
    doc: &NativeDocument,
    layer: LayerMetadata<'_>,
    profile: Option<[u8; 32]>,
) -> Option<LayerThumbnailKey> {
    let metadata = doc.metadata();
    if metadata.read_only()
        || !matches!(layer.kind(), "paint" | "fill")
        || layer.effects().any(|effect| effect.enabled() && effect.weight() > 0.)
    {
        return None;
    }
    Some(LayerThumbnailKey {
        frame: [metadata.width(), metadata.height()],
        depth: metadata.depth(),
        origin: layer.origin(),
        pixels: doc.source(layer.pixels().slot()).ok()?.key(),
        fill: (layer.kind() == "fill").then(|| layer.color()),
        profile,
    })
}

mod guide;
mod selection;
pub(crate) use selection::SelectionView;
pub(crate) mod crossfade;
pub(crate) mod presentation;
mod transient;
pub(crate) use guide::{stage as stage_guide, stage_layer_frame as stage_layer_guide};
pub(crate) use transient::{
    preview_shared, preview_shared_with, render_prepared, render_prepared_with, render_review, render_review_with, Origin, SourceSupplement, TransientFrame,
};

/// Field order releases every source owner before the final session owner.
#[derive(Clone)]
pub(crate) struct Request {
    scene: Arc<SceneSnapshot>,
    prepared: Arc<Prepared>,
    session: SourceReadSession,
}

impl Request {
    /// Capture and source-session admission run outside Engine/UI locks.
    pub(crate) fn capture(
        scene: Arc<SceneSnapshot>,
        session: impl Into<SourceReadSession>,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
    ) -> Result<Self> {
        Self::capture_selection(scene, session, rect, edge, target, mask, None)
    }
    pub(crate) fn capture_selection(
        scene: Arc<SceneSnapshot>,
        session: impl Into<SourceReadSession>,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        selection_view: Option<&str>,
    ) -> Result<Self> {
        let selection_view = selection_view.map(SelectionView::parse).transpose()?;
        if selection_view.is_some() {
            scene.native_document()?;
        }
        let session = session.into();
        session.check_scene(&scene)?;
        if !scene.budget().same_pool(&session.resident())
            || !scene.disk_budget().same_pool(&session.disk())
        {
            return Err("Preview scene and source session have different admission".into());
        }
        let prepared = Arc::new(Prepared::capture(
            &scene,
            rect,
            edge,
            target,
            mask,
            selection_view,
            &mut || session.check(),
        )?);
        session.check()?;
        Ok(Self {
            scene,
            prepared,
            session,
        })
    }
    /// Canvas uploads stream their pixels; complete CPU-frame callers retain
    /// the separate frame limit. Tags cannot change after being shared.
    pub(crate) fn for_canvas(mut self, edge: u32) -> Result<Self> {
        self.check()?;
        let depth = self.scene.metadata().depth();
        let prepared = Arc::get_mut(&mut self.prepared).ok_or("Canvas sampling tag is already shared")?;
        prepared.stream_grid(depth, edge);
        self.check()?;
        Ok(self)
    }
    pub(crate) fn key(&self) -> &Key {
        &self.prepared.key
    }
    pub(crate) fn tag(&self) -> Tag {
        Tag(self.prepared.clone())
    }
    pub(crate) fn is_thumbnail(&self) -> bool {
        let key = self.key();
        let doc = self.scene.metadata();
        key.target.is_some()
            && key.selection_view.is_none()
            && key.width <= 48
            && key.height <= 48
            && key.rect == [0, 0, doc.width() as i32, doc.height() as i32]
    }
    pub(crate) fn budget(&self) -> &ResidentBudget { &self.prepared.pool }
    pub(crate) fn check(&self) -> Result<()> { self.session.check() }
    fn check_pinned_work(&self) -> Result<()> { self.session.check_pinned_work() }
    pub(crate) fn render(&self, check: &mut dyn FnMut() -> Result<()>) -> Result<Frame> {
        self.render_with_pan_baseline(None, check)
    }
    pub(crate) fn render_with_pan_baseline(
        &self,
        baseline: Option<&crossfade::Baseline>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Frame> {
        check()?;
        Frame::collect(&self.prepared, |consume| {
            self.render_into_with_baseline(baseline, check, consume)
        })
    }
    /// Consumers receive only complete projected stripes. They must keep their
    /// candidate private until this call returns, including derived cleanup.
    pub(crate) fn render_into(
        &self,
        check: &mut dyn FnMut() -> Result<()>,
        consume: &mut dyn FnMut([u32; 2], PinnedBytes) -> Result<()>,
    ) -> Result<Completed> {
        self.render_into_with_baseline(None, check, consume)
    }
    fn render_into_with_baseline(
        &self,
        baseline: Option<&crossfade::Baseline>,
        check: &mut dyn FnMut() -> Result<()>,
        consume: &mut dyn FnMut([u32; 2], PinnedBytes) -> Result<()>,
    ) -> Result<Completed> {
        self.with_sources(check, |scene, topology, stored, checks| {
            let composite = self.prepared.target.is_none() && !self.prepared.key.mask;
            let retained = baseline.filter(|baseline| {
                stored.is_none() && composite && self.prepared.can_reuse_pan(baseline.tag())
                    && effect_change_reach(scene.metadata().filters()).is_some()
            }).map(RetainedDisplay::Rows);
            self.prepared.render_into(scene, topology, stored, retained, checks, &mut |rows, pixels| {
                consume(rows, complete_pixels(pixels)?)
            })
        })
    }
    fn with_sources<T>(
        &self,
        check: &mut dyn FnMut() -> Result<()>,
        render: impl for<'a> FnOnce(
            &'a SceneSnapshot, Option<&Arc<Topology<'a>>>, Option<&Stored<'a>>, &mut ReadChecks<'_>,
        ) -> Result<T>,
    ) -> Result<T> {
        let mut checks = ReadChecks::with_session(&self.session, check);
        checks.full()?;
        let Some(topology) = self.prepared.topology(&self.scene)? else {
            return render(&self.scene, None, None, &mut checks);
        };
        let composite = self.prepared.target.is_none() && !self.prepared.key.mask;
        let crop = Some(self.prepared.key.rect);
        let needed = if composite { Stored::composite_needed(&topology, crop) } else { Stored::needed(&topology) };
        if !needed {
            return render(&self.scene, Some(&topology), None, &mut checks);
        }
        let session = self.session.begin_derived(&self.scene)?;
        let stored = if composite {
            Stored::prepare_composite(topology.clone(), &session, crop, &mut || checks.full())?
        } else {
            Stored::prepare_cropped(topology.clone(), &session, self.key().rect, &mut || checks.full())?
        };
        let result = render(&self.scene, Some(&topology), Some(&stored), &mut checks);
        let cleanup = stored.retire(&mut || Ok(()));
        match (result, cleanup) {
            (result, Ok(())) => result.and_then(|complete| {
                checks.full()?;
                Ok(complete)
            }),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => Err(format!("{error}; preview effect cleanup: {cleanup}")),
        }
    }

    pub(crate) fn sample16(&self, check: &mut dyn FnMut() -> Result<()>) -> Result<[u16; 4]> {
        let key = self.key();
        let metadata = self.scene.metadata();
        let [x, y, right, bottom] = key.rect;
        if metadata.depth() != 16 || key.target.is_some() != key.mask || key.selection_view.is_some()
            || key.width != 1 || key.height != 1 || right - x != 1 || bottom - y != 1
        {
            return Err("Sample requires exactly one native 16-bit color or mask pixel".into());
        }
        self.with_sources(check, |scene, topology, stored, checks| {
            if let Some(target) = self.prepared.raw_mask_target() {
                let layer = scene.metadata().layer(target).ok_or("Sample mask layer no longer exists")?;
                let gray = if layer.mask().is_some() {
                    let mask = crate::mask::region::prepare_scene_with_checks(scene, layer, key.rect, None, checks)?;
                    let [origin_x, origin_y] = layer.origin();
                    let x = x.checked_sub(origin_x).ok_or("Regional mask coordinate overflow")?;
                    let y = y.checked_sub(origin_y).ok_or("Regional mask coordinate overflow")?;
                    (mask.value16(x, y)? * 65535.).round() as u16
                } else {
                    u16::MAX
                };
                checks.full()?;
                return Ok([gray, gray, gray, u16::MAX]);
            }
            let topology = topology.ok_or("Sample composition topology is missing")?;
            topology.check_preview(scene, None)?;
            let region = Region::new(key.rect).map_err(|error| error.to_string())?;
            let pixel = if let Some(filtered) = prepare_filters_with_checks(topology, region, stored, checks)? {
                filtered.pixel16(x, y)?
            } else {
                let layers = LayerWindow::prepare_with_checks(topology, region, stored, checks)?;
                Plan::<Native16, _>::with_topology(topology.clone(), &layers, region)?.pixel(x, y)?
            };
            let display = DisplayPlan::prepare(scene.metadata().profile(), false, self.budget(), &mut || checks.full())?;
            if display.profile_binding() != self.prepared.profile {
                return Err("Sample profile differs from the captured source".into());
            }
            let result = if display.status() == DisplayStatus::Converted {
                let mut raw = ChargedWords::new(4, self.budget())?;
                let mut converted = ChargedWords::new(4, self.budget())?;
                raw.copy_from_slice(&pixel);
                display.convert16(&raw, &mut converted, &mut || checks.pinned())?;
                <[u16; 4]>::try_from(&converted[..]).map_err(|_| "Incomplete native color sample")?
            } else {
                pixel
            };
            checks.full()?;
            Ok(result)
        })
    }

    pub(crate) fn with_raster_damage(mut self, damage: Option<RasterDamage>) -> Result<Self> {
        let Some(mut damage) = damage else { return Ok(self); };
        if self.key().source != damage.candidate || damage.revision.checked_add(1) != Some(self.key().identity.revision) {
            return Err("Preview damage belongs to another source generation".into());
        }
        if self.prepared.target.is_some() || self.key().mask || self.key().selection_view.is_some()
            || self.prepared.profile.is_some()
        {
            return Ok(self);
        }
        let metadata = self.scene.metadata();
        // A backdrop can flow through groups, adjustments and document
        // filters. Summing every supported reach bounds all such paths.
        let Some(halo) = effect_change_reach(metadata.filters().chain(metadata.layers().flat_map(|layer| layer.effects())))
            else { return Ok(self); };
        let [left, top, right, bottom] = damage.bounds;
        damage.bounds = [left.saturating_sub(halo), top.saturating_sub(halo),
            right.saturating_add(halo), bottom.saturating_add(halo)];
        let prepared = Arc::get_mut(&mut self.prepared).ok_or("Preview damage tag is already shared")?;
        prepared.raster_damage = Some(damage);
        Ok(self)
    }

    pub(crate) fn texture_reuse(&self, prior: &Tag) -> Result<Option<TextureReuse<'_>>> {
        self.check()?;
        if self.prepared.target.is_some() || self.prepared.key.mask || self.prepared.profile.is_some()
            || !self.prepared.same_display_grid(prior)
        {
            return Ok(None);
        }
        let damage = if self.prepared.can_reuse_pan(prior) {
            if effect_change_reach(self.scene.metadata().filters()).is_none() { return Ok(None); }
            None
        } else if let Some(edit) = self.prepared.raster_damage {
            if prior.key().source == edit.base && prior.key().identity.revision == edit.revision {
                Some(edit.bounds)
            } else if let Some(old) = prior.0.raster_damage.filter(|old| old.base == edit.base && old.revision == edit.revision) {
                Some(crate::engine::reframe_scope::union(old.bounds, edit.bounds))
            } else {
                return Ok(None);
            }
        } else {
            return Ok(None);
        };
        let topology = Topology::for_composite(&self.scene, None)?;
        Ok((!Stored::composite_needed(&topology, Some(self.key().rect)))
            .then_some(TextureReuse { request: self, topology, damage }))
    }
}

fn effect_change_reach<'a>(effects: impl Iterator<Item = EffectMetadata<'a>>) -> Option<i32> {
    effects.filter(|effect| effect.enabled() && effect.weight() > 0.).try_fold(0i32, |sum, effect| {
        let reach = crate::effects::regional::local_change_reach(effect.kind(), effect.settings()).ok()??;
        sum.checked_add(reach)
    })
}

/// Only used by a presenter that already owns the omitted display pixels.
pub(crate) trait MissingColumns {
    fn stripe_end(&self, rows: [u32; 2]) -> u32 { rows[1] }
    fn visit(&self, rows: [u32; 2], render: &mut dyn FnMut(Range<u32>) -> Result<()>) -> Result<()>;
}

pub(crate) enum StripePixels {
    Full(PinnedBytes),
    Packed(PackedStripe),
}

pub(crate) struct PackedStripe {
    spans: Vec<(Range<u32>, PinnedBytes)>,
    _index: Reservation,
}

pub(super) fn complete_pixels(pixels: Option<StripePixels>) -> Result<PinnedBytes> {
    match pixels {
        Some(StripePixels::Full(bytes)) => Ok(bytes),
        _ => Err("Complete preview is missing rendered rows".into()),
    }
}

impl StripePixels {
    pub(crate) fn matches(&self, width: u32, height: u32, budget: &ResidentBudget) -> bool {
        let matches = |columns: u32, bytes: &PinnedBytes| {
            bytes.uses_budget(budget) && bytes.len() == columns as usize * height as usize * 4
        };
        match self {
            Self::Full(bytes) => matches(width, bytes),
            Self::Packed(packed) => packed._index.uses_budget(budget) && packed.spans.iter().all(|(cols, bytes)| {
                cols.end <= width && matches(cols.end - cols.start, bytes)
            }),
        }
    }

    pub(crate) fn columns(&self, row: u32, width: u32, columns: Range<u32>) -> Result<&[u8]> {
        let (available, bytes) = match self {
            Self::Full(bytes) => (0..width, bytes),
            Self::Packed(packed) => {
                let index = packed.spans.partition_point(|(span, _)| span.end <= columns.start);
                let (span, bytes) = packed.spans.get(index).ok_or("Preview stripe is missing texture columns")?;
                (span.clone(), bytes)
            }
        };
        if columns.start < available.start || columns.end > available.end || columns.start >= columns.end {
            return Err("Preview stripe is missing texture columns".into());
        }
        let at = row as usize * (available.end - available.start) as usize * 4;
        bytes.get(at + (columns.start - available.start) as usize * 4..at + (columns.end - available.start) as usize * 4)
            .ok_or_else(|| "Preview stripe is missing texture rows".into())
    }
}

pub(crate) struct TextureReuse<'a> {
    request: &'a Request,
    topology: Arc<Topology<'a>>,
    damage: Option<[i32; 4]>,
}
impl TextureReuse<'_> {
    pub(crate) fn damage(&self) -> Option<[i32; 4]> { self.damage }
    /// A stripe without pixels is covered entirely by retained textures.
    pub(crate) fn render_into(
        &self,
        columns: &impl MissingColumns,
        check: &mut dyn FnMut() -> Result<()>,
        consume: &mut dyn FnMut([u32; 2], Option<StripePixels>) -> Result<()>,
    ) -> Result<Completed> {
        let request = self.request;
        let mut checks = ReadChecks::with_session(&request.session, check);
        request.prepared.render_into(&request.scene, Some(&self.topology), None, Some(RetainedDisplay::Textures(columns)), &mut checks, consume)
    }
}

enum RetainedDisplay<'a> {
    Rows(&'a crossfade::Baseline),
    Textures(&'a dyn MissingColumns),
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    pub source: DocumentKey,
    pub identity: SceneIdentity,
    pub rect: [i32; 4],
    pub width: u32,
    pub height: u32,
    pub target: Option<String>,
    pub mask: bool,
    pub selection_view: Option<SelectionView>,
}
#[derive(Clone, Copy, PartialEq)]
enum Grid {
    Eight(f32),
    Sixteen(f64),
}
impl Grid {
    fn bounded(depth: u16, width: u32, height: u32, edge: u32) -> (Self, u32, u32) {
        Self::with_limit(depth, width, height, edge, MAX_PREVIEW_PIXELS)
    }
    fn with_limit(depth: u16, width: u32, height: u32, edge: u32, limit: u64) -> (Self, u32, u32) {
        let sample = |edge: u32| {
            if depth == 8 {
                let scale = (edge as f32 / width.max(height) as f32).min(1.);
                (Self::Eight(scale), (width as f32 * scale).round().max(1.) as u32,
                    (height as f32 * scale).round().max(1.) as u32)
            } else {
                let scale = (edge as f64 / width.max(height) as f64).min(1.);
                (Self::Sixteen(scale), (width as f64 * scale).round().max(1.) as u32,
                    (height as f64 * scale).round().max(1.) as u32)
            }
        };
        let mut high = edge.clamp(1, 16000);
        let full = sample(high);
        if u64::from(full.1) * u64::from(full.2) <= limit {
            return full;
        }
        // Search the actual rounded dimensions so neither depth can exceed the pixel bound.
        let mut low = 1;
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            let (_, w, h) = sample(middle);
            if u64::from(w) * u64::from(h) <= limit {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        sample(low)
    }
    fn coordinate(self, value: u32) -> i32 {
        match self {
            Self::Eight(scale) => (value as f32 / scale) as i32,
            Self::Sixteen(scale) => (value as f64 / scale) as i32,
        }
    }
    fn is_full(self) -> bool {
        match self {
            Self::Eight(scale) => scale == 1.0,
            Self::Sixteen(scale) => scale == 1.0,
        }
    }
}
struct Prepared {
    requested_rect: Option<[i32; 4]>,
    key: Key,
    target: Option<usize>,
    grid: Grid,
    pixel_limit: u64,
    pool: ResidentBudget,
    profile: Option<[u8; 32]>,
    raster_damage: Option<RasterDamage>,
    _tags: Reservation,
}
/// Reply tracking shares the exact admitted tags instead of cloning strings.
#[derive(Clone)]
pub(crate) struct Tag(Arc<Prepared>);
impl Tag {
    pub(crate) fn matches_region(&self, rect: Option<[i32; 4]>) -> bool {
        self.0.requested_rect == rect
    }
    pub(crate) fn same_allocation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn matches_edge(&self, edge: u32) -> bool {
        let rect = self.0.key.rect;
        let depth = match self.0.grid { Grid::Eight(_) => 8, Grid::Sixteen(_) => 16 };
        let (grid, width, height) = Grid::with_limit(
            depth, (rect[2] - rect[0]) as u32, (rect[3] - rect[1]) as u32, edge, self.0.pixel_limit,
        );
        self.0.grid == grid && self.0.key.width == width && self.0.key.height == height
    }
    pub(crate) fn key(&self) -> &Key {
        &self.0.key
    }
}
impl Prepared {
    fn raw_mask_target(&self) -> Option<usize> {
        self.target.filter(|_| self.key.mask)
    }

    fn topology<'a>(&self, scene: &'a SceneSnapshot) -> Result<Option<Arc<Topology<'a>>>> {
        if self.raw_mask_target().is_some()
            || self.key.selection_view == Some(SelectionView::Mask) && scene.metadata().selection().is_some()
        {
            Ok(None)
        } else {
            Topology::for_preview(scene, self.target).map(Some)
        }
    }

    fn can_reuse_pan(&self, prior: &Tag) -> bool {
        prior.key().source == self.key.source && prior.key().identity == self.key.identity
            && self.same_display_grid(prior)
    }
    fn same_display_grid(&self, prior: &Tag) -> bool {
        let old = prior.key();
        let new = &self.key;
        old.identity.project_id == new.identity.project_id
            && old.identity.document_id == new.identity.document_id
            && old.target == new.target
            && old.mask == new.mask
            && old.selection_view.is_none()
            && new.selection_view.is_none()
            && self.pool.same_pool(&prior.0.pool)
            && self.profile == prior.0.profile
            && self.grid == prior.0.grid
            && self.grid.is_full()
            && old.rect[0] < new.rect[2]
            && new.rect[0] < old.rect[2]
            && old.rect[1] < new.rect[3]
            && new.rect[1] < old.rect[3]
    }
    fn stream_grid(&mut self, depth: u16, edge: u32) {
        let rect = self.key.rect;
        let (grid, width, height) = Grid::with_limit(
            depth, (rect[2] - rect[0]) as u32, (rect[3] - rect[1]) as u32, edge, MAX_CANVAS_PIXELS,
        );
        self.grid = grid;
        self.key.width = width;
        self.key.height = height;
        self.pixel_limit = MAX_CANVAS_PIXELS;
    }
    fn capture(
        scene: &SceneSnapshot,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        selection_view: Option<SelectionView>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        let doc = scene.metadata();
        if doc.width() == 0
            || doc.height() == 0
            || doc.width() > 16000
            || doc.height() > 16000
            || ![8, 16].contains(&doc.depth())
        {
            return Err("Invalid captured preview dimensions or precision".into());
        }
        if doc.read_only() {
            let composite = scene.native_document()?.protected_composite()?;
            if mask || selection_view.is_some() || target.is_some_and(|id| id != composite.id()) {
                return Err("Protected Photoshop previews expose only the saved composite".into());
            }
        }
        for layer in doc.layers() {
            Stored::validate_layer(layer)?;
        }
        let index = target
            .map(|id| {
                doc.layers()
                    .position(|layer| layer.id() == id)
                    .ok_or("Layer no longer exists")
            })
            .transpose()?;
        let requested_rect = rect;
        let mut rect = rect.unwrap_or([0, 0, doc.width() as i32, doc.height() as i32]);
        rect[0] = rect[0].clamp(0, doc.width() as i32 - 1);
        rect[1] = rect[1].clamp(0, doc.height() as i32 - 1);
        rect[2] = rect[2].clamp(rect[0] + 1, doc.width() as i32);
        rect[3] = rect[3].clamp(rect[1] + 1, doc.height() as i32);
        let rw = (rect[2] - rect[0]) as u32;
        let rh = (rect[3] - rect[1]) as u32;
        let (grid, width, height) = Grid::bounded(doc.depth(), rw, rh, edge);
        let bytes = scene
            .identity()
            .project_id
            .len()
            .checked_add(scene.identity().document_id.len())
            .and_then(|n| n.checked_add(target.map_or(0, str::len)))
            .and_then(|n| n.checked_add(512))
            .ok_or("Preview tag admission overflow")?;
        let tags = scene
            .budget()
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let profile = profile_binding(doc.profile(), check)?;
        check()?;
        Ok(Self {
            requested_rect,
            key: Key {
                source: scene.key(),
                identity: scene.identity().clone(),
                rect,
                width,
                height,
                target: target.map(str::to_owned),
                mask,
                selection_view,
            },
            target: index,
            grid,
            pixel_limit: MAX_PREVIEW_PIXELS,
            pool: scene.budget().clone(),
            profile,
            raster_damage: None,
            _tags: tags,
        })
    }
    fn x(&self, x: u32) -> i32 {
        self.key.rect[0] + self.grid.coordinate(x)
    }
    fn y(&self, y: u32) -> i32 {
        self.key.rect[1] + self.grid.coordinate(y)
    }
    fn sample_columns(&self, [left, right]: [i32; 2]) -> Range<u32> {
        let first_sample_at = |limit| {
            let mut first = 0;
            let mut end = self.key.width;
            while first < end {
                let middle = first + (end - first) / 2;
                if self.x(middle) < limit {
                    first = middle + 1;
                } else {
                    end = middle;
                }
            }
            first
        };
        first_sample_at(left)..first_sample_at(right)
    }
    fn stripe(&self, start: u32, stop: u32, missing: Option<&dyn MissingColumns>) -> Result<(u32, Region)> {
        let left = self.x(0);
        let right = self.x(self.key.width - 1) + 1;
        let top = self.y(start);
        let rows = MAX_REGION_PIXELS / (right - left) as usize;
        let mut low = start + 1;
        let mut high = stop.min(start.saturating_add(rows as u32));
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if (self.y(middle - 1) - top + 1) as usize <= rows {
                low = middle
            } else {
                high = middle - 1
            }
        }
        let end = missing.map_or(low, |columns| columns.stripe_end([start, low]));
        if end <= start || end > low {
            return Err("Preview stripe split is outside its sampled rows".into());
        }
        let region =
            Region::new([left, top, right, self.y(end - 1) + 1]).map_err(|e| e.to_string())?;
        Ok((end, region))
    }
    #[cfg(test)]
    fn render(
        self: &Arc<Self>,
        scene: &SceneSnapshot,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Frame> {
        check()?;
        let topology = self.topology(scene)?;
        let mut checks = ReadChecks::caller_only(check);
        Frame::collect(self, |consume| self.render_into(scene, topology.as_ref(), None, None, &mut checks, &mut |rows, bytes| {
            consume(rows, complete_pixels(bytes)?)
        }))
    }
    fn render_into<'a>(
        self: &Arc<Self>,
        scene: &'a SceneSnapshot,
        topology: Option<&Arc<Topology<'a>>>,
        stored: Option<&Stored<'a>>,
        retained: Option<RetainedDisplay<'_>>,
        checks: &mut ReadChecks<'_>,
        consume: &mut dyn FnMut([u32; 2], Option<StripePixels>) -> Result<()>,
    ) -> Result<Completed> {
        let display_status = self.render_stripes(scene, topology, stored, retained, false, checks, consume)?;
        let outline = {
            let checked = std::cell::RefCell::new(&mut *checks);
            scene.metadata().selection_outline(&self.pool, &|| checked.borrow_mut().pinned())?
        };
        checks.full()?;
        Ok(Completed { outline: Arc::new(outline), tag: Tag(self.clone()), display_status })
    }
    fn render_stripes<'a>(
        &self,
        scene: &'a SceneSnapshot,
        topology: Option<&Arc<Topology<'a>>>,
        stored: Option<&Stored<'a>>,
        retained: Option<RetainedDisplay<'_>>,
        skip_transparent_stripes: bool,
        checks: &mut ReadChecks<'_>,
        mut consume: impl FnMut([u32; 2], Option<StripePixels>) -> Result<()>,
    ) -> Result<DisplayStatus> {
        checks.full()?;
        if scene.key() != self.key.source
            || scene.identity() != &self.key.identity
            || !scene.budget().same_pool(&self.pool)
        {
            return Err("Preview capture belongs to another source generation".into());
        }
        let selection = selection::capture(scene, &self.key)?;
        let mut projector = selection.as_ref()
            .map(|selection| selection::Projector::new(selection, self))
            .transpose()?;
        let selection_mask = projector.is_some() && self.key.selection_view == Some(SelectionView::Mask);
        let raw_layer = self.raw_mask_target().filter(|_| !selection_mask).map(|target| {
            scene.metadata().layer(target).ok_or("Layer no longer exists")
        }).transpose()?;
        if selection_mask || raw_layer.is_some() {
            let mut start = 0;
            while start < self.key.height {
                checks.full()?;
                let (end, region) = self.stripe(start, self.key.height, None)?;
                let mut bytes = ScratchBytes::new(
                    &self.pool,
                    self.key.width as usize * (end - start) as usize * 4,
                    MAX_REGION_PIXELS * 4,
                ).map_err(|e| e.to_string())?;
                if let Some(layer) = raw_layer {
                    self.render_mask_stripe(scene, layer, region, start, &mut bytes, checks)?;
                }
                if let Some(projector) = &mut projector {
                    projector.apply([start, end], &mut bytes, checks)?;
                }
                checks.full()?;
                consume([start, end], Some(StripePixels::Full(bytes.freeze())))?;
                start = end;
            }
            checks.full()?;
            return Ok(DisplayStatus::Mask);
        }
        let topology = topology.ok_or("Preview composition topology is missing")?;
        topology.check_preview(scene, self.target)?;
        // Only the active worker creates CMS executors. Queued requests and
        // completed presentation tags retain no128MiB transform reservation.
        let display =
            DisplayPlan::prepare(scene.metadata().profile(), self.key.mask, &self.pool, &mut || checks.full())?;
        if display.profile_binding() != self.profile {
            return Err("Preview profile differs from the captured source".into());
        }
        let rect = self.key.rect;
        let same_columns = |old: [i32; 4]| old[0] == rect[0] && old[2] == rect[2];
        let skip_transparent_stripes = skip_transparent_stripes
            && retained.is_none()
            && self.grid.is_full();
        let (baseline, missing) = match retained {
            Some(RetainedDisplay::Rows(baseline)) => (Some(baseline), None),
            Some(RetainedDisplay::Textures(missing)) => (None, Some(missing)),
            None => (None, None),
        };
        let baseline = baseline.filter(|baseline| {
            baseline.status() == display.status()
                // Converted rows keep the same width and sample positions in
                // the CMS executor, including its final partial vector.
                && (display.status() != DisplayStatus::Converted || same_columns(baseline.tag().key().rect))
        });
        // Isolated plain paint has no source outside its occupied tiles.
        let plain_target_guide = skip_transparent_stripes && self.target.is_some_and(|index| {
            scene.metadata().layer(index).is_some_and(|layer| {
                layer.kind() == "paint"
                    && layer.source().is_none()
                    && layer.mask().is_none()
                    && layer.clip_to().is_none()
                    && layer.effects().len() == 0
            })
        });
        let occupied = if (self.target.is_none() || plain_target_guide)
            && !self.key.mask
            && projector.is_none()
            && display.status() != DisplayStatus::Converted
        {
            // The occupancy map reads only the captured tile index. The stripe
            // loop validates source ownership before using it or reading tiles.
            topology.composite_tiles(Some(rect), &mut || checks.pinned())?
        } else {
            None
        };
        let mut y = 0;
        while y < self.key.height {
            checks.full()?;
            if let Some(tiles) = occupied.as_ref().filter(|_| skip_transparent_stripes) {
                let top = self.y(y);
                let next = tiles
                    .first_occupied_row([rect[0], top, rect[2], rect[3]])
                    .unwrap_or(rect[3]);
                if top < next {
                    let end = (next - rect[1]) as u32;
                    consume([y, end], None)?;
                    y = end;
                    continue;
                }
            }
            let shared = baseline.filter(|baseline| same_columns(baseline.tag().key().rect))
                .and_then(|baseline| baseline.pan_stripe(rect, y));
            let mut stop = if let Some((rows, bytes)) = shared {
                if rows[0] == y {
                    consume(rows, Some(StripePixels::Full(bytes.clone())))?;
                    y = rows[1];
                    continue;
                }
                rows[0]
            } else {
                self.key.height
            };
            let mut prior = None;
            if let Some(baseline) = baseline {
                let [_, old_top, _, old_bottom] = baseline.tag().key().rect;
                let row = rect[1] + y as i32;
                if row < old_top {
                    stop = stop.min((old_top - rect[1]) as u32);
                } else if row < old_bottom {
                    stop = stop.min((old_bottom - rect[1]) as u32);
                    prior = Some(baseline);
                }
            }
            let (end, region) = self.stripe(y, stop, missing)?;
            let allocate = |width: u32| ScratchBytes::new(&self.pool,
                width as usize * (end - y) as usize * 4, MAX_REGION_PIXELS * 4)
                .map_err(|e| e.to_string());
            let [left, top, right, bottom] = region.bounds();
            let render_span = |[left, right]: [i32; 2], output_columns: Range<u32>, output: &mut [u8],
                               checks: &mut ReadChecks<'_>| -> Result<()> {
                if left >= right { return Ok(()); }
                let mut render = |span| {
                    let columns = self.sample_columns(span);
                    if columns.is_empty() { return Ok(()); }
                    let window = Region::new([self.x(columns.start), top, self.x(columns.end - 1) + 1, bottom])
                        .map_err(|e| e.to_string())?;
                    self.render_window(topology, stored, &display, window, columns, y..end,
                        output_columns.clone(), output, checks)
                };
                if let Some(tiles) = &occupied {
                    for span in tiles.column_spans([left, top, right, bottom]) {
                        render(span)?;
                    }
                } else {
                    render([left, right])?;
                }
                Ok(())
            };
            if let Some(missing) = missing {
                let mut count = 0;
                missing.visit([y, end], &mut |_| { count += 1; Ok(()) })?;
                let pixels = if count == 0 {
                    None
                } else {
                    let entry = std::mem::size_of::<(Range<u32>, PinnedBytes)>();
                    if count > self.key.width as usize {
                        return Err("Preview stripe has too many column spans".into());
                    }
                    let index = self.pool.reserve_bytes(count * entry, 16000 * entry).map_err(|e| e.to_string())?;
                    let mut spans = Vec::<(Range<u32>, PinnedBytes)>::new();
                    spans.try_reserve_exact(count).map_err(|_| "Preview column index allocation refused")?;
                    if spans.capacity() > count { return Err("Preview column index exceeds admission".into()); }
                    missing.visit([y, end], &mut |columns| {
                        checks.full()?;
                        if spans.len() == count || columns.start >= columns.end || columns.end > self.key.width
                            || spans.last().is_some_and(|(prior, _)| prior.end >= columns.start)
                        {
                            return Err("Preview stripe has invalid column spans".into());
                        }
                        let mut output = allocate(columns.end - columns.start)?;
                        let window = [rect[0] + columns.start as i32, rect[0] + columns.end as i32];
                        render_span(window, columns.clone(), &mut output, checks)?;
                        spans.push((columns, output.freeze()));
                        Ok(())
                    })?;
                    if spans.len() != count { return Err("Preview column index is incomplete".into()); }
                    Some(StripePixels::Packed(PackedStripe { spans, _index: index }))
                };
                checks.full()?;
                consume([y, end], pixels)?;
                y = end;
                continue;
            }
            let mut output = allocate(self.key.width)?;
            let windows = if let Some(prior) = prior {
                let [start, end_x] = prior.copy_pan_rows(rect, y..end, &mut output, &mut || checks.pinned())?;
                [[left, start], [end_x, right]]
            } else {
                [[left, right], [right, right]]
            };
            for window in windows { render_span(window, 0..self.key.width, &mut output, checks)?; }
            if let Some(projector) = &mut projector {
                projector.apply([y, end], &mut output, checks)?;
            }
            checks.full()?;
            consume([y, end], Some(StripePixels::Full(output.freeze())))?;
            y = end;
        }
        checks.full()?;
        Ok(display.status())
    }

    fn render_mask_stripe(
        &self,
        scene: &SceneSnapshot,
        layer: crate::engine::native::LayerMetadata<'_>,
        region: Region,
        first_row: u32,
        output: &mut [u8],
        checks: &mut ReadChecks<'_>,
    ) -> Result<()> {
        if layer.mask().is_none() {
            output.fill(255);
            return Ok(());
        }
        let mask = crate::mask::region::prepare_scene_with_checks(scene, layer, region.bounds(), None, checks)?;
        let [origin_x, origin_y] = layer.origin();
        let high = scene.metadata().depth() == 16;
        for (row, output) in output.chunks_exact_mut(self.key.width as usize * 4).enumerate() {
            checks.pinned()?;
            let y = self.y(first_row + row as u32).checked_sub(origin_y).ok_or("Regional mask coordinate overflow")?;
            for (column, pixel) in output.chunks_exact_mut(4).enumerate() {
                let x = self.x(column as u32).checked_sub(origin_x).ok_or("Regional mask coordinate overflow")?;
                let gray = if high {
                    let value = (mask.value16(x, y)? * 65535.).round() as u16;
                    crate::raster::project16(value)
                } else {
                    (mask.value8(x, y)? * 255.) as u8
                };
                pixel.copy_from_slice(&[gray, gray, gray, 255]);
            }
        }
        checks.full()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_window<'a>(
        &self,
        topology: &Arc<Topology<'a>>,
        stored: Option<&Stored<'a>>,
        display: &DisplayPlan,
        region: Region,
        columns: Range<u32>,
        rows_range: Range<u32>,
        output_columns: Range<u32>,
        output: &mut [u8],
        checks: &mut ReadChecks<'_>,
    ) -> Result<()> {
        checks.full()?;
        let row_length = (output_columns.end - output_columns.start) as usize * 4;
        if columns.start < output_columns.start || columns.end > output_columns.end
            || display.status() == DisplayStatus::Converted && output_columns != (0..self.key.width)
        {
            return Err("Preview render window is outside its output columns".into());
        }
        let filtered = if self.target.is_none() && !self.key.mask {
            prepare_filters_with_checks(topology, region, stored, checks)?
        } else {
            None
        };
        let prepared = if filtered.is_none() {
            Some(LayerWindow::prepare_with_checks(topology, region, stored, checks)?)
        } else {
            None
        };
        checks.full()?;
        // Both providers now sample admitted memory only. The caller checks
        // the live filesystem again before handing off the completed stripe.
        if topology.scene().metadata().depth() == 8 {
            let plan = prepared
                .as_ref()
                .map(|prepared| {
                    Plan::<Native8, _>::with_topology(topology.clone(), prepared, region)
                })
                .transpose()?;
            let mut rows = if display.status() == DisplayStatus::Converted {
                Some((
                    ScratchBytes::new(&self.pool, row_length, 16000 * 4)
                        .map_err(|e| e.to_string())?,
                    ScratchBytes::new(&self.pool, row_length, 16000 * 4)
                        .map_err(|e| e.to_string())?,
                ))
            } else {
                None
            };
            for oy in rows_range.clone() {
                checks.pinned()?;
                for x in columns.clone() {
                    let sx = self.x(x);
                    let sy = self.y(oy);
                    let pixel = if let Some(image) = &filtered {
                        image.pixel(sx, sy)?
                    } else if let Some(target) = self.target {
                        let plan = plan.as_ref().ok_or("Missing native target provider")?;
                        plan.layer(target, sx, sy)?
                    } else {
                        plan.as_ref()
                            .ok_or("Missing native composite provider")?
                            .pixel(sx, sy)?
                    };
                    let at = ((x - output_columns.start) * 4) as usize;
                    if let Some((raw, _)) = &mut rows {
                        raw[at..at + 4].copy_from_slice(&pixel);
                    } else {
                        let at = (oy - rows_range.start) as usize * row_length + at;
                        output[at..at + 4].copy_from_slice(&pixel);
                    }
                }
                if let Some((raw, converted)) = &mut rows {
                    display.convert8(raw, converted, &mut || checks.pinned())?;
                    let at = (oy - rows_range.start) as usize * row_length;
                    output[at..at + row_length].copy_from_slice(converted);
                }
            }
        } else {
            let plan = prepared
                .as_ref()
                .map(|prepared| {
                    Plan::<Native16, _>::with_topology(topology.clone(), prepared, region)
                })
                .transpose()?;
            let mut rows = if display.status() == DisplayStatus::Converted {
                Some((
                    ChargedWords::new(row_length, &self.pool)?,
                    ChargedWords::new(row_length, &self.pool)?,
                ))
            } else {
                None
            };
            for oy in rows_range.clone() {
                checks.pinned()?;
                for x in columns.clone() {
                    let sx = self.x(x);
                    let sy = self.y(oy);
                    let pixel = if let Some(image) = &filtered {
                        image.pixel16(sx, sy)?
                    } else if let Some(target) = self.target {
                        let plan = plan.as_ref().ok_or("Missing native target provider")?;
                        plan.layer(target, sx, sy)?
                    } else {
                        plan.as_ref()
                            .ok_or("Missing native composite provider")?
                            .pixel(sx, sy)?
                    };
                    let at = ((x - output_columns.start) * 4) as usize;
                    if let Some((raw, _)) = &mut rows {
                        raw[at..at + 4].copy_from_slice(&pixel);
                    } else {
                        let at = (oy - rows_range.start) as usize * row_length + at;
                        output[at..at + 4]
                            .copy_from_slice(&crate::depth16::display_pixel(pixel));
                    }
                }
                if let Some((raw, converted)) = &mut rows {
                    display.convert16(raw, converted, &mut || checks.pinned())?;
                    for (x, pixel) in converted.chunks_exact(4).enumerate() {
                        let at = (oy - rows_range.start) as usize * row_length + x * 4;
                        output[at..at + 4].copy_from_slice(&crate::depth16::display_pixel(
                            pixel.try_into().unwrap(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}
pub(crate) struct Stripe {
    rows: [u32; 2],
    bytes: PinnedBytes,
}
impl Stripe {
    pub(crate) fn rows(&self) -> [u32; 2] {
        self.rows
    }
    pub(crate) fn bytes(&self) -> &PinnedBytes {
        &self.bytes
    }
}
/// Worker render sources produce complete display stripes and a final receipt.
pub(crate) trait DisplayRequest {
    fn key(&self) -> &Key;
    fn tag(&self) -> Tag;
    fn budget(&self) -> &ResidentBudget;
    fn check(&self) -> Result<()>;
    fn check_pinned_work(&self) -> Result<()>;
    fn render_into(&self, check: &mut dyn FnMut() -> Result<()>,
        consume: &mut dyn FnMut([u32; 2], PinnedBytes) -> Result<()>) -> Result<Completed>;
}
impl DisplayRequest for Request {
    fn key(&self) -> &Key { self.key() }
    fn tag(&self) -> Tag { self.tag() }
    fn budget(&self) -> &ResidentBudget { self.budget() }
    fn check(&self) -> Result<()> { self.check() }
    fn check_pinned_work(&self) -> Result<()> { self.check_pinned_work() }
    fn render_into(&self, check: &mut dyn FnMut() -> Result<()>,
        consume: &mut dyn FnMut([u32; 2], PinnedBytes) -> Result<()>) -> Result<Completed> {
        self.render_into(check, consume)
    }
}
/// Completion receipt retains display metadata only, never a source/session.
pub(crate) struct Completed {
    pub(crate) outline: Arc<crate::engine::native::SelectionOutline>,
    pub(crate) tag: Tag,
    pub(crate) display_status: DisplayStatus,
}
/// Outputs may retain display pixels, textures and admitted metadata only.
/// Implementations must not retain a source, document or source session.
pub(crate) trait DisplayOutput {
    fn display_tag(&self) -> Tag;
}
impl DisplayOutput for Frame {
    fn display_tag(&self) -> Tag { self.tag() }
}
pub(crate) struct Frame {
    outline: Option<Arc<crate::engine::native::SelectionOutline>>,
    stripes: Vec<Stripe>,
    prepared: Arc<Prepared>,
    display_status: DisplayStatus,
    _index: Reservation,
}
/// Consuming presentation access releases pixel owners monotonically while
/// retaining the original vector's admission until its allocation is dropped.
pub(crate) struct StripeStream {
    stripes: std::vec::IntoIter<Stripe>,
    _index: Reservation,
}
impl StripeStream {
    pub(crate) fn stripes(&self) -> &[Stripe] { self.stripes.as_slice() }
    pub(crate) fn take_next(&mut self) -> Option<([u32; 2], PinnedBytes)> {
        self.stripes.next().map(|Stripe { rows, bytes }| (rows, bytes))
    }
    pub(crate) fn retire_before(&mut self, row: u32) {
        while self.stripes.as_slice().first().is_some_and(|stripe| stripe.rows[1] <= row) {
            drop(self.stripes.next());
        }
    }
}
impl Frame {
    fn collect(
        prepared: &Arc<Prepared>,
        render: impl FnOnce(&mut dyn FnMut([u32; 2], PinnedBytes) -> Result<()>) -> Result<Completed>,
    ) -> Result<Self> {
        if u64::from(prepared.key.width) * u64::from(prepared.key.height) > MAX_PREVIEW_PIXELS {
            return Err("Full-detail canvas requires streamed texture delivery".into());
        }
        let index_bytes = (prepared.key.height as usize)
            .checked_mul(std::mem::size_of::<Stripe>())
            .and_then(|n| n.checked_add(128))
            .ok_or("Preview stripe index overflow")?;
        let index = prepared.pool.reserve_bytes(index_bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut stripes = Vec::new();
        stripes.try_reserve_exact(prepared.key.height as usize)
            .map_err(|_| "Preview stripe index allocation refused")?;
        let complete = render(&mut |rows, bytes| {
            stripes.push(Stripe { rows, bytes });
            Ok(())
        })?;
        Ok(Self {
            outline: Some(complete.outline), stripes, prepared: complete.tag.0,
            display_status: complete.display_status, _index: index,
        })
    }
    pub(crate) fn into_stripes(self) -> StripeStream {
        StripeStream { stripes: self.stripes.into_iter(), _index: self._index }
    }
    pub(crate) fn take_outline(&mut self) -> Result<Arc<crate::engine::native::SelectionOutline>> {
        self.outline
            .take()
            .ok_or_else(|| "Native preview selection metadata is missing".into())
    }
    pub(crate) fn budget(&self) -> &ResidentBudget {
        &self.prepared.pool
    }
    pub(crate) fn display_status(&self) -> DisplayStatus {
        self.display_status
    }
    pub(crate) fn profile_warning(&self) -> Option<&'static str> {
        self.display_status.warning()
    }
    pub(crate) fn key(&self) -> &Key {
        &self.prepared.key
    }
    pub(crate) fn stripes(&self) -> &[Stripe] {
        &self.stripes
    }
    pub(crate) fn tag(&self) -> Tag {
        Tag(self.prepared.clone())
    }
    pub(crate) fn matches(&self, tag: &Tag) -> bool {
        self.key() == tag.key()
    }
    /// Explicit caller-admitted contiguous preview, never a document raster.
    /// Larger presentations can consume the already-admitted stripes directly.
    pub(crate) fn copy_rgba8(&self, check: &mut dyn FnMut() -> Result<()>) -> Result<PinnedBytes> {
        check()?;
        let length = (self.key().width as usize)
            .checked_mul(self.key().height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or("Preview contiguous dimensions overflow")?;
        let mut output = ScratchBytes::new(&self.prepared.pool, length, 64 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut at = 0;
        for stripe in &self.stripes {
            check()?;
            let end = at + stripe.bytes.len();
            output[at..end].copy_from_slice(&stripe.bytes);
            at = end;
        }
        if at != length {
            return Err("Preview stripe coverage is incomplete".into());
        }
        check()?;
        Ok(output.freeze())
    }
}

#[cfg(test)]
pub(crate) fn test_frame(
    scene: &SceneSnapshot,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Frame> {
    test_frame_selection(scene, rect, edge, target, mask, None, check)
}

#[cfg(test)]
pub(crate) fn test_frame_selection(
    scene: &SceneSnapshot,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    mode: Option<&str>,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Frame> {
    let mode = mode.map(SelectionView::parse).transpose()?;
    Arc::new(Prepared::capture(
        scene, rect, edge, target, mask, mode, check,
    )?).render(scene, check)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::native::prepare_filters_with_layers;
    use crate::engine::{Document, Layer};
    #[test]
    fn wide_preview_grids_keep_detail_within_the_existing_pixel_limit() {
        for depth in [8, 16] {
            for (width, height) in [(16000, 1), (1, 16000), (16000, 4194), (4194, 16000), (8192, 8192)] {
                let (grid, w, h) = Grid::bounded(depth, width, height, 16000);
                assert_eq!((w, h), (width, height));
                assert_eq!(grid.coordinate(w - 1), (width - 1) as i32);
                assert_eq!(grid.coordinate(h - 1), (height - 1) as i32);
            }
            let (_, w, h) = Grid::bounded(depth, 16000, 16000, 16000);
            assert_eq!((w, h), (8192, 8192));
            for height in [4195, 4200, 8191, 8193, 15999] {
                let (_, w, h) = Grid::bounded(depth, 16000, height, 16000);
                assert!(u64::from(w) * u64::from(h) <= MAX_PREVIEW_PIXELS);
                assert!(w >= 8192);
                let (_, transposed_w, transposed_h) = Grid::bounded(depth, height, 16000, 16000);
                assert_eq!((w, h), (transposed_h, transposed_w));
            }
            let (_, w, h) = Grid::bounded(depth, 16000, 1, 1536);
            assert_eq!((w, h), (1536, 1));
        }
    }
    fn document(depth: u16, width: u32, height: u32) -> Document {
        let mut doc = Document::new_depth(width, height, depth).unwrap();
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let alpha = match (x + y) % 7 {
                    0 => 0,
                    1 => 128,
                    2 => 129,
                    _ => 65535,
                };
                doc.layers[0].pixels.set16(
                    x,
                    y,
                    [
                        ((x as u32 * 941 + y as u32 * 31 + 12347) % 65536) as u16,
                        ((y as u32 * 617 + x as u32 * 43 + 23521) % 65536) as u16,
                        43217,
                        alpha,
                    ],
                );
            }
        }
        doc
    }
    #[test]
    fn full_resolution_pan_reuses_overlapping_rows_and_shares_complete_stripes() {
        use crate::engine::{native::{worker, NativeDocument}, scene::{CapturedScene, LegacyTileCache}};

        for depth in [8, 16] {
            let fixture = worker::tests::Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
            let staged = CapturedScene::from_parts(
                document(depth, 600, 1024), "pan-test".into(), vec![], None, None,
            ).stage(&runtime, &cache, &|| Ok(())).unwrap();
            let document = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
            let scene = Arc::new(SceneSnapshot::from_native(
                SceneIdentity {
                    project_id: "pan-test".into(),
                    document_id: document.metadata().id().into(),
                    revision: document.metadata().revision(),
                },
                document.clone(), vec![], None, None,
            ).unwrap());
            let request = |rect, edge| {
                let read = runtime.begin_read(&scene).unwrap();
                Request::capture(scene.clone(), read, Some(rect), edge, None, false).unwrap()
            };
            let prior = request([32, 10, 568, 780], 8192).render(&mut || Ok(())).unwrap();
            let baseline = crossfade::Baseline::capture(&prior, &document, "pan-test", &|| Ok(())).unwrap();
            for (rect, edge, shared, starts_at_old_top) in [
                ([32, 140, 568, 970], 8192, 3, false),
                ([0, 5, 600, 970], 8192, 0, true),
                ([41, 5, 577, 970], 8192, 0, true),
                ([21, 5, 557, 970], 8192, 0, true),
                ([568, 5, 600, 970], 8192, 0, false),
                ([32, 5, 568, 970], 400, 0, false),
            ] {
                let next = request(rect, edge);
                let frame = next.render_with_pan_baseline(Some(&baseline), &mut || Ok(())).unwrap();
                let reference = next.render(&mut || Ok(())).unwrap();
                assert_eq!(&*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                           &*reference.copy_rgba8(&mut || Ok(())).unwrap());
                let shared_pins = frame.stripes().iter().filter(|stripe| {
                    prior.stripes().iter().any(|old| std::ptr::eq(&**stripe.bytes(), &**old.bytes()))
                }).count();
                assert_eq!(shared_pins, shared);
                assert_eq!(frame.stripes()[0].rows()[1] == 5, starts_at_old_top);
                assert!(shared_pins < frame.stripes().len());
                assert!(frame.tag().same_allocation(&next.tag()));
            }
        }
    }

    #[test]
    fn paint_damage_includes_finite_layer_and_filter_halos() {
        use crate::{effects::{regional, Effect}, engine::{authority::DocumentAuthority,
            native::{worker, NativeDocument}, scene::{CapturedScene, LegacyTileCache}}};
        use serde_json::json;

        struct Columns([Range<u32>; 2]);
        impl MissingColumns for Columns {
            fn visit(&self, _: [u32; 2], render: &mut dyn FnMut(Range<u32>) -> Result<()>) -> Result<()> {
                for span in &self.0 { render(span.clone())?; }
                Ok(())
            }
        }
        let effect = |kind: &str, settings: serde_json::Value| Effect {
            id: crate::engine::id(), kind: kind.into(), enabled: true, weight: 1., settings: Arc::new(settings),
        };
        for depth in [8, 16] {
            let fixture = worker::tests::Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
            let mut doc = document(depth, 96, 48);
            let layer = doc.layers[0].id.clone();
            let layer_blur = json!({"radius":1.7});
            let filter_blur = json!({"radius":2.3});
            let outline = json!({"size":7.25,"color":[1.,0.,0.],"opacity":1.});
            let shadow = json!({"size":0.,"distance":9.5,"angle":0.,"opacity":1.});
            let halo = regional::reach("blur", &layer_blur).unwrap()
                + regional::reach("blur", &filter_blur).unwrap()
                + 9 // ceil(7.25 px) and the Stroke fringe
                + 11; // ceil(9.5 px) and the bilinear neighbor
            assert_eq!(regional::local_change_reach("drop_shadow", &json!({"distance":9.5})).unwrap(), None);
            doc.layers[0].effects.push(effect("invert", json!({})));
            doc.layers[0].effects.push(effect("stroke", outline));
            doc.layers[0].effects.push(effect("blur", layer_blur));
            doc.layers[0].effects.push(effect("drop_shadow", shadow));
            doc.filters.push(effect("channel_clamp", json!({"channel":"r","minimum":0.05})));
            doc.filters.push(effect("blur", filter_blur));
            let staged = CapturedScene::from_parts(doc, "damage-test".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(())).unwrap();
            let document = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
            let authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
            let scene = Arc::new(authority.capture_scene(None, "damage-test".into(), vec![], None, None)
                .unwrap().into_native().unwrap());
            let request = |rect| Request::capture(scene.clone(), runtime.begin_read(&scene).unwrap(), rect, 96, None, false).unwrap();
            let before = request(None).render(&mut || Ok(())).unwrap();
            let baseline = crossfade::Baseline::capture(&before, &document, "damage-test", &|| Ok(())).unwrap();
            {
                let panned = request(Some([1, 0, 96, 48]));
                assert!(panned.texture_reuse(&before.tag()).unwrap().is_some());
                let reused = panned.render_with_pan_baseline(Some(&baseline), &mut || Ok(())).unwrap();
                let fresh = panned.render(&mut || Ok(())).unwrap();
                assert_eq!(&*reused.copy_rgba8(&mut || Ok(())).unwrap(), &*fresh.copy_rgba8(&mut || Ok(())).unwrap());
            }
            let before_bytes = before.copy_rgba8(&mut || Ok(())).unwrap();
            let session = Arc::new(runtime.begin().unwrap());
            let edit = worker::prepare(worker::tests::capture(&authority, &[
                json!({"op":"paint.fill","layer":layer,"rect":[48,24,49,25],"color":[255,23,147,255]}),
            ]), &session, &|| Ok(())).unwrap();
            let expected = [48 - halo, 24 - halo, 49 + halo, 25 + halo];
            render_prepared_with(scene.clone(), edit, session, None, 96, None, false, None, &|| Ok(()), true,
                |request, check| {
                    let proof = request.texture_reuse(&before.tag())?.unwrap();
                    assert_eq!(proof.damage(), Some(expected));
                    let full = request.render(&mut || check())?;
                    let after = full.copy_rgba8(&mut || check())?;
                    let stroke_alpha = (24 * 96 + 41) * 4 + 3;
                    let shadow_alpha = (24 * 96 + 64) * 4 + 3;
                    assert_eq!(before_bytes[stroke_alpha], 0);
                    assert_eq!(before_bytes[shadow_alpha], 0);
                    assert!(after[stroke_alpha] > 0);
                    assert!(after[shadow_alpha] > 0);
                    let mut reached_neighbor = false;
                    for (i, (old, new)) in before_bytes.chunks_exact(4).zip(after.chunks_exact(4)).enumerate() {
                        if old == new { continue; }
                        let (x, y) = ((i % 96) as i32, (i / 96) as i32);
                        assert!(expected[0] <= x && x < expected[2] && expected[1] <= y && y < expected[3]);
                        reached_neighbor |= (x, y) != (48, 24);
                    }
                    assert!(reached_neighbor);
                    let columns = Columns([expected[0] as u32..48, 49..expected[2] as u32]);
                    proof.render_into(&columns, &mut || check(), &mut |rows, bytes| {
                        let bytes = bytes.expect("The requested columns need rendered pixels");
                        let StripePixels::Packed(packed) = &bytes else { panic!("Expected two packed spans") };
                        assert_eq!(packed.spans.iter().map(|(_, bytes)| bytes.len()).sum::<usize>(),
                            (expected[2] - expected[0] - 1) as usize * (rows[1] - rows[0]) as usize * 4);
                        for y in rows[0]..rows[1] {
                            for span in &columns.0 {
                                let source = (y * 96 + span.start) as usize * 4..(y * 96 + span.end) as usize * 4;
                                assert_eq!(bytes.columns(y - rows[0], 96, span.clone())?, &after[source]);
                            }
                            assert!(bytes.columns(y - rows[0], 96, 48..49).is_err());
                        }
                        Ok(())
                    })?;
                    Ok(full)
                },
            ).unwrap();
            assert_eq!(authority.history_lengths(), [0, 0]);
        }
    }
    fn capture(doc: &Document) -> (Arc<SceneSnapshot>, std::path::PathBuf) {
        let (scene, path) = crate::compositor::native::tests::capture_kind(doc, true);
        (Arc::new(scene), path)
    }
    fn prepare(
        scene: &SceneSnapshot,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
    ) -> Arc<Prepared> {
        Arc::new(Prepared::capture(scene, rect, edge, target, mask, None, &mut || Ok(())).unwrap())
    }
    #[test]
    fn sparse_preview_uses_disjoint_windows_on_exact_and_reduced_grids() {
        let rect = [5, 0, 8189, 16];
        for depth in [8, 16] {
            let mut doc = Document::new_depth(8192, 16, depth).unwrap();
            for (left, color) in [
                (240, [12347, 23459, 34571, 65535]),
                (8000, [34571, 23459, 12347, 53979]),
            ] {
                for y in 4..13 {
                    for x in left..left + 8 {
                        doc.layers[0].pixels.set16(x, y, color);
                    }
                }
            }
            doc.layers[0].effects.push(crate::effects::Effect {
                id: crate::engine::id(), kind: "invert".into(), enabled: true,
                weight: 0.5, settings: crate::effects::defaults("invert").into(),
            });
            let (scene, path) = capture(&doc);
            let topology = Topology::new(&scene).unwrap();
            let tiles = topology.composite_tiles(None, &mut || Ok(())).unwrap().unwrap();
            assert_eq!(
                tiles.column_spans(rect).collect::<Vec<_>>(),
                [[5, 256], [7936, 8189]]
            );
            for edge in [8192, 2049] {
                let prepared = prepare(&scene, Some(rect), edge, None, false);
                let (_, _, expected, _) = doc.preview(Some(rect), edge, None, false).unwrap();
                let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
                let bytes = frame.copy_rgba8(&mut || Ok(())).unwrap();
                assert_eq!(&*bytes, &expected);
                let gap = prepared.sample_columns([256, 7936]);
                assert!(!gap.is_empty());
                let stride = prepared.key.width as usize * 4;
                assert!(bytes.chunks_exact(stride).all(|row| {
                    row[gap.start as usize * 4..gap.end as usize * 4]
                        .iter()
                        .all(|&byte| byte == 0)
                }));
                assert!(bytes.chunks_exact(stride).any(|row| {
                    row[..gap.start as usize * 4].iter().any(|&byte| byte != 0)
                        && row[gap.end as usize * 4..].iter().any(|&byte| byte != 0)
                }));
            }
            drop((tiles, topology));
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }

    #[test]
    fn cropped_occupancy_limits_tile_checks_and_keeps_shifted_effect_halos() {
        let rect = [256, 7, 264, 15];
        for depth in [8, 16] {
            let mut doc = Document::new_depth(8192, 32, depth).unwrap();
            let layer = &mut doc.layers[0];
            layer.x = 7;
            layer.y = -2;
            let sample = [12347, 23459, 34571, 65535];
            for y in 8..16 {
                for x in 245..249 {
                    layer.pixels.set16(x, y, sample);
                }
            }
            for x in (512..8192).step_by(256) {
                layer.pixels.set16(x, 11, sample);
            }
            layer.effects.push(crate::effects::Effect {
                id: crate::engine::id(),
                kind: "blur".into(),
                enabled: true,
                weight: 1.,
                settings: serde_json::json!({"radius":3.}).into(),
            });

            let (scene, path) = capture(&doc);
            let topology = Topology::new(&scene).unwrap();
            let scan = |region| {
                let mut checks = 0;
                let tiles = topology.composite_tiles(region, &mut || {
                    checks += 1;
                    Ok(())
                }).unwrap().unwrap();
                (tiles, checks)
            };
            let (full, full_checks) = scan(None);
            let (cropped, cropped_checks) = scan(Some(rect));
            assert!(cropped_checks < full_checks);
            assert_eq!(cropped.rects().collect::<Vec<_>>(), [[256, 0, 512, 32]]);
            assert_eq!(cropped.column_spans(rect).collect::<Vec<_>>(), full.column_spans(rect).collect::<Vec<_>>());
            let prepared = prepare(&scene, Some(rect), 64, None, false);
            let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
            let pixels = frame.copy_rgba8(&mut || Ok(())).unwrap();
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] != 0));
            assert_eq!(&*pixels, &doc.preview(Some(rect), 64, None, false).unwrap().2);

            drop((pixels, frame, prepared, cropped, full, topology));
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }

    #[test]
    fn wide_live_stroke_keeps_its_halo_under_a_small_resident_budget() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(4097, 129, depth).unwrap();
            let layer = &mut doc.layers[0];
            layer.pixels.set16(511, 64, [12347, 23459, 34571, 65535]);
            layer.effects.push(crate::effects::Effect {
                id: crate::engine::id(),
                kind: "stroke".into(),
                enabled: true,
                weight: 1.,
                settings: serde_json::json!({
                    "size": 128., "position": "outside", "color": [1., 0., 0.], "opacity": 1.,
                }).into(),
            });
            let budget = crate::large_document::ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let (scene, path) = crate::compositor::native::tests::capture_kind_with_budget(&doc, true, budget);
            let crop = [0, 63, 4097, 67];
            let target = Some(doc.layers[0].id.as_str());
            let prepared = prepare(&scene, Some(crop), 8192, target, false);
            let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
            let pixels = frame.copy_rgba8(&mut || Ok(())).unwrap();
            let sample = |x: usize| {
                let offset = (4097 + x) * 4;
                &pixels[offset..offset + 4]
            };
            for x in [510, 512] {
                assert_eq!(sample(x), [255, 0, 0, 255]);
            }
            assert_eq!(sample(639), [255, 0, 0, 128]);
            assert_eq!(sample(640), [0; 4]);
            assert_eq!(&*pixels, &doc.preview(Some(crop), 8192, target, false).unwrap().2);

            let topology = Topology::new(&scene).unwrap();
            let tiles = topology.composite_tiles(None, &mut || Ok(())).unwrap().unwrap();
            assert_eq!(tiles.column_spans(crop).collect::<Vec<_>>(), [[0, 768]]);
            let composite = prepare(&scene, Some(crop), 8192, None, false);
            let frame_composite = composite.render(&scene, &mut || Ok(())).unwrap();
            let sparse_pixels = frame_composite.copy_rgba8(&mut || Ok(())).unwrap();
            assert_eq!(&*sparse_pixels, &*pixels);

            use crate::production_export::{native, native_rows::Rows, Format, Resampling};
            let rows = Rows::new(&scene, &|| Ok(())).unwrap();
            let output = native::canvas(&scene, Format::Png);
            let stride = usize::from(depth / 2);
            let bytes = 4097 * stride;
            let mut scanline = crate::large_document::ScratchBytes::new(scene.budget(), bytes, bytes).unwrap();
            rows.fill(&output, depth, Resampling::Nearest, 64, 1, &mut scanline, &|| Ok(())).unwrap();
            let fringe = &scanline[639 * stride..640 * stride];
            if depth == 16 {
                assert_eq!(fringe, &[255, 255, 0, 0, 0, 0, 128, 0]);
            } else {
                assert_eq!(fringe, &[255, 0, 0, 128]);
            }
            assert!(scanline[640 * stride..].iter().all(|&byte| byte == 0));
            drop((scanline, rows, tiles, topology, sparse_pixels, frame_composite, composite, pixels, frame, prepared));
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }

    #[test]
    fn sparse_canvas_skips_empty_stripes_and_keeps_masked_pixels_exact() {
        use std::cell::Cell;

        for depth in [8, 16] {
            let mut doc = Document::new_depth(257, 513, depth).unwrap();
            doc.layers[0].pixels.set16(3, 512, [12347, 23459, 34571, 65535]);
            let mut paint = crate::raster::Raster::new_depth(257, 513, depth);
            paint.set16(3, 512, [50237, 0, 0, 41801]);
            paint.set16(256, 4, [47531, 0, 0, 52931]);
            doc.layers[0].mask = Some(crate::engine::Mask {
                enabled: true,
                linked: true,
                cache_key: crate::engine::id(),
                steps: vec![crate::engine::MaskStep {
                    id: crate::engine::id(),
                    kind: "fill".into(),
                    enabled: true,
                    weight: 1.,
                    value: 128.,
                    pixels: crate::raster::Raster::new_depth(257, 513, depth),
                    settings: serde_json::Value::Null,
                }, crate::engine::MaskStep {
                    id: crate::engine::id(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 0.73,
                    value: 0.,
                    pixels: paint,
                    settings: serde_json::Value::Null,
                }, crate::engine::MaskStep {
                    id: crate::engine::id(),
                    kind: "gaussian".into(),
                    enabled: true,
                    weight: 0.61,
                    value: 0.,
                    pixels: crate::raster::Raster::new_depth(257, 513, depth),
                    settings: serde_json::json!({"radius":2.3}),
                }],
            });
            let (scene, path) = capture(&doc);
            let prepared = prepare(&scene, None, 8192, None, false);
            let expected = doc.preview(None, 8192, None, false).unwrap().2;
            let checks = Cell::new(0);
            let mut next = 0;
            let topology = prepared.topology(&scene).unwrap();
            let occupied = topology.as_ref().unwrap().composite_tiles(None, &mut || Ok(())).unwrap().unwrap();
            assert!(!occupied.intersects([0, 0, 257, 512]));
            drop(occupied);
            let rows = crate::production_export::native_rows::Rows::new(&scene, &|| Ok(())).unwrap();
            assert!(rows.region_is_transparent([0, 0, 257, 512]));
            drop(rows);
            let mut observe = || {
                checks.set(checks.get() + 1);
                Ok(())
            };
            let mut guards = ReadChecks::caller_only(&mut observe);
            prepared.render_stripes(&scene, topology.as_ref(), None, None, false, &mut guards, |rows, bytes| {
                let bytes = complete_pixels(bytes)?;
                assert_eq!(rows[0], next);
                if next == 0 {
                    assert!(rows[1] > 256 && rows[1] < 512);
                    assert!(checks.get() < 100, "Empty stripe entered the pixel renderer");
                    assert!(bytes.iter().all(|&byte| byte == 0));
                }
                let stride = 257 * 4;
                assert_eq!(&*bytes, &expected[rows[0] as usize * stride..rows[1] as usize * stride]);
                next = rows[1];
                Ok(())
            }).unwrap();
            assert_eq!(next, 513);
            assert!(expected.iter().any(|&byte| byte != 0));
            drop(topology);
            drop((prepared, scene));
            crate::compositor::native::tests::cleanup(&path);
        }
    }

    #[test]
    fn streamed_selection_stripes_match_legacy_without_retaining_completed_pixels() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(257, 1025, depth).unwrap();
            doc.layers[0].kind = "fill".into();
            doc.layers[0].color = [23, 47, 71, 199];
            doc.selection = Some([1, 1, 256, 1024]);
            let original = serde_json::to_value(&doc).unwrap();
            let (scene, path) = capture(&doc);
            let pool = scene.budget().clone();
            for (mode, name) in [
                (SelectionView::Mask, "mask"),
                (SelectionView::Overlay, "overlay"),
                (SelectionView::Cutout, "cutout"),
            ] {
                let prepared = Prepared::capture(
                    &scene, None, 8192, None, false, Some(mode), &mut || Ok(()),
                ).unwrap();
                let (width, height, mut expected, area) = doc.preview(None, 8192, None, false).unwrap();
                crate::selection::display::apply_region(&doc, width, height, &mut expected, area, name);
                let topology = prepared.topology(&scene).unwrap();
                assert_eq!(topology.is_none(), mode == SelectionView::Mask);
                if mode == SelectionView::Cutout {
                    let wrong = Topology::new(&scene).unwrap();
                    let error = prepared.render_stripes(&scene, Some(&wrong), None, None, false,
                        &mut ReadChecks::caller_only(&mut || Ok(())),
                        |_, _| panic!("Mismatched topology published a stripe"))
                        .unwrap_err();
                    assert!(error.contains("differs from the captured preview"));
                }
                let baseline = pool.used();
                let mut next = 0;
                let mut count = 0;
                let status = prepared.render_stripes(&scene, topology.as_ref(), None, None, false,
                    &mut ReadChecks::caller_only(&mut || Ok(())), |rows, bytes| {
                    let bytes = complete_pixels(bytes)?;
                    assert_eq!(rows[0], next);
                    let start = rows[0] as usize * width as usize * 4;
                    let end = rows[1] as usize * width as usize * 4;
                    assert_eq!(&*bytes, &expected[start..end]);
                    assert!(pool.used() - baseline < bytes.len() + 128 * 1024,
                        "Completed stripes or a duplicate display stripe remain admitted");
                    next = rows[1];
                    count += 1;
                    Ok(())
                }).unwrap();
                assert_eq!(next, height);
                assert!(count > 1);
                assert_eq!(status == DisplayStatus::Mask, mode == SelectionView::Mask);
                assert_eq!(pool.used(), baseline);
                let error = prepared.render_stripes(&scene, topology.as_ref(), None, None, false,
                    &mut ReadChecks::caller_only(&mut || Ok(())), |_, _| {
                    Err("Selection consumer cancelled".into())
                }).unwrap_err();
                assert_eq!(error, "Selection consumer cancelled");
                assert_eq!(pool.used(), baseline);
            }
            assert_eq!(serde_json::to_value(&doc).unwrap(), original);
            assert_eq!(scene.metadata().revision(), doc.revision);
            drop(scene);
            assert_eq!(pool.used(), 0);
            crate::compositor::native::tests::cleanup(&path);
        }
    }
    #[test]
    fn selection_projection_cancels_after_partial_stripes_without_leaking_a_frame_or_admission() {
        let mut doc = Document::new_depth(513, 259, 16).unwrap();
        doc.layers[0].kind = "fill".into();
        doc.layers[0].color = [23, 47, 71, 199];
        doc.selection = Some([1, 1, 512, 258]);
        let (scene, path) = capture(&doc);
        let pool = scene.budget().clone();
        let prepared = Arc::new(
            Prepared::capture(
                &scene,
                None,
                8192,
                None,
                false,
                Some(SelectionView::Cutout),
                &mut || Ok(()),
            )
            .unwrap(),
        );
        let baseline = pool.used();
        let mut calls = 0;
        let frame = prepared.render(&scene, &mut || {
            calls += 1;
            Ok(())
        }).unwrap();
        assert!(frame.stripes().len() > 1);
        drop(frame);
        assert_eq!(pool.used(), baseline);
        for stop in [2, calls / 2, calls - 1] {
            let mut count = 0;
            let failure = prepared.render(&scene, &mut || {
                count += 1;
                if count == stop {
                    Err("selection cancelled".into())
                } else {
                    Ok(())
                }
            }).err().unwrap();
            assert_eq!(failure, "selection cancelled");
            assert_eq!(pool.used(), baseline);
            assert_eq!(scene.metadata().revision(), doc.revision);
        }
        drop((prepared, scene));
        assert_eq!(pool.used(), 0);
        crate::compositor::native::tests::cleanup(&path);
    }
    #[test]
    fn native_preview_matches_legacy_raw_targets_hidden_rgb_clipping_and_rect_grids() {
        for depth in [8, 16] {
            let mut doc = document(depth, 29, 23);
            doc.layers[0].x = -2;
            doc.layers[0].y = 1;
            doc.layers[0].opacity = 0.27;
            doc.layers[0].visible = false;
            doc.layers[0].blend = "multiply".into();
            let mut base = Layer::new("Base", "fill", 29, 23);
            base.color = [17, 43, 79, 203];
            base.pixels = crate::raster::Raster::new_depth(29, 23, depth);
            doc.layers[0].clip_to = Some(base.id.clone());
            doc.layers.push(base);
            let (scene, path) = capture(&doc);
            let target = doc.layers[0].id.as_str();
            for rect in [
                None,
                Some([-7, 2, 35, 19]),
                Some([8, 9, 2, -6]),
                Some([2, 3, 27, 22]),
            ] {
                for edge in [0, 7, 19, 8193] {
                    for (target, mask) in [
                        (None, false),
                        (Some(target), false),
                        (Some(target), true),
                        (None, true),
                    ] {
                        let prepared = prepare(&scene, rect, edge, target, mask);
                        let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
                        let actual = frame.copy_rgba8(&mut || Ok(())).unwrap();
                        let expected = doc.preview(rect, edge, target, mask).unwrap();
                        assert_eq!(
                            (frame.key().width, frame.key().height, frame.key().rect),
                            (expected.0, expected.1, expected.3)
                        );
                        assert_eq!(
                            &*actual, &expected.2,
                            "native preview depth{depth} edge{edge} mask{mask}"
                        );
                    }
                }
            }
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }
    #[test]
    fn legacy_left_edge_samples_are_not_export_pixel_centers() {
        for depth in [8, 16] {
            let doc = document(depth, 7, 3);
            let (scene, path) = capture(&doc);
            let prepared = prepare(&scene, None, 3, Some(&doc.layers[0].id), false);
            assert_eq!([prepared.x(0), prepared.x(1), prepared.x(2)], [0, 2, 4]);
            assert_eq!((prepared.key.width, prepared.key.height), (3, 1));
            let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
            let pixels = frame.copy_rgba8(&mut || Ok(())).unwrap();
            for x in 0..3 {
                let value = doc.layers[0].pixels.get16(x as i32 * 2, 0);
                let expected = if depth == 16 {
                    crate::depth16::display_pixel(value)
                } else {
                    value.map(crate::raster::project16)
                };
                assert_eq!(&pixels[x * 4..x * 4 + 4], &expected);
            }
            drop(frame);
            drop(prepared);
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }
    #[test]
    fn native_effect_preview_matches_source_and_adjustment_targets() {
        for depth in [8, 16] {
            for kind in [
                "levels",
                "curves",
                "adjust",
                "color_balance",
                "hsl",
                "invert",
                "grayscale",
                "posterize",
                "threshold",
                "channel_clamp",
                "blur",
                "bloom",
                "color_overlay",
                "gradient_overlay",
                "pattern_overlay",
                "texture",
                "contour",
                "stroke",
                "bitmap_pattern_overlay",
                "bitmap_texture",
            ] {
                let bitmap = kind.starts_with("bitmap_");
                let kind = kind.strip_prefix("bitmap_").unwrap_or(kind);
                let mut settings = crate::effects::defaults(kind);
                if bitmap {
                    use base64::{engine::general_purpose::STANDARD, Engine as _};
                    let words = [
                        12347, 33559, 51237, 65535, 54321, 12345, 45679, 27139, 19001, 27003,
                        37007, 49151, 61001, 51003, 41009, 65535,
                    ];
                    let png = if depth == 16 {
                        crate::raster::png16(2, 2, &words).unwrap()
                    } else {
                        crate::raster::png(2, 2, &words.map(crate::raster::project16)).unwrap()
                    };
                    settings["pattern"] = serde_json::json!("bitmap");
                    settings["bitmap_png"] = serde_json::json!(STANDARD.encode(png));
                    settings["bitmap_scale"] = serde_json::json!(1.7);
                    settings["bitmap_angle"] = serde_json::json!(23.);
                    settings["phase_x"] = serde_json::json!(-1.3);
                    settings["phase_y"] = serde_json::json!(2.7);
                    settings["sampling"] = serde_json::json!("bilinear");
                }
                let mut doc = document(depth, 29, 23);
                doc.layers[0].x = -3;
                doc.layers[0].y = 2;
                if crate::effects::styles::contains(kind) {
                    doc.layers[0].effects.push(crate::effects::Effect {
                        id: crate::engine::id(),
                        kind: "blur".into(),
                        enabled: true,
                        weight: 0.57,
                        settings: serde_json::json!({"radius":1.7}).into(),
                    });
                }
                doc.layers[0].effects.push(crate::effects::Effect {
                    id: crate::engine::id(),
                    kind: kind.into(),
                    enabled: true,
                    weight: 0.61,
                    settings: settings.into(),
                });
                if crate::effects::styles::contains(kind) {
                    doc.layers[0].effects.push(crate::effects::Effect {
                        id: crate::engine::id(),
                        kind: "blur".into(),
                        enabled: true,
                        weight: 0.63,
                        settings: serde_json::json!({"radius":2.3}).into(),
                    });
                }
                doc.layers[0].effects.push(crate::effects::Effect {
                    id: crate::engine::id(),
                    kind: "blur".into(),
                    enabled: false,
                    weight: 1.,
                    settings: crate::effects::defaults("blur").into(),
                });
                crate::adjustment::add(&mut doc, &serde_json::json!({"kind":"invert"})).unwrap();
                doc.layers[0].opacity = 0.37;
                doc.filters.push(crate::effects::Effect {
                    id: crate::engine::id(),
                    kind: "blur".into(),
                    enabled: true,
                    weight: 0.73,
                    settings: serde_json::json!({"radius":2.3}).into(),
                });
                let (scene, path) = if bitmap {
                    profile_scene(&doc)
                } else {
                    capture(&doc)
                };
                for rect in [None, Some([3, 5, 17, 19])] {
                    for edge in [11, 48] {
                        for target in [
                            None,
                            Some(doc.layers[0].id.as_str()),
                            Some(doc.layers[1].id.as_str()),
                        ] {
                            for mask in [false, true] {
                                let prepared = prepare(&scene, rect, edge, target, mask);
                                let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
                                assert_eq!(
                                    &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                                    &doc.preview(rect, edge, target, mask).unwrap().2,
                                    "{depth} {kind} {rect:?} {target:?} {mask}"
                                );
                            }
                        }
                    }
                }
                let prepared = prepare(&scene, None, 48, None, false);
                let baseline = scene.budget().used();
                let mut checks = 0;
                assert!(prepared
                    .render(&scene, &mut || {
                        checks += 1;
                        if checks > 25 {
                            Err("cancel derived preview".into())
                        } else {
                            Ok(())
                        }
                    })
                    .is_err());
                assert_eq!(scene.budget().used(), baseline);
                drop(prepared);
                drop(scene);
                crate::compositor::native::tests::cleanup(&path);
            }
        }
    }
    #[test]
    fn native_preview_folder_and_fill_targets_match_the_legacy_raw_contract() {
        for depth in [8, 16] {
            for blend in ["normal", "pass_through"] {
                let mut doc = document(depth, 21, 19);
                let mut folder = Layer::new("Folder", "group", 1, 1);
                folder.opacity = 0.37;
                folder.visible = false;
                folder.blend = blend.into();
                doc.layers[0].parent = Some(folder.id.clone());
                let mut fill = Layer::new("Fill", "fill", 21, 19);
                fill.color = [43, 71, 119, 183];
                fill.pixels = crate::raster::Raster::new_depth(21, 19, depth);
                fill.x = 3;
                fill.y = -2;
                fill.effects.push(crate::effects::Effect {
                    id: crate::engine::id(),
                    kind: "bloom".into(),
                    enabled: true,
                    weight: 0.67,
                    settings: serde_json::json!({"threshold":0.1,"spread":2.1,"strength":0.73})
                        .into(),
                });
                doc.layers.insert(0, folder);
                doc.layers.push(fill);
                let (scene, path) = capture(&doc);
                for target in [&doc.layers[0].id, &doc.layers[1].id, &doc.layers[2].id] {
                    for mask in [false, true] {
                        let prepared =
                            prepare(&scene, Some([2, 3, 20, 18]), 11, Some(target), mask);
                        let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
                        let expected = doc
                            .preview(Some([2, 3, 20, 18]), 11, Some(target), mask)
                            .unwrap();
                        assert_eq!(
                            (frame.key().width, frame.key().height, frame.key().rect),
                            (expected.0, expected.1, expected.3)
                        );
                        assert_eq!(
                            &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                            &expected.2,
                            "folder target depth{depth} blend{blend} mask{mask}"
                        );
                    }
                }
                drop(scene);
                crate::compositor::native::tests::cleanup(&path);
            }
        }
    }
    #[test]
    fn display_stripe_admission_refusal_returns_no_frame_and_releases_candidate_charge() {
        for depth in [8, 16] {
            let doc = document(depth, 128, 128);
            let (scene, path) = capture(&doc);
            let prepared = prepare(&scene, None, 8192, Some(&doc.layers[0].id), false);
            drop(prepared.render(&scene, &mut || Ok(())).unwrap());
            let pool = scene.budget();
            let baseline = pool.used();
            let remaining = pool.limit() - baseline - 64 * 1024;
            let other_owner = pool.reserve_bytes(remaining, pool.limit()).unwrap();
            let refused = prepared.render(&scene, &mut || Ok(()));
            assert!(
                refused.is_err(),
                "Display output must not escape its shared admission"
            );
            assert_eq!(pool.used(), baseline + remaining);
            drop(other_owner);
            assert_eq!(pool.used(), baseline);
            let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
            assert_eq!(
                &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                &doc.preview(None, 8192, Some(&doc.layers[0].id), false)
                    .unwrap()
                    .2
            );
            drop(frame);
            drop(prepared);
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }
    #[test]
    fn multistripe_native_previews_keep_complete_owned_bytes_and_bounded_source_regions() {
        for depth in [8, 16] {
            let doc = document(depth, 521, 523);
            let (scene, path) = capture(&doc);
            let prepared = prepare(&scene, None, 8192, Some(&doc.layers[0].id), false);
            let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
            assert!(frame.stripes().len() > 1);
            let mut next = 0;
            for stripe in frame.stripes() {
                assert_eq!(stripe.rows()[0], next);
                let (end, region) = prepared.stripe(next, frame.key().height, None).unwrap();
                assert_eq!(stripe.rows()[1], end);
                assert!(region.pixels() <= MAX_REGION_PIXELS);
                assert_eq!(stripe.bytes().len(), (end - next) as usize * 521 * 4);
                assert!(stripe.bytes().uses_budget(scene.budget()));
                next = end;
            }
            assert_eq!(next, 523);
            assert_eq!(
                &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                &doc.preview(None, 8192, Some(&doc.layers[0].id), false)
                    .unwrap()
                    .2
            );
            let weak = Arc::downgrade(&scene);
            drop(scene);
            assert!(weak.upgrade().is_none());
            // A completed presentation has no backing file/profile owner.
            crate::compositor::native::tests::cleanup(&path);
            assert_eq!(
                frame.copy_rgba8(&mut || Ok(())).unwrap().len(),
                521 * 523 * 4
            );
        }
    }
    #[test]
    fn cancellation_and_foreign_captures_cannot_return_a_partial_or_retagged_frame() {
        let doc = document(16, 29, 23);
        let (scene, path) = capture(&doc);
        let (other, other_path) = capture(&doc);
        let prepared = prepare(&scene, None, 13, None, false);
        let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
        assert!(frame.matches(&Tag(prepared.clone())));
        let wrong = prepare(&other, None, 13, None, false);
        assert!(!frame.matches(&Tag(wrong.clone())));
        assert!(prepared.render(&other, &mut || Ok(())).is_err());
        drop(frame);
        let baseline = scene.budget().used();
        let mut checks = 0;
        let result = prepared.render(&scene, &mut || {
            checks += 1;
            if checks > 10 {
                Err("Cancelled preview".into())
            } else {
                Ok(())
            }
        });
        assert!(result.err().unwrap().contains("Cancelled preview"));
        assert_eq!(scene.budget().used(), baseline);
        drop(prepared);
        drop(wrong);
        drop(scene);
        drop(other);
        crate::compositor::native::tests::cleanup(&path);
        crate::compositor::native::tests::cleanup(&other_path);
    }
    #[test]
    fn merged_output_ignores_hidden_folders_and_clips_under_a_small_disk_budget() {
        use crate::{
            engine::{native::{worker, NativeDocument}, scene::{CapturedScene, LegacyTileCache}, Layer},
            production_export::{native, Format, Settings},
            raster::Raster,
        };

        for depth in [8, 16] {
            let fixture = worker::tests::Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
            let mut doc = Document::new_depth(4097, 513, depth).unwrap();
            let crop = [5, 4, 12, 9];
            let mut live_clip = Layer::new("Visible clipped paint", "paint", 7, 5);
            live_clip.pixels = Raster::new_depth(7, 5, depth);
            live_clip.x = crop[0];
            live_clip.y = crop[1];
            live_clip.clip_to = Some(doc.layers[0].id.clone());
            live_clip.opacity = 0.47;
            for y in crop[1]..crop[3] {
                for x in crop[0]..crop[2] {
                    doc.layers[0].pixels.set16(x, y, [12347, 31119, 52973, 65535]);
                    live_clip.pixels.set16(x - crop[0], y - crop[1], [52413, 21657, 19073, 51321]);
                }
            }
            doc.layers.insert(0, live_clip);
            let expected = doc.preview(Some(crop), 8192, None, false).unwrap().2;
            let expected_words = if depth == 16 {
                crate::depth16::preview16(&doc, Some(crop), 8192, None, false).unwrap().2
            } else {
                expected.iter().map(|&byte| u16::from(byte) * 257).collect()
            };

            let mut hidden = Layer::new("Hidden folder", "group", 4097, 513);
            hidden.pixels = Raster::new_depth(4097, 513, depth);
            hidden.blend = "pass_through".into();
            hidden.visible = false;
            let mut hidden_child = Layer::new("Hidden folder child", "paint", 4097, 513);
            hidden_child.parent = Some(hidden.id.clone());
            let mut base = Layer::new("Hidden clipping base", "paint", 4097, 513);
            base.pixels = Raster::new_depth(4097, 513, depth);
            base.visible = false;
            let mut clip = Layer::new("Clip over hidden base", "paint", 4097, 513);
            clip.clip_to = Some(base.id.clone());
            for layer in [&mut hidden_child, &mut clip] {
                layer.pixels = Raster::new_depth(4097, 513, depth);
                layer.effects.push(crate::effects::Effect {
                    id: crate::engine::id(), kind: "drop_shadow".into(), enabled: true, weight: 1.,
                    settings: crate::effects::defaults("drop_shadow").into(),
                });
            }
            doc.layers.splice(0..0, [hidden, hidden_child, clip, base]);
            doc.selection = Some(crop);
            let staged = CapturedScene::from_parts(doc, "visible-composite".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(())).unwrap();
            let document = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
            let scene = Arc::new(SceneSnapshot::from_native(
                SceneIdentity { project_id: "visible-composite".into(), document_id: document.metadata().id().into(), revision: 0 },
                document.clone(), vec![], None, None,
            ).unwrap());
            assert!(Stored::needed(&Topology::new(&scene).unwrap()));
            assert!(!Stored::needed(&Topology::for_composite(&scene, None).unwrap()));
            let disk = runtime.disk();
            let held = disk.reserve(disk.limit() - disk.admitted().unwrap() - 4 * 1024 * 1024).unwrap();
            let baseline = disk.used().unwrap();
            let read = runtime.begin_read(&scene).unwrap();
            let request = Request::capture(scene.clone(), read.clone(), Some(crop), 8192, None, false).unwrap();
            let frame = request.render(&mut || Ok(())).unwrap();
            assert_eq!(&*frame.copy_rgba8(&mut || Ok(())).unwrap(), &expected);

            let mut output = native::canvas(&scene, Format::Png);
            output.document_rect = crop;
            output.width = 7;
            output.height = 5;
            let mut png = Vec::new();
            native::render(&scene, &read, &output, &Settings::default(), &mut png, 1024 * 1024, &|| Ok(())).unwrap();
            assert_eq!(image::load_from_memory(&png).unwrap().to_rgba16().into_raw(), expected_words);

            let session = staged.session.as_ref().unwrap();
            let copy = crate::clipboard::native::stage_copy(
                &document, &scene, crate::clipboard::native::Request { target: "", mask: false, merged: true },
                session, &mut crate::transform::native::Work::new(1024).unwrap(), &|| Ok(()),
            ).unwrap();
            let (_, bounds, copy) = copy.into_parts();
            assert_eq!(bounds, crop);
            let pixels = copy.surface().prepare_region(Region::new([0, 0, 7, 5]).unwrap(), &mut || Ok(())).unwrap();
            for y in 0..5 {
                for x in 0..7 {
                    let offset = (y * 7 + x) as usize * 4;
                    assert_eq!(pixels.pixel16(x, y).unwrap(), expected_words[offset..offset + 4]);
                }
            }
            drop(pixels);
            copy.abort(session, &|| session.cleanup_check()).unwrap();
            assert_eq!(disk.used().unwrap(), baseline);
            drop(held);
        }
    }

    #[test]
    fn hidden_clipped_raster_previews_keep_pixels_without_staging_the_backdrop() {
        use crate::engine::{native::{worker, NativeDocument}, scene::{CapturedScene, LegacyTileCache}, Layer};

        for depth in [8, 16] {
            for kind in ["invert", "drop_shadow"] {
                let fixture = worker::tests::Fixture::new();
                let (runtime, _connection) = fixture.runtime();
                let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
                let mut doc = document(depth, 30, 29);
                let effect = |kind: &str| crate::effects::Effect {
                    id: crate::engine::id(), kind: kind.into(), enabled: true, weight: 1.,
                    settings: crate::effects::defaults(kind).into(),
                };
                let mut base = Layer::new("Clipping base", "paint", 30, 29);
                base.pixels = doc.layers[0].pixels.clone();
                base.effects.push(effect("drop_shadow"));
                doc.layers[0].visible = false;
                doc.layers[0].x = 3;
                doc.layers[0].clip_to = Some(base.id.clone());
                if kind == "drop_shadow" {
                    doc.layers[0].effects.push(effect("invert"));
                }
                doc.layers[0].effects.push(effect(kind));
                if kind == "drop_shadow" {
                    doc.layers[0].effects.push(effect("blur"));
                }
                let target = doc.layers[0].id.clone();
                doc.layers.push(base);
                doc.filters.push(effect("blur"));
                let staged = CapturedScene::from_parts(doc, "raw-color".into(), vec![], None, None)
                    .stage(&runtime, &cache, &|| Ok(())).unwrap();
                let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
                let scene = Arc::new(SceneSnapshot::from_native(
                    SceneIdentity { project_id: "raw-color".into(), document_id: native.metadata().id().into(), revision: 0 },
                    native, vec![], None, None,
                ).unwrap());
                assert!(Topology::for_target(&scene, Some(&target)).is_err());
                let crop = [2, 1, 18, 15];
                let region = Region::new(crop).unwrap();
                let topology = Topology::new(&scene).unwrap();
                let session = staged.session.as_ref().unwrap();
                let baseline = runtime.disk().used().unwrap();
                let stored = Stored::prepare(topology.clone(), session, &mut || Ok(())).unwrap();
                let full_disk = runtime.disk().used().unwrap();
                let window = LayerWindow::prepare(&topology, region, Some(&stored), &mut || Ok(())).unwrap();
                let expected = if depth == 8 {
                    let plan = Plan::<Native8, _>::with_topology(topology.clone(), &window, region).unwrap();
                    crate::render::rgba(16, 14, &|| Ok(()), |x, y| {
                        plan.layer(0, x as i32 + crop[0], y as i32 + crop[1]).unwrap()
                    }).unwrap()
                } else {
                    let plan = Plan::<Native16, _>::with_topology(topology.clone(), &window, region).unwrap();
                    crate::render::rgba(16, 14, &|| Ok(()), |x, y| {
                        crate::depth16::display_pixel(plan.layer(0, x as i32 + crop[0], y as i32 + crop[1]).unwrap())
                    }).unwrap()
                };
                drop(window);
                let merged = prepare_filters_with_layers(&topology, region, Some(&stored), &mut || Ok(())).unwrap().unwrap();
                let expected_merged = crate::render::rgba(16, 14, &|| Ok(()), |x, y| {
                    crate::depth16::display_pixel(merged.pixel16(x as i32 + crop[0], y as i32 + crop[1]).unwrap())
                }).unwrap();
                drop(merged);
                stored.retire(&mut || session.cleanup_check()).unwrap();
                assert_eq!(runtime.disk().used().unwrap(), baseline);
                let request = Request::capture(scene.clone(), runtime.begin_read(&scene).unwrap(),
                    Some(crop), 8192, Some(&target), false).unwrap();
                let mut actual = Vec::new();
                request.render_into(&mut || Ok(()), &mut |_, bytes| {
                    let used = runtime.disk().used().unwrap();
                    if kind == "invert" { assert_eq!(used, baseline); }
                    else { assert!(used > baseline && used < full_disk); }
                    actual.extend_from_slice(&bytes);
                    Ok(())
                }).unwrap();
                assert_eq!(actual, expected);
                assert_eq!(runtime.disk().used().unwrap(), baseline);

                let merged = Request::capture(scene.clone(), runtime.begin_read(&scene).unwrap(),
                    Some(crop), 8192, None, false).unwrap().render(&mut || Ok(())).unwrap();
                assert_eq!(&*merged.copy_rgba8(&mut || Ok(())).unwrap(), &expected_merged);
                if kind == "drop_shadow" {
                    let scoped = Topology::for_preview(&scene, Some(0)).unwrap();
                    let cropped = Stored::prepare_cropped(scoped.clone(), session, crop, &mut || Ok(())).unwrap();
                    let outside = Region::new([0, 0, 1, 1]).unwrap();
                    let error = LayerWindow::prepare(&scoped, outside, Some(&cropped), &mut || Ok(()))
                        .err().unwrap();
                    assert!(error.contains("exceeds its prepared crop"));
                    cropped.retire(&mut || session.cleanup_check()).unwrap();
                }
                assert_eq!(runtime.disk().used().unwrap(), baseline);
            }
        }
    }

    #[test]
    fn cropped_folder_and_adjustment_stages_match_full_backdrops() {
        use crate::engine::{native::{worker, NativeDocument}, scene::{CapturedScene, LegacyTileCache}, Layer};
        use crate::raster::Raster;

        for depth in [8, 16] {
            for folder_effect in ["blur", "drop_shadow"] {
                let fixture = worker::tests::Fixture::new();
                let (runtime, _connection) = fixture.runtime();
                let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
                let mut doc = document(depth, 97, 65);
                let effect = |kind: &str| crate::effects::Effect {
                    id: crate::engine::id(), kind: kind.into(), enabled: true, weight: 0.73,
                    settings: crate::effects::defaults(kind).into(),
                };
                let mut folder = Layer::new("Folder", "group", 97, 65);
                folder.blend = "normal".into();
                folder.pixels = Raster::new_depth(97, 65, depth);
                folder.effects.push(effect(folder_effect));
                let folder_id = folder.id.clone();
                doc.layers[0].parent = Some(folder.id.clone());
                doc.layers[0].effects.push(effect("drop_shadow"));
                let mut paint = Raster::new_depth(97, 65, depth);
                paint.set16(70, 37, [18731, 0, 0, 54413]);
                doc.layers[0].mask = Some(crate::engine::Mask {
                    enabled: true,
                    linked: true,
                    cache_key: crate::engine::id(),
                    steps: vec![
                        crate::engine::MaskStep {
                            id: crate::engine::id(),
                            kind: "paint".into(),
                            enabled: true,
                            weight: 0.8,
                            value: 0.,
                            pixels: paint,
                            settings: serde_json::Value::Null,
                        },
                        crate::engine::MaskStep {
                            id: crate::engine::id(),
                            kind: "gaussian".into(),
                            enabled: true,
                            weight: 1.,
                            value: 0.,
                            pixels: Raster::new_depth(97, 65, depth),
                            settings: serde_json::json!({"radius":2.3}),
                        },
                    ],
                });
                let mut adjustment = Layer::new("Invert", "adjustment", 97, 65);
                adjustment.opacity = 0.47;
                adjustment.pixels = Raster::new_depth(97, 65, depth);
                adjustment.effects.push(effect("invert"));
                let mut base = Layer::new("Backdrop", "fill", 97, 65);
                base.color = [27, 61, 103, 255];
                base.pixels = Raster::new_depth(97, 65, depth);
                doc.layers.splice(0..0, [adjustment, folder]);
                doc.layers.push(base);
                let staged = CapturedScene::from_parts(doc, "folder-crop".into(), vec![], None, None)
                    .stage(&runtime, &cache, &|| Ok(())).unwrap();
                let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
                let scene = Arc::new(SceneSnapshot::from_native(
                    SceneIdentity { project_id: "folder-crop".into(), document_id: native.metadata().id().into(), revision: 0 },
                    native, vec![], None, None,
                ).unwrap());
                let topology = Topology::for_composite(&scene, None).unwrap();
                let session = staged.session.as_ref().unwrap();
                let disk = runtime.disk();
                let baseline = disk.used().unwrap();
                let full = Stored::prepare(topology.clone(), session, &mut || Ok(())).unwrap();
                let full_bytes = disk.used().unwrap() - baseline;
                let crop = [66, 31, 81, 44];
                let region = Region::new(crop).unwrap();
                let window = LayerWindow::prepare(&topology, region, Some(&full), &mut || Ok(())).unwrap();
                let expected = [false, true].map(|raw_folder| {
                    if depth == 8 {
                        let plan = Plan::<Native8, _>::with_topology(topology.clone(), &window, region).unwrap();
                        crate::render::rgba(15, 13, &|| Ok(()), |x, y| {
                            let (x, y) = (x as i32 + crop[0], y as i32 + crop[1]);
                            if raw_folder { plan.layer(1, x, y) } else { plan.pixel(x, y) }.unwrap()
                        }).unwrap()
                    } else {
                        let plan = Plan::<Native16, _>::with_topology(topology.clone(), &window, region).unwrap();
                        crate::render::rgba(15, 13, &|| Ok(()), |x, y| {
                            let (x, y) = (x as i32 + crop[0], y as i32 + crop[1]);
                            let pixel = if raw_folder { plan.layer(1, x, y) } else { plan.pixel(x, y) }.unwrap();
                            crate::depth16::display_pixel(pixel)
                        }).unwrap()
                    }
                });
                drop(window);
                full.retire(&mut || session.cleanup_check()).unwrap();
                assert_eq!(disk.used().unwrap(), baseline);

                for (target, expected) in [None, Some(folder_id.as_str())].into_iter().zip(expected) {
                    let request = Request::capture(scene.clone(), runtime.begin_read(&scene).unwrap(),
                        Some(crop), 8192, target, false).unwrap();
                    let mut pixels = Vec::new();
                    request.render_into(&mut || Ok(()), &mut |_, bytes| {
                        let cropped_bytes = disk.used().unwrap() - baseline;
                        assert!(cropped_bytes > 0 && cropped_bytes < full_bytes);
                        pixels.extend_from_slice(&bytes);
                        Ok(())
                    }).unwrap();
                    assert_eq!(pixels, expected, "{target:?}, {folder_effect}, native{depth}");
                    assert_eq!(disk.used().unwrap(), baseline);
                }
            }
        }
    }

    #[test]
    fn raw_mask_requests_skip_composition_and_other_masks_at_both_depths() {
        use crate::engine::{native::{worker, NativeDocument}, scene::{CapturedScene, LegacyTileCache}, Mask, MaskStep};

        for depth in [8, 16] {
            for has_mask in [false, true] {
                let fixture = worker::tests::Fixture::new();
                let (runtime, _connection) = fixture.runtime();
                let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
                let mut doc = document(depth, 41, 37);
                doc.layers[0].x = 7;
                doc.layers[0].y = -3;
                doc.layers[0].visible = false;
                doc.layers[0].opacity = 0.23;
                let target = doc.layers[0].id.clone();
                if has_mask {
                    let mut paint = crate::raster::Raster::new_depth(41, 37, depth);
                    for y in 0..37 {
                        for x in 0..41 {
                            paint.set16(x, y, [(12347 + x * 617 + y * 317) as u16, 0, 0, 41503]);
                        }
                    }
                    doc.layers[0].mask = Some(Mask {
                        enabled: false, linked: false, cache_key: crate::engine::id(),
                        steps: vec![
                            MaskStep { id: crate::engine::id(), kind: "paint".into(), enabled: true, weight: 0.73,
                                value: 0., pixels: paint, settings: serde_json::Value::Null },
                            MaskStep { id: crate::engine::id(), kind: "gaussian".into(), enabled: true, weight: 0.61,
                                value: 0., pixels: crate::raster::Raster::new_depth(41, 37, depth),
                                settings: serde_json::json!({"radius":2.3}) },
                        ],
                    });
                }
                let mut references = Vec::new();
                for edge in [13, 8192] {
                    references.push((edge, doc.preview(Some([2, 1, 39, 35]), edge, Some(&target), true).unwrap().2));
                }
                let mut unrelated = doc.layers[0].clone();
                unrelated.id = crate::engine::id();
                unrelated.visible = true;
                if let Some(mask) = &mut unrelated.mask {
                    mask.cache_key = crate::engine::id();
                    for step in &mut mask.steps { step.id = crate::engine::id(); }
                }
                doc.layers.push(unrelated);
                for layer in &mut doc.layers {
                    layer.effects.push(crate::effects::Effect {
                        id: crate::engine::id(), kind: "drop_shadow".into(), enabled: true, weight: 1.,
                        settings: crate::effects::defaults("drop_shadow").into(),
                    });
                }
                let original = serde_json::to_value(&doc).unwrap();
                let staged = CapturedScene::from_parts(doc.clone(), "raw-mask".into(), vec![], None, None)
                    .stage(&runtime, &cache, &|| Ok(())).unwrap();
                let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
                let scene = Arc::new(SceneSnapshot::from_native(
                    SceneIdentity { project_id: "raw-mask".into(), document_id: native.metadata().id().into(), revision: 0 },
                    native, vec![], None, None,
                ).unwrap());
                assert!(Stored::needed(&Topology::new(&scene).unwrap()));
                let baseline = runtime.disk().used().unwrap();
                for (edge, expected) in references {
                    assert_eq!(doc.preview(Some([2, 1, 39, 35]), edge, Some(&target), true).unwrap().2, expected);
                    let request = Request::capture(scene.clone(), runtime.begin_read(&scene).unwrap(),
                        Some([2, 1, 39, 35]), edge, Some(&target), true).unwrap();
                    assert!(request.prepared.topology(&scene).unwrap().is_none());
                    let mut pixels = Vec::new();
                    let mut next = 0;
                    let complete = request.render_into(&mut || Ok(()), &mut |rows, bytes| {
                        assert_eq!(rows[0], next);
                        assert_eq!(runtime.disk().used().unwrap(), baseline, "Mask preview created a color stage");
                        pixels.extend_from_slice(&bytes);
                        next = rows[1];
                        Ok(())
                    }).unwrap();
                    assert_eq!(complete.display_status, DisplayStatus::Mask);
                    assert_eq!(next, request.key().height);
                    assert_eq!(pixels, expected);
                    assert_eq!(runtime.disk().used().unwrap(), baseline);
                    assert!(request.render(&mut || Err("raw mask cancellation".into())).err().unwrap().contains("raw mask cancellation"));
                }
                assert_eq!(serde_json::to_value(&doc).unwrap(), original);
            }
        }
    }

    #[test]
    fn disabled_mask_renders_its_raw_channel_and_unprepared_effects_refuse() {
        for variant in 0..3 {
            let mut doc = document(16, 9, 7);
            match variant {
                0 | 1 => {
                    doc.layers[0].mask = Some(crate::engine::Mask {
                        enabled: variant == 1,
                        linked: true,
                        cache_key: crate::engine::id(),
                        steps: vec![crate::engine::MaskStep {
                            id: crate::engine::id(),
                            kind: "fill".into(),
                            enabled: true,
                            weight: 0.7,
                            value: 91.,
                            settings: serde_json::Value::Null,
                            pixels: crate::raster::Raster::new_depth(9, 7, 16),
                        }],
                    })
                }
                _ => doc.layers[0].effects.push(crate::effects::Effect {
                    id: crate::engine::id(),
                    kind: "unsupported_effect".into(),
                    enabled: true,
                    weight: 1.,
                    settings: Arc::new(serde_json::json!({})),
                }),
            }
            let (scene, path) = capture(&doc);
            let target = Some(doc.layers[0].id.as_str());
            for (target, mask) in [(target, true), (None, false)] {
                let result =
                    Prepared::capture(&scene, None, 48, target, mask, None, &mut || Ok(()));
                if variant == 2 {
                    assert!(result.is_err());
                } else {
                    let frame = Arc::new(result.unwrap())
                        .render(&scene, &mut || Ok(()))
                        .unwrap();
                    assert_eq!(
                        &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                        &doc.preview(None, 48, target, mask).unwrap().2
                    );
                }
            }
            drop(scene);
            crate::compositor::native::tests::cleanup(&path);
        }
    }

    fn profile_scene(doc: &Document) -> (Arc<SceneSnapshot>, std::path::PathBuf) {
        let (scene, path) = crate::compositor::native::tests::capture_kind_with_budget(
            doc,
            true,
            ResidentBudget::new(192 * 1024 * 1024).unwrap(),
        );
        (Arc::new(scene), path)
    }
    #[test]
    fn icc_preview_matches_legacy_rows_before_projection_with_raw_warning_and_mask_bypass() {
        let mut unsupported = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        unsupported[16..20].copy_from_slice(b"CMYK");
        for depth in [8, 16] {
            for (profile, supported) in [
                (crate::color_profile::srgb_profile().to_vec(), true),
                (
                    moxcms::ColorProfile::new_display_p3().encode().unwrap(),
                    true,
                ),
                (unsupported.clone(), false),
                (vec![0; 132], false),
            ] {
                let mut doc = document(depth, 29, 23);
                doc.icc_profile = Some(Arc::new(profile));
                let before = doc.icc_profile.clone();
                let (scene, path) = profile_scene(&doc);
                for mask in [false, true] {
                    for target in [None, Some(doc.layers[0].id.as_str())] {
                        let prepared = prepare(&scene, Some([2, 3, 27, 22]), 13, target, mask);
                        let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
                        let expected = doc.preview(Some([2, 3, 27, 22]), 13, target, mask).unwrap();
                        assert_eq!(
                            &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                            &expected.2,
                            "ICC depth{depth} mask{mask} supported{supported}"
                        );
                        assert_eq!(
                            frame.display_status(),
                            if mask {
                                DisplayStatus::Mask
                            } else if supported {
                                DisplayStatus::Converted
                            } else {
                                DisplayStatus::Unsupported
                            }
                        );
                        assert_eq!(frame.profile_warning().is_some(), !mask && !supported);
                        assert_eq!(
                            scene.metadata().profile(),
                            before.as_deref().map(Vec::as_slice)
                        );
                        assert_eq!(doc.icc_profile, before);
                    }
                }
                drop(scene);
                crate::compositor::native::tests::cleanup(&path);
            }
        }
    }
    #[test]
    fn cms_executor_ends_before_completed_frame_and_tag_allow_the_next_request() {
        let mut doc = document(16, 29, 23);
        doc.icc_profile = Some(Arc::new(
            moxcms::ColorProfile::new_display_p3().encode().unwrap(),
        ));
        let (scene, path) = profile_scene(&doc);
        let pool = scene.budget().clone();
        let prepared = prepare(&scene, None, 48, None, false);
        drop(prepared.render(&scene, &mut || Ok(())).unwrap());
        let baseline = pool.used();
        let frame = prepared.render(&scene, &mut || Ok(())).unwrap();
        let retained = frame._index.bytes()
            + frame
                .stripes
                .iter()
                .map(|stripe| stripe.bytes.allocated_bytes())
                .sum::<usize>();
        assert_eq!(pool.used(), baseline + retained);
        let tag = frame.tag();
        let next = prepare(&scene, None, 48, None, false);
        let next_frame = next.render(&scene, &mut || Ok(())).unwrap();
        assert!(frame.matches(&tag));
        assert_eq!(frame.key(), next_frame.key());
        drop(next_frame);
        drop(next);
        assert_eq!(pool.used(), baseline + retained);
        drop(scene);
        crate::compositor::native::tests::cleanup(&path);
        assert_eq!(frame.copy_rgba8(&mut || Ok(())).unwrap().len(), 29 * 23 * 4);
        drop(frame);
        drop(tag);
        drop(prepared);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn icc_cancel_owner_mismatch_and_insufficient_admission_return_no_fallback_frame() {
        let mut doc = document(16, 29, 23);
        doc.selection = Some([4, 3, 19, 12]);
        doc.icc_profile = Some(Arc::new(
            moxcms::ColorProfile::new_display_p3().encode().unwrap(),
        ));
        let (scene, path) = profile_scene(&doc);
        let prepared = prepare(&scene, None, 48, None, false);
        let crop = Some([2, 1, 25, 20]);
        let mask = Arc::new(Prepared::capture(
            &scene, crop, 13, None, false, Some(SelectionView::Mask), &mut || Ok(()),
        ).unwrap());
        let (width, height, mut expected, area) = doc.preview(crop, 13, None, false).unwrap();
        crate::selection::display::apply_region(&doc, width, height, &mut expected, area, "mask");
        let pool = scene.budget().clone();
        let baseline = scene.budget().used();
        let remaining = scene.budget().limit() - baseline - 127 * 1024 * 1024;
        let owner: Vec<_> = (0..remaining.div_ceil(64 * 1024 * 1024))
            .map(|index| {
                scene
                    .budget()
                    .reserve_bytes(
                        (remaining - index * 64 * 1024 * 1024).min(64 * 1024 * 1024),
                        64 * 1024 * 1024,
                    )
                    .unwrap()
            })
            .collect();
        assert!(prepared.render(&scene, &mut || Ok(())).is_err());
        assert_eq!(scene.budget().used(), baseline + remaining);
        let frame = mask.render(&scene, &mut || Ok(())).unwrap();
        assert_eq!(frame.display_status(), DisplayStatus::Mask);
        assert_eq!(&*frame.copy_rgba8(&mut || Ok(())).unwrap(), &expected);
        drop(frame);
        assert_eq!(scene.budget().used(), baseline + remaining);
        drop(owner);
        let reason = prepared
            .render(&scene, &mut || Err("profile source lease ended".into()))
            .err()
            .unwrap();
        assert_eq!(reason, "profile source lease ended");
        assert_eq!(scene.budget().used(), baseline);
        let mut wrong =
            Prepared::capture(&scene, None, 48, None, false, None, &mut || Ok(())).unwrap();
        wrong.profile.as_mut().unwrap()[0] ^= 1;
        let wrong = Arc::new(wrong);
        let with_tag = scene.budget().used();
        assert!(wrong.render(&scene, &mut || Ok(())).is_err());
        assert_eq!(scene.budget().used(), with_tag);
        drop(wrong);
        drop(prepared);
        drop(mask);
        drop(scene);
        crate::compositor::native::tests::cleanup(&path);
        assert_eq!(pool.used(), 0);
    }
}
