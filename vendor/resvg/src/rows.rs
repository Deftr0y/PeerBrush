// PeerBrush isolated bounded-row prototype. Upstream renderer notices remain
// in LICENSE-APACHE and LICENSE-MIT; no upstream version is changed.
use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    sync::Arc,
};

/// Keep the actual allocation reservation alive until its pixel buffer drops.
pub trait MemoryLease: Send {}
impl<T: Send> MemoryLease for T {}
/// Caller-owned shared accounting and operation cancellation/staleness checks.
pub trait RowBudget {
    /// Check the same captured operation; no publication occurs in this crate.
    fn check(&self) -> Result<(), String>;
    /// Reserve actual bytes before allocation. Returned lease owns the charge.
    fn reserve(&self, bytes: usize) -> Result<Box<dyn MemoryLease>, String>;
    /// Pure captured cancellation for allocation and recursive work checkpoints.
    fn path_cancellation(&self) -> Arc<dyn tiny_skia::allocation::Cancellation>;
}
pub(crate) struct OwnedPixmap {
    pub(crate) pixmap: tiny_skia::Pixmap,
    pub(crate) _lease: Option<Box<dyn MemoryLease>>,
}
impl Deref for OwnedPixmap {
    type Target = tiny_skia::Pixmap;
    fn deref(&self) -> &Self::Target {
        &self.pixmap
    }
}
impl DerefMut for OwnedPixmap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.pixmap
    }
}
/// Charged physical rows, with their logical full-frame coordinates retained.
/// It cannot detach owned pixel bytes from their allocation lease.
pub struct RowImage {
    image: tiny_skia::allocation::Bounded<OwnedPixmap>,
}
impl RowImage {
    /// Logical canvas width.
    pub fn width(&self) -> u32 {
        self.image.get().width()
    }
    /// This row's unchanged logical full-frame Y coordinate.
    pub fn y(&self) -> u32 {
        self.image.get().stored_row_start()
    }
    /// Number of complete physical rows in this bounded band.
    pub fn row_count(&self) -> u32 {
        self.image.get().stored_row_count()
    }
    /// Native renderer premultiplied RGBA8 bytes, in complete row order.
    pub fn data(&self) -> &[u8] {
        self.image.get().data()
    }
}
pub(crate) struct RowContext<'a> {
    budget: &'a dyn RowBudget,
    error: RefCell<Option<String>>,
}
impl RowContext<'_> {
    pub(crate) fn refuse(&self, error: &str) {
        let mut slot = self.error.borrow_mut();
        if slot.is_none() {
            *slot = Some(error.into());
        }
    }
    pub(crate) fn check(&self) -> bool {
        if self.error.borrow().is_some() {
            return false;
        }
        if let Err(error) = self.budget.check() {
            self.refuse(&error);
            return false;
        }
        true
    }
    pub(crate) fn reserve(&self, bytes: usize) -> Option<Box<dyn MemoryLease>> {
        if !self.check() {
            return None;
        }
        match self.budget.reserve(bytes) {
            Ok(lease) => Some(lease),
            Err(error) => {
                self.refuse(&error);
                None
            }
        }
    }
    pub(crate) fn new_pixmap(&self, w: u32, h: u32, start: u32, count: u32) -> Option<OwnedPixmap> {
        if u64::from(w) * u64::from(h) > i32::MAX as u64 {
            self.refuse("SVG logical sampler index exceeds its admitted range");
            return None;
        }
        let length = (w as usize)
            .checked_mul(count as usize)?
            .checked_mul(4)?
            .checked_add(256)?;
        let lease = self.reserve(length)?;
        match tiny_skia::Pixmap::new_rows(w, h, start, count) {
            Some(pixmap) => Some(OwnedPixmap {
                pixmap,
                _lease: Some(lease),
            }),
            None => {
                self.refuse("SVG bounded row allocation was refused");
                None
            }
        }
    }
    fn result(&self) -> Result<(), String> {
        self.check();
        self.error.borrow().clone().map_or(Ok(()), Err)
    }
}

