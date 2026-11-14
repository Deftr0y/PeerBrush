//! Layer effect commands preserve typed historic clips and native source owners.
use super::*;
use crate::effects::styles::{apply_global_light_request, global_light_request};
use crate::engine::scene::{
    Coverage36, Effect36, MetadataPayload, RasterCollector, Retained36, Warp36,
};
use crate::workspace::source_runtime::SourceSession;
use serde::Deserialize;
use serde_json::Value;

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let cancellation = edit.cancellation();
    let checked = || {
        cancellation.check()?;
        session.check()?;
        check()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Effect capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Effect command missing")?;
    let c = command.parameters();
    if descriptor.commands().len() != 1
        || command.method() != Method::Effects
        || !matches!(c["op"].as_str(), Some("effect.add" | "effect.update"))
    {
        return Err("Effect producer differs from its normalized command".into());
    }
    if base.metadata().read_only() {
        return Err("This document is read-only".into());
    }
    let id = c["layer"].as_str().ok_or("Choose a target layer")?;
    super::patch::unlocked(&base.core.metadata, id)?;
    let index = base
        .core
        .metadata
        .layers
        .iter()
        .position(|l| l.id == id)
        .ok_or("Unknown layer")?;
    let layer = &base.core.metadata.layers[index];
    let adding = c["op"] == "effect.add";
    if adding && layer.effects.len() >= 32 {
        return Err("Effect stack limit: 32".into());
    }
    let old = if adding {
        None
    } else {
        Some(
            layer
                .effects
                .iter()
                .position(|e| Some(e.id.as_str()) == c["effect"].as_str())
                .ok_or("Unknown effect")?,
        )
    };
    let kind = old.map_or_else(
        || c["kind"].as_str().unwrap_or("levels"),
        |i| layer.effects[i].kind.as_str(),
    );
    let global = global_light_request(kind, c)?;
    let weight = if adding || c.get("weight").is_some() {
        crate::effects::command_weight(c)?
    } else {
        layer.effects[old.unwrap()].weight
    };
    let enabled = old.map_or(true, |i| {
        c["enabled"].as_bool().unwrap_or(layer.effects[i].enabled)
    });
    let defaults;
    let settings = if adding {
        defaults = crate::effects::defaults(kind);
        Some(c.get("settings").unwrap_or(&defaults))
    } else {
        c.get("settings").or_else(|| global.and(old).map(|i| layer.effects[i].settings.value()))
    };
    let mut bytes = 64 * 1024 + 32 * std::mem::size_of::<Effect36>() + kind.len();
    if let Some(value) = settings {
        settings_bytes(kind, value, &mut bytes)?;
    }
    // Coverage-free capture and normalization coexist; binding moves the result.
    let scratch = MetadataCharge::reserve(
        base.budget(),
        bytes
            .checked_mul(2)
            .ok_or("Effect metadata footprint overflow")?,
        &checked,
    )?;
    let copied = copy_metadata(&base.core.metadata, base.budget(), &checked)?
        .retain_owner(&scratch, &checked)?;
    let intent = edit.planned_intent(Intent::from_scopes(
        IntentKind::Capture,
        command.write_scopes(),
        base.budget(),
    )?)?;
    let mut outputs = Vec::new();
    outputs
        .try_reserve_exact(256)
        .map_err(|_| "Effect source output allocation refused")?;
    let prepared = (|| {
        let mut collector = Clips {
            base: &base,
            session,
            check: &checked,
            outputs: &mut outputs,
            owners: Vec::new(),
            bytes: 0,
        };
        collector
            .owners
            .try_reserve_exact(128)
            .map_err(|_| "Effect clip admission allocation refused")?;
        let mut global_angle = base.core.metadata.psd_global_angle;
        let replacement = if let Some(value) = settings {
            let (value, clips) = Effect36::capture_settings(kind, value, &mut collector)?;
            let mut value = if adding || c.get("settings").is_some() {
                normalize(kind, &value, base.budget(), base.metadata().depth(), &checked)?
            } else {
                value
            };
            apply_global_light_request(&mut value, &mut global_angle, global);
            Some((
                MetadataPayload::from(value).bind(base.budget(), &checked)?,
                clips,
            ))
        } else {
            None
        };
        copied.rehome_unique(|mut metadata, _| {
            metadata.psd_global_angle = global_angle;
            let effects = &mut metadata.layers[index].effects;
            if let Some(i) = old {
                let effect = &mut effects[i];
                effect.enabled = enabled;
                effect.weight = weight;
                if let Some((settings, clips)) = replacement {
                    effect.settings = settings;
                    effect.clips = clips;
                }
            } else {
                let (settings, clips) = replacement.ok_or("New effect settings missing")?;
                effects
                    .try_reserve_exact(1)
                    .map_err(|_| "Effect header allocation refused")?;
                effects.push(Effect36 {
                    id: crate::engine::id(),
                    kind: kind.into(),
                    enabled,
                    weight,
                    settings,
                    clips,
                });
            }
            super::metadata::validate_clipping(&metadata)?;
            validate_budget(&metadata, base.budget(), &checked)?;
            let mut locations = 0;
            metadata.remap_rasters(|_| {
                locations += 1;
                Ok(())
            })?;
            let roster = rebuild_from(
                &mut metadata,
                locations,
                base.budget(),
                base.disk_budget(),
                &checked,
                |reference| {
                    if reference.id as usize <= base.roster().len() {
                        base.roster().get(reference).cloned()
                    } else {
                        collector
                            .outputs
                            .get(reference.id as usize - base.roster().len() - 1)
                            .map(|output| output.surface().clone())
                            .ok_or_else(|| "Effect source missing".into())
                    }
                },
            )?;
            metadata.revision = metadata
                .revision
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            checked()?;
            Core::prepare(
                metadata,
                roster,
                base.budget().clone(),
                base.disk_budget().clone(),
                &checked,
            )
        })
    })();
    match prepared {
        Ok(core) => Ok(PreparedDelta {
            base: base.key(),
            candidate: NativeDocument { core },
            cancellation: edit.cancellation,
            intent,
            outputs,
            admission: edit.admission,
        }),
        Err(reason) => Err(
            match super::output::abort_outputs(outputs, session, &|| session.cleanup_check()) {
                Ok(()) => reason,
                Err(cleanup) => format!("{reason}; effect source retirement refused: {cleanup}"),
            },
        ),
    }
}

