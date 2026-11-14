//! Derived layers share the raw source/mask provider and native
//! compositor. Preparation follows backdrop dependencies before publishing.
use super::*;
use crate::effects::{regional, Effect};
use std::{cell::RefCell, sync::Arc};

struct Colour {
    images: Vec<regional::RegionalImage>,
    origin: [i32; 2],
    size: [u32; 2],
    _index: Option<Reservation>,
}
impl Colour {
    fn sample<P: Precision>(&self, x: i32, y: i32) -> Result<P::Pixel> {
        let x = i64::from(x) - i64::from(self.origin[0]);
        let y = i64::from(y) - i64::from(self.origin[1]);
        if x < 0 || y < 0 || x >= i64::from(self.size[0]) || y >= i64::from(self.size[1]) {
            return Ok(P::zero());
        }
        let index = self
            .images
            .partition_point(|image| image.bounds()[3] <= y as i32);
        P::colour_sample(
            self.images
                .get(index)
                .ok_or("Derived sample has no prepared footprint")?,
            x as i32,
            y as i32,
        )
    }
}

struct RawStripe<'a> {
    bounds: [i32; 4],
    layers: RawLayers<'a>,
}
pub(crate) struct Layers<'a> {
    scene: &'a SceneSnapshot,
    scope: LayerScope<'a>,
    bounds: [i32; 4],
    raw: Vec<RawStripe<'a>>,
    colours: Vec<Option<Colour>>,
    _index: Reservation,
}