fn inspect(
    group: &usvg::Group,
    depth: usize,
    nodes: &mut usize,
    geometry: &mut usize,
) -> Result<(), String> {
    if depth > 64 || !group.filters().is_empty() || group.mask().is_some() {
        return Err("Unsupported SVG row group or depth".into());
    }
    for node in group.children() {
        *nodes = nodes
            .checked_add(1)
            .filter(|n| *n <= 4096)
            .ok_or("SVG row node admission exceeded")?;
        match node {
            usvg::Node::Group(g) => inspect(g, depth + 1, nodes, geometry)?,
            usvg::Node::Path(p) => {
                for _ in p.data().segments() {
                    tiny_skia::allocation::checkpoint();
                    *geometry = geometry
                        .checked_add(1)
                        .filter(|n| *n <= 65536)
                        .ok_or("SVG row geometry admission exceeded")?;
                }
                if p.stroke()
                    .and_then(|s| s.dasharray())
                    .is_some_and(|a| a.len() > 4096)
                {
                    return Err("SVG dash array exceeds bounded admission".into());
                }
                let paints = p
                    .fill()
                    .map(|f| f.paint())
                    .into_iter()
                    .chain(p.stroke().map(|s| s.paint()));
                for paint in paints {
                    match paint {
                        usvg::Paint::Pattern(_) => {
                            return Err(
                                "SVG row patterns are outside the supported static subset".into()
                            )
                        }
                        usvg::Paint::LinearGradient(g) => {
                            if g.stops().len() > 4096 {
                                return Err("SVG gradient stop admission exceeded".into());
                            }
                        }
                        usvg::Paint::RadialGradient(g) => {
                            if g.stops().len() > 4096 {
                                return Err("SVG gradient stop admission exceeded".into());
                            }
                        }
                        _ => (),
                    }
                }
            }
            _ => return Err("SVG row images and text must be outlined before import".into()),
        }
    }
    if let Some(clip) = group.clip_path() {
        inspect_clip(clip, depth + 1, nodes, geometry)?;
    }
    Ok(())
}
fn inspect_clip(
    clip: &usvg::ClipPath,
    depth: usize,
    nodes: &mut usize,
    geometry: &mut usize,
) -> Result<(), String> {
    inspect(clip.root(), depth, nodes, geometry)?;
    if let Some(parent) = clip.clip_path() {
        inspect_clip(parent, depth + 1, nodes, geometry)?;
    }
    Ok(())
}
/// Prototype exact full-origin row projection. No geometry/CTM/shader or scan
/// clip rectangle is translated to the stored row. The original logical tiler
/// and horizontal SIMD spans are preserved, including off-row AA scan state.
/// Caller must retain/admit the immutable tree separately.
pub fn render_row(
    tree: &usvg::Tree,
    transform: tiny_skia::Transform,
    frame: [u32; 2],
    y: u32,
    budget: &dyn RowBudget,
) -> Result<RowImage, String> {
    render_rows(tree, transform, frame, y, 1, budget)
}
/// Render a bounded complete-width band. A streaming caller must consume each
/// band once, rather than rerendering a whole row for each horizontal tile.
/// This prototype still replays scan-conversion prefixes for separate bands;
/// it makes no whole-document throughput claim.
pub fn render_rows(
    tree: &usvg::Tree,
    transform: tiny_skia::Transform,
    frame: [u32; 2],
    y: u32,
    count: u32,
    budget: &dyn RowBudget,
) -> Result<RowImage, String> {
    #[cfg(panic = "abort")]
    {
        let _ = (tree, transform, frame, y, count, budget);
        return Err("SVG bounded rendering requires an unwind-capable Rust profile".into());
    }
    if frame.iter().any(|n| *n == 0 || *n > 16000)
        || count == 0
        || y.checked_add(count).is_none_or(|end| end > frame[1])
        || u64::from(frame[0]) * u64::from(count) > 128 * 1024
    {
        return Err("SVG logical row frame exceeds admission".into());
    }
    if !transform.is_finite()
        || (transform.sx * transform.sy - transform.kx * transform.ky).abs() < 0.000001
    {
        return Err("SVG row transform is invalid".into());
    }
    budget.check()?;
    let working = budget.reserve(128 * 1024 * 1024)?;
    let limits = tiny_skia::allocation::Limits {
        bytes: 128 * 1024 * 1024,
        elements: 262144,
        work: 32_000_000,
        cancellation: budget.path_cancellation(),
    };
    let rendered = tiny_skia::allocation::with_limits(
        limits,
        Box::new(working),
        || -> Result<OwnedPixmap, String> {
            let mut nodes = 0;
            let mut geometry = 0;
            inspect(tree.root(), 0, &mut nodes, &mut geometry)?;
            let rows = RowContext {
                budget,
                error: RefCell::new(None),
            };
            let mut image = rows
                .new_pixmap(frame[0], frame[1], y, count)
                .ok_or_else(|| {
                    rows.result()
                        .err()
                        .unwrap_or_else(|| "SVG row allocation was refused".into())
                })?;
            let context = crate::render::Context {
                max_bbox: crate::max_filter_bbox(frame[0], frame[1]),
                rows: Some(&rows),
            };
            crate::render::render_nodes(tree.root(), &context, transform, &mut image.as_mut());
            if image.row_storage_failed() {
                rows.refuse("SVG required an unprepared source row");
            }
            rows.result()?;
            Ok(image)
        },
    );
    match rendered {
        Ok(image) => Ok(RowImage { image }),
        Err(tiny_skia::allocation::Failure::Operation(error)) => Err(error.get().clone()),
        Err(tiny_skia::allocation::Failure::Refused(
            tiny_skia::allocation::Refusal::Cancelled(error),
        )) => Err(error),
        Err(tiny_skia::allocation::Failure::Refused(error)) => {
            Err(format!("SVG path processing refused: {error:?}"))
        }
    }
}

