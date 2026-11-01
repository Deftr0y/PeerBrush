// Copyright 2019 the Resvg Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::render::Context;

pub fn apply(
    clip: &usvg::ClipPath,
    transform: tiny_skia::Transform,
    pixmap: &mut tiny_skia::Pixmap,
) {
    let ctx = Context {
        max_bbox: tiny_skia::IntRect::from_xywh(0, 0, 1, 1).unwrap(),
        rows: None,
        prepared: None,
    };
    apply_with_context(clip, transform, pixmap, &ctx);
}
pub fn apply_with_context(
    clip: &usvg::ClipPath,
    transform: tiny_skia::Transform,
    pixmap: &mut tiny_skia::Pixmap,
    ctx: &Context,
) {
    let Some(clip_context) = ctx.child(2, clip as *const usvg::ClipPath as usize as u64) else {
        return;
    };
    let ctx = &clip_context;
    let Some(mut clip_pixmap) = ctx.new_pixmap(
        pixmap.width(),
        pixmap.height(),
        pixmap.stored_row_start(),
        pixmap.stored_row_count(),
    ) else {
        return;
    };
    clip_pixmap.fill(tiny_skia::Color::BLACK);

    draw_children(
        clip.root(),
        tiny_skia::BlendMode::Clear,
        transform.pre_concat(clip.transform()),
        &mut clip_pixmap.as_mut(),
        ctx,
    );

    if let Some(clip) = clip.clip_path() {
        apply_with_context(clip, transform, pixmap, ctx);
    }

    if clip_pixmap.row_storage_failed() {
        if let Some(rows) = ctx.rows {
            rows.refuse("SVG clip required an unprepared source row");
            return;
        }
    }
    let _mask_lease = if let Some(rows) = ctx.rows {
        let Some(lease) =
            rows.reserve(pixmap.width() as usize * pixmap.stored_row_count() as usize + 256)
        else {
            return;
        };
        Some(lease)
    } else {
        None
    };
    let mut mask = tiny_skia::Mask::from_pixmap(clip_pixmap.as_ref(), tiny_skia::MaskType::Alpha);
    mask.invert();
    pixmap.apply_mask(&mask);
    if mask.row_storage_failed() || pixmap.row_storage_failed() {
        if let Some(rows) = ctx.rows {
            rows.refuse("SVG clip mask escaped prepared rows");
        }
    }
}

fn draw_children(
    parent: &usvg::Group,
    mode: tiny_skia::BlendMode,
    transform: tiny_skia::Transform,
    pixmap: &mut tiny_skia::PixmapMut,
    ctx: &Context,
) {
    if !ctx.valid_transform(transform) {
        return;
    }
    for child in parent.children() {
        if !ctx.check() {
            return;
        }
        match child {
            usvg::Node::Path(path) => {
                if !path.is_visible() {
                    continue;
                }

                // We could use any values here. They will not be used anyway.
                let ctx = Context {
                    max_bbox: tiny_skia::IntRect::from_xywh(0, 0, 1, 1).unwrap(),
                    rows: ctx.rows,
                    prepared: ctx.prepared,
                };

                crate::path::fill_path(path, mode, &ctx, transform, pixmap);
            }
            usvg::Node::Text(text) => {
                draw_children(text.flattened(), mode, transform, pixmap, ctx);
            }
            usvg::Node::Group(group) => {
                let Some(group_context) =
                    ctx.child(1, group.as_ref() as *const usvg::Group as usize as u64)
                else {
                    return;
                };
                let ctx = &group_context;
                let transform = transform.pre_concat(group.transform());

                if let Some(clip) = group.clip_path() {
                    // If a `clipPath` child also has a `clip-path`
                    // then we should render this child on a new canvas,
                    // clip it, and only then draw it to the `clipPath`.
                    clip_group(group, clip, transform, pixmap, ctx);
                } else {
                    draw_children(group, mode, transform, pixmap, ctx);
                }
            }
            _ => {}
        }
    }
}

fn clip_group(
    children: &usvg::Group,
    clip: &usvg::ClipPath,
    transform: tiny_skia::Transform,
    pixmap: &mut tiny_skia::PixmapMut,
    ctx: &Context,
) -> Option<()> {
    let mut clip_pixmap = ctx.new_pixmap(
        pixmap.width(),
        pixmap.height(),
        pixmap.stored_row_start(),
        pixmap.stored_row_count(),
    )?;

    draw_children(
        children,
        tiny_skia::BlendMode::SourceOver,
        transform,
        &mut clip_pixmap.as_mut(),
        ctx,
    );
    apply_with_context(clip, transform, &mut clip_pixmap, ctx);

    let mut paint = tiny_skia::PixmapPaint::default();
    paint.blend_mode = tiny_skia::BlendMode::Xor;
    ctx.draw_pixmap(pixmap, 0, 0, clip_pixmap.as_ref(), &paint, 1);
    if clip_pixmap.row_storage_failed() {
        if let Some(rows) = ctx.rows {
            rows.refuse("SVG nested clip escaped prepared rows");
        }
    }

    Some(())
}
