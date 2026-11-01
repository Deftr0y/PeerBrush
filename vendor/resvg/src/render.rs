// Copyright 2018 the Resvg Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::OptionLog;

#[derive(Clone, Copy)]
pub(crate) struct PreparedContext<'a> {
    pub cache: &'a tiny_skia::ScanCache,
    pub key: tiny_skia::InvocationKey,
}
pub struct Context<'a> {
    pub max_bbox: tiny_skia::IntRect,
    pub rows: Option<&'a crate::rows::RowContext<'a>>,
    pub prepared: Option<PreparedContext<'a>>,
}
impl<'a> Context<'a> {
    pub(crate) fn child(&self, tag: u8, site: u64) -> Option<Self> {
        let prepared = if let Some(prepared) = self.prepared {
            let Some(key) = prepared.key.child(tag, site) else {
                prepared
                    .cache
                    .refuse("SVG invocation ancestry exceeded admission");
                if let Some(rows) = self.rows {
                    rows.refuse("SVG invocation ancestry exceeded admission");
                }
                return None;
            };
            Some(PreparedContext {
                cache: prepared.cache,
                key,
            })
        } else {
            None
        };
        Some(Self {
            max_bbox: self.max_bbox,
            rows: self.rows,
            prepared,
        })
    }
    pub(crate) fn path_key(
        &self,
        path: &usvg::Path,
        phase: u64,
    ) -> Option<tiny_skia::InvocationKey> {
        let key = self
            .prepared?
            .key
            .child(3, path as *const usvg::Path as usize as u64)
            .and_then(|key| key.child(4, phase));
        if key.is_none() {
            self.prepared?
                .cache
                .refuse("SVG invocation ancestry exceeded admission");
            if let Some(rows) = self.rows {
                rows.refuse("SVG invocation ancestry exceeded admission");
            }
        }
        key
    }

    pub(crate) fn draw_pixmap(
        &self,
        destination: &mut tiny_skia::PixmapMut,
        x: i32,
        y: i32,
        source: tiny_skia::PixmapRef,
        paint: &tiny_skia::PixmapPaint,
        site: u64,
    ) {
        if let Some(prepared) = self.prepared {
            let Some(key) = prepared.key.child(5, site) else {
                prepared
                    .cache
                    .refuse("SVG invocation ancestry exceeded admission");
                if let Some(rows) = self.rows {
                    rows.refuse("SVG invocation ancestry exceeded admission");
                }
                return;
            };
            if let Err(error) = destination.draw_pixmap_prepared(
                x,
                y,
                source,
                paint,
                tiny_skia::Transform::identity(),
                None,
                prepared.cache,
                key,
            ) {
                if let Some(rows) = self.rows {
                    rows.refuse(&error);
                }
            }
        } else {
            destination.draw_pixmap(x, y, source, paint, tiny_skia::Transform::identity(), None);
        }
    }
    pub(crate) fn new_pixmap(
        &self,
        w: u32,
        h: u32,
        start: u32,
        count: u32,
    ) -> Option<crate::rows::OwnedPixmap> {
        if let Some(rows) = self.rows {
            rows.new_pixmap(w, h, start, count)
        } else {
            tiny_skia::Pixmap::new(w, h).map(|pixmap| crate::rows::OwnedPixmap {
                pixmap,
                _lease: None,
            })
        }
    }
    pub(crate) fn check(&self) -> bool {
        if let Some(prepared) = self.prepared {
            if let Err(error) = prepared.cache.status() {
                if let Some(rows) = self.rows {
                    rows.refuse(&error);
                }
                return false;
            }
        }
        self.rows.is_none_or(|rows| rows.check())
    }
    pub(crate) fn valid_transform(&self, transform: tiny_skia::Transform) -> bool {
        if self.rows.is_some() && !transform.is_finite() {
            self.rows
                .unwrap()
                .refuse("SVG transform overflowed bounded rendering");
            false
        } else {
            true
        }
    }
}

pub fn render_nodes(
    parent: &usvg::Group,
    ctx: &Context,
    transform: tiny_skia::Transform,
    pixmap: &mut tiny_skia::PixmapMut,
) {
    for node in parent.children() {
        render_node(node, ctx, transform, pixmap);
    }
}

pub fn render_node(
    node: &usvg::Node,
    ctx: &Context,
    transform: tiny_skia::Transform,
    pixmap: &mut tiny_skia::PixmapMut,
) {
    if !ctx.check() {
        return;
    }
    match node {
        usvg::Node::Group(group) => {
            if let Some(child) = ctx.child(1, group.as_ref() as *const usvg::Group as usize as u64)
            {
                render_group(group, &child, transform, pixmap);
            }
        }
        usvg::Node::Path(path) => {
            crate::path::render(
                path,
                tiny_skia::BlendMode::SourceOver,
                ctx,
                transform,
                pixmap,
            );
        }
        usvg::Node::Image(image) => {
            crate::image::render(image, transform, pixmap);
        }
        usvg::Node::Text(text) => {
            render_group(text.flattened(), ctx, transform, pixmap);
        }
    }
}

