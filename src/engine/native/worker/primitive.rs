//! Bounded primitive rasterization; the paint coordinator owns adoption/history.
use crate::{
    edit_bounds::{
        input::{restrict_frame_tile, restrict_tile, InputFrame, RasterInput, WriteGuard},
        output::{stage_frame_with, BuiltTiles, TileOutput},
    },
    engine::primitive::Primitive,
    fill::input::{FillBaseline, FillStage},
    large_document::{PinnedBytes, ScratchBytes},
    raster::TILE,
    workspace::source_runtime::SourceSession,
};

pub(super) fn stage<'d, 's>(
    input: &RasterInput<'d>,
    frame: Option<&InputFrame>,
    guard: &WriteGuard<'_>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
    check_active: &dyn Fn() -> Result<(), String>,
) -> Result<FillStage<'d, 's>, String> {
    guard.validate(
        input,
        frame,
        &["shape", "gradient", "fill", "adjust", "pixels.replace"],
    )?;
    if guard
        .parameters()
        .get("rect")
        .is_some_and(|r| !r.is_null() && crate::engine::rect(r).is_none())
    {
        return Err("Invalid rectangle coordinates".into());
    }
    let adjusting = guard.parameters()["op"] == "adjust";
    if adjusting && (input.is_mask() || frame.is_some()) {
        return Err(
            "Destructive adjustment requires an existing color raster; use mask effects for masks"
                .into(),
        );
    }
    let baseline = if guard.parameters()["op"] == "fill" {
        FillBaseline::capture(input)?
    } else {
        None
    };
    if !input.is_mask() && input.layer_metadata().kind() != "paint" && baseline.is_none() {
        return Err("Paint on a paint layer or its mask".into());
    }
    let mut checked = || {
        guard.check_active()?;
        session.check()?;
        check()?;
        guard.check_active()
    };
    let mut pinned = || {
        guard.check_active()?;
        session.check_pinned_work()?;
        check_active()?;
        guard.check_active()
    };
    let selection = input.selection()?;
    let replacement = if guard.parameters()["op"] == "pixels.replace" {
        Some(super::replacement::Replacement::new(
            input,
            guard.parameters(),
            &mut checked,
        )?)
    } else {
        None
    };
    let metadata = input.document().metadata();
    let area = guard
        .parameters()
        .get("rect")
        .and_then(crate::engine::rect)
        .or(selection.bounds())
        .unwrap_or(if adjusting {
            [
                input.origin()[0],
                input.origin()[1],
                input.origin()[0] + input.shape()[0] as i32,
                input.origin()[1] + input.shape()[1] as i32,
            ]
        } else {
            [0, 0, metadata.width() as i32, metadata.height() as i32]
        });
    let primitive = Primitive::new(guard.parameters(), area, input.depth())?;
    let mut clip = area;
    if let Some(bounds) = selection.bounds() {
        clip = [
            clip[0].max(bounds[0]),
            clip[1].max(bounds[1]),
            clip[2].min(bounds[2]),
            clip[3].min(bounds[3]),
        ];
    }
    // The resident command clips raw mask edits to the color layer's frame too.
    let color = input.layer_metadata().pixels();
    let (origin, shape) = if let Some(frame) = frame {
        (frame.geometry().new_origin(), frame.geometry().new_shape())
    } else {
        (
            input.origin(),
            [
                input.shape()[0].min(color.width()),
                input.shape()[1].min(color.height()),
            ],
        )
    };
    clip = [
        clip[0] - origin[0],
        clip[1] - origin[1],
        clip[2] - origin[0],
        clip[3] - origin[1],
    ];
    clip = [
        clip[0].max(0),
        clip[1].max(0),
        clip[2].min(shape[0] as i32),
        clip[3].min(shape[1] as i32),
    ];
    if (clip[0] >= clip[2] || clip[1] >= clip[3]) && baseline.is_none() {
        checked()?;
        return Ok(FillStage {
            output: BuiltTiles::Unchanged,
            baseline,
        });
    }
    let mut render =
        |key: (u32, u32), bytes: &PinnedBytes, check: &mut dyn FnMut() -> Result<(), String>| {
            let semantic_before = if let Some(baseline) = &baseline {
                baseline.tile(input, frame, key, check)?
            } else {
                bytes.clone()
            };
            let bytes = &semantic_before;
            let coverage = if selection.has_coverage() {
                Some(selection.prepare(
                    if let Some(frame) = frame {
                        frame.tile_document_rect(key)?
                    } else {
                        input.tile_document_rect(key)?
                    },
                    check,
                )?)
            } else {
                None
            };
            let (tx, ty) = key;
            let mut after = ScratchBytes::new(input.pool(), bytes.len(), bytes.len())
                .map_err(|e| e.to_string())?;
            after.copy_from_slice(bytes);
            let left = (tx * TILE) as i32;
            let top = (ty * TILE) as i32;
            let bpp = if input.depth() == 16 { 8 } else { 4 };
            for y in clip[1].max(top)..clip[3].min(top + TILE as i32) {
                pinned()?;
                for x in clip[0].max(left)..clip[2].min(left + TILE as i32) {
                    let dx = x + origin[0];
                    let dy = y + origin[1];
                    if !primitive.contains(dx, dy)
                        || (!(replacement.is_some() && selection.has_coverage())
                            && selection.polygon().is_some_and(|p| {
                                !crate::selection::contains(p, dx as f32 + 0.5, dy as f32 + 0.5)
                            }))
                    {
                        continue;
                    }
                    if replacement.is_some()
                        && coverage
                            .as_ref()
                            .map(|c| c.value(dx, dy))
                            .transpose()?
                            .is_some_and(|v| v <= 0.)
                    {
                        continue;
                    }
                    let at = ((y - top) as usize * TILE as usize + (x - left) as usize) * bpp;
                    if input.depth() == 16 {
                        let old = std::array::from_fn(|c| {
                            u16::from_be_bytes([bytes[at + c * 2], bytes[at + c * 2 + 1]])
                        });
                        let pixel = replacement
                            .as_ref()
                            .map_or_else(|| primitive.blend16(dx, dy, old), |p| p.pixel(dx, dy));
                        for (c, value) in pixel.into_iter().enumerate() {
                            after[at + c * 2..at + c * 2 + 2].copy_from_slice(&value.to_be_bytes());
                        }
                    } else {
                        let old = std::array::from_fn(|c| bytes[at + c]);
                        let pixel = replacement.as_ref().map_or_else(
                            || primitive.blend8(dx, dy, old),
                            |p| p.pixel(dx, dy).map(crate::raster::project16),
                        );
                        after[at..at + 4].copy_from_slice(&pixel);
                    }
                }
            }
            let mut after = after.freeze();
            if let Some(coverage) = &coverage {
                after = if let Some(frame) = frame {
                    restrict_frame_tile(input, frame, key, bytes, &after, coverage, &mut pinned)?
                } else {
                    restrict_tile(input, key, bytes, &after, coverage, &mut pinned)?
                };
            }
            guard.check_tile(input, frame, key, bytes, &after, &mut pinned)?;
            check()?;
            Ok(after)
        };
    if let Some(frame) = frame {
        let output = BuiltTiles::Draft(stage_frame_with(
            input,
            frame,
            session,
            &mut checked,
            None,
            render,
        )?);
        return Ok(FillStage { output, baseline });
    }
    let mut output = TileOutput::new(input, session)?;
    let tiles = if baseline.is_some() {
        [0, 0, shape[0] as i32, shape[1] as i32]
    } else {
        clip
    };
    let result = (|| {
        let mut update = |key| -> Result<(), String> {
            checked()?;
            let before = input.prepare_tile(key, &mut checked)?;
            let after = render(key, before.bytes(), &mut checked)?;
            output.replace_tile(&before, after, &mut checked)
        };
        if adjusting {
            for tile in input
                .document()
                .source(input.metadata().slot())?
                .physical_tiles()
            {
                let (tx, ty) = tile.coordinate();
                if (tx * TILE) as i32 >= clip[2]
                    || (ty * TILE) as i32 >= clip[3]
                    || ((tx + 1) * TILE) as i32 <= clip[0]
                    || ((ty + 1) * TILE) as i32 <= clip[1]
                {
                    continue;
                }
                update((tx, ty))?;
            }
        } else {
            for ty in tiles[1] as u32 / TILE..=(tiles[3] - 1) as u32 / TILE {
                for tx in tiles[0] as u32 / TILE..=(tiles[2] - 1) as u32 / TILE {
                    update((tx, ty))?;
                }
            }
        }
        checked()
    })();
    if let Err(reason) = result {
        return Err(super::cleanup(reason, output.abort()));
    }
    let output = if baseline.is_some() {
        output.finish_for_metadata(&mut checked)
    } else {
        output.finish(&mut checked)
    }
    .map_err(|refused| super::cleanup(refused.reason().to_owned(), refused.abort()))?;
    Ok(FillStage { output, baseline })
}
