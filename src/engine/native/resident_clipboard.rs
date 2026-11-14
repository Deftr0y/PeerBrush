//! Read the selected source planes into the resident clipboard's sparse format.
use super::*;
use crate::{
    clipboard::native::layers::LayerCopyPlan,
    effects::Effect,
    engine::{id, scene::Effect36, Layer, Mask, MaskStep},
    layer_clipboard::Layers,
    raster::{check_size, Raster, TILE},
    selection::Coverage,
};
use std::sync::Arc;

const RASTER_LIMIT: usize = 512 * 1024 * 1024;

pub(crate) fn materialize_layer_clipboard(
    clipboard: &LayerCopyPlan,
    destination: [u32; 2],
    check: &dyn Fn() -> Result<()>,
) -> Result<(Layers, Arc<MetadataCharge>)> {
    let source = clipboard.source();
    let layers = &source.core.metadata.layers;
    let metadata = source.metadata();
    let mut raster_bytes = 0usize;
    let mut coverage_bytes = 0usize;
    let mut coverage_json_bytes = 0usize;
    let mut metadata_bytes = 4096usize;
    for &index in clipboard.members() {
        check()?;
        let layer = &layers[index];
        for plane in std::iter::once(&layer.pixels).chain(
            layer
                .mask
                .iter()
                .flat_map(|mask| &mask.steps)
                .map(|step| &step.pixels),
        ) {
            admit_plane(source, plane, &mut raster_bytes)?;
        }
        let mut admit = |bytes: Option<usize>| -> Result<()> {
            metadata_bytes = metadata_bytes
                .checked_add(bytes.ok_or("Unbound clipboard metadata")?)
                .ok_or("Resident clipboard metadata footprint overflow")?;
            Ok(())
        };
        admit(Some(layer.header_bytes(check)?))?;
        admit(layer.psd_metadata.retained_bytes())?;
        if let Some(definition) = &layer.source {
            check_size(definition.width, definition.height)?;
            admit(definition.content.retained_bytes())?;
        }
        for step in layer.mask.iter().flat_map(|mask| &mask.steps) {
            admit(step.settings.retained_bytes())?;
        }
        for effect in &layer.effects {
            admit(effect.settings.retained_bytes())?;
            for clip in &effect.clips {
                check()?;
                let contours = &clip.coverage.contours;
                admit(contours.retained_bytes())?;
                // Each contour and point is an array in the resident JSON.
                let values = contours.iter().map(|row| 1 + 3 * row.len()).sum::<usize>();
                coverage_json_bytes = coverage_json_bytes
                    .checked_add(values * std::mem::size_of::<serde_json::Value>())
                    .ok_or("Resident clipboard coverage footprint overflow")?;
                admit_plane(source, &clip.coverage.mask, &mut coverage_bytes)?;
            }
        }
        if raster_bytes + coverage_bytes > RASTER_LIMIT {
            return Err("Layer clipboard exceeds the 512 MiB resident raster budget".into());
        }
    }
    // Resident Liquify stores coverage as base64 in its settings. Admit the
    // sparse tile copy, that encoding and validation's temporary decoded tiles.
    let bytes = metadata_bytes
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(raster_bytes + 3 * coverage_bytes))
        .and_then(|bytes| bytes.checked_add(coverage_json_bytes))
        .ok_or("Resident clipboard footprint overflow")?;
    let charge = MetadataCharge::reserve(source.budget(), bytes, check)?;
    let [width, height] = if check_size(metadata.width(), metadata.height()).is_ok() {
        [metadata.width(), metadata.height()]
    } else {
        destination
    };
    let snapshot = Layers {
        width,
        height,
        roots: clipboard
            .roots()
            .iter()
            .map(|&i| layers[i].id.clone())
            .collect(),
        layers: clipboard
            .members()
            .iter()
            .map(|&i| layer(source, &layers[i], check))
            .collect::<Result<_>>()?,
    };
    check()?;
    Ok((snapshot, charge))
}

fn admit_plane(source: &NativeDocument, plane: &RasterRef36, bytes: &mut usize) -> Result<()> {
    for plane in std::iter::once(plane).chain(plane.retained.iter().map(|r| &r.pixels)) {
        check_size(plane.width, plane.height)?;
        let surface = source.roster().get(plane)?;
        let tile_bytes = (TILE * TILE * 4) as usize * usize::from(plane.depth / 8);
        *bytes = bytes
            .checked_add(surface.physical_tiles().count() * (tile_bytes + 128))
            .filter(|&bytes| bytes <= RASTER_LIMIT)
            .ok_or("Layer clipboard exceeds the 512 MiB resident raster budget")?;
    }
    Ok(())
}

