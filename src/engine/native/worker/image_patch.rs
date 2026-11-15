//! Patch sources are temporary, admitted inputs; only changed tiles survive.
use crate::{
    edit_bounds::{
        input::{restrict_tile, RasterInput, WriteGuard},
        output::{BuiltTiles, TileOutput},
    },
    engine::image_patch::{precision, Placement},
    image_import::{
        png_admission,
        prepared_png::{ColorPolicy, FilePng},
    },
    large_document::{Error, ScratchBytes},
    raster::{backed::Region, Surface, TILE},
    workspace::source_runtime::SourceSession,
};

pub(super) fn stage<'d, 's>(
    input: &RasterInput<'d>,
    guard: &WriteGuard<'_>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<BuiltTiles<'d, 's>, String> {
    guard.validate(input, None, &["image.patch"])?;
    if input.is_mask() || input.layer_metadata().kind() != "paint" {
        return Err("Image patches require a color paint layer".into());
    }
    let placement = Placement::new(guard.parameters())?;
    let path = guard.parameters()["path"]
        .as_str()
        .ok_or("Image patch path missing")?;
    // shortcut: native patches admit PNG and bounded file producers; enable other codecs
    // when their decoder allocations and file ownership have native admission.
    let mut checked = || {
        guard.check_active()?;
        session.check()?;
        check()?;
        guard.check_active()
    };
    let checkpoint = std::cell::RefCell::new(&mut checked);
    let source_path = std::path::Path::new(path);
    let streamed = if source_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
    {
        Some(crate::image_import::gif::prepare_patch(
            input.document(),
            session,
            source_path,
            &|| checkpoint.borrow_mut()(),
        ))
    } else {
        crate::image_import::native::stream_file(input.document(), session, source_path, &|| {
            checkpoint.borrow_mut()()
        })
    };
    drop(checkpoint);
    if let Some(source) = streamed {
        let source = source?;
        let built = precision(source.surface().depth(), input.depth()).and_then(|()| {
            render(
                input,
                guard,
                session,
                source.surface(),
                &placement,
                &mut checked,
            )
        });
        return match (built, source.abort(session, &|| session.cleanup_check())) {
            (Ok(output), Ok(())) => Ok(output),
            (Err(reason), cleanup) => Err(super::cleanup(reason, cleanup)),
            (Ok(output), Err(reason)) => Err(super::cleanup(reason, abort(output))),
        };
    }
    let mut file = FilePng::open(
        session,
        std::path::Path::new(path),
        crate::image_import::prepared_png::apng::NATIVE_OPEN_LIMITS,
        ColorPolicy::RawSelection,
        &mut checked,
    )?;
    let source = png_admission::stage_file(session, &mut file, &mut checked)?;
    let built = (|| {
        precision(source.surface().depth(), input.depth())?;
        let output = render(
            input,
            guard,
            session,
            source.surface(),
            &placement,
            &mut checked,
        )?;
        if let Err(reason) = checked().and_then(|()| file.recheck()) {
            return Err(super::cleanup(reason, abort(output)));
        }
        Ok(output)
    })();
    // Retirement is required on no-op, cancellation, decode/precision failure,
    // and successful output alike. The published patch never retains this PNG.
    match (built, source.abort(session)) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(reason), cleanup) => Err(super::cleanup(reason, cleanup)),
        (Ok(output), Err(reason)) => Err(super::cleanup(reason, abort(output))),
    }
}

fn abort(output: BuiltTiles<'_, '_>) -> Result<(), String> {
    match output {
        BuiltTiles::Unchanged => Ok(()),
        BuiltTiles::Draft(draft) => draft.abort(),
    }
}