impl<'a> Layers<'a> {
    pub(crate) fn validate_layer(layer: LayerMetadata<'_>) -> Result<()> {
        for effect in layer.effects().filter(|e| e.enabled() && e.weight() > 0.) {
            if ![
                "levels",
                "curves",
                "adjust",
                "color_balance",
                "hsl",
                "invert",
                "grayscale",
                "posterize",
                "threshold",
                crate::effects::legacy_brightness::KIND,
                crate::effects::modern_brightness::KIND,
                crate::effects::exposure::KIND,
                crate::effects::channel_mixer::KIND,
                crate::effects::black_white::KIND,
                crate::effects::selective_color::KIND,
                crate::effects::vibrance::KIND,
                crate::effects::photo_filter::KIND,
                crate::effects::gradient_map::KIND,
                crate::effects::photoshop_color_balance::KIND,
                crate::effects::photoshop_hue_saturation::KIND,
                crate::effects::color_lookup::KIND,
                "channel_clamp",
                "blur",
                "bloom",
                "color_overlay",
                "gradient_overlay",
                "pattern_overlay",
                "texture",
                "contour",
                "stroke",
                "drop_shadow",
                "inner_shadow",
                "outer_glow",
                "inner_glow",
                "satin",
                "bevel_emboss",
            ]
            .contains(&effect.kind())
                || effect.clips().len() != 0
                || super::Stored::effect_needed(effect.kind(), effect.settings())
            {
                return Err(
                    "Native regional effects require prepared style fields, alpha bounds or Liquify sources".into(),
                );
            }
            if crate::effects::styles::contains(effect.kind())
                && !crate::effects::patterns::active(effect.kind(), effect.settings())
            {
                crate::effects::validate(effect.kind(), effect.settings())?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn for_topology(
        topology: &Arc<Topology<'a>>,
        region: Region,
        check: &mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> Result<Self> {
        Self::for_topology_with_checks(topology, region, &mut ReadChecks::caller_only(&mut || {
            check().map_err(|error| error.to_string())
        }))
    }

    pub(super) fn for_topology_with_checks(
        topology: &Arc<Topology<'a>>,
        region: Region,
        checks: &mut ReadChecks<'_>,
    ) -> Result<Self> {
        for (i, layer) in topology.layers.iter().enumerate() {
            if topology.active[i] {
                Self::validate_layer(*layer)?;
            }
        }
        let scene = topology.scene;
        // A conservative suffix halo lets each derived layer render once in
        // dependency order. Later backdrops sample retained earlier stripes,
        // rather than recursively rendering the lower tree for every effect.
        let mut remaining = 0i32;
        for (i, layer) in topology.layers.iter().enumerate() {
            if topology.active[i] {
                remaining = remaining
                    .checked_add(backdrop_reach(*layer)?)
                    .ok_or("Derived backdrop halo overflow")?;
            }
        }
        let bounds = expand(scene, region.bounds(), remaining);
        let stripes = stripe_count(bounds)?;
        let charge = scene
            .budget()
            .reserve_bytes(
                topology
                    .layers
                    .len()
                    .checked_mul(512)
                    .and_then(|n| n.checked_add(stripes.checked_mul(512)?))
                    .ok_or("Derived layer index overflow")?,
                4 * 1024 * 1024,
            )
            .map_err(|e| e.to_string())?;
        let mut colours = Vec::new();
        colours
            .try_reserve_exact(topology.layers.len())
            .map_err(|_| "Derived layer index allocation refused")?;
        colours.resize_with(topology.layers.len(), || None);
        let mut order = Vec::new();
        order
            .try_reserve_exact(topology.layers.len())
            .map_err(|_| "Derived layer order allocation refused")?;
        // Same bottom-up, children-first order as the resident compositor,
        // including dependencies across pass-through folder boundaries.
        fn visit(topology: &Topology<'_>, parent: Option<&str>, order: &mut Vec<usize>) {
            for (i, layer) in topology
                .layers
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, l)| l.parent() == parent)
            {
                if layer.kind() == "group" {
                    visit(topology, Some(layer.id()), order);
                }
                order.push(i);
            }
        }
        visit(topology, None, &mut order);
        let mut raw = Vec::new();
        raw.try_reserve_exact(stripes)
            .map_err(|_| "Derived raw stripe index allocation refused")?;
        for bounds in bands(bounds) {
            let region = Region::new(bounds).map_err(|e| e.to_string())?;
            raw.push(RawStripe {
                bounds,
                layers: RawLayers::prepare_inner(scene, Some(topology), region, true, checks)?,
            });
        }
        let mut output = Self {
            scene,
            scope: topology.scope(),
            bounds,
            raw,
            colours,
            _index: charge,
        };
        for i in order {
            checks.full()?;
            let layer = topology.layers[i];
            if !topology.active[i]
                || (layer.kind() != "adjustment"
                    && !layer.effects().any(|e| e.enabled() && e.weight() > 0.))
            {
                continue;
            }
            remaining -= backdrop_reach(layer)?;
            let requested = expand(scene, region.bounds(), remaining);
            let colour = output.prepare_colour(topology, requested, i, checks)?;
            output.colours[i] = Some(colour);
        }
        checks.full()?;
        // Only complete providers may leave preparation; pending effects never
        // become an implicit raw-source fallback in preview/export/save.
        if scene.metadata().depth() == 8 {
            Plan::<Native8, _>::with_topology(topology.clone(), &output, region)?;
        } else {
            Plan::<Native16, _>::with_topology(topology.clone(), &output, region)?;
        }
        Ok(output)
    }

    fn prepare_colour(
        &self,
        topology: &Arc<Topology<'a>>,
        requested: [i32; 4],
        i: usize,
        checks: &mut ReadChecks<'_>,
    ) -> Result<Colour> {
        let scene = topology.scene;
        let layer = topology.layers[i];
        let source_canvas = ["group", "adjustment"].contains(&layer.kind());
        let style = layer
            .effects()
            .filter(|e| e.enabled() && e.weight() > 0.)
            .position(|e| crate::effects::styles::contains(e.kind()));
        let canvas = source_canvas || style.is_some();
        let origin = if canvas { [0, 0] } else { layer.origin() };
        let size = if canvas {
            [scene.metadata().width(), scene.metadata().height()]
        } else {
            [layer.pixels().width(), layer.pixels().height()]
        };
        let b = requested;
        let local = [
            (i64::from(b[0]) - i64::from(origin[0])).max(0),
            (i64::from(b[1]) - i64::from(origin[1])).max(0),
            (i64::from(b[2]) - i64::from(origin[0])).min(i64::from(size[0])),
            (i64::from(b[3]) - i64::from(origin[1])).min(i64::from(size[1])),
        ];
        let mut result = Colour {
            images: Vec::new(),
            origin,
            size,
            _index: None,
        };
        let mut bytes = 65536;
        for effect in layer.effects().filter(|e| e.enabled() && e.weight() > 0.) {
            regional::metadata_bytes(effect.settings(), 0, &mut bytes)?;
        }
        let _metadata = scene
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let effects: Vec<_> = layer
            .effects()
            .filter(|e| e.enabled() && e.weight() > 0.)
            .map(|e| Effect {
                id: String::new(),
                kind: e.kind().into(),
                enabled: true,
                weight: e.weight(),
                settings: Arc::new(e.settings().clone()),
            })
            .collect();
        if local[0] >= local[2] || local[1] >= local[3] {
            // Validate even when the current stripe does not intersect a layer.
            for effect in &effects {
                crate::effects::validate_weight(effect.weight)?;
                crate::effects::normalized(&effect.kind, &effect.settings)?;
            }
            return Ok(result);
        }
        let local = local.map(|v| v as i32);
        // A cropped source expands exactly once, immediately before its first
        // style. Earlier blur/colour kernels keep their original layer edges.
        let split = if source_canvas {
            0
        } else {
            style.unwrap_or(effects.len())
        };
        let (prefix, suffix) = effects.split_at(split);
        let bitmaps = effects
            .iter()
            .map(|effect| {
                if !crate::effects::patterns::active(&effect.kind, &effect.settings) {
                    return Ok(None);
                }
                let source = effect.settings["bitmap_png"]
                    .as_str()
                    .ok_or("Bitmap pattern requires bitmap_png")?;
                let bitmap = crate::effects::patterns::PreparedBitmap::from_png(
                    source,
                    scene.budget(),
                    &mut || checks.pinned(),
                )?;
                Ok(Some(Arc::new(bitmap)))
            })
            .collect::<Result<Vec<_>>>()?;
        let count = stripe_count(local)?;
        result._index = Some(
            scene
                .budget()
                .reserve_bytes(
                    count
                        .checked_mul(256)
                        .ok_or("Derived colour stripe index overflow")?,
                    4 * 1024 * 1024,
                )
                .map_err(|e| e.to_string())?,
        );
        result
            .images
            .try_reserve_exact(count)
            .map_err(|_| "Derived colour stripe allocation refused")?;
        let active = if canvas { suffix } else { &effects };
        let split_band = has_halo(active)?;
        let checks = RefCell::new(checks);
        let mut render = |local, check: &mut dyn FnMut() -> Result<()>| {
            let plan = regional::Plan::new_prepared(
                regional::Frame::new(size[0], size[1])?,
                local,
                scene.metadata().depth(),
                active,
                scene.budget(),
                if canvas { &bitmaps[split..] } else { &bitmaps },
            )?;
            if !canvas {
                return self.source_colour(layer, &plan, effects.len(), &mut **checks.borrow_mut());
            }
            if !source_canvas {
                let input = self.source_prefix(
                    layer, plan.input_bounds(), prefix, &mut **checks.borrow_mut(),
                )?;
                return plan.render(
                    scene.budget(),
                    &vec![None; suffix.len()],
                    &vec![None; suffix.len()],
                    check,
                    |x, y| input.sample::<Native16>(x, y),
                );
            }
            let statistics = vec![None; effects.len()];
            let fields = vec![None; effects.len()];
            // This private partial plan is used only while visiting dependencies.
            // No full-composite sample can escape until every active stage is ready.
            if scene.metadata().depth() == 8 {
                let base = Plan::<Native8, Self> {
                    topology: topology.clone(),
                    prepared: self,
                    region: self.bounds,
                    _precision: std::marker::PhantomData,
                };
                plan.render(
                    scene.budget(),
                    &statistics,
                    &fields,
                    check,
                    |x, y| {
                        let (x, y) = (x + origin[0], y + origin[1]);
                        let p = if layer.kind() == "adjustment" {
                            base.adjustment_input(i, x, y)?
                        } else {
                            base.layer(i, x, y)?
                        };
                        Ok(p.map(|v| u16::from(v) * 257))
                    },
                )
            } else {
                let base = Plan::<Native16, Self> {
                    topology: topology.clone(),
                    prepared: self,
                    region: self.bounds,
                    _precision: std::marker::PhantomData,
                };
                plan.render(
                    scene.budget(),
                    &statistics,
                    &fields,
                    check,
                    |x, y| {
                        let (x, y) = (x + origin[0], y + origin[1]);
                        if layer.kind() == "adjustment" {
                            base.adjustment_input(i, x, y)
                        } else {
                            base.layer(i, x, y)
                        }
                    },
                )
            }
        };
        let mut poll = || checks.borrow_mut().pinned();
        for band in bands(local) {
            let band = Region::new(band).map_err(|e| e.to_string())?;
            let image = if split_band {
                regional::render_band(band, scene.metadata().depth(), scene.budget(), &mut poll, &mut render)?
            } else {
                render(band, &mut poll)?
            };
            result.images.push(image);
        }
        Ok(result)
    }

    fn source_prefix(
        &self,
        layer: LayerMetadata<'_>,
        requested: [i32; 4],
        effects: &[Effect],
        checks: &mut ReadChecks<'_>,
    ) -> Result<Colour> {
        let origin = layer.origin();
        let size = [layer.pixels().width(), layer.pixels().height()];
        let b = [
            (i64::from(requested[0]) - i64::from(origin[0])).max(0),
            (i64::from(requested[1]) - i64::from(origin[1])).max(0),
            (i64::from(requested[2]) - i64::from(origin[0])).min(i64::from(size[0])),
            (i64::from(requested[3]) - i64::from(origin[1])).min(i64::from(size[1])),
        ];
        let mut colour = Colour {
            images: Vec::new(),
            origin,
            size,
            _index: None,
        };
        if b[0] >= b[2] || b[1] >= b[3] {
            return Ok(colour);
        }
        let b = b.map(|v| v as i32);
        let count = stripe_count(b)?;
        colour._index = Some(
            self.scene
                .budget()
                .reserve_bytes(
                    count
                        .checked_mul(256)
                        .ok_or("Style source prefix index overflow")?,
                    4 * 1024 * 1024,
                )
                .map_err(|e| e.to_string())?,
        );
        colour
            .images
            .try_reserve_exact(count)
            .map_err(|_| "Style source prefix allocation refused")?;
        let split_band = has_halo(effects)?;
        let checks = RefCell::new(checks);
        let mut render = |band, _: &mut dyn FnMut() -> Result<()>| {
            let plan = regional::Plan::new_prepared(
                regional::Frame::new(size[0], size[1])?,
                band,
                self.scene.metadata().depth(),
                effects,
                self.scene.budget(),
                &vec![None; effects.len()],
            )?;
            self.source_colour(layer, &plan, effects.len(), &mut **checks.borrow_mut())
        };
        let mut poll = || checks.borrow_mut().pinned();
        for band in bands(b) {
            let band = Region::new(band).map_err(|e| e.to_string())?;
            let image = if split_band {
                regional::render_band(band, self.scene.metadata().depth(), self.scene.budget(), &mut poll, &mut render)?
            } else {
                render(band, &mut poll)?
            };
            colour.images.push(image);
        }
        Ok(colour)
    }

    fn source_colour(
        &self,
        layer: LayerMetadata<'_>,
        plan: &regional::Plan,
        count: usize,
        checks: &mut ReadChecks<'_>,
    ) -> Result<regional::RegionalImage> {
        let scene = self.scene;
        let source = scene.raster(layer.pixels().slot())?;
        let [left, top, right, bottom] = plan.input_bounds();
        let rows = (crate::raster::backed::MAX_REGION_PIXELS / (right - left) as usize) as i32;
        let mut current: Option<SurfaceRegion> = None;
        let checks = RefCell::new(checks);
        plan.render(
            scene.budget(),
            &vec![None; count],
            &vec![None; count],
            &mut || checks.borrow_mut().pinned(),
            |x, y| {
                if layer.kind() == "fill" {
                    return Ok(layer.color().map(|v| u16::from(v) * 257));
                }
                if current.as_ref().is_none_or(|view| {
                    let b = view.region().bounds();
                    y < b[1] || y >= b[3]
                }) {
                    drop(current.take());
                    let start = top + (y - top) / rows * rows;
                    let region = Region::new([left, start, right, (start + rows).min(bottom)])
                        .map_err(|e| e.to_string())?;
                    current = Some(checks.borrow_mut().surface_region(source, region)?);
                }
                current
                    .as_ref()
                    .ok_or("Missing native effect source footprint")?
                    .pixel16(x, y)
                    .map_err(|e| e.to_string())
            },
        )
    }

    fn raw_at(&self, layer: usize, x: i32, y: i32) -> Result<&RawLayers<'a>> {
        let origin = self
            .scene
            .metadata()
            .layer(layer)
            .ok_or("Missing raw layer")?
            .origin();
        let x = x
            .checked_add(origin[0])
            .ok_or("Raw source coordinate overflow")?;
        let y = y
            .checked_add(origin[1])
            .ok_or("Raw source coordinate overflow")?;
        let index = self.raw.partition_point(|stripe| stripe.bounds[3] <= y);
        let stripe = self
            .raw
            .get(index)
            .ok_or("Raw sample has no prepared stripe")?;
        if x < stripe.bounds[0] || x >= stripe.bounds[2] || y < stripe.bounds[1] {
            return Err("Raw sample is outside its prepared stripe".into());
        }
        Ok(&stripe.layers)
    }
}

impl<P: Precision> PreparedLayers<P> for Layers<'_> {
    fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
    fn scope(&self) -> LayerScope<'_> {
        self.scope
    }
    fn has_colour(&self, layer: usize) -> bool {
        self.colours[layer].is_some()
    }
    fn colour(&self, layer: usize, x: i32, y: i32) -> Result<P::Pixel> {
        let colour = self
            .colours
            .get(layer)
            .and_then(Option::as_ref)
            .ok_or("No derived layer output was prepared")?;
        colour.sample::<P>(x, y)
    }
    fn source(&self, layer: usize, x: i32, y: i32) -> Result<P::Pixel> {
        <RawLayers<'_> as PreparedLayers<P>>::source(self.raw_at(layer, x, y)?, layer, x, y)
    }
    fn mask(&self, layer: usize, x: i32, y: i32, raw: bool) -> Result<P::Amount> {
        <RawLayers<'_> as PreparedLayers<P>>::mask(self.raw_at(layer, x, y)?, layer, x, y, raw)
    }
}

