//! Full-stage native dependencies for global styles. Completed pixels live in
//! session-owned storage; composition pins only the requested native window.
use super::*;
use crate::{
    effects::{regional, stored, Effect},
    workspace::source_runtime::SourceSession,
};
use std::sync::Arc;
type Check<'a> = &'a mut dyn FnMut() -> Result<()>;

pub(crate) enum LayerWindow<'a> {
    Regional(Layers<'a>),
    Stored(Window<'a>),
}
impl<'a> LayerWindow<'a> {
    pub(crate) fn prepare(
        topology: &Arc<Topology<'a>>,
        region: Region,
        stored: Option<&Stored<'a>>,
        check: Check<'_>,
    ) -> Result<Self> {
        Self::prepare_with_checks(topology, region, stored, &mut ReadChecks::caller_only(check))
    }
    pub(crate) fn prepare_with_checks(
        topology: &Arc<Topology<'a>>,
        region: Region,
        stored: Option<&Stored<'a>>,
        checks: &mut ReadChecks<'_>,
    ) -> Result<Self> {
        if let Some(stored) = stored {
            stored.check_tree(topology)?;
            if !stored.colours.is_empty() {
                return stored.window_with_checks(region, checks).map(Self::Stored);
            }
        }
        Layers::for_topology_with_checks(topology, region, checks).map(Self::Regional)
    }
}
impl<P: Precision> PreparedLayers<P> for LayerWindow<'_> {
    fn scene(&self) -> &SceneSnapshot {
        match self {
            Self::Regional(v) => <Layers<'_> as PreparedLayers<P>>::scene(v),
            Self::Stored(v) => <Window<'_> as PreparedLayers<P>>::scene(v),
        }
    }
    fn scope(&self) -> LayerScope<'_> {
        match self {
            Self::Regional(v) => <Layers<'_> as PreparedLayers<P>>::scope(v),
            Self::Stored(v) => <Window<'_> as PreparedLayers<P>>::scope(v),
        }
    }
    fn has_colour(&self, i: usize) -> bool {
        match self {
            Self::Regional(v) => <Layers<'_> as PreparedLayers<P>>::has_colour(v, i),
            Self::Stored(v) => <Window<'_> as PreparedLayers<P>>::has_colour(v, i),
        }
    }
    fn colour(&self, i: usize, x: i32, y: i32) -> Result<P::Pixel> {
        match self {
            Self::Regional(v) => <Layers<'_> as PreparedLayers<P>>::colour(v, i, x, y),
            Self::Stored(v) => <Window<'_> as PreparedLayers<P>>::colour(v, i, x, y),
        }
    }
    fn source(&self, i: usize, x: i32, y: i32) -> Result<P::Pixel> {
        match self {
            Self::Regional(v) => <Layers<'_> as PreparedLayers<P>>::source(v, i, x, y),
            Self::Stored(v) => <Window<'_> as PreparedLayers<P>>::source(v, i, x, y),
        }
    }
    fn mask(&self, i: usize, x: i32, y: i32, raw: bool) -> Result<P::Amount> {
        match self {
            Self::Regional(v) => <Layers<'_> as PreparedLayers<P>>::mask(v, i, x, y, raw),
            Self::Stored(v) => <Window<'_> as PreparedLayers<P>>::mask(v, i, x, y, raw),
        }
    }
}

pub(crate) struct Stored<'a> {
    topology: Arc<Topology<'a>>,
    session: &'a SourceSession,
    colours: Vec<Option<stored::Image<'a>>>,
    filtered: Option<stored::Image<'a>>,
    layer_crop: Option<[i32; 4]>,
    _index: Reservation,
}
struct View {
    frame: stored::Frame,
    image: Option<regional::RegionalImage>,
}
pub(crate) struct Window<'a> {
    scene: &'a SceneSnapshot,
    scope: LayerScope<'a>,
    raw: RawLayers<'a>,
    colours: Vec<Option<View>>,
    _index: Reservation,
}
impl<'a> Stored<'a> {
    pub(crate) fn composite_needed(topology: &Topology<'_>, crop: Option<[i32; 4]>) -> bool {
        Self::needed(topology) || super::filters::needs_stage(topology, crop)
    }

