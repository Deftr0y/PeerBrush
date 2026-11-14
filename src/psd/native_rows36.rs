//! Actual bounded standard-channel rows from one immutable captured scene.
//! This provider grants no save, source-adoption or destination authority.
use super::{
    bulk36::Digest,
    scene_save36::{self, CapturedRows},
    standard36::{NativeRows, RowRequest, RowSource},
};
use crate::{
    compositor::native::{LayerWindow, Native16, Native8, Plan, Stored, Topology},
    engine::{native::LayerMetadata, scene::SceneSnapshot},
    large_document::{Error, PinnedTile, Reservation, ResidentBudget, ScratchBytes},
    production_export::{native_rows, Format, Output, Resampling},
    raster::{backed::Region, Surface, SurfaceKey, TILE},
    workspace::source_runtime::SourceSession,
};

type Result<T> = std::result::Result<T, String>;
const STRIPE_PIXELS: usize = 128 * 1024;
const MERGED_PASS_BYTES: usize = 16 * 1024 * 1024;

/// Run a Photoshop row operation with the same full-layer effect preparation
/// as native rendering. If cleanup fails, explicitly retire any produced save
/// candidate before refusing publication.
pub(crate) fn with_styles<T>(
    scene: &SceneSnapshot,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
    work: impl FnOnce(&mut Rows36<'_>) -> Result<T>,
    discard: impl FnOnce(T) -> Result<()>,
) -> Result<T> {
    check()?;
    let topology = Topology::new(scene)?;
    if !Stored::composite_needed(&topology, None) {
        let mut rows = Rows36::new(scene)?;
        rows.session = Some(session);
        return work(&mut rows);
    }
    let stored = Stored::prepare_composite(topology, session, None, &mut || check())?;
    let result = Rows36::with_prepared(scene, &stored).and_then(|mut rows| {
        rows.session = Some(session);
        work(&mut rows)
    });
    let Err(cleanup) = stored.retire(&mut || session.cleanup_check()) else {
        return result;
    };
    match result {
        Err(reason) => Err(format!("{reason}; Photoshop effect staging retained: {cleanup}")),
        Ok(value) => match discard(value) {
            Ok(()) => Err(format!("Photoshop effect staging retained: {cleanup}")),
            Err(candidate) => Err(format!(
                "Photoshop effect staging retained: {cleanup}; candidate cleanup retained: {candidate}"
            )),
        },
    }
}

pub(crate) struct Rows36<'a> {
    scene: &'a SceneSnapshot,
    provenance: Digest,
    // Construct only on the first current-composite request. Unsupported
    // derived work cannot prevent inspection of explicit raw source channels.
    composite: Option<native_rows::Rows<'a>>,
    merged_pass: Option<ScratchBytes>,
    stored: Option<&'a Stored<'a>>,
    paint_rows: Option<[i32; 2]>,
    single_paint: Option<LayerMetadata<'a>>,
    session: Option<&'a SourceSession>,
    pinned_rows: Option<PinnedRows>,
}
impl<'a> Rows36<'a> {
    pub(crate) fn new(scene: &'a SceneSnapshot) -> Result<Self> {
        let doc = scene.metadata();
        if doc.read_only()
            || doc.profile().is_some()
            || ![8, 16].contains(&doc.depth())
            || !(1..=16000).contains(&doc.width())
            || !(1..=16000).contains(&doc.height())
        {
            return Err("Native standard rows require a supported editable RGB scene".into());
        }
        let mut layers = doc.layers();
        let single_paint = layers.next().filter(|layer| {
            // With this exact topology the normal compositor returns the
            // straight paint samples where alpha is nonzero. The standard
            // PSB white matte also defines the color where alpha is zero.
            layers.next().is_none()
                && doc.filters().len() == 0
                && layer.kind() == "paint"
                && layer.blend() == "normal"
                && layer.visible()
                && layer.opacity() == 1.
                && crate::psd::fill_opacity::value(layer.psd_metadata()) == 255
                && crate::psd::blend_ranges::ranges(layer.psd_metadata()).is_none()
                && crate::psd::blend_channels::excluded(layer.psd_metadata()) == 0
                && layer.origin() == [0, 0]
                && layer.pixels().width() == doc.width()
                && layer.pixels().height() == doc.height()
                && layer.parent().is_none()
                && layer.clip_to().is_none()
                && layer.mask().is_none()
                && layer.effects().len() == 0
                && doc.width() as usize * TILE as usize > STRIPE_PIXELS
        });
        Ok(Self {
            scene,
            provenance: scene_save36::provenance(scene),
            composite: None,
            merged_pass: None,
            stored: None,
            paint_rows: paint_rows(scene),
            single_paint,
            session: None,
            pinned_rows: None,
        })
    }
    pub(crate) fn with_prepared(scene: &'a SceneSnapshot, stored: &'a Stored<'a>) -> Result<Self> {
        let mut rows = Self::new(scene)?;
        let topology = Topology::new(scene)?;
        stored.check_tree(&topology)?;
        rows.stored = Some(stored);
        Ok(rows)
    }
    fn validate(&self, request: RowRequest, output: &[u8]) -> Result<Stripe> {
        let width = i64::from(request.bounds[2]) - i64::from(request.bounds[0]);
        let height = i64::from(request.bounds[3]) - i64::from(request.bounds[1]);
        let end = request.first_row.checked_add(request.rows);
        // PSD layer alpha uses channel -1; the merged planar image uses 3.
        let channel = if request.source == RowSource::MergedWhiteMatte {
            (0..=3).contains(&request.channel)
        } else {
            [-2, -1, 0, 1, 2].contains(&request.channel)
        };
        if request.provenance != self.provenance
            || request.depth != self.scene.metadata().depth()
            || !channel
            || !(1..=16000).contains(&width)
            || !(1..=16000).contains(&height)
            || request.bounds.iter().any(|v| v.unsigned_abs() > 116000)
            || request.rows == 0
            || request.rows > 256
            || end.is_none_or(|n| i64::from(n) > height)
        {
            return Err("Native standard row request differs from its captured frame".into());
        }
        let pixels = (width as usize)
            .checked_mul(request.rows as usize)
            .filter(|n| *n <= STRIPE_PIXELS)
            .ok_or("Native standard stripe exceeds 128 KiPixel admission")?;
        let bytes = pixels
            .checked_mul(usize::from(request.depth / 8))
            .ok_or("Native standard channel stripe size overflow")?;
        if output.len() != bytes {
            return Err("Native standard channel buffer has the wrong exact length".into());
        }
        let top = i64::from(request.bounds[1]) + i64::from(request.first_row);
        Ok(Stripe {
            width: width as usize,
            pixels,
            bounds: [
                request.bounds[0],
                top.try_into().map_err(|_| "Native stripe top overflow")?,
                request.bounds[2],
                (top + i64::from(request.rows))
                    .try_into()
                    .map_err(|_| "Native stripe bottom overflow")?,
            ],
        })
    }
    fn layer(&self, index: usize) -> Result<LayerMetadata<'a>> {
        self.scene
            .metadata()
            .layer(index)
            .ok_or_else(|| "Native standard row layer is missing".into())
    }
    fn raw(
        &mut self,
        request: RowRequest,
        stripe: Stripe,
        layer: LayerMetadata<'a>,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        if !["paint", "fill", "group", "adjustment"].contains(&layer.kind()) {
            return Err("Native standard layer kind requires explicit source support".into());
        }
        let before_density = matches!(request.source, RowSource::MaskBeforeDensity { .. });
        let view = crate::engine::view::LayerView::Native(layer);
        let prefix = if before_density {
            if request.channel != -2 || super::standard_mask_density(view).is_none() {
                return Err("Native mask density request differs from its captured stack".into());
            }
            Some(view.mask().expect("Density requires a mask").step_count() - 1)
        } else {
            None
        };
        let frame = scene_save36::layer_frame(
            self.scene.metadata(),
            layer,
            matches!(request.source, RowSource::OriginalLayer(_) | RowSource::MaskBeforeDensity { originals: true, .. }),
        )?;
        let retained = if request.bounds == frame {
            None
        } else if request.channel == -2 {
            let mask = super::extended_mask(view, frame, before_density)
                .filter(|mask| mask.bounds == request.bounds)
                .ok_or("Native mask channel request differs from its planned source frame")?;
            let super::MaskOriginal::Native(original) = mask.original else {
                return Err("Native standard mask has no backed original".into());
            };
            let source = self.scene.raster(original.slot())?;
            if source.depth() != request.depth
                || !source.uses_budget(self.scene.budget())
                || !source.uses_disk_budget(self.scene.disk_budget())
            {
                return Err("Native original mask differs from the captured admission".into());
            }
            let [ox, oy] = mask.origin;
            let [l, t, r, b] = stripe.bounds;
            let region =
                Region::new([l - ox, t - oy, r - ox, b - oy]).map_err(|e| e.to_string())?;
            let mut failed_check = None;
            let prepared = source
                .prepare_region(region, &mut || {
                    check().map_err(|reason| {
                        failed_check = Some(reason);
                        Error::Cancelled
                    })
                })
                .map_err(|e| failed_check.unwrap_or_else(|| e.to_string()))?;
            Some((mask, prepared))
        } else {
            return Err(
                "Native layer channel request differs from its planned source frame".into(),
            );
        };
        if request.channel == -2 {
            let mask =
                crate::mask::region::prepare_scene_prefix(self.scene, layer, stripe.bounds, prefix, &mut || {
                    check()
                })?;
            let [left, top, _, _] = mask.bounds();
            let stride = usize::from(request.depth / 8);
            for (row, bytes) in output.chunks_exact_mut(stripe.width * stride).enumerate() {
                check()?;
                let y = top + row as i32;
                for (x, sample) in bytes.chunks_exact_mut(stride).enumerate() {
                    let x = left + x as i32;
                    let value = match &retained {
                        Some((original, pixels)) if !super::in_mask_frame(frame, x, y) => {
                            let [ox, oy] = original.origin;
                            let pixel =
                                pixels.pixel16(x - ox, y - oy).map_err(|e| e.to_string())?;
                            super::original_mask_sample(original.default, pixel, request.depth)
                        }
                        _ if request.depth == 16 => (mask.value16(x, y)? * 65535.).round() as u16,
                        _ => (mask.value8(x, y)? * 255.).round() as u16,
                    };
                    if request.depth == 16 {
                        sample.copy_from_slice(&value.to_be_bytes());
                    } else {
                        sample[0] = value as u8;
                    }
                }
            }
            return check();
        }
        let source = self.scene.raster(layer.pixels().slot())?;
        if !source.uses_budget(self.scene.budget())
            || !source.uses_disk_budget(self.scene.disk_budget())
            || source.depth() != request.depth
        {
            return Err("Native standard source differs from the captured global admission".into());
        }
        if let RowSource::Layer(index) = request.source {
            if ["group", "adjustment"].contains(&layer.kind())
                || layer.effects().any(|e| e.enabled() && e.weight() > 0.)
            {
                return self.colour(request, stripe, index, output, check);
            }
        }
        // Legacy standard fill channels contain the procedural colour, including
        // hidden RGB; a fill's stored raster is not a substitute for that colour.
        if layer.kind() == "fill" {
            let pixel = if request.depth == 16 {
                layer.color().map(|v| u16::from(v) * 257)
            } else {
                layer.color().map(u16::from)
            };
            return write_channel(request, stripe, output, check, |_, _| Ok(pixel));
        }
        if source.is_known_zero() {
            output.fill(0);
            return check();
        }
        let [ox, oy] = layer.origin();
        let local = [
            stripe.bounds[0].checked_sub(ox),
            stripe.bounds[1].checked_sub(oy),
            stripe.bounds[2].checked_sub(ox),
            stripe.bounds[3].checked_sub(oy),
        ];
        let [Some(l), Some(t), Some(r), Some(b)] = local else {
            return Err("Native standard local source coordinates overflow".into());
        };
        let region = Region::new([l, t, r, b]).map_err(|e| e.to_string())?;
        if !source.has_physical_tiles(region.bounds()) {
            output.fill(0);
            return check();
        }
        if l == 0
            && r == source.width() as i32
            && 0 <= t
            && b <= source.height() as i32
            && source.width() as usize * TILE as usize > STRIPE_PIXELS
        {
            if self.write_pinned_rows(source, t as u32, request, output, check)? {
                return Ok(());
            }
            self.pinned_rows = None;
        }
        let mut failed_check = None;
        let prepared = source
            .prepare_region(region, &mut || {
                check().map_err(|reason| {
                    failed_check = Some(reason);
                    Error::Cancelled
                })
            })
            .map_err(|e| failed_check.unwrap_or_else(|| e.to_string()))?;
        check()?;
        // From this point sampling is memory-only; source I/O has completed.
        write_channel(request, stripe, output, check, |x, y| {
            if request.depth == 16 {
                prepared.pixel16(x - ox, y - oy).map_err(|e| e.to_string())
            } else {
                prepared
                    .pixel(x - ox, y - oy)
                    .map(|p| p.map(u16::from))
                    .map_err(|e| e.to_string())
            }
        })
    }
    fn write_pinned_rows(
        &mut self,
        source: &Surface,
        top: u32,
        request: RowRequest,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<bool> {
        let sample = usize::from(source.depth() / 8);
        let channel = if request.channel == -1 {
            3
        } else {
            request.channel as usize
        };
        let matte = request.source == RowSource::MergedWhiteMatte && channel < 3;
        let maximum = if sample == 2 { 65535 } else { 255 };
        self.with_pinned_rows(source, top, sample, output, check, |source, output| {
            if let Some(source) = source {
                for (pixel, out) in source.chunks_exact(sample * 4).zip(output.chunks_exact_mut(sample)) {
                    let value = &pixel[channel * sample..(channel + 1) * sample];
                    let alpha = &pixel[3 * sample..];
                    if matte && alpha.iter().any(|&byte| byte != 255) {
                        let value = white_matte_sample(value, alpha, maximum);
                        out.copy_from_slice(&value.to_be_bytes()[2 - sample..]);
                    } else {
                        out.copy_from_slice(value);
                    }
                }
            } else {
                output.fill(if matte { 255 } else { 0 });
            }
        })
    }

    fn with_pinned_rows(
        &mut self,
        source: &Surface,
        top: u32,
        pixel_bytes: usize,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
        mut copy: impl FnMut(Option<&[u8]>, &mut [u8]),
    ) -> Result<bool> {
        let source_bytes = usize::from(source.depth() / 8) * 4;
        let session = self.session;
        let active = || match session {
            Some(session) => session.check_pinned_work(),
            None => check(),
        };
        let width = source.width() as usize;
        for (row, output) in output.chunks_exact_mut(width * pixel_bytes).enumerate() {
            active()?;
            let y = top + row as u32;
            let tile_y = y / TILE;
            if self.pinned_rows.as_ref().is_none_or(|p| p.key != source.key() || p.y != tile_y) {
                self.pinned_rows = None;
                let Some(pins) = PinnedRows::load(source, tile_y, self.scene.budget(), &active, check)?
                else {
                    return Ok(false);
                };
                self.pinned_rows = Some(pins);
            }
            let row_start = (y % TILE * TILE) as usize * source_bytes;
            let tiles = &self.pinned_rows.as_ref().expect("Prepared Photoshop tile row").tiles;
            for (tile, dest) in tiles.iter().zip(output.chunks_mut(TILE as usize * pixel_bytes)) {
                let end = row_start + dest.len() / pixel_bytes * source_bytes;
                copy(tile.as_ref().map(|tile| &tile.bytes()[row_start..end]), dest);
            }
        }
        check()?;
        Ok(true)
    }
    fn colour(
        &self,
        request: RowRequest,
        stripe: Stripe,
        index: usize,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let region = Region::new(stripe.bounds).map_err(|e| e.to_string())?;
        // Standard layer channels contain raw derived colour. Export isolation
        // applies layer opacity/masks and would lose their independent channels.
        let topology = Topology::new(self.scene)?;
        let prepared = LayerWindow::prepare(&topology, region, self.stored, &mut || check())?;
        if request.depth == 8 {
            let plan = Plan::<Native8, _>::with_topology(topology, &prepared, region)?;
            write_channel(request, stripe, output, check, |x, y| {
                Ok(plan.layer(index, x, y)?.map(u16::from))
            })
        } else {
            let plan = Plan::<Native16, _>::with_topology(topology, &prepared, region)?;
            write_channel(request, stripe, output, check, |x, y| {
                plan.layer(index, x, y)
            })
        }
    }
    fn canvas(&self) -> [i32; 4] {
        [
            0,
            0,
            self.scene.metadata().width() as i32,
            self.scene.metadata().height() as i32,
        ]
    }
    fn composite(
        &mut self,
        request: RowRequest,
        stripe: Stripe,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        if request.bounds != self.canvas() || request.channel == -2 {
            return Err("Native composite request differs from its full captured canvas".into());
        }
        let matte = request.source == RowSource::MergedWhiteMatte;
        if self.empty_composite_stripe(stripe.bounds[1], stripe.bounds[3]) {
            output.fill(if matte && request.channel < 3 { 255 } else { 0 });
            return check();
        }
        if matte {
            if let Some(layer) = self.single_paint {
                let source = self.scene.raster(layer.pixels().slot())?;
                if self.write_pinned_rows(source, request.first_row, request, output, check)? {
                    return Ok(());
                }
                self.pinned_rows = None;
            }
        }
        let sample = usize::from(request.depth / 8);
        let bytes = stripe.pixels * sample * 4;
        let fallback;
        let (rgba, apply_matte) = if let Some(cached) = self.merged_pass.as_ref().filter(|_| matte) {
            let start = request.first_row as usize * stripe.width * sample * 4;
            (&cached[start..start + bytes], false)
        } else {
            let mut stripe = ScratchBytes::new(self.scene.budget(), bytes, STRIPE_PIXELS * 8)
                .map_err(|e| e.to_string())?;
            self.fill_composite_rgba(request.first_row, request.rows, false, &mut stripe, check)?;
            fallback = stripe;
            (&fallback[..], matte)
        };
        let channel = if request.channel == -1 { 3 } else { request.channel as usize };
        let maximum = if sample == 2 { 65535 } else { 255 };
        for (row, out) in rgba.chunks_exact(stripe.width * sample * 4)
            .zip(output.chunks_exact_mut(stripe.width * sample))
        {
            check()?;
            for (pixel, out) in row.chunks_exact(sample * 4).zip(out.chunks_exact_mut(sample)) {
                let value = &pixel[channel * sample..(channel + 1) * sample];
                if apply_matte && channel < 3 {
                    let value = white_matte_sample(value, &pixel[3 * sample..], maximum);
                    out.copy_from_slice(&value.to_be_bytes()[2 - sample..]);
                } else {
                    out.copy_from_slice(value);
                }
            }
        }
        check()
    }

    fn empty_composite_stripe(&self, first: i32, end: i32) -> bool {
        self.paint_rows.is_some_and(|[top, bottom]| top >= bottom || end <= top || first >= bottom)
            || self.composite.as_ref().is_some_and(|rows| {
                rows.region_is_transparent([0, first, self.scene.metadata().width() as i32, end])
            })
    }

    pub(super) fn fill_composite_rgba(
        &mut self,
        first: u32,
        rows: u32,
        white_matte: bool,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        check()?;
        let doc = self.scene.metadata();
        let sample = usize::from(doc.depth() / 8);
        let width = doc.width() as usize;
        let end = first.checked_add(rows).filter(|&end| end <= doc.height())
            .ok_or("Native composite stripe exceeds its canvas")?;
        if !(1..=256).contains(&rows) || width * rows as usize > STRIPE_PIXELS
            || output.len() != width * rows as usize * sample * 4
        {
            return Err("Native RGBA composite stripe differs from admission".into());
        }
        if self.empty_composite_stripe(first as i32, end as i32) {
            if white_matte {
                fill_transparent_matte(output, sample);
            } else {
                output.fill(0);
            }
            return check();
        }
        if let Some(layer) = self.single_paint.filter(|_| white_matte) {
            let source = self.scene.raster(layer.pixels().slot())?;
            if self.with_pinned_rows(source, first, sample * 4, output, check, |source, output| {
                if let Some(source) = source {
                    output.copy_from_slice(source);
                    write_white_matte(output, sample);
                } else {
                    fill_transparent_matte(output, sample);
                }
            })? {
                return Ok(());
            }
            self.pinned_rows = None;
        }
        {
            if self.composite.is_none() {
                self.composite = Some(if let Some(stored) = self.stored {
                    native_rows::Rows::with_prepared(self.scene, None, stored, check)?
                } else {
                    native_rows::Rows::new(self.scene, check)?
                });
            }
            let renderer = self.composite.as_ref().ok_or("Missing captured composite provider")?;
            if !std::ptr::eq(renderer.scene(), self.scene) {
                return Err("Native composite provider belongs to another scene".into());
            }
            let render = Output {
                target: None,
                target_name: String::new(),
                variant: String::new(),
                filename: String::new(),
                document_rect: self.canvas(),
                width: doc.width(),
                height: doc.height(),
                format: Format::Png,
                native_source_depth: doc.depth(),
                bit_depth: doc.depth(),
                scale_xy: [1., 1.],
            };
            renderer.fill(&render, doc.depth(), Resampling::Nearest, first, rows, output, check)?;
        }
        if white_matte {
            for row in output.chunks_exact_mut(width * sample * 4) {
                check()?;
                write_white_matte(row, sample);
            }
        }
        check()
    }
}