fn backdrop_reach(layer: LayerMetadata<'_>) -> Result<i32> {
    if !["group", "adjustment"].contains(&layer.kind()) {
        return Ok(0);
    }
    layer
        .effects()
        .filter(|e| e.enabled() && e.weight() > 0.)
        .try_fold(0i32, |sum, e| {
            sum.checked_add(regional::reach(e.kind(), e.settings())?)
                .ok_or("Backdrop halo overflow".into())
        })
}

fn expand(scene: &SceneSnapshot, output: [i32; 4], halo: i32) -> [i32; 4] {
    let [l, t, r, b] = output;
    let (w, h) = (
        scene.metadata().width() as i32,
        scene.metadata().height() as i32,
    );
    if r <= 0 || b <= 0 || l >= w || t >= h {
        return output;
    }
    [
        l.min(l.saturating_sub(halo).max(0)),
        t.min(t.saturating_sub(halo).max(0)),
        r.max(r.saturating_add(halo).min(w)),
        b.max(b.saturating_add(halo).min(h)),
    ]
}

fn stripe_count(bounds: [i32; 4]) -> Result<usize> {
    let [l, t, r, b] = bounds;
    let width = usize::try_from(r - l).map_err(|_| "Invalid derived stripe width")?;
    if width == 0 || width > crate::raster::backed::MAX_REGION_PIXELS || t >= b {
        return Err("Invalid derived stripe bounds".into());
    }
    Ok(((b - t) as usize).div_ceil(crate::raster::backed::MAX_REGION_PIXELS / width))
}

fn has_halo(effects: &[Effect]) -> Result<bool> {
    for effect in effects {
        if regional::reach(&effect.kind, &effect.settings)? > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn bands(bounds: [i32; 4]) -> impl Iterator<Item = [i32; 4]> {
    let [l, t, r, b] = bounds;
    let rows = crate::raster::backed::MAX_REGION_PIXELS / (r - l) as usize;
    (t..b)
        .step_by(rows)
        .map(move |y| [l, y, r, (y + rows as i32).min(b)])
}
