//! Temporary edge guides use the same rendered, color-managed bytes as preview.
use super::*;
use crate::{
    engine::native::{NativeDocument, OutputDraft, StagedOutput},
    psd::memory36::MetadataCharge,
    workspace::source_runtime::SourceSession,
};
#[cfg(test)]
#[path = "guide_tests.rs"]
mod tests;

pub(crate) fn stage(
    base: &NativeDocument,
    session: &SourceSession,
    rect: [i32; 4],
    target: Option<&str>,
    check: &dyn Fn() -> Result<()>,
) -> Result<StagedOutput> {
    stage_in_frame(base, session, rect, rect, target, check)
}

/// Render the visible layer into its original frame, including transparent
/// padding beyond the document. Profile conversion keeps canvas row alignment.
pub(crate) fn stage_layer_frame(
    base: &NativeDocument,
    session: &SourceSession,
    frame: [i32; 4],
    target: &str,
    check: &dyn Fn() -> Result<()>,
) -> Result<StagedOutput> {
    let metadata = base.metadata();
    let mut rect = crate::preview::regions::intersect(frame, metadata.width(), metadata.height());
    if rect[0] >= rect[2] || rect[1] >= rect[3] {
        check()?;
        if metadata.read_only() && base.protected_composite()?.id() != target {
            return Err("Protected Photoshop previews expose only the saved composite".into());
        }
        if !metadata.layers().any(|layer| layer.id() == target) {
            return Err("Layer no longer exists".into());
        }
        let [width, height] = frame_size(frame)?;
        let empty = crate::raster::Surface::zero(width, height, 8).map_err(|e| e.to_string())?;
        return crate::retained::transaction::seal(base, session, OutputDraft::new(empty, vec![]), check);
    }
    if rect != frame && metadata.profile().is_some() {
        rect[0] = 0;
        rect[2] = metadata.width() as i32;
    }
    stage_in_frame(base, session, rect, frame, Some(target), check)
}

fn frame_size(rect: [i32; 4]) -> Result<[u32; 2]> {
    let size = |axis: usize| {
        rect[axis + 2].checked_sub(rect[axis]).filter(|value| *value > 0)
            .map(|value| value as u32).ok_or("Invalid edge guide frame")
    };
    let [width, height] = [size(0)?, size(1)?];
    crate::raster::CanvasLimits::Native.check(width, height)?;
    Ok([width, height])
}

fn stage_in_frame(
    base: &NativeDocument,
    session: &SourceSession,
    rect: [i32; 4],
    frame: [i32; 4],
    target: Option<&str>,
    check: &dyn Fn() -> Result<()>,
) -> Result<StagedOutput> {
    check()?;
    let [width, height] = frame_size(rect)?;
    let [frame_width, frame_height] = frame_size(frame)?;
    let left = frame[0].max(rect[0]);
    let right = frame[2].min(rect[2]);
    if left >= right || rect[1] < frame[1] || rect[3] > frame[3] {
        return Err("Rendered edge guide does not overlap its output frame".into());
    }
    let source_x = (left - rect[0]) as usize * 4;
    let output_x = (left - frame[0]) as usize * 4;
    let copy_bytes = (right - left) as usize * 4;
    let first_row = (rect[1] - frame[1]) as u32;
    let header = MetadataCharge::reserve(base.budget(), base.metadata().id().len() + 1024, check)?;
    let scene = SceneSnapshot::from_native(
        SceneIdentity {
            project_id: String::new(),
            document_id: base.metadata().id().into(),
            revision: base.metadata().revision(),
        },
        base.clone(),
        vec![],
        None,
        None,
    )?;
    drop(header);
    let mut prepared = Prepared::capture(
        &scene,
        Some(rect),
        width.max(height),
        target,
        false,
        None,
        &mut || check(),
    )?;
    prepared.stream_grid(base.metadata().depth(), width.max(height));
    // Refinement requires exact document coordinates and one sample per pixel.
    if prepared.key.rect != rect || prepared.key.width != width || prepared.key.height != height {
        return Err("Edge refinement needs an image matching the mask frame".into());
    }
    let topology = Topology::for_preview(&scene, prepared.target)?;
    let stored = if target.is_none() && Stored::composite_needed(&topology, Some(rect)) {
        Some(Stored::prepare_composite(topology.clone(), session, Some(rect), &mut || check())?)
    } else if Stored::needed(&topology) {
        Some(Stored::prepare_cropped(topology.clone(), session, rect, &mut || check())?)
    } else {
        None
    };
    let result = crate::retained::transaction::stage(base, session, check, |store, checked| {
        let mut writer = crate::selection::NativeRows::new(frame_width, frame_height, 8, store.clone())
            .map_err(|e| e.to_string())?;
        let pad_row = frame_width as usize * 4 != copy_bytes;
        let bytes = if pad_row { frame_width as usize * 4 } else { 0 };
        let mut padded = ScratchBytes::new(base.budget(), bytes, bytes).map_err(|e| e.to_string())?;
        writer.zero_until(first_row, &mut || checked()).map_err(|e| e.to_string())?;
        prepared.render_stripes(&scene, Some(&topology), stored.as_ref(), None, true,
            &mut ReadChecks::caller_only(&mut || check()), |rows, bytes| {
            if bytes.is_none() {
                return writer.zero_rows(rows[1] - rows[0], &mut || checked())
                    .map_err(|e| e.to_string());
            }
            let bytes = super::complete_pixels(bytes)?;
            for (y, input) in (rows[0]..rows[1]).zip(bytes.chunks_exact(width as usize * 4)) {
                let input = &input[source_x..source_x + copy_bytes];
                let input = if pad_row {
                    padded[output_x..output_x + copy_bytes].copy_from_slice(input);
                    &padded[..]
                } else {
                    input
                };
                writer
                    .row8(first_row + y, input, &mut || checked())
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })?;
        writer.zero_until(frame_height, &mut || checked()).map_err(|e| e.to_string())?;
        writer.finish(&mut || checked()).map_err(|e| e.to_string())
    });
    let cleanup = stored.map_or(Ok(()), |stored| {
        stored.retire(&mut || session.cleanup_check())
    });
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => {
            Err(format!("{reason}; edge guide style retirement: {cleanup}"))
        }
        (Ok(output), Err(reason)) => match output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!("{reason}; edge guide source retirement: {cleanup}")),
        },
    }
}