fn raster(
    source: &NativeDocument,
    plane: &RasterRef36,
    check: &dyn Fn() -> Result<()>,
) -> Result<Raster> {
    check()?;
    let surface = source.roster().get(plane)?;
    let mut raster = Raster::new_depth(plane.width, plane.height, plane.depth);
    for tile in surface.physical_tiles() {
        check()?;
        let pinned = tile
            .owner()
            .pin(tile.reference(), &mut || {
                check().map_err(|_| crate::large_document::Error::Cancelled)
            })
            .map_err(|error| error.to_string())?;
        let bytes = pinned.bytes();
        if plane.depth == 16 {
            let mut samples = Vec::new();
            samples
                .try_reserve_exact(bytes.len() / 2)
                .map_err(|_| "Resident 16-bit clipboard tile allocation refused")?;
            samples.extend(
                bytes
                    .chunks_exact(2)
                    .map(|word| u16::from_be_bytes([word[0], word[1]])),
            );
            raster
                .samples16
                .insert(tile.coordinate(), Arc::new(samples));
        } else {
            let mut samples = Vec::new();
            samples
                .try_reserve_exact(bytes.len())
                .map_err(|_| "Resident clipboard tile allocation refused")?;
            samples.extend_from_slice(bytes);
            raster.tiles.insert(tile.coordinate(), Arc::new(samples));
        }
    }
    if let Some(original) = &plane.retained {
        raster.retained = Some(Box::new(crate::retained::Original {
            pixels: Box::new(self::raster(source, &original.pixels, check)?),
            matrix: original.matrix,
            projective: original.projective,
            warps: original
                .warps
                .iter()
                .map(|warp| crate::warp::Stage {
                    before: warp.before,
                    grid: crate::warp::Grid {
                        rect: warp.grid.rect,
                        points: warp.grid.points,
                    },
                })
                .collect(),
        }));
    }
    Ok(raster)
}

fn effect(
    source: &NativeDocument,
    effect: &Effect36,
    check: &dyn Fn() -> Result<()>,
) -> Result<Effect> {
    check()?;
    let mut settings = effect.settings.value().clone();
    for clip in &effect.clips {
        let coverage = &clip.coverage;
        let coverage = Coverage {
            mask: raster(source, &coverage.mask, check)?,
            bounds: coverage.bounds,
            origin: coverage.origin,
            contours: coverage.contours.value().clone(),
        };
        let stroke = settings
            .get_mut("strokes")
            .and_then(|strokes| strokes.get_mut(clip.stroke as usize))
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("Missing native Liquify stroke")?;
        stroke.insert(
            "coverage".into(),
            serde_json::to_value(coverage).map_err(|error| error.to_string())?,
        );
    }
    Ok(Effect {
        id: effect.id.clone(),
        kind: effect.kind.clone(),
        enabled: effect.enabled,
        weight: effect.weight,
        settings: Arc::new(settings),
    })
}

fn layer(
    source: &NativeDocument,
    layer: &Layer36,
    check: &dyn Fn() -> Result<()>,
) -> Result<Layer> {
    let mask = layer
        .mask
        .as_ref()
        .map(|mask| -> Result<Mask> {
            Ok(Mask {
                enabled: mask.enabled,
                linked: mask.linked,
                steps: mask
                    .steps
                    .iter()
                    .map(|step| {
                        Ok(MaskStep {
                            id: step.id.clone(),
                            kind: step.kind.clone(),
                            enabled: step.enabled,
                            weight: step.weight,
                            value: step.value,
                            pixels: raster(source, &step.pixels, check)?,
                            settings: step.settings.value().clone(),
                        })
                    })
                    .collect::<Result<_>>()?,
                cache_key: id(),
            })
        })
        .transpose()?;
    Ok(Layer {
        id: layer.id.clone(),
        name: layer.name.clone(),
        kind: layer.kind.clone(),
        parent: layer.parent.clone(),
        clip_to: layer.clip_to.clone(),
        visible: layer.visible,
        locked: layer.locked,
        opacity: layer.opacity,
        blend: layer.blend.clone(),
        x: layer.x,
        y: layer.y,
        pixels: raster(source, &layer.pixels, check)?,
        color: layer.color,
        mask,
        effects: layer
            .effects
            .iter()
            .map(|entry| effect(source, entry, check))
            .collect::<Result<_>>()?,
        psd_metadata: layer.psd_metadata.value().clone(),
        source: layer
            .source
            .as_ref()
            .map(|definition| Arc::new(definition.to_resident())),
        effect_key: id(),
    })
}