fn render_group(
    group: &usvg::Group,
    ctx: &Context,
    transform: tiny_skia::Transform,
    pixmap: &mut tiny_skia::PixmapMut,
) -> Option<()> {
    let transform = transform.pre_concat(group.transform());
    if !ctx.valid_transform(transform) {
        return None;
    }

    if !group.should_isolate() {
        render_nodes(group, ctx, transform, pixmap);
        return Some(());
    }

    let bbox = group.layer_bounding_box().transform(transform)?;

    let mut ibbox = if group.filters().is_empty() {
        // Convert group bbox into an integer one, expanding each side outwards by 2px
        // to make sure that anti-aliased pixels would not be clipped.
        tiny_skia::IntRect::from_xywh(
            (bbox.x().floor() as i32).checked_sub(2)?,
            (bbox.y().floor() as i32).checked_sub(2)?,
            (bbox.width().ceil() as u32).checked_add(4)?,
            (bbox.height().ceil() as u32).checked_add(4)?,
        )?
    } else {
        // The bounding box for groups with filters is special and should not be expanded by 2px,
        // because it's already acting as a clipping region.
        let bbox = tiny_skia::IntRect::from_xywh(
            bbox.x().floor() as i32,
            bbox.y().floor() as i32,
            bbox.width().ceil().max(1.0) as u32,
            bbox.height().ceil().max(1.0) as u32,
        )?;
        // Make sure our filter region is not bigger than 4x the canvas size.
        // This is required mainly to prevent huge filter regions that would tank the performance.
        // It should not affect the final result in any way.
        crate::geom::fit_to_rect(bbox, ctx.max_bbox)?
    };

    // Make sure our layer is not bigger than 4x the canvas size.
    // This is required to prevent huge layers.
    if group.filters().is_empty() {
        ibbox = crate::geom::fit_to_rect(ibbox, ctx.max_bbox)?;
    }

    let shift_ts = {
        // Original shift.
        let mut dx = bbox.x();
        let mut dy = bbox.y();

        // Account for subpixel positioned layers.
        dx -= bbox.x() - ibbox.x() as f32;
        dy -= bbox.y() - ibbox.y() as f32;

        tiny_skia::Transform::from_translate(-dx, -dy)
    };

    let transform = shift_ts.pre_concat(transform);

    let (start, count) = if ctx.rows.is_some() {
        let top = (i64::from(pixmap.stored_row_start()) - i64::from(ibbox.y())).max(0);
        let bottom = (i64::from(pixmap.stored_row_start() + pixmap.stored_row_count())
            - i64::from(ibbox.y()))
        .min(i64::from(ibbox.height()));
        if top >= bottom {
            return Some(());
        }
        (top as u32, (bottom - top) as u32)
    } else {
        (0, ibbox.height())
    };
    let mut sub_pixmap = ctx
        .new_pixmap(ibbox.width(), ibbox.height(), start, count)
        .log_none(|| log::warn!("Failed to allocate a group layer for: {:?}.", ibbox))?;

    render_nodes(group, ctx, transform, &mut sub_pixmap.as_mut());

    if !group.filters().is_empty() {
        for filter in group.filters() {
            crate::filter::apply(filter, transform, &mut sub_pixmap);
        }
    }

    if let Some(clip_path) = group.clip_path() {
        crate::clip::apply_with_context(clip_path, transform, &mut sub_pixmap, ctx);
    }

    if let Some(mask) = group.mask() {
        crate::mask::apply(mask, ctx, transform, &mut sub_pixmap);
    }

    let paint = tiny_skia::PixmapPaint {
        opacity: group.opacity().get(),
        blend_mode: convert_blend_mode(group.blend_mode()),
        quality: tiny_skia::FilterQuality::Nearest,
    };

    ctx.draw_pixmap(pixmap, ibbox.x(), ibbox.y(), sub_pixmap.as_ref(), &paint, 0);
    if sub_pixmap.row_storage_failed() {
        if let Some(rows) = ctx.rows {
            rows.refuse("SVG group required an unprepared source row");
        }
    }

    Some(())
}

pub fn convert_blend_mode(mode: usvg::BlendMode) -> tiny_skia::BlendMode {
    match mode {
        usvg::BlendMode::Normal => tiny_skia::BlendMode::SourceOver,
        usvg::BlendMode::Multiply => tiny_skia::BlendMode::Multiply,
        usvg::BlendMode::Screen => tiny_skia::BlendMode::Screen,
        usvg::BlendMode::Overlay => tiny_skia::BlendMode::Overlay,
        usvg::BlendMode::Darken => tiny_skia::BlendMode::Darken,
        usvg::BlendMode::Lighten => tiny_skia::BlendMode::Lighten,
        usvg::BlendMode::ColorDodge => tiny_skia::BlendMode::ColorDodge,
        usvg::BlendMode::ColorBurn => tiny_skia::BlendMode::ColorBurn,
        usvg::BlendMode::HardLight => tiny_skia::BlendMode::HardLight,
        usvg::BlendMode::SoftLight => tiny_skia::BlendMode::SoftLight,
        usvg::BlendMode::Difference => tiny_skia::BlendMode::Difference,
        usvg::BlendMode::Exclusion => tiny_skia::BlendMode::Exclusion,
        usvg::BlendMode::Hue => tiny_skia::BlendMode::Hue,
        usvg::BlendMode::Saturation => tiny_skia::BlendMode::Saturation,
        usvg::BlendMode::Color => tiny_skia::BlendMode::Color,
        usvg::BlendMode::Luminosity => tiny_skia::BlendMode::Luminosity,
    }
}

#[cfg(test)]
mod tests {

    // Derived from https://github.com/servo/servo/issues/42258.
    #[test]
    fn filter_bbox_outside_int_rect() {
        let svg = r#"<svg filter="url(#f)"><filter id="f" x="2em"><feFlood/></filter><path d="M0 0H1e8V1"/></svg>"#;
        let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).unwrap();
        let mut pixmap = tiny_skia::Pixmap::new(1, 1).unwrap();

        // Just make sure we don't panic.
        crate::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
    }
}