    pub(crate) fn check_tree(&self, topology: &Topology<'_>) -> Result<()> {
        if !std::ptr::eq(topology.scene, self.topology.scene)
            || topology.scope() != self.topology.scope()
        {
            return Err("Stored layers belong to another captured tree".into());
        }
        self.session.check()
    }
    pub(crate) fn needed(topology: &Topology<'_>) -> bool {
        topology.layers.iter().enumerate().any(|(i, layer)| {
            topology.active[i]
                && layer.effects().any(|effect| {
                    effect.enabled()
                        && effect.weight() > 0.
                        && Self::effect_needed(effect.kind(), effect.settings())
                })
        })
    }
    pub(super) fn effect_needed(kind: &str, settings: &serde_json::Value) -> bool {
        kind == "liquify"
            || (kind == "gradient_overlay"
                && settings["pixel_grid"] == true
                && settings["align_layer"] == true)
            || crate::effects::styles::smoothing_radii(kind, settings)
                .iter()
                .any(|&radius| radius > 0)
    }
    pub(crate) fn validate_layer(layer: LayerMetadata<'_>) -> Result<()> {
        for effect in layer.effects().filter(|e| e.enabled() && e.weight() > 0.) {
            if (effect.kind() != "liquify" && effect.clips().len() != 0)
                || !crate::effects::KINDS.contains(&effect.kind())
            {
                return Err(
                    "Native styles require recognized effects and typed Liquify clips".into(),
                );
            }
        }
        Ok(())
    }
    pub(crate) fn prepare(
        topology: Arc<Topology<'a>>,
        session: &'a SourceSession,
        check: Check<'_>,
    ) -> Result<Self> {
        Self::prepare_inner(topology, session, true, None, None, check)
    }

    pub(crate) fn prepare_cropped(
        topology: Arc<Topology<'a>>,
        session: &'a SourceSession,
        crop: [i32; 4],
        check: Check<'_>,
    ) -> Result<Self> {
        let layer_crop = Self::layer_crop(&topology, Some(crop), false)?;
        Self::prepare_inner(topology, session, true, None, layer_crop, check)
    }

    pub(crate) fn prepare_composite(
        topology: Arc<Topology<'a>>,
        session: &'a SourceSession,
        crop: Option<[i32; 4]>,
        check: Check<'_>,
    ) -> Result<Self> {
        let layers = Self::needed(&topology);
        let filters = super::filters::needs_stage(&topology, crop).then(|| {
            let doc = topology.scene.metadata();
            crop.unwrap_or([0, 0, doc.width() as i32, doc.height() as i32])
        });
        let layer_crop = Self::layer_crop(&topology, crop, true)?;
        Self::prepare_inner(topology, session, layers, filters, layer_crop, check)
    }

    fn layer_crop(topology: &Topology<'_>, crop: Option<[i32; 4]>, composite: bool) -> Result<Option<[i32; 4]>> {
        let Some(crop) = crop else { return Ok(None); };
        let doc = topology.scene.metadata();
        let frame = regional::Frame::new(doc.width(), doc.height())?;
        if crop != frame.expand(crop, 0) || crop[0] >= crop[2] || crop[1] >= crop[3] {
            return Ok(None);
        }
        let halo = if composite && topology.target().is_none() {
            doc.filters().filter(|f| f.enabled() && f.weight() > 0.)
                .try_fold(0i32, |halo, filter| {
                    halo.checked_add(regional::reach(filter.kind(), filter.settings())?)
                        .ok_or_else(|| "Stored composite filter halo overflow".to_string())
                })?
        } else {
            0
        };
        Ok(Some(frame.expand(crop, halo)))
    }