/// Cache only the source tile row shared by successive bounded PSD stripes.
/// The original resident quota owns every pin, including a partial final row.
struct PinnedRows {
    key: SurfaceKey,
    y: u32,
    tiles: Vec<Option<PinnedTile>>,
    _index: Reservation,
}
impl PinnedRows {
    fn load(
        source: &Surface,
        y: u32,
        budget: &ResidentBudget,
        active: &dyn Fn() -> Result<()>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Option<Self>> {
        let count = source.width().div_ceil(TILE) as usize;
        let size = count * std::mem::size_of::<Option<PinnedTile>>();
        let Ok(index) = budget.reserve_bytes(size, size) else {
            return Ok(None);
        };
        let mut tiles = Vec::new();
        tiles
            .try_reserve_exact(count)
            .map_err(|_| "Native Photoshop tile row allocation refused")?;
        if tiles.capacity() > count {
            return Err("Native Photoshop tile row exceeds index admission".into());
        }
        tiles.resize_with(count, || None);
        let failure = std::cell::RefCell::new(None);
        let guarded = |result: Result<()>| result.map_err(|reason| {
            *failure.borrow_mut() = Some(reason);
            Error::Cancelled
        });
        check()?;
        match source.pin_row(0, y, &mut tiles, &mut || guarded(active()), &mut || guarded(check())) {
            Ok(()) => {}
            Err(Error::Budget) => return Ok(None),
            Err(error) => return Err(failure.into_inner().unwrap_or_else(|| error.to_string())),
        }
        check()?;
        Ok(Some(Self {
            key: source.key(),
            y,
            tiles,
            _index: index,
        }))
    }
}

fn white_matte_sample(value: &[u8], alpha: &[u8], maximum: u64) -> u16 {
    let word = |bytes: &[u8]| if maximum == 65535 {
        u64::from(u16::from_be_bytes([bytes[0], bytes[1]]))
    } else {
        u64::from(bytes[0])
    };
    let alpha = word(alpha);
    ((word(value) * alpha + maximum * (maximum - alpha) + maximum / 2) / maximum) as u16
}

fn write_white_matte(rgba: &mut [u8], sample: usize) {
    let maximum = if sample == 2 { 65535 } else { 255 };
    for pixel in rgba.chunks_exact_mut(sample * 4) {
        let (rgb, alpha) = pixel.split_at_mut(3 * sample);
        if alpha.iter().any(|&byte| byte != 255) {
            for channel in rgb.chunks_exact_mut(sample) {
                let value = white_matte_sample(channel, alpha, maximum);
                channel.copy_from_slice(&value.to_be_bytes()[2 - sample..]);
            }
        }
    }
}

fn fill_transparent_matte(rgba: &mut [u8], sample: usize) {
    rgba.fill(255);
    for pixel in rgba.chunks_exact_mut(sample * 4) {
        pixel[3 * sample..].fill(0);
    }
}

/// Only plain paint without derived or backdrop-dependent inputs can prove
/// transparency outside the occupied source tile rows without rendering.
fn paint_rows(scene: &SceneSnapshot) -> Option<[i32; 2]> {
    let doc = scene.metadata();
    if doc.filters().len() != 0 {
        return None;
    }
    let mut support = [i32::MAX, i32::MIN];
    let tile_edge = crate::raster::TILE as i32;
    for layer in doc.layers() {
        if layer.kind() != "paint"
            || layer.blend() != "normal"
            || layer.parent().is_some()
            || layer.clip_to().is_some()
            || layer.effects().len() != 0
            || layer.mask().is_some()
        {
            return None;
        }
        let surface = scene.raster(layer.pixels().slot()).ok()?;
        let y = layer.origin()[1];
        for tile in surface.physical_tiles() {
            let top = y.checked_add((tile.coordinate().1 as i32).checked_mul(tile_edge)?)?;
            support[0] = support[0].min(top);
            support[1] = support[1].max(top.checked_add(tile_edge)?);
        }
    }
    Some(support)
}

#[derive(Clone, Copy)]
struct Stripe {
    width: usize,
    pixels: usize,
    bounds: [i32; 4],
}
fn write_channel(
    request: RowRequest,
    stripe: Stripe,
    output: &mut [u8],
    check: &dyn Fn() -> Result<()>,
    mut sample: impl FnMut(i32, i32) -> Result<[u16; 4]>,
) -> Result<()> {
    let channel = if request.channel == -1 {
        3
    } else {
        request.channel as usize
    };
    if channel > 3 {
        return Err("Native standard colour channel is invalid".into());
    }
    let stride = usize::from(request.depth / 8);
    for row in 0..request.rows as usize {
        check()?;
        for column in 0..stripe.width {
            let value = sample(
                stripe.bounds[0] + column as i32,
                stripe.bounds[1] + row as i32,
            )?[channel];
            let at = (row * stripe.width + column) * stride;
            if request.depth == 16 {
                output[at..at + 2].copy_from_slice(&value.to_be_bytes());
            } else {
                output[at] = value
                    .try_into()
                    .map_err(|_| "Native8 standard sample exceeds its precision")?;
            }
        }
    }
    check()
}
impl CapturedRows for Rows36<'_> {
    fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
}
impl NativeRows for Rows36<'_> {
    fn provenance(&self) -> Digest {
        self.provenance
    }
    fn prepare_merged_pass(&mut self, check: &dyn Fn() -> Result<()>) -> Result<()> {
        self.pinned_rows = None;
        self.merged_pass = None;
        check()?;
        let doc = self.scene.metadata();
        let (width, height) = (doc.width() as usize, doc.height() as usize);
        if self.empty_composite_stripe(0, height as i32) {
            return Ok(());
        }
        let row_bytes = width * usize::from(doc.depth() / 2);
        let bytes = row_bytes * height;
        let pool = self.scene.budget();
        // Leave the existing stripe renderer and source pins most of the pool.
        let allowance = MERGED_PASS_BYTES.min(pool.limit().saturating_sub(pool.used()) / 8);
        let Ok(mut frame) = ScratchBytes::new(pool, bytes, allowance) else { return Ok(()); };
        let stripe = (STRIPE_PIXELS / width).min(256);
        for first in (0..height).step_by(stripe) {
            let rows = stripe.min(height - first);
            let output = &mut frame[first * row_bytes..(first + rows) * row_bytes];
            if self.fill_composite_rgba(first as u32, rows as u32, true, output, check).is_err() {
                // If the optional buffer leaves too little render space, the
                // normal guarded stripe pass still owns the result and errors.
                return check();
            }
        }
        self.merged_pass = Some(frame);
        Ok(())
    }
    fn finish_merged_pass(&mut self) {
        self.merged_pass = None;
        self.pinned_rows = None;
    }
    fn fill(
        &mut self,
        request: RowRequest,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        check()?;
        let stripe = self.validate(request, output)?;
        match request.source {
            RowSource::Layer(index) | RowSource::OriginalLayer(index) | RowSource::MaskBeforeDensity { layer: index, .. } => {
                let layer = self.layer(index)?;
                self.raw(request, stripe, layer, output, check)
            }
            RowSource::BakedComposite | RowSource::MergedWhiteMatte => {
                self.composite(request, stripe, output, check)
            }
            RowSource::Zero if request.channel != -2 => {
                output.fill(0);
                check()
            }
            RowSource::Zero => Err("Explicit zero records do not have a mask channel".into()),
        }
    }
}

#[cfg(test)]
#[path = "native_rows36_tests.rs"]
mod tests;