fn render<'d, 's>(
    input: &RasterInput<'d>,
    guard: &WriteGuard<'_>,
    session: &'s SourceSession,
    source: &Surface,
    placement: &Placement,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<BuiltTiles<'d, 's>, String> {
    let selection = input.selection()?;
    let origin = input.origin();
    let Some(area) = placement.clip(
        [source.width(), source.height()],
        origin,
        input.shape(),
        selection.bounds(),
    )?
    else {
        check()?;
        return Ok(BuiltTiles::Unchanged);
    };
    let clip = [
        area[0] - origin[0],
        area[1] - origin[1],
        area[2] - origin[0],
        area[3] - origin[1],
    ];
    let mut output = TileOutput::new(input, session)?;
    let result = (|| {
        for ty in clip[1] as u32 / TILE..=(clip[3] - 1) as u32 / TILE {
            for tx in clip[0] as u32 / TILE..=(clip[2] - 1) as u32 / TILE {
                check()?;
                let key = (tx, ty);
                let left = (tx * TILE) as i32;
                let top = (ty * TILE) as i32;
                let local = [
                    clip[0].max(left),
                    clip[1].max(top),
                    clip[2].min(left + TILE as i32),
                    clip[3].min(top + TILE as i32),
                ];
                let sx = |x: i32| {
                    (i64::from(x) + i64::from(origin[0]) - i64::from(placement.origin[0])) as i32
                };
                let sy = |y: i32| {
                    (i64::from(y) + i64::from(origin[1]) - i64::from(placement.origin[1])) as i32
                };
                let source_rect =
                    Region::new([sx(local[0]), sy(local[1]), sx(local[2]), sy(local[3])])
                        .map_err(|e| e.to_string())?;
                let source = source
                    .prepare_region(source_rect, &mut || {
                        check().map_err(|e| Error::Io(std::io::Error::other(e)))
                    })
                    .map_err(|e| e.to_string())?;
                let before = input.prepare_tile(key, check)?;
                let bytes = before.bytes();
                let mut after = ScratchBytes::new(input.pool(), bytes.len(), bytes.len())
                    .map_err(|e| e.to_string())?;
                after.copy_from_slice(bytes);
                let coverage = if selection.has_coverage() {
                    Some(selection.prepare(input.tile_document_rect(key)?, check)?)
                } else {
                    None
                };
                let bpp = usize::from(input.depth() / 8) * 4;
                for y in local[1]..local[3] {
                    check()?;
                    for x in local[0]..local[2] {
                        let (dx, dy) = (x + origin[0], y + origin[1]);
                        let selected = if let Some(coverage) = &coverage {
                            coverage.value(dx, dy)? > 0.
                        } else {
                            selection.polygon().is_none_or(|p| {
                                crate::selection::contains(p, dx as f32 + 0.5, dy as f32 + 0.5)
                            })
                        };
                        if !selected {
                            continue;
                        }
                        let at = ((y - top) as usize * TILE as usize + (x - left) as usize) * bpp;
                        if input.depth() == 16 {
                            let old = std::array::from_fn(|c| {
                                u16::from_be_bytes([bytes[at + 2 * c], bytes[at + 2 * c + 1]])
                            });
                            let pixel = crate::raster::blend16(
                                old,
                                source.pixel16(sx(x), sy(y)).map_err(|e| e.to_string())?,
                                1.,
                                "normal",
                            );
                            for (c, value) in pixel.into_iter().enumerate() {
                                after[at + c * 2..at + c * 2 + 2]
                                    .copy_from_slice(&value.to_be_bytes());
                            }
                        } else {
                            let old = std::array::from_fn(|c| bytes[at + c]);
                            let pixel = crate::raster::blend(
                                old,
                                source.pixel(sx(x), sy(y)).map_err(|e| e.to_string())?,
                                1.,
                                "normal",
                            );
                            after[at..at + 4].copy_from_slice(&pixel);
                        }
                    }
                }
                let mut after = after.freeze();
                if let Some(coverage) = &coverage {
                    after = restrict_tile(input, key, bytes, &after, coverage, check)?;
                }
                guard.check_tile(input, None, key, bytes, &after, check)?;
                output.replace_tile(&before, after, check)?;
            }
        }
        check()
    })();
    if let Err(reason) = result {
        return Err(super::cleanup(reason, output.abort()));
    }
    output
        .finish(check)
        .map_err(|refused| super::cleanup(refused.reason().to_owned(), refused.abort()))
}
