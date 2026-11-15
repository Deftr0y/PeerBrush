//! Cumulative mask thumbnails from the original native channel sources.
use crate::{
    engine::{
        native::{LayerMetadata, NativeDocument},
        scene::SceneSnapshot,
    },
    large_document::{PinnedBytes, ScratchBytes},
    workspace::source_runtime::{ReadChecks, SourceReadSession},
};
use ring::digest::{Context, SHA256};
use std::io::{self, Write};
#[cfg(test)]
mod tests;
const EDGE: usize = 48;

/// The raw layer thumbnail uses the canvas grid. A step thumbnail uses the
/// layer-local grid and the prefix ending at that step. Both ignore mask bypass.
pub(crate) fn cache_key(
    doc: &NativeDocument,
    layer: LayerMetadata<'_>,
    step_id: Option<&str>,
) -> Option<[u8; 32]> {
    struct DigestWriter(Context, usize);
    impl Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.1 = self.1.checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("Mask thumbnail key exceeds its metadata bound"))?;
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let metadata = doc.metadata();
    if metadata.read_only() {
        return None;
    }
    let mask = layer.mask()?;
    let mut key = DigestWriter(Context::new(&SHA256), 16 * 1024);
    let pixel = layer.pixels();
    let canvas = step_id.is_none().then(|| {
        ([metadata.width(), metadata.height()], layer.origin(), metadata.depth())
    });
    serde_json::to_writer(&mut key, &(pixel.width(), pixel.height(), pixel.depth(), canvas)).ok()?;
    for step in mask.steps() {
        let paint = if step.kind() == "paint" && step.enabled() && step.weight() > 0. {
            Some(doc.source(step.pixels().slot()).ok()?.key().bytes())
        } else {
            None
        };
        serde_json::to_writer(
            &mut key,
            &(
                step.kind(),
                step.enabled(),
                step.weight().to_bits(),
                step.value().to_bits(),
                matches!(step.kind(), "curves" | "adjust" | "gaussian").then(|| step.settings()),
                paint,
            ),
        ).ok()?;
        if step_id == Some(step.id()) {
            return key.0.finish().as_ref().try_into().ok();
        }
    }
    if step_id.is_some() {
        return None;
    }
    key.0.finish().as_ref().try_into().ok()
}

pub(crate) struct Thumbnail {
    pub size: [u32; 2],
    pub pixels: PinnedBytes,
}
pub(crate) fn render(
    scene: &SceneSnapshot,
    session: &SourceReadSession,
    layer: &str,
    step: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Thumbnail, String> {
    session.check_scene(scene)?;
    let doc = scene.native_document()?;
    let layer = doc
        .metadata()
        .layers()
        .find(|entry| entry.id() == layer)
        .ok_or("Thumbnail layer disappeared")?;
    let mask = layer.mask().ok_or("Thumbnail mask disappeared")?;
    let count = mask
        .steps()
        .position(|entry| entry.id() == step)
        .ok_or("Thumbnail mask step disappeared")?
        + 1;
    let [width, height] = [layer.pixels().width(), layer.pixels().height()];
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err("Invalid mask thumbnail dimensions".into());
    }
    let scale = (EDGE as f64 / width.max(height) as f64).min(1.);
    let size = [
        (width as f64 * scale).round().max(1.) as u32,
        (height as f64 * scale).round().max(1.) as u32,
    ];
    let mut pixels = ScratchBytes::new(
        doc.budget(),
        size[0] as usize * size[1] as usize * 4,
        EDGE * EDGE * 4,
    )
    .map_err(|e| e.to_string())?;
    let axis: [i32; EDGE] = std::array::from_fn(|index| (index as f64 / scale) as i32);
    let columns = &axis[..size[0] as usize];
    let rows = &axis[..size[1] as usize];
    let row_bytes = size[0] as usize * 4;
    let mut cancel = || check();
    let mut checks = ReadChecks::with_session_after_cancel(session, &mut cancel);
    let grid = crate::mask::region::prepare_bake_grid(
        doc, layer, count, columns, rows, &mut checks,
    )?;
    for (y, output) in pixels.chunks_exact_mut(row_bytes).enumerate() {
        checks.pinned()?;
        for (x, pixel) in output.chunks_exact_mut(4).enumerate() {
            let gray = (grid.value16(x as i32, y as i32)? * 255.).round() as u8;
            pixel.copy_from_slice(&[gray, gray, gray, 255]);
        }
    }
    checks.full()?;
    Ok(Thumbnail {
        size,
        pixels: pixels.freeze(),
    })
}