#[cfg(all(test, panic = "unwind"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Lease {
        live: Arc<AtomicUsize>,
        bytes: usize,
    }
    impl Drop for Lease {
        fn drop(&mut self) {
            self.live.fetch_sub(self.bytes, Ordering::SeqCst);
        }
    }
    struct Control {
        calls: AtomicUsize,
        stop: usize,
    }
    impl tiny_skia::allocation::Cancellation for Control {
        fn check(&self) -> Result<(), String> {
            if self.calls.fetch_add(1, Ordering::SeqCst) >= self.stop {
                Err("row fixture canceled".into())
            } else {
                Ok(())
            }
        }
    }
    struct Budget {
        live: Arc<AtomicUsize>,
        limit: usize,
        control: Arc<Control>,
    }
    impl Budget {
        fn new(limit: usize, stop: usize) -> Self {
            Self {
                live: Arc::new(AtomicUsize::new(0)),
                limit,
                control: Arc::new(Control {
                    calls: AtomicUsize::new(0),
                    stop,
                }),
            }
        }
    }
    impl RowBudget for Budget {
        fn check(&self) -> Result<(), String> {
            tiny_skia::allocation::Cancellation::check(&*self.control)
        }
        fn reserve(&self, bytes: usize) -> Result<Box<dyn MemoryLease>, String> {
            self.live
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                    current.checked_add(bytes).filter(|n| *n <= self.limit)
                })
                .map_err(|_| "row fixture resource refusal")?;
            Ok(Box::new(Lease {
                live: self.live.clone(),
                bytes,
            }))
        }
        fn path_cancellation(&self) -> Arc<dyn tiny_skia::allocation::Cancellation> {
            self.control.clone()
        }
    }
    fn tree(body: &str) -> usvg::Tree {
        usvg::Tree::from_str(
            &format!(
                "<svg xmlns='http://www.w3.org/2000/svg' width='113' height='79'>{body}</svg>"
            ),
            &usvg::Options::default(),
        )
        .unwrap()
    }
    const BODY:&str="<defs><clipPath id='c'><ellipse cx='53.375' cy='36.625' rx='43.75' ry='28.125'/></clipPath></defs><g opacity='.63' clip-path='url(#c)'><rect x='3.125' y='2.375' width='103.625' height='69.125' rx='13.375' fill='#743fc2'/><path d='M7.125 63.375 C27.375 -11.25 57.625 88.375 102.875 8.625' fill='none' stroke='#df5329' stroke-width='3.375'/></g>";
    #[test]
    fn full_origin_rows_and_bands_match_default_full_renderer_exactly() {
        let tree = tree(BODY);
        let transform = tiny_skia::Transform::from_row(0.9375, 0.125, -0.0625, 1.125, 7.25, 0.625);
        let mut full = tiny_skia::Pixmap::new(113, 79).unwrap();
        crate::render(&tree, transform, &mut full.as_mut());
        let budget = Budget::new(130 * 1024 * 1024, usize::MAX);
        for count in [1, 7] {
            for y in (0..79).step_by(count as usize) {
                let count = count.min(79 - y);
                let band = render_rows(&tree, transform, [113, 79], y, count, &budget).unwrap();
                assert_eq!((band.y(), band.row_count()), (y, count));
                let start = y as usize * 113 * 4;
                assert_eq!(
                    band.data(),
                    &full.data()[start..start + count as usize * 113 * 4],
                    "row{y} count{count}"
                );
                assert!(budget.live.load(Ordering::SeqCst) >= 128 * 1024 * 1024);
                drop(band);
                assert_eq!(budget.live.load(Ordering::SeqCst), 0);
            }
        }
    }
    #[test]
    fn resource_and_cancellation_refusal_release_every_pixel_and_work_owner() {
        let tree = tree(BODY);
        for budget in [
            Budget::new(8, usize::MAX),
            Budget::new(130 * 1024 * 1024, 25),
        ] {
            let result = render_rows(
                &tree,
                tiny_skia::Transform::identity(),
                [113, 79],
                23,
                7,
                &budget,
            );
            assert!(result.is_err());
            assert_eq!(budget.live.load(Ordering::SeqCst), 0);
        }
    }
    #[test]
    fn invalid_or_unsupported_sources_fail_without_a_successful_row() {
        let budget = Budget::new(130 * 1024 * 1024, usize::MAX);
        let tree = tree(BODY);
        for (frame, y, count) in [
            ([0, 79], 0, 1),
            ([113, 79], 79, 1),
            ([113, 79], 0, 0),
            ([113, 79], u32::MAX, 2),
            ([16000, 79], 0, 9),
        ] {
            assert!(render_rows(
                &tree,
                tiny_skia::Transform::identity(),
                frame,
                y,
                count,
                &budget
            )
            .is_err());
        }
        let unsupported=super::tests::tree("<defs><mask id='m'><rect width='113' height='79' fill='white'/></mask></defs><rect width='113' height='79' mask='url(#m)'/>");
        assert!(render_row(
            &unsupported,
            tiny_skia::Transform::identity(),
            [113, 79],
            0,
            &budget
        )
        .is_err());
        assert_eq!(budget.live.load(Ordering::SeqCst), 0);
    }
}
