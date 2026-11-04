// Copyright 2026 the PeerBrush contributors.
// Same BSD-3-Clause terms as the containing pinned tiny-skia package.
use super::{Binding, InvocationKey, Owner, ScanCache};
use crate::{geom::ScreenIntRect, painter::DrawTiler, FillRule, Path, Stroke, Transform};
use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::Ordering;

pub(super) struct Entry {
    key: InvocationKey,
    value: PreparedGeometry,
}
pub(crate) struct Piece {
    pub tile: Option<ScreenIntRect>,
    pub path: Path,
}
struct Payload {
    binding: Binding,
    stroke: Option<(Stroke, bool)>,
    matrix: [u32; 6],
    frame: [u32; 2],
    pieces: Vec<Piece>,
    _owner: Arc<Owner>,
}
#[derive(Clone)]
pub(crate) struct PreparedGeometry(Arc<Payload>);
impl PreparedGeometry {
    pub fn pieces(&self) -> &[Piece] {
        &self.0.pieces
    }
}
fn matrix(value: Transform) -> [u32; 6] {
    [value.sx, value.ky, value.kx, value.sy, value.tx, value.ty].map(f32::to_bits)
}
fn stroke_matches(a: Option<&(Stroke, bool)>, b: Option<(&Stroke, bool)>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some((a, mode_a)), Some((b, mode_b))) => {
            mode_a == &mode_b
                && a.width.to_bits() == b.width.to_bits()
                && a.miter_limit.to_bits() == b.miter_limit.to_bits()
                && a.line_cap == b.line_cap
                && a.line_join == b.line_join
                && match (&a.dash, &b.dash) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a.matches_bits(b),
                    _ => false,
                }
        }
        _ => false,
    }
}
fn path_bytes(path: &Path) -> Option<usize> {
    // Every retained Path is a fresh checked clone: clone_vec admits exactly
    // its length and refuses any allocator capacity larger than that request.
    path.verbs()
        .len()
        .checked_mul(core::mem::size_of::<tiny_skia_path::PathVerb>())?
        .checked_add(
            path.points()
                .len()
                .checked_mul(core::mem::size_of::<crate::Point>())?,
        )
}
fn build(
    path: &Path,
    stroke: Option<(&Stroke, bool)>,
    transform: Transform,
    frame: [u32; 2],
    owner: Arc<Owner>,
) -> Result<(PreparedGeometry, usize), String> {
    let mut base = if let Some((stroke, hairline)) = stroke {
        let scale = tiny_skia_path::PathStroker::compute_resolution_scale(&transform);
        let dashed;
        let input = if let Some(dash) = &stroke.dash {
            dashed = path
                .dash(dash, scale)
                .ok_or("SVG prepared path dashing failed")?;
            &dashed
        } else {
            path
        };
        if hairline {
            input.clone()
        } else {
            input
                .stroke(stroke, scale)
                .ok_or("SVG prepared path stroking failed")?
        }
    } else {
        path.clone()
    };
    if !transform.is_identity() {
        base = base
            .transform(transform)
            .ok_or("SVG prepared geometry transform failed")?;
    }
    let filling = stroke.is_none_or(|(_, hairline)| !hairline);
    if filling && crate::painter::is_too_big_for_math(&base) {
        return Err("SVG prepared path kernel refused its input".into());
    }
    let mut pieces = Vec::new();
    if filling
        && (tiny_skia_path::Scalar::is_nearly_zero(base.bounds().width())
            || tiny_skia_path::Scalar::is_nearly_zero(base.bounds().height()))
    {
        // Preserve the original transformed fill's empty/axis-line no-op.
    } else if let Some(tiler) = DrawTiler::new(frame[0], frame[1]) {
        for tile in tiler {
            tiny_skia_path::allocation::checkpoint();
            // Preserve both original consuming transforms in original tile
            // order, including inactive tiles. Never derive a later tile by
            // translating the original path directly to that tile.
            base = base
                .transform(Transform::from_translate(
                    -(tile.x() as f32),
                    -(tile.y() as f32),
                ))
                .ok_or("SVG prepared tile transform failed")?;
            tiny_skia_path::allocation::push(
                &mut pieces,
                Piece {
                    tile: Some(tile),
                    path: base.clone(),
                },
            );
            base = base
                .transform(Transform::from_translate(tile.x() as f32, tile.y() as f32))
                .ok_or("SVG prepared tile restoration failed")?;
        }
    } else {
        tiny_skia_path::allocation::push(
            &mut pieces,
            Piece {
                tile: None,
                path: base.clone(),
            },
        );
    }
    let clip = ScreenIntRect::from_xywh(0, 0, frame[0], frame[1])
        .ok_or("SVG prepared frame is invalid")?;
    let binding = Binding {
        path: path.clone(),
        clip: [clip.x(), clip.y(), clip.width(), clip.height()],
        start: 0,
        stop: 0,
        shift: 0,
        contained: false,
        rule: FillRule::Winding,
    };
    let retained_stroke = stroke.map(|(stroke, mode)| (stroke.clone(), mode));
    let mut bytes = core::mem::size_of::<Payload>()
        .checked_add(2 * core::mem::size_of::<usize>())
        .and_then(|n| n.checked_add(path_bytes(&binding.path)?))
        .and_then(|n| {
            n.checked_add(
                pieces
                    .capacity()
                    .checked_mul(core::mem::size_of::<Piece>())?,
            )
        })
        .ok_or("SVG prepared geometry size overflow")?;
    for piece in &pieces {
        tiny_skia_path::allocation::checkpoint();
        bytes = bytes
            .checked_add(path_bytes(&piece.path).ok_or("SVG prepared geometry size overflow")?)
            .ok_or("SVG prepared geometry size overflow")?;
    }
    if let Some((stroke, _)) = &retained_stroke {
        if let Some(dash) = &stroke.dash {
            bytes = bytes
                .checked_add(
                    dash.allocation_bytes()
                        .ok_or("SVG prepared dash size overflow")?,
                )
                .ok_or("SVG prepared dash size overflow")?;
        }
    }
    tiny_skia_path::allocation::charge(
        core::mem::size_of::<Payload>() + 2 * core::mem::size_of::<usize>(),
    );
    Ok((
        PreparedGeometry(Arc::new(Payload {
            binding,
            stroke: retained_stroke,
            matrix: matrix(transform),
            frame,
            pieces,
            _owner: owner,
        })),
        bytes,
    ))
}
impl ScanCache {
    pub(crate) fn geometry(
        &self,
        key: InvocationKey,
        path: &Path,
        stroke: Option<(&Stroke, bool)>,
        transform: Transform,
        frame: [u32; 2],
    ) -> Option<PreparedGeometry> {
        if !tiny_skia_path::allocation::is_active() {
            self.refuse("SVG retained geometry requires an admitted Rust scope");
            return None;
        }
        let Ok(mut state) = self.state.try_lock() else {
            self.failed.store(true, Ordering::Release);
            return None;
        };
        if state.error.is_some() || self.failed.load(Ordering::Acquire) {
            return None;
        }
        let position = state.geometry.binary_search_by(|entry| {
            tiny_skia_path::allocation::checkpoint();
            entry.key.cmp(&key)
        });
        match position {
            Ok(index) => {
                let value = &state.geometry[index].value;
                let clip = ScreenIntRect::from_xywh(0, 0, frame[0], frame[1]);
                if value.0.frame != frame
                    || value.0.matrix != matrix(transform)
                    || !stroke_matches(value.0.stroke.as_ref(), stroke)
                    || clip.is_none_or(|clip| {
                        !value
                            .0
                            .binding
                            .matches(path, &clip, 0, 0, 0, false, FillRule::Winding)
                    })
                {
                    state.error = Some(
                        "SVG retained geometry source, stroke, matrix or frame changed".into(),
                    );
                    return None;
                }
                Some(value.clone())
            }
            Err(index) => {
                if state.geometry.len() + state.entries.len() + state.hairlines.len()
                    >= self.maximum
                {
                    state.error =
                        Some("SVG retained geometry invocation admission exceeded".into());
                    return None;
                }
                let (value, bytes) =
                    match build(path, stroke, transform, frame, self._owner.clone()) {
                        Ok(value) => value,
                        Err(error) => {
                            state.error = Some(error);
                            return None;
                        }
                    };
                if !self.charge(&mut state, bytes) {
                    return None;
                }
                if state.geometry.len() == state.geometry.capacity() {
                    let capacity = state
                        .geometry
                        .capacity()
                        .saturating_mul(2)
                        .max(4)
                        .min(self.maximum);
                    let Some(bytes) = capacity.checked_mul(core::mem::size_of::<Entry>()) else {
                        state.error = Some("SVG prepared geometry metadata overflow".into());
                        return None;
                    };
                    if !self.charge(&mut state, bytes) {
                        return None;
                    }
                    let additional = capacity - state.geometry.len();
                    if state.geometry.try_reserve_exact(additional).is_err()
                        || state.geometry.capacity() > capacity
                    {
                        state.error =
                            Some("SVG prepared geometry metadata allocation failed".into());
                        return None;
                    }
                }
                tiny_skia_path::allocation::work(state.geometry.len() - index);
                state.geometry.insert(
                    index,
                    Entry {
                        key,
                        value: value.clone(),
                    },
                );
                Some(value)
            }
        }
    }
}