    fn prepare_inner(
        topology: Arc<Topology<'a>>,
        session: &'a SourceSession,
        layers: bool,
        filters: Option<[i32; 4]>,
        layer_crop: Option<[i32; 4]>,
        check: Check<'_>,
    ) -> Result<Self> {
        let scene = topology.scene;
        session.check()?;
        if scene.metadata().read_only()
            || !scene.budget().same_pool(&session.resident())
            || !scene.disk_budget().same_pool(&session.disk())
        {
            return Err("Stored styles require an editable scene in their source operation".into());
        }
        let count = if layers { topology.layers.len() } else { 0 };
        let index = scene
            .budget()
            .reserve_bytes(
                count
                    .checked_mul(512)
                    .ok_or("Stored layer index overflow")?,
                4 * 1024 * 1024,
            )
            .map_err(|e| e.to_string())?;
        let mut colours = Vec::new();
        colours
            .try_reserve_exact(count)
            .map_err(|_| "Stored layer allocation refused")?;
        colours.resize_with(count, || None);
        let mut order = Vec::new();
        order
            .try_reserve_exact(count)
            .map_err(|_| "Stored layer order allocation refused")?;
        fn visit(topology: &Topology<'_>, parent: Option<&str>, order: &mut Vec<(usize, Option<[i32; 4]>)>) {
            for (i, layer) in topology
                .layers
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, layer)| layer.parent() == parent)
            {
                if layer.kind() == "group" {
                    visit(topology, Some(layer.id()), order);
                }
                order.push((i, None));
            }
        }
        if layers {
            visit(&topology, None, &mut order);
        }
        let mut needed = layer_crop;
        if needed.is_some() {
            let doc = scene.metadata();
            let frame = regional::Frame::new(doc.width(), doc.height())?;
            // Every earlier stage covers at least the current stage's input.
            // A global backdrop dependency expands the earlier stages to the full frame.
            for (i, crop) in order.iter_mut().rev() {
                if !topology.active[*i] {
                    continue;
                }
                let Some(bounds) = needed else { break; };
                check()?;
                *crop = Some(bounds);
                let layer = topology.layers[*i];
                if matches!(layer.kind(), "group" | "adjustment") {
                    let mut halo = 0i32;
                    for effect in layer.effects().filter(|e| e.enabled() && e.weight() > 0.) {
                        let Some(reach) = regional::local_change_reach(effect.kind(), effect.settings())? else {
                            needed = None;
                            break;
                        };
                        halo = halo.checked_add(reach).ok_or("Stored backdrop halo overflow")?;
                    }
                    needed = needed.map(|bounds| frame.expand(bounds, halo));
                }
            }
        }
        let binding = scene.render_key()?;
        let mut output = Self {
            topology,
            session,
            colours,
            filtered: None,
            layer_crop: None,
            _index: index,
        };
        let result = (|| {
            for (i, crop) in order {
                check()?;
                if !output.topology.active[i] {
                    continue;
                }
                let layer = output.topology.layers[i];
                Self::validate_layer(layer)?;
                if layer.kind() != "adjustment"
                    && !layer.effects().any(|e| e.enabled() && e.weight() > 0.)
                {
                    continue;
                }
                let image = output.layer(i, binding, crop, check)?;
                output.colours[i] = Some(image);
                output.layer_crop = crop;
            }
            output.layer_crop = layer_crop;
            if let Some(bounds) = filters {
                output.filtered = Some(super::filters::stage(&output.topology, &output, session, bounds, check)?);
            }
            session.check()?;
            check()
        })();
        if let Err(error) = result {
            return Err(match output.retire(&mut || Ok(())) {
                Ok(()) => error,
                Err(cleanup) => format!("{error}; stored scene cleanup: {cleanup}"),
            });
        }
        Ok(output)
    }
    pub(super) fn filtered(&self, region: Region, check: Check<'_>) -> Result<Option<regional::RegionalImage>> {
        self.filtered.as_ref().map(|image| image.read_document(self.session, region, check)).transpose()
    }
    fn layer(
        &self,
        i: usize,
        scene_binding: [u8; 32],
        crop: Option<[i32; 4]>,
        check: Check<'_>,
    ) -> Result<stored::Image<'a>> {
        let scene = self.topology.scene;
        let layer = self.topology.layers[i];
        let canvas = [scene.metadata().width(), scene.metadata().height()];
        let is_canvas = matches!(layer.kind(), "group" | "adjustment");
        let frame = stored::Frame {
            size: if is_canvas {
                canvas
            } else {
                [layer.pixels().width(), layer.pixels().height()]
            },
            origin: if is_canvas { [0, 0] } else { layer.origin() },
        };
        let mut bytes = 65536;
        for effect in layer.effects() {
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
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(b"PeerBrush derived native layer v1");
        digest.update(&scene_binding);
        digest.update(&(i as u64).to_be_bytes());
        let binding = digest.finish().as_ref().try_into().unwrap();
        stored::stack(
            self.session,
            binding,
            frame,
            canvas,
            scene.metadata().depth(),
            &effects,
            crop,
            |index, frame, check| {
                let effect = layer
                    .effects()
                    .filter(|e| e.enabled() && e.weight() > 0.)
                    .nth(index)
                    .ok_or("Missing captured Liquify stage")?;
                crate::liquify::native::prepare(scene, effect, frame, check)
            },
            check,
            |region, guard| {
                if is_canvas {
                    let window = self.window(region, guard)?;
                    let pixels = if scene.metadata().depth() == 8 {
                        let plan = Plan::<Native8, _> {
                            topology: self.topology.clone(),
                            prepared: &window,
                            region: region.bounds(),
                            _precision: std::marker::PhantomData,
                        };
                        crate::render::region::native(region, 8, scene.budget(), guard, |x, y| {
                            let pixel = if layer.kind() == "adjustment" {
                                plan.adjustment_input(i, x, y)?
                            } else {
                                plan.layer(i, x, y)?
                            };
                            Ok(pixel.map(|v| u16::from(v) * 257))
                        })?
                    } else {
                        let plan = Plan::<Native16, _> {
                            topology: self.topology.clone(),
                            prepared: &window,
                            region: region.bounds(),
                            _precision: std::marker::PhantomData,
                        };
                        crate::render::region::native(region, 16, scene.budget(), guard, |x, y| {
                            if layer.kind() == "adjustment" {
                                plan.adjustment_input(i, x, y)
                            } else {
                                plan.layer(i, x, y)
                            }
                        })?
                    };
                    regional::RegionalImage::from_native(region, scene.metadata().depth(), pixels)
                } else {
                    let source = if layer.kind() == "fill" {
                        None
                    } else {
                        let mut interrupted = None;
                        let view = scene
                            .raster(layer.pixels().slot())?
                            .prepare_region(region, &mut || {
                                guard().map_err(|error| {
                                    interrupted = Some(error);
                                    crate::large_document::Error::Cancelled
                                })
                            })
                            .map_err(|e| interrupted.unwrap_or_else(|| e.to_string()))?;
                        Some(view)
                    };
                    let pixels = crate::render::region::native(
                        region,
                        scene.metadata().depth(),
                        scene.budget(),
                        guard,
                        |x, y| match &source {
                            Some(source) => source.pixel16(x, y).map_err(|e| e.to_string()),
                            None => Ok(layer.color().map(|v| u16::from(v) * 257)),
                        },
                    )?;
                    regional::RegionalImage::from_native(region, scene.metadata().depth(), pixels)
                }
            },
        )
    }
    pub(crate) fn window(&self, region: Region, check: Check<'_>) -> Result<Window<'a>> {
        self.window_with_checks(region, &mut ReadChecks::caller_only(check))
    }
    fn window_with_checks(&self, region: Region, checks: &mut ReadChecks<'_>) -> Result<Window<'a>> {
        let [left, top, right, bottom] = region.bounds();
        if self.layer_crop.is_some_and(|crop| {
            left < crop[0] || top < crop[1] || right > crop[2] || bottom > crop[3]
        }) {
            return Err("Stored layer window exceeds its prepared crop".into());
        }
        let scene = self.topology.scene;
        let bytes = self
            .colours
            .len()
            .checked_mul(256)
            .ok_or("Stored window index overflow")?;
        let index = scene
            .budget()
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut colours = Vec::new();
        colours
            .try_reserve_exact(self.colours.len())
            .map_err(|_| "Stored window allocation refused")?;
        for colour in &self.colours {
            checks.full()?;
            let Some(colour) = colour else {
                colours.push(None);
                continue;
            };
            let frame = colour.frame();
            let [l, t, r, b] = region.bounds();
            let bounds = [
                (i64::from(l) - i64::from(frame.origin[0])).max(0),
                (i64::from(t) - i64::from(frame.origin[1])).max(0),
                (i64::from(r) - i64::from(frame.origin[0])).min(i64::from(frame.size[0])),
                (i64::from(b) - i64::from(frame.origin[1])).min(i64::from(frame.size[1])),
            ];
            let image = if bounds[0] < bounds[2] && bounds[1] < bounds[3] {
                Some(colour.read(
                    self.session,
                    Region::new(bounds.map(|v| v as i32)).map_err(|e| e.to_string())?,
                    &mut || checks.full(),
                )?)
            } else {
                None
            };
            colours.push(Some(View { frame, image }));
        }
        let raw = RawLayers::prepare_inner(scene, Some(&self.topology), region, true, checks)?;
        self.session.check()?;
        checks.full()?;
        Ok(Window {
            scene,
            scope: self.topology.scope(),
            raw,
            colours,
            _index: index,
        })
    }
    pub(crate) fn retire(mut self, check: Check<'_>) -> Result<()> {
        let mut errors = Vec::new();
        for image in self.filtered.take().into_iter().chain(self.colours.iter_mut().filter_map(Option::take)) {
            if let Err(error) = image.retire(check) {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
impl<P: Precision> PreparedLayers<P> for Window<'_> {
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
            .ok_or("Missing stored layer output")?;
        let (x, y) = (
            i64::from(x) - i64::from(colour.frame.origin[0]),
            i64::from(y) - i64::from(colour.frame.origin[1]),
        );
        if x < 0
            || y < 0
            || x >= i64::from(colour.frame.size[0])
            || y >= i64::from(colour.frame.size[1])
        {
            return Ok(P::zero());
        }
        P::colour_sample(
            colour
                .image
                .as_ref()
                .ok_or("Missing stored colour window")?,
            x as i32,
            y as i32,
        )
    }
    fn source(&self, layer: usize, x: i32, y: i32) -> Result<P::Pixel> {
        <RawLayers<'_> as PreparedLayers<P>>::source(&self.raw, layer, x, y)
    }
    fn mask(&self, layer: usize, x: i32, y: i32, raw: bool) -> Result<P::Amount> {
        <RawLayers<'_> as PreparedLayers<P>>::mask(&self.raw, layer, x, y, raw)
    }
}
