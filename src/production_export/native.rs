//! Bounded output rows shared by native File Export, batch export and previews.
use super::{native_png, native_rows, native_webp, Format, Output, PreviewGrid, Resampling, Settings};
use crate::{
    compositor::native::{Stored, Topology},
    engine::scene::SceneSnapshot,
    large_document::ScratchBytes,
    workspace::source_runtime::{SourceReadSession, SourceSession},
};
use std::io::Write;

pub(crate) const MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub(crate) fn size(width: u32, height: u32) -> Result<(), String> {
    if !(1..=16000).contains(&width) || !(1..=16000).contains(&height) {
        return Err("Native exports require 1–16000 pixels per edge".into());
    }
    Ok(())
}

pub(crate) fn canvas(scene: &SceneSnapshot, format: Format) -> Output {
    let doc = scene.metadata();
    Output {
        target: None,
        target_name: "composite".into(),
        variant: "native".into(),
        filename: String::new(),
        document_rect: [0, 0, doc.width() as i32, doc.height() as i32],
        width: doc.width(),
        height: doc.height(),
        format,
        native_source_depth: doc.depth(),
        bit_depth: format.bit_depth(doc.depth()),
        scale_xy: [1., 1.],
    }
}

fn with_rows<T>(
    scene: &SceneSnapshot,
    read: &SourceReadSession,
    target: Option<&str>,
    crop: Option<[i32; 4]>,
    needs_stage: bool,
    check: &dyn Fn() -> Result<(), String>,
    run: impl FnOnce(&mut native_rows::Rows<'_>, Option<&SourceSession>) -> Result<T, String>,
) -> Result<T, String> {
    let topology = Topology::for_composite(scene, target)?;
    let needs_effects = Stored::composite_needed(&topology, crop);
    let session = (needs_stage || needs_effects)
        .then(|| read.begin_derived(scene))
        .transpose()?;
    let stored = if needs_effects {
        Some(Stored::prepare_composite(
            topology.clone(),
            session.as_deref().ok_or("Export staging session missing")?,
            crop,
            &mut || check(),
        )?)
    } else {
        None
    };
    let result = (|| {
        let mut rows = native_rows::Rows::from_topology(topology, stored.as_ref(), check)?;
        run(&mut rows, session.as_deref())
    })();
    match stored {
        Some(stored) => finish(result, stored.retire(&mut || Ok(()))),
        None => result,
    }
}

fn filter_support(scene: &SceneSnapshot, output: &Output, method: Resampling) -> Result<[i32; 4], String> {
    let [l, t, r, b] = super::native_grid::Grid::document_support(output, method)?;
    let doc = scene.metadata();
    Ok([l.max(0), t.max(0), r.min(doc.width() as i32), b.min(doc.height() as i32)])
}

pub(crate) fn render(
    scene: &SceneSnapshot,
    read: &SourceReadSession,
    output: &Output,
    settings: &Settings,
    writer: &mut dyn Write,
    limit: u64,
    check: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<(), String> {
    settings.validate()?;
    size(output.width, output.height)?;
    with_rows(
        scene,
        read,
        output.target.as_deref(),
        Some(filter_support(scene, output, settings.resampling)?),
        output.format == Format::Jxl,
        check,
        |rows, session| match output.format {
            Format::Png => {
                native_png::write_to(scene, output, settings, rows, writer, limit, check)?
                    .recheck(check)
            }
            Format::Webp => {
                native_webp::write_to(scene, output, settings, rows, writer, limit, check)?
                    .recheck(check)
            }
            Format::Jxl => render_jxl(
                scene,
                output,
                settings,
                rows,
                session.ok_or("JPEG XL staging session missing")?,
                writer,
                limit,
                check,
            ),
        },
    )
}

pub(super) fn bounds(
    scene: &SceneSnapshot,
    read: &SourceReadSession,
    target: Option<&str>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<[i32; 4], String> {
    with_rows(scene, read, target, None, false, check, |rows, _| rows.alpha_bounds(check))
}

/// PNG and lossless WebP reproduce these sampled output pixels exactly. Keep
/// the planned sampling grid, including its padding and resampling settings.
pub(super) fn preview_lossless(
    scene: &SceneSnapshot,
    read: &SourceReadSession,
    output: &Output,
    settings: &Settings,
    grid: PreviewGrid,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(Vec<u8>, bool), String> {
    let depth = output.native_source_depth;
    let stride = usize::from(depth / 2);
    let sampled = (grid.scale < 1.).then_some(grid);
    let row_bytes = grid.width as usize * stride;
    // At the preview edge limit this keeps the native transfer within 128 KiB.
    let stripe_rows = native_png::stripe_rows(grid.width, grid.height).min(16);
    let capacity = row_bytes * stripe_rows as usize;
    let mut stripe =
        ScratchBytes::new(scene.budget(), capacity, capacity).map_err(|e| e.to_string())?;
    let mut words = Vec::new();
    words
        .try_reserve_exact(grid.width as usize * grid.height as usize * 4)
        .map_err(|_| "Native preview sample allocation refused")?;
    with_rows(
        scene,
        read,
        output.target.as_deref(),
        Some(filter_support(scene, output, settings.resampling)?),
        false,
        check,
        |rows, _| {
            for first in (0..grid.height).step_by(stripe_rows as usize) {
                check()?;
                let count = stripe_rows.min(grid.height - first);
                let samples = &mut stripe[..row_bytes * count as usize];
                rows.fill_sampled(
                    output,
                    depth,
                    settings.resampling,
                    first,
                    count,
                    sampled,
                    samples,
                    check,
                )?;
                for pixel in samples.chunks_exact(stride) {
                    for c in 0..4 {
                        let word = if depth == 8 {
                            u16::from(pixel[c]) * 257
                        } else {
                            u16::from_be_bytes([pixel[2 * c], pixel[2 * c + 1]])
                        };
                        words.push(if output.format == Format::Webp && depth == 16 {
                            u16::from(crate::raster::project16(word)) * 257
                        } else {
                            word
                        });
                    }
                }
            }
            Ok(())
        },
    )?;
    let managed = super::manage_preview(scene.metadata().profile(), &mut words)?;
    check()?;
    let rgba = crate::render::rgba8(grid.width, grid.height, |x, y| {
        let at = (y as usize * grid.width as usize + x as usize) * 4;
        crate::depth16::display_pixel(words[at..at + 4].try_into().unwrap())
    });
    Ok((rgba, managed))
}

fn render_jxl(
    scene: &SceneSnapshot,
    output: &Output,
    settings: &Settings,
    rows: &mut native_rows::Rows<'_>,
    session: &crate::workspace::source_runtime::SourceSession,
    writer: &mut dyn Write,
    limit: u64,
    check: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<(), String> {
    use crate::{
        jpeg_xl::{stream_parent, stream_protocol as wire},
        production_export::staged_rows::StagedNativeRows,
    };
    let raw =
        u64::from(output.width) * u64::from(output.height) * 4 * u64::from(output.bit_depth / 8);
    let limits = stream_parent::Limits {
        output_bytes: limit,
        log_bytes: 32 * 1024 * 1024,
        records: wire::MAX_RECORDS,
        input_bytes: wire::source_work_ceiling(raw)?,
        deadline_ms: wire::MAX_ELAPSED_MS,
    };
    let input = StagedNativeRows::stage(
        session,
        scene,
        output,
        settings,
        rows,
        wire::source_read_ceiling(raw)?,
        check,
    )?;
    let encoded = stream_parent::encode(input, limits, check)?;
    let result = (|| {
        encoded.recheck(check)?;
        let mut buffer = ScratchBytes::new(scene.budget(), wire::BUFFER_BYTES, wire::BUFFER_BYTES)
            .map_err(|e| e.to_string())?;
        let mut at = 0;
        while encoded.bytes() - at >= buffer.len() as u64 {
            encoded.copy_range(at, &mut buffer, check)?;
            writer.write_all(&buffer).map_err(|e| e.to_string())?;
            at += buffer.len() as u64;
        }
        if at < encoded.bytes() {
            let mut tail = ScratchBytes::new(
                scene.budget(),
                (encoded.bytes() - at) as usize,
                wire::BUFFER_BYTES,
            )
            .map_err(|e| e.to_string())?;
            encoded.copy_range(at, &mut tail, check)?;
            writer.write_all(&tail).map_err(|e| e.to_string())?;
        }
        check()
    })();
    finish(result, encoded.retire(&mut || session.cleanup_check()))
}

fn finish<T>(result: Result<T, String>, cleanup: Result<(), String>) -> Result<T, String> {
    match (result, cleanup) {
        (Err(reason), Err(cleanup)) => Err(format!("{reason}; export staging retained: {cleanup}")),
        (result, Ok(())) => result,
        (Ok(_), Err(cleanup)) => Err(cleanup),
    }
}