#[cfg(all(test, panic = "unwind"))]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use core::sync::atomic::AtomicUsize;
    struct Control;
    impl tiny_skia_path::allocation::Cancellation for Control {
        fn check(&self) -> Result<(), String> {
            Ok(())
        }
    }
    struct Lease(Arc<AtomicUsize>);
    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn scope<T>(
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<tiny_skia_path::allocation::Bounded<T>, tiny_skia_path::allocation::Failure<String>>
    {
        tiny_skia_path::allocation::with_limits(
            tiny_skia_path::allocation::Limits {
                bytes: 16 * 1024 * 1024,
                elements: 262144,
                work: 1_000_000,
                cancellation: Arc::new(Control),
            },
            Box::new(()),
            operation,
        )
    }
    fn path() -> Path {
        let mut b = crate::PathBuilder::new();
        b.move_to(2.125, 1.375);
        b.cubic_to(31.625, 3.25, 4.375, 27.625, 35.125, 30.75);
        b.finish().unwrap()
    }
    fn stroke() -> Stroke {
        Stroke {
            width: 3.625,
            line_cap: crate::LineCap::Round,
            dash: crate::StrokeDash::new(alloc::vec![3.125, 1.375, 2.625, 4.25], 0.625),
            ..Stroke::default()
        }
    }
    #[test]
    fn escaped_geometry_retains_actual_owner_after_cache_and_temporary_scope_drop() {
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = ScanCache::new(4 * 1024 * 1024, 128, Box::new(Lease(drops.clone()))).unwrap();
        let path = path();
        let stroke = stroke();
        let value = scope(|| {
            cache
                .geometry(
                    InvocationKey::root(),
                    &path,
                    Some((&stroke, false)),
                    Transform::identity(),
                    [39, 37],
                )
                .ok_or("fixture refused".into())
        })
        .unwrap();
        let escaped = value.get().clone();
        drop(value);
        drop(cache);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(escaped.pieces().len(), 1);
        assert!(!escaped.pieces()[0].path.points().is_empty());
        drop(escaped);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn geometry_reuse_requires_exact_stroke_bits_matrix_frame_and_source() {
        let path = path();
        let stroke = stroke();
        let key = InvocationKey::root();
        for variant in 0..6 {
            let cache = ScanCache::new(4 * 1024 * 1024, 128, Box::new(())).unwrap();
            drop(
                scope(|| {
                    cache
                        .geometry(
                            key,
                            &path,
                            Some((&stroke, false)),
                            Transform::identity(),
                            [39, 37],
                        )
                        .ok_or("fixture refused".into())
                })
                .unwrap(),
            );
            let mut changed = stroke.clone();
            match variant {
                0 => changed.width = 3.6250002,
                1 => changed.line_cap = crate::LineCap::Square,
                2 => {
                    changed.dash =
                        crate::StrokeDash::new(alloc::vec![3.125, 1.5, 2.625, 4.25], 0.625)
                }
                _ => {}
            }
            let changed_path = path
                .clone()
                .transform(Transform::from_translate(0.125, 0.0))
                .unwrap();
            let result = scope(|| {
                cache
                    .geometry(
                        key,
                        if variant == 3 { &changed_path } else { &path },
                        Some((&changed, false)),
                        if variant == 4 {
                            Transform::from_translate(0.0, -0.0)
                        } else {
                            Transform::identity()
                        },
                        if variant == 5 { [41, 37] } else { [39, 37] },
                    )
                    .ok_or("expected refusal".into())
            });
            assert!(result.is_err(), "variant{variant}");
            assert!(cache.status().is_err());
        }
    }
}
