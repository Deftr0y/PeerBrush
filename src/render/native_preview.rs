//! Captured, source-backed preview rows. Display bytes own their admission;
//! completed frames retain no source handle or profile operation.
use crate::{
    color_profile::prepared::{profile_binding, ChargedWords, DisplayPlan, DisplayStatus},
    compositor::native::{Native16, Native8, Plan, RawLayers, Topology},
    engine::{
        native::DocumentKey,
        scene::{SceneIdentity, SceneSnapshot},
    },
    large_document::{Error, PinnedBytes, Reservation, ResidentBudget, ScratchBytes},
    raster::backed::{Region, MAX_REGION_PIXELS},
    workspace::source_runtime::SourceReadSession,
};
use std::sync::Arc;
type Result<T> = std::result::Result<T, String>;
mod transient;
pub(crate) use transient::{
    preview_shared, render_prepared, Origin, SourceSupplement, TransientFrame,
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
            &mut || session.check(),
        )?);
        session.check()?;
        Ok(Self {
            scene,
            prepared,
            session,
        })
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
            && key.width <= 48
            && key.height <= 48
            && key.rect == [0, 0, doc.width() as i32, doc.height() as i32]
    }
    pub(crate) fn render(&self, check: &mut dyn FnMut() -> Result<()>) -> Result<Frame> {
        let mut checked = || {
            self.session.check()?;
            check()?;
            self.session.check()
        };
        self.prepared.render(&self.scene, &mut checked)
    }
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
}
#[derive(Clone, Copy)]
enum Grid {
    Eight(f32),
    Sixteen(f64),
}
impl Grid {
    fn coordinate(self, value: u32) -> i32 {
        match self {
            Self::Eight(scale) => (value as f32 / scale) as i32,
            Self::Sixteen(scale) => (value as f64 / scale) as i32,
        }
    }
}
struct Prepared {
    key: Key,
    target: Option<usize>,
    grid: Grid,
    pool: ResidentBudget,
    profile: Option<[u8; 32]>,
    _tags: Reservation,
}
/// Reply tracking shares the exact admitted tags instead of cloning strings.
#[derive(Clone)]
pub(crate) struct Tag(Arc<Prepared>);
impl Tag {
    pub(crate) fn key(&self) -> &Key {
        &self.0.key
    }
}
impl Prepared {
    fn capture(
        scene: &SceneSnapshot,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
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
            return Err("Native preview requires its prepared protected-document provider".into());
        }
        if doc.filters().any(|e| e.enabled() && e.weight() > 0.) {
            return Err("Native preview requires prepared document filters".into());
        }
        for layer in doc.layers() {
            if layer.mask().is_some() {
                return Err("Native preview requires prepared raw and composed masks".into());
            }
            if layer.source().is_some() {
                return Err("Native preview requires its prepared editable-source provider".into());
            }
            if layer.effects().any(|e| e.enabled() && e.weight() > 0.) {
                return Err("Native preview requires prepared layer effects".into());
            }
        }
        let index = target
            .map(|id| {
                doc.layers()
                    .position(|layer| layer.id() == id)
                    .ok_or("Layer no longer exists")
            })
            .transpose()?;
        if index.is_some_and(|i| doc.layer(i).unwrap().kind() == "adjustment") {
            return Err("Native adjustment preview requires its prepared backdrop".into());
        }
        let mut rect = rect.unwrap_or([0, 0, doc.width() as i32, doc.height() as i32]);
        rect[0] = rect[0].clamp(0, doc.width() as i32 - 1);
        rect[1] = rect[1].clamp(0, doc.height() as i32 - 1);
        rect[2] = rect[2].clamp(rect[0] + 1, doc.width() as i32);
        rect[3] = rect[3].clamp(rect[1] + 1, doc.height() as i32);
        let rw = (rect[2] - rect[0]) as u32;
        let rh = (rect[3] - rect[1]) as u32;
        let (grid, width, height) = if doc.depth() == 8 {
            let scale = (edge.clamp(1, 8192) as f32 / rw.max(rh) as f32).min(1.);
            (
                Grid::Eight(scale),
                (rw as f32 * scale).round().max(1.) as u32,
                (rh as f32 * scale).round().max(1.) as u32,
            )
        } else {
            let scale = (edge.clamp(1, 8192) as f64 / rw.max(rh) as f64).min(1.);
            (
                Grid::Sixteen(scale),
                (rw as f64 * scale).round().max(1.) as u32,
                (rh as f64 * scale).round().max(1.) as u32,
            )
        };
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
            key: Key {
                source: scene.key(),
                identity: scene.identity().clone(),
                rect,
                width,
                height,
                target: target.map(str::to_owned),
                mask,
            },
            target: index,
            grid,
            pool: scene.budget().clone(),
            profile,
            _tags: tags,
        })
    }
    fn x(&self, x: u32) -> i32 {
        self.key.rect[0] + self.grid.coordinate(x)
    }
    fn y(&self, y: u32) -> i32 {
        self.key.rect[1] + self.grid.coordinate(y)
    }
    fn stripe(&self, start: u32) -> Result<(u32, Region)> {
        let left = self.x(0);
        let right = self.x(self.key.width - 1) + 1;
        let top = self.y(start);
        let rows = MAX_REGION_PIXELS / (right - left) as usize;
        let mut low = start + 1;
        let mut high = self.key.height.min(start.saturating_add(rows as u32));
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if (self.y(middle - 1) - top + 1) as usize <= rows {
                low = middle
            } else {
                high = middle - 1
            }
        }
        let region =
            Region::new([left, top, right, self.y(low - 1) + 1]).map_err(|e| e.to_string())?;
        Ok((low, region))
    }
    fn render(
        self: &Arc<Self>,
        scene: &SceneSnapshot,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Frame> {
        check()?;
        if scene.key() != self.key.source
            || scene.identity() != &self.key.identity
            || !scene.budget().same_pool(&self.pool)
        {
            return Err("Preview capture belongs to another source generation".into());
        }
        // Only the active worker creates CMS executors. Queued requests and
        // completed presentation tags retain no128MiB transform reservation.
        let display =
            DisplayPlan::prepare(scene.metadata().profile(), self.key.mask, &self.pool, check)?;
        if display.profile_binding() != self.profile {
            return Err("Preview profile differs from the captured source".into());
        }
        // Preview's target is raw. Export's isolated topology rejects clipped
        // roots and would reinterpret the legacy target preview contract.
        let topology = Topology::new(scene)?;
        let index_bytes = (self.key.height as usize)
            .checked_mul(std::mem::size_of::<Stripe>())
            .and_then(|n| n.checked_add(128))
            .ok_or("Preview stripe index overflow")?;
        let index = self
            .pool
            .reserve_bytes(index_bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut stripes = Vec::new();
        stripes
            .try_reserve_exact(self.key.height as usize)
            .map_err(|_| "Preview stripe index allocation refused")?;
        let mut y = 0;
        while y < self.key.height {
            check()?;
            let (end, region) = self.stripe(y)?;
            let mut stopped = None;
            let prepared = {
                let mut io_check = || {
                    check().map_err(|reason| {
                        stopped = Some(reason);
                        Error::Cancelled
                    })
                };
                RawLayers::for_topology(&topology, region, &mut io_check)
            };
            if let Some(reason) = stopped {
                return Err(reason);
            }
            let prepared = prepared?;
            check()?;
            let length = (self.key.width as usize)
                .checked_mul((end - y) as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or("Preview stripe dimensions overflow")?;
            let mut output = ScratchBytes::new(&self.pool, length, MAX_REGION_PIXELS * 4)
                .map_err(|e| e.to_string())?;
            if scene.metadata().depth() == 8 {
                let plan = Plan::<Native8, _>::with_topology(topology.clone(), &prepared, region)?;
                let row_length = self.key.width as usize * 4;
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
                for oy in y..end {
                    check()?;
                    for x in 0..self.key.width {
                        let sx = self.x(x);
                        let sy = self.y(oy);
                        let pixel = if let Some(target) = self.target {
                            if self.key.mask {
                                let gray = (plan.mask(target, sx, sy, true)? * 255.) as u8;
                                [gray, gray, gray, 255]
                            } else {
                                plan.layer(target, sx, sy)?
                            }
                        } else {
                            plan.pixel(sx, sy)?
                        };
                        let at = (x * 4) as usize;
                        if let Some((raw, _)) = &mut rows {
                            raw[at..at + 4].copy_from_slice(&pixel);
                        } else {
                            let at = (oy - y) as usize * row_length + at;
                            output[at..at + 4].copy_from_slice(&pixel);
                        }
                    }
                    if let Some((raw, converted)) = &mut rows {
                        display.convert8(raw, converted, check)?;
                        let at = (oy - y) as usize * row_length;
                        output[at..at + row_length].copy_from_slice(converted);
                    }
                }
            } else {
                let plan = Plan::<Native16, _>::with_topology(topology.clone(), &prepared, region)?;
                let row_length = self.key.width as usize * 4;
                let mut rows = if display.status() == DisplayStatus::Converted {
                    Some((
                        ChargedWords::new(row_length, &self.pool)?,
                        ChargedWords::new(row_length, &self.pool)?,
                    ))
                } else {
                    None
                };
                for oy in y..end {
                    check()?;
                    for x in 0..self.key.width {
                        let sx = self.x(x);
                        let sy = self.y(oy);
                        let pixel = if let Some(target) = self.target {
                            if self.key.mask {
                                let gray =
                                    (plan.mask(target, sx, sy, true)? * 65535.).round() as u16;
                                [gray, gray, gray, 65535]
                            } else {
                                plan.layer(target, sx, sy)?
                            }
                        } else {
                            plan.pixel(sx, sy)?
                        };
                        let at = (x * 4) as usize;
                        if let Some((raw, _)) = &mut rows {
                            raw[at..at + 4].copy_from_slice(&pixel);
                        } else {
                            let at = (oy - y) as usize * row_length + at;
                            output[at..at + 4]
                                .copy_from_slice(&crate::depth16::display_pixel(pixel));
                        }
                    }
                    if let Some((raw, converted)) = &mut rows {
                        display.convert16(raw, converted, check)?;
                        for (x, pixel) in converted.chunks_exact(4).enumerate() {
                            let at = (oy - y) as usize * row_length + x * 4;
                            output[at..at + 4].copy_from_slice(&crate::depth16::display_pixel(
                                pixel.try_into().unwrap(),
                            ));
                        }
                    }
                }
            }
            check()?;
            stripes.push(Stripe {
                rows: [y, end],
                bytes: output.freeze(),
            });
            y = end;
        }
        check()?;
        Ok(Frame {
            stripes,
            prepared: self.clone(),
            display_status: display.status(),
            _index: index,
        })
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
pub(crate) struct Frame {
    stripes: Vec<Stripe>,
    prepared: Arc<Prepared>,
    display_status: DisplayStatus,
    _index: Reservation,
}
impl Frame {
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
    Arc::new(Prepared::capture(scene, rect, edge, target, mask, check)?).render(scene, check)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Document, Layer};
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
        Arc::new(Prepared::capture(scene, rect, edge, target, mask, &mut || Ok(())).unwrap())
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
                let (end, region) = prepared.stripe(next).unwrap();
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
    fn unsupported_derived_providers_are_explicit_refusals_not_blank_frames() {
        for variant in 0..2 {
            let mut doc = document(16, 9, 7);
            match variant {
                0 => {
                    doc.layers[0].mask = Some(crate::engine::Mask {
                        enabled: false,
                        linked: true,
                        cache_key: crate::engine::id(),
                        steps: vec![],
                    })
                }
                _ => doc.layers[0].effects.push(crate::effects::Effect {
                    id: crate::engine::id(),
                    kind: "invert".into(),
                    enabled: true,
                    weight: 1.,
                    settings: Arc::new(serde_json::json!({})),
                }),
            }
            let (scene, path) = capture(&doc);
            assert!(Prepared::capture(
                &scene,
                None,
                48,
                Some(&doc.layers[0].id),
                true,
                &mut || Ok(())
            )
            .is_err());
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
        doc.icc_profile = Some(Arc::new(
            moxcms::ColorProfile::new_display_p3().encode().unwrap(),
        ));
        let (scene, path) = profile_scene(&doc);
        let prepared = prepare(&scene, None, 48, None, false);
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
        drop(owner);
        let reason = prepared
            .render(&scene, &mut || Err("profile source lease ended".into()))
            .err()
            .unwrap();
        assert_eq!(reason, "profile source lease ended");
        assert_eq!(scene.budget().used(), baseline);
        let mut wrong = Prepared::capture(&scene, None, 48, None, false, &mut || Ok(())).unwrap();
        wrong.profile.as_mut().unwrap()[0] ^= 1;
        let wrong = Arc::new(wrong);
        let with_tag = scene.budget().used();
        assert!(wrong.render(&scene, &mut || Ok(())).is_err());
        assert_eq!(scene.budget().used(), with_tag);
        drop(wrong);
        drop(prepared);
        drop(scene);
        crate::compositor::native::tests::cleanup(&path);
        assert_eq!(pool.used(), 0);
    }
}