// Inline coverage is decoded separately under its measured raster admission.
fn settings_bytes(kind: &str, value: &Value, bytes: &mut usize) -> Result<()> {
    if kind != "liquify" {
        return crate::effects::regional::metadata_bytes(value, 0, bytes);
    }
    let object = value
        .as_object()
        .ok_or("Liquify settings must be an object")?;
    for (key, value) in object {
        *bytes = bytes
            .checked_add(key.len() + 1024)
            .ok_or("Effect metadata footprint overflow")?;
        if key != "strokes" {
            crate::effects::regional::metadata_bytes(value, 0, bytes)?;
            continue;
        }
        let strokes = value
            .as_array()
            .filter(|s| s.len() <= 128)
            .ok_or("Invalid bounded Liquify strokes")?;
        for stroke in strokes {
            for (key, value) in stroke.as_object().ok_or("Invalid Liquify stroke")? {
                *bytes = bytes
                    .checked_add(key.len() + 1024)
                    .ok_or("Effect metadata footprint overflow")?;
                if key != "coverage" {
                    crate::effects::regional::metadata_bytes(value, 0, bytes)?;
                }
            }
        }
    }
    Ok(())
}

struct Clips<'a> {
    base: &'a NativeDocument,
    session: &'a SourceSession,
    check: &'a dyn Fn() -> Result<()>,
    outputs: &'a mut Vec<StagedOutput>,
    owners: Vec<Arc<MetadataCharge>>,
    bytes: usize,
}
impl RasterCollector for Clips<'_> {
    fn resident(&mut self, raster: &crate::raster::Raster) -> Result<RasterRef36> {
        (self.check)()?;
        raster.validate_layout()?;
        if self.outputs.len() >= 256 {
            return Err("Too many effect clip sources".into());
        }
        let id = u32::try_from(self.base.roster().len() + self.outputs.len() + 1)
            .map_err(|_| "Effect source ordinal overflow")?;
        let output = crate::retained::transaction::stage(
            self.base,
            self.session,
            self.check,
            |store, checked| {
                Surface::stage_current_pixels(raster, store.clone(), &mut || checked())
                    .map_err(|e| e.to_string())
            },
        )?;
        self.outputs.push(output);
        let retained = raster
            .retained
            .as_ref()
            .map(|original| -> Result<Box<Retained36>> {
                Ok(Box::new(Retained36 {
                    pixels: self.resident(&original.pixels)?,
                    matrix: original.matrix,
                    projective: original.projective,
                    warps: original.warps.iter().map(Warp36::capture).collect(),
                }))
            })
            .transpose()?;
        Ok(RasterRef36 {
            id,
            width: raster.width,
            height: raster.height,
            depth: raster.depth,
            retained,
        })
    }
    fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
        Err("Inline coverage must carry explicit raster pixels".into())
    }
    fn embedded_coverage(&mut self, value: &Value) -> Result<Coverage36> {
        (self.check)()?;
        let bytes = crate::engine::scene::legacy_coverage_bytes(value)?;
        let owner = MetadataCharge::reserve(self.base.budget(), bytes, self.check)?;
        let coverage = crate::selection::Coverage::deserialize(value)
            .map_err(|_| "Invalid captured Liquify coverage")?;
        coverage.validate()?;
        self.bytes = self
            .bytes
            .checked_add(coverage.mask.bytes())
            .ok_or("Liquify coverage footprint overflow")?;
        if self.bytes > 128 * 1024 * 1024 {
            return Err("Liquify selection coverage exceeds 128 MiB".into());
        }
        self.owners.push(owner);
        Coverage36::from_capture(&coverage, self)
    }
}

