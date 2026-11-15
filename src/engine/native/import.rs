//! Import and any required precision promotion are one guarded generation.
use super::*;
use crate::placement::input::{self, Plan};
use crate::{
    clipboard::native::PasteImage,
    image_import::{
        native::OpenFormat,
        png_admission,
        prepared_png::{ColorPolicy, EncodedPng, FileLimits, FilePng, Limits},
    },
    workspace::source_runtime::SourceSession,
};
use std::{borrow::Cow, path::Path};

fn planes(metadata: &Document36) -> impl Iterator<Item = &RasterRef36> {
    metadata.layers.iter().flat_map(|l| {
        std::iter::once(&l.pixels).chain(
            l.mask
                .iter()
                .flat_map(|m| m.steps.iter().map(|s| &s.pixels)),
        )
    })
}
fn stored(source: &Surface) -> u64 {
    source
        .physical_tiles()
        .map(|t| t.reference().spec().bytes() as u64)
        .sum()
}
fn finish_input(
    output: Result<StagedOutput>,
    input: StagedOutput,
    session: &SourceSession,
) -> Result<StagedOutput> {
    match (output, input.abort(session, &|| session.cleanup_check())) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(reason), cleanup) => Err(super::worker::cleanup(reason, cleanup)),
        (Ok(output), Err(reason)) => Err(super::worker::cleanup(
            reason,
            output.abort(session, &|| session.cleanup_check()),
        )),
    }
}
fn decode(
    base: &NativeDocument,
    session: &SourceSession,
    command: &serde_json::Value,
    check: &dyn Fn() -> Result<()>,
    active: &dyn Fn() -> Result<()>,
) -> Result<StagedOutput> {
    if command.get("png").is_none() {
        if let Some(path) = command["path"].as_str().map(Path::new) {
            if crate::image_import::jpeg::matches(path) {
                return crate::image_import::jpeg::prepare(base, session, path, true, check);
            }
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("jxl"))
            {
                return crate::image_import::jxl::prepare(base, session, path, command, check);
            }
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("webp"))
            {
                return crate::image_import::webp::prepare(
                    base,
                    session,
                    path,
                    Some(command),
                    check,
                );
            }
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("png"))
            {
                return crate::image_import::prepared_png::apng::prepare(
                    base, session, path, command, active,
                );
            }
            if crate::image_import::tiff::matches(path) {
                return crate::image_import::tiff::prepare(
                    base,
                    session,
                    path,
                    Some(command),
                    check,
                );
            }
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("ico"))
            {
                return crate::image_import::ico::prepare(
                    base,
                    session,
                    path,
                    Some(command),
                    check,
                );
            }
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
            {
                return crate::image_import::gif::prepare(base, session, path, command, check);
            }
            if let Some(output) =
                crate::image_import::native::stream_file(base, session, path, check)
            {
                return output;
            }
        }
    }
    let limits = Limits {
        edge: 8192,
        pixels: 8192 * 8192,
        encoded: 128 * 1024 * 1024,
    };
    if command.get("path").is_none()
        && matches!(command["op"].as_str(), Some("image.place" | "image.paste"))
        && command.get("png").is_some()
    {
        let encoded = EncodedPng::base64(
            command["png"]
                .as_str()
                .ok_or("png must contain base64 PNG bytes")?,
            Limits::NATIVE_COMMAND,
            base.budget(),
            &mut || check(),
        )?;
        return crate::image_import::prepared_png::apng::prepare_encoded(
            base, session, &encoded, command, active,
        );
    }
    let mut file = match (
        command.get("path"),
        matches!(command["op"].as_str(), Some("image.place" | "image.paste"))
            .then(|| command.get("png"))
            .flatten(),
    ) {
        (Some(path), None) => Some(FilePng::open(
            session,
            Path::new(path.as_str().ok_or("Image path must be a string")?),
            FileLimits {
                edge: limits.edge,
                pixels: limits.pixels,
                encoded_bytes: limits.encoded as u64,
            },
            ColorPolicy::ImportSource,
            &mut || check(),
        )?),
        (None, Some(_)) => None,
        _ => return Err("Provide exactly one absolute path or base64 png".into()),
    };
    let input = if let Some(file) = &mut file {
        png_admission::stage_file(session, file, &mut || active())?
    } else {
        let encoded = EncodedPng::base64(
            command["png"]
                .as_str()
                .ok_or("png must contain base64 PNG bytes")?,
            limits,
            base.budget(),
            &mut || check(),
        )?;
        png_admission::stage(
            session,
            &encoded,
            limits,
            ColorPolicy::ImportSource,
            &mut || active(),
        )?
    };
    let decoded = (|| {
        let h = input.complete().header();
        crate::image_import::check_budget(h.width, h.height, h.depth, crate::image_import::BUDGET)?;
        let output = crate::image_import::native::prepare(base, session, &input, check)?;
        if let Err(reason) = file
            .as_ref()
            .map_or(Ok(()), |file| file.recheck())
            .and_then(|()| check())
        {
            return Err(super::worker::cleanup(
                reason,
                output.abort(session, &|| session.cleanup_check()),
            ));
        }
        Ok(output)
    })();
    match (decoded, input.abort(session)) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(reason), cleanup) => Err(super::worker::cleanup(reason, cleanup)),
        (Ok(output), Err(reason)) => Err(super::worker::cleanup(
            reason,
            output.abort(session, &|| session.cleanup_check()),
        )),
    }
}

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    imported: &mut u64,
    clipboard: Option<PasteImage<'_>>,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let active = || {
        edit.cancellation.check()?;
        check()
    };
    let checked = || {
        active()?;
        session.check()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Image import capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Image import command missing")?;
    if descriptor.commands().len() != 1
        || command.method() != Method::Import
        || !matches!(
            command.parameters()["op"].as_str(),
            Some("image.import" | "image.place" | "image.paste")
        )
        || base.metadata().read_only()
    {
        return Err("Image import requires its editable captured command".into());
    }
    let c = command.parameters();
    let placing = c["op"] == "image.place";
    let pasting = c["op"] == "image.paste";
    let file_format = c["path"].as_str().map(Path::new).and_then(OpenFormat::from_path);
    let row_file = c.get("png").is_none() && matches!(
        file_format,
        Some(OpenFormat::Bmp | OpenFormat::Jpeg | OpenFormat::Jxl | OpenFormat::Qoi | OpenFormat::Pnm | OpenFormat::Tga | OpenFormat::Tiff)
    );
    let layer_digest = if !placing && !pasting {
        crate::image_import::selected_digest(c)?
    } else {
        None
    };
    if layer_digest.is_some() && c.get("png").is_some() {
        return Err("An inspected image import requires its original file path".into());
    }
    let svg_path = c["path"]
        .as_str()
        .map(Path::new)
        .filter(|path| crate::image_import::svg::matches(path));
    if !placing && !pasting && c["path"].as_str().is_none() {
        return Err("Missing image path".into());
    }
    // shortcut: native imports admit PNG and bounded static file producers; connect remaining
    // codecs only with their native allocation and frame-selection admission.
    if svg_path.is_some()
        && ["frame", "page", "variant"]
            .iter()
            .any(|key| c.get(*key).is_some())
    {
        return Err("Static SVG import does not accept frame/page/variant choices".into());
    }
    let ico = c["path"].as_str().map(Path::new).is_some_and(|path| {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ico"))
    });
    let tiff = c["path"]
        .as_str()
        .map(Path::new)
        .is_some_and(crate::image_import::tiff::matches);
    if (!tiff && c.get("page").is_some()) || (!ico && c.get("variant").is_some()) {
        return Err("This image accepts frame, not page or variant".into());
    }
    let gif = matches!(file_format, Some(OpenFormat::Gif));
    let png_path = c["path"].as_str().map(Path::new).filter(|path| {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    });
    let webp = c["path"].as_str().map(Path::new).is_some_and(|path| {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("webp"))
    });
    if !gif
        && !webp
        && !ico
        && !tiff
        && png_path.is_none()
        && c.get("png").is_none()
        && c.get("frame").is_some_and(|v| v.as_u64() != Some(0))
    {
        return Err("Image selection index is outside the available items".into());
    }
    if !placing && !pasting && base.metadata().layers().len() >= 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    // Context-derived parent IDs are copied into both the plan and candidate.
    let parent_bytes = base
        .core
        .metadata
        .layers
        .iter()
        .map(|l| {
            l.id.len()
                .max(l.parent.as_ref().map_or(0, String::len))
                .max(l.name.len() + l.blend.len())
        })
        .max()
        .unwrap_or(0);
    let strings = c["path"]
        .as_str()
        .map_or(0, str::len)
        .checked_mul(3)
        .and_then(|n| n.checked_add(c["name"].as_str().map_or(0, str::len)))
        .and_then(|n| n.checked_add(c["parent"].as_str().map_or(0, str::len)))
        .and_then(|n| parent_bytes.checked_mul(2).and_then(|p| n.checked_add(p)))
        .ok_or("Import placement footprint overflow")?;
    let extra = MetadataCharge::reserve(
        base.budget(),
        strings + (base.metadata().layers().len() + 2) * std::mem::size_of::<Layer36>() + 4096,
        &checked,
    )?;
    let imported_placement = if placing || pasting {
        None
    } else {
        Some(crate::image_import::placement::Placement::new(c)?)
    };
    let pasted_placement = if pasting {
        Some(crate::clipboard::paste::Plan::new(
            c,
            base.core.metadata.layers.len(),
            |i| {
                let l = &base.core.metadata.layers[i];
                crate::clipboard::paste::Layer {
                    id: &l.id,
                    parent: l.parent.as_deref(),
                    kind: &l.kind,
                    clip_to: l.clip_to.as_deref(),
                    locked: l.locked,
                }
            },
        )?)
    } else {
        None
    };
    let plan = if let Some(paste) = &pasted_placement {
        Plan {
            target: None,
            parent: paste.parent.clone(),
            index: paste.index,
            replace: true,
        }
    } else if let Some(placement) = &imported_placement {
        let index = if let Some(parent) = placement.parent {
            super::patch::unlocked(&base.core.metadata, parent)?;
            base.core
                .metadata
                .layers
                .iter()
                .position(|l| l.id == parent && l.kind == "group")
                .ok_or("Image parent must be a folder")?
                + 1
        } else {
            0
        };
        Plan {
            target: None,
            parent: placement.parent.map(str::to_owned),
            index,
            replace: true,
        }
    } else {
        Plan::new(c, base.core.metadata.layers.len(), |i| {
            let l = &base.core.metadata.layers[i];
            input::Layer {
                id: &l.id,
                parent: l.parent.as_deref(),
                kind: &l.kind,
                locked: l.locked,
                source: l.source.is_some(),
            }
        })?
    };
    let name = imported_placement
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| {
            Cow::Borrowed(if pasting {
                "Pasted image"
            } else {
                c["name"].as_str().unwrap_or("Placed image")
            })
        });
    let count = 1 + planes(&base.core.metadata)
        .map(|r| 1 + usize::from(r.retained.is_some()))
        .sum::<usize>();
    let bytes = count * (std::mem::size_of::<StagedOutput>() + std::mem::size_of::<u32>()) + 4096;
    let charge = Arc::new(
        base.budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?,
    );
    let admission = edit
        .admission
        .as_ref()
        .ok_or("Import admission missing")?
        .with_scopes(
            &base,
            descriptor.shared_scopes(),
            charge,
            edit.admission
                .as_ref()
                .is_some_and(|a| a.is_transaction_step()),
        )?;
    let mut outputs = Vec::new();
    let mut ordinals = Vec::new();
    outputs
        .try_reserve_exact(count)
        .map_err(|_| "Import output allocation refused")?;
    ordinals
        .try_reserve_exact(count)
        .map_err(|_| "Import output index allocation refused")?;
    if outputs.capacity() > count || ordinals.capacity() > count {
        return Err("Import outputs exceed admission".into());
    }
    let _clipboard_png = match clipboard {
        Some(PasteImage::Png(bytes)) => Some(
            base.budget()
                .reserve_bytes(bytes.capacity(), bytes.capacity())
                .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };
    let prepared = (|| {
        let (source, native_tiled_source) = if let Some(clipboard) = clipboard {
            match clipboard {
                PasteImage::Bitmap(bitmap) => {
                    outputs.push(bitmap.stage_native(&base, session, &checked)?);
                    (None, false)
                }
                PasteImage::Png(bytes) => {
                    outputs.push(crate::image_import::prepared_png::apng::prepare_clipboard(
                        &base, session, bytes, &active,
                    )?);
                    (None, true)
                }
            }
        } else if let Some(path) = png_path.filter(|_| c.get("png").is_none()) {
            let output = crate::image_import::prepared_png::apng::prepare(
                &base, session, path, c, &active,
            )?;
            outputs.push(output);
            (None, true)
        } else if let Some(digest) = layer_digest {
            let path = Path::new(c["path"].as_str().ok_or("Missing inspected image path")?);
            let format = OpenFormat::from_path(path)
                .ok_or("Unsupported inspected image format")?;
            let (output, source) = format.prepare_pinned_layer(
                &base, session, path, digest, c, &edit.cancellation, &checked,
            )?;
            outputs.push(output);
            (source, row_file)
        } else if let Some(path) = svg_path {
            if c.get("png").is_some() {
                return Err("Provide exactly one absolute path or base64 png".into());
            }
            let (output, source) = crate::image_import::svg::prepare(
                &base,
                session,
                path,
                &edit.cancellation,
                &checked,
            )?;
            outputs.push(output);
            ((!placing && !pasting).then_some(source), false)
        } else {
            outputs.push(decode(&base, session, c, &checked, &active)?);
            (None, row_file)
        };
        // Wide still images use their native decoder and store admission.
        let large_static_file = matches!(file_format, Some(OpenFormat::Gif | OpenFormat::Webp))
            && clipboard.is_none() && c.get("png").is_none() && {
            let source = outputs[0].surface();
            crate::raster::CanvasLimits::Resident.check(source.width(), source.height()).is_err()
        };
        let native_tiled_source = native_tiled_source || large_static_file;
        // Static tiled sources are admitted by the native stores instead of a full-frame estimate.
        let total = imported
            .checked_add(if pasting && !native_tiled_source {
                let source = outputs[0].surface();
                // Match the clipboard's retained display/native-word footprint,
                // without materializing either buffer for file paste.
                let remaining =
                    (crate::image_import::BUDGET as u64).saturating_sub(*imported) as usize;
                crate::image_import::check_budget_with_limits(
                    source.width(),
                    source.height(),
                    source.depth(),
                    remaining,
                    crate::raster::CanvasLimits::Native,
                )?;
                u64::from(source.width())
                    * u64::from(source.height())
                    * if source.depth() == 16 { 12 } else { 4 }
            } else if placing || native_tiled_source {
                0
            } else {
                stored(outputs[0].surface())
            })
            .ok_or("Import budget overflow")?;
        if total > crate::image_import::BUDGET as u64 {
            return Err(
                "Decoded images exceed the 256 MiB import budget; import smaller or fewer images"
                    .into(),
            );
        }
        let depth = base.metadata().depth().max(outputs[0].surface().depth());
        if outputs[0].surface().depth() != depth {
            let input = outputs.pop().unwrap();
            let output =
                crate::retained::transaction::stage(&base, session, &checked, |store, guard| {
                    super::settings::convert(input.surface(), depth, store, &mut || guard())
                        .map_err(|e| e.to_string())
                });
            outputs.push(finish_input(output, input, session)?);
        }
        let size = [outputs[0].surface().width(), outputs[0].surface().height()];
        let area = if pasting {
            let origin = crate::clipboard::paste::origin(
                c,
                [base.core.metadata.width, base.core.metadata.height],
                size,
            )?;
            [
                origin[0],
                origin[1],
                origin[0] + size[0] as i32,
                origin[1] + size[1] as i32,
            ]
        } else if let Some(p) = &imported_placement {
            [
                p.origin[0],
                p.origin[1],
                p.origin[0] + size[0] as i32,
                p.origin[1] + size[1] as i32,
            ]
        } else {
            input::bounds(c, size, crate::raster::CanvasLimits::Native)?
        };
        let target = plan.target.map(|i| &base.core.metadata.layers[i]);
        let frame = target.map_or(Ok(area), |l| {
            input::frame([l.x, l.y], [l.pixels.width, l.pixels.height], area, crate::raster::CanvasLimits::Native)
        })?;
        let shape = input::shape(frame, crate::raster::CanvasLimits::Native)?;
        let shift = target.map_or([0, 0], |l| [l.x - frame[0], l.y - frame[1]]);
        let expanded =
            target.is_some_and(|l| shift != [0, 0] || shape != [l.pixels.width, l.pixels.height]);
        let added = target.map_or(base.roster().len() as u32 + 1, |l| l.pixels.id);
        ordinals.push(added);
        let mut changed = false;
        if target.is_some() || shape != size {
            let incoming = outputs.pop().unwrap();
            let output =
                crate::retained::transaction::stage(&base, session, &checked, |store, guard| {
                    let background = target
                        .map(|l| -> Result<_> {
                            Ok(super::placement::Background {
                                pixels: base.roster().get(&l.pixels)?,
                                shift,
                                fill: (l.kind == "fill").then_some(l.color),
                            })
                        })
                        .transpose()?;
                    let (surface, modified) = super::placement::render(
                        shape,
                        depth,
                        background,
                        Some(super::placement::Image {
                            pixels: incoming.surface(),
                            area: [
                                area[0] - frame[0],
                                area[1] - frame[1],
                                area[2] - frame[0],
                                area[3] - frame[1],
                            ],
                            replace: target.is_none() || plan.replace,
                        }),
                        store,
                        &mut || guard(),
                    )
                    .map_err(|e| e.to_string())?;
                    changed = modified;
                    Ok(surface)
                });
            outputs.push(finish_input(output, incoming, session)?);
        }
        let mask_changed =
            |r: &RasterRef36| expanded && (shift != [0, 0] || shape != [r.width, r.height]);
        if let Some(layer) = target.filter(|_| expanded) {
            for step in layer.mask.iter().flat_map(|m| &m.steps) {
                if !mask_changed(&step.pixels) {
                    continue;
                }
                let source = base.roster().get(&step.pixels)?;
                outputs.push(crate::retained::transaction::stage(
                    &base,
                    session,
                    &checked,
                    |store, guard| {
                        super::placement::render(
                            shape,
                            depth,
                            Some(super::placement::Background {
                                pixels: source,
                                shift,
                                fill: None,
                            }),
                            None,
                            store,
                            &mut || guard(),
                        )
                        .map(|(surface, _)| surface)
                        .map_err(|e| e.to_string())
                    },
                )?);
                ordinals.push(step.pixels.id);
            }
        }
        let promoting = depth != base.metadata().depth();
        if promoting {
            for plane in planes(&base.core.metadata) {
                let clear = target.is_some_and(|l| {
                    (l.pixels.id == plane.id && changed)
                        || l.mask
                            .iter()
                            .flat_map(|m| &m.steps)
                            .any(|s| s.pixels.id == plane.id && mask_changed(plane))
                });
                for reference in std::iter::once(plane)
                    .chain(plane.retained.iter().filter(|_| !clear).map(|o| &o.pixels))
                {
                    if ordinals.contains(&reference.id) {
                        continue;
                    }
                    checked()?;
                    let source = base.roster().get(reference)?;
                    outputs.push(crate::retained::transaction::stage(
                        &base,
                        session,
                        &checked,
                        |store, guard| {
                            super::settings::convert(source, depth, store, &mut || guard())
                                .map_err(|e| e.to_string())
                        },
                    )?);
                    ordinals.push(reference.id);
                }
            }
        }
        let intent = edit.planned_intent(Intent::from_scopes(
            IntentKind::Capture,
            command.write_scopes(),
            base.budget(),
        )?)?;
        let copied = copy_metadata(&base.core.metadata, base.budget(), &checked)?
            .retain_owner(&extra, &checked)?;
        let wrap = pasted_placement.as_ref().and_then(|p| p.wrap_base);
        let folder_pixels = wrap
            .map(|_| {
                Surface::zero(base.core.metadata.width, base.core.metadata.height, depth)
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        let core = copied.rehome_unique(|mut metadata, _| {
            if promoting {
                for layer in &mut metadata.layers {
                    for r in std::iter::once(&mut layer.pixels).chain(
                        layer
                            .mask
                            .iter_mut()
                            .flat_map(|m| m.steps.iter_mut().map(|s| &mut s.pixels)),
                    ) {
                        r.depth = depth;
                        if let Some(original) = &mut r.retained {
                            original.pixels.depth = depth;
                        }
                    }
                }
            }
            let pixels = outputs[0].surface();
            if let Some(index) = plan.target {
                let layer = &mut metadata.layers[index];
                layer.x = frame[0];
                layer.y = frame[1];
                layer.kind = "paint".into();
                layer.pixels.width = shape[0];
                layer.pixels.height = shape[1];
                layer.pixels.depth = depth;
                if changed {
                    layer.pixels.retained = None;
                }
                if expanded {
                    for step in layer.mask.iter_mut().flat_map(|m| &mut m.steps) {
                        if mask_changed(&step.pixels) {
                            step.pixels.retained = None;
                        }
                        step.pixels.width = shape[0];
                        step.pixels.height = shape[1];
                        step.pixels.depth = depth;
                    }
                }
            } else {
                metadata
                    .layers
                    .try_reserve_exact(1 + usize::from(wrap.is_some()))
                    .map_err(|_| "Image layer allocation refused")?;
                let mut pasted = image_layer(
                    &name,
                    plan.parent.clone(),
                    [frame[0], frame[1]],
                    RasterRef36 {
                        id: added,
                        width: pixels.width(),
                        height: pixels.height(),
                        depth,
                        retained: None,
                    },
                );
                pasted.source = source
                    .as_ref()
                    .map(|source| source.copy_runtime(base.budget(), &checked))
                    .transpose()?;
                if let Some(base_index) = wrap {
                    let old = &metadata.layers[base_index];
                    let base_id = old.id.clone();
                    let mut folder = image_layer(
                        &format!("{} group", old.name),
                        None,
                        [0, 0],
                        RasterRef36 {
                            id: added + 1,
                            width: metadata.width,
                            height: metadata.height,
                            depth,
                            retained: None,
                        },
                    );
                    folder.kind = "group".into();
                    folder.blend = old.blend.clone();
                    metadata.layers[base_index].blend = "normal".into();
                    for layer in &mut metadata.layers {
                        if layer.parent.is_none()
                            && (layer.id == base_id || layer.clip_to.as_deref() == Some(&base_id))
                        {
                            layer.parent = Some(folder.id.clone());
                        }
                    }
                    pasted.parent = Some(folder.id.clone());
                    metadata.layers.insert(plan.index - 1, folder);
                }
                metadata.layers.insert(plan.index, pasted);
            }
            metadata.bit_depth = depth;
            if pasting {
                metadata.selection = None;
                metadata.selection_polygon = None;
                metadata.selection_coverage = None;
            } else if !metadata.psd_intentionally_untagged {
                metadata.srgb_tagged = true;
            }
            let roster = rebuild_from(
                &mut metadata,
                base.roster().len() + usize::from(target.is_none()) + usize::from(wrap.is_some()),
                base.budget(),
                base.disk_budget(),
                &checked,
                |reference| {
                    if reference.id == added + 1 && folder_pixels.is_some() {
                        return Ok(folder_pixels.as_ref().unwrap().clone());
                    }
                    if let Some(at) = ordinals.iter().position(|id| *id == reference.id) {
                        Ok(outputs[at].surface().clone())
                    } else {
                        base.roster().get(reference).cloned()
                    }
                },
            )?;
            metadata.revision = metadata
                .revision
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            Core::prepare(
                metadata,
                roster,
                base.budget().clone(),
                base.disk_budget().clone(),
                &checked,
            )
        })?;
        checked()?;
        Ok((NativeDocument { core }, intent, total))
    })();
    match prepared {
        Ok((candidate, intent, total)) => {
            *imported = total;
            Ok(PreparedDelta {
                base: base.key(),
                candidate,
                cancellation: edit.cancellation,
                intent,
                outputs,
                admission: Some(admission),
            })
        }
        Err(reason) => Err(super::worker::cleanup(
            reason,
            super::output::abort_outputs(outputs, session, &|| session.cleanup_check()),
        )),
    }
}

pub(super) fn image_layer(
    name: &str,
    parent: Option<String>,
    origin: [i32; 2],
    pixels: RasterRef36,
) -> Layer36 {
    Layer36 {
        id: crate::engine::id(),
        name: name.into(),
        kind: "paint".into(),
        parent,
        clip_to: None,
        visible: true,
        locked: false,
        opacity: 1.,
        blend: "normal".into(),
        x: origin[0],
        y: origin[1],
        pixels,
        color: [233, 84, 32, 255],
        mask: None,
        effects: vec![],
        psd_metadata: vec![].into(),
        source: None,
    }
}