fn bitmap(
    kind: &str,
    settings: &Value,
    pool: &ResidentBudget,
    depth: u16,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<crate::effects::patterns::PreparedBitmap>> {
    if !crate::effects::patterns::active(kind, settings) {
        return Ok(None);
    }
    let source = settings["bitmap_png"]
        .as_str()
        .ok_or("Bitmap pattern requires bitmap_png")?;
    let bitmap = crate::effects::patterns::PreparedBitmap::from_png(source, pool, &mut || check())?;
    if bitmap.depth() == 16 && depth != 16 {
        return Err("A 16-bit bitmap pattern requires a 16-bit document".into());
    }
    Ok(Some(bitmap))
}
pub(super) fn normalize(
    kind: &str,
    settings: &Value,
    pool: &ResidentBudget,
    depth: u16,
    check: &dyn Fn() -> Result<()>,
) -> Result<Value> {
    if crate::effects::styles::contains(kind) {
        let bitmap = bitmap(kind, settings, pool, depth, check)?;
        crate::effects::styles::normalized_prepared(kind, settings, bitmap.as_ref())
    } else {
        crate::effects::normalized(kind, settings)
    }
}

pub(super) fn validate_budget(
    metadata: &Document36,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let mut patterns = 0usize;
    for layer in &metadata.layers {
        check()?;
        if layer.kind == "adjustment"
            && layer
                .effects
                .iter()
                .any(|e| e.kind == "channel_clamp" || crate::effects::styles::contains(&e.kind))
        {
            return Err("RGBO Clamp and layer styles need a regular layer or isolated folder; clipped adjustments preserve base opacity".into());
        }
        let canvas = [metadata.width, metadata.height];
        let mut size = if matches!(layer.kind.as_str(), "group" | "adjustment") {
            canvas
        } else {
            [layer.pixels.width, layer.pixels.height]
        };
        for effect in &layer.effects {
            check()?;
            let bitmap = bitmap(
                &effect.kind,
                &effect.settings,
                pool,
                metadata.bit_depth,
                check,
            )?;
            if let Some(bitmap) = &bitmap {
                patterns = patterns
                    .checked_add(
                        bitmap.bytes() + effect.settings["bitmap_png"].as_str().unwrap().len(),
                    )
                    .ok_or("Bitmap pattern footprint overflow")?;
                if patterns > crate::effects::patterns::DOCUMENT_LIMIT {
                    return Err("Bitmap pattern sources exceed the 32 MiB document budget".into());
                }
            }
            if !effect.enabled || effect.weight <= 0. {
                continue;
            }
            if crate::effects::styles::contains(&effect.kind) {
                size = canvas;
            }
            // Window scratch and stored stages charge their actual shared pools
            // at render time. Liquify also has intrinsic stroke and field limits.
            if effect.kind == "liquify" {
                let frame = crate::effects::regional::Frame::new(size[0], size[1])?;
                let clips = effect.clips.iter().map(|clip| {
                    let coverage = &clip.coverage;
                    (clip.stroke, coverage.origin, [coverage.mask.width, coverage.mask.height])
                });
                drop(crate::liquify::native::plan(frame, &effect.settings, clips, pool, &mut || check())?);
            } else {
                crate::effects::regional::reach(&effect.kind, &effect.settings)?;
            }
        }
    }
    Ok(())
}
