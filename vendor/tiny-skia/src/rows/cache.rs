// Copyright 2026 the PeerBrush contributors.
// Same BSD-3-Clause terms as the containing pinned tiny-skia package.
use crate::{
    blitter::Blitter,
    geom::ScreenIntRect,
    scan::path::{prepare_edges, PreparedEdges},
    FillRule, Path,
};
use alloc::{boxed::Box, string::String, vec::Vec};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Site {
    tag: u8,
    value: u64,
}

#[cfg(all(test, feature = "std", panic = "unwind"))]
mod tests {
    use super::*;
    use crate::{Paint, PathBuilder, Pixmap, Rect, Transform};
    struct Check;
    impl tiny_skia_path::allocation::Cancellation for Check {
        fn check(&self) -> Result<(), String> {
            Ok(())
        }
    }
    fn path() -> Path {
        PathBuilder::from_rect(Rect::from_xywh(1.125, 2.375, 26.625, 31.25).unwrap())
    }
    fn draw(
        cache: &ScanCache,
        key: InvocationKey,
        path: &Path,
        frame: [u32; 2],
        y: u32,
        transform: Transform,
    ) -> Result<(), String> {
        let result = tiny_skia_path::allocation::with_limits(
            tiny_skia_path::allocation::Limits {
                bytes: 8 * 1024 * 1024,
                elements: 262144,
                work: 1_000_000,
                cancellation: Arc::new(Check),
            },
            Box::new(()),
            || {
                let mut image = Pixmap::new_rows(frame[0], frame[1], y, 7).unwrap();
                let mut paint = Paint::default();
                paint.set_color_rgba8(121, 57, 213, 173);
                image.as_mut().fill_path_prepared(
                    path,
                    &paint,
                    FillRule::Winding,
                    transform,
                    None,
                    cache,
                    key,
                )
            },
        );
        match result {
            Ok(value) => {
                drop(value);
                Ok(())
            }
            Err(tiny_skia_path::allocation::Failure::Operation(error)) => Err(error.get().clone()),
            Err(error) => Err(alloc::format!("{error:?}")),
        }
    }
    #[test]
    fn distinct_reused_geometry_sites_advance_independently_without_hash_aliases() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<crate::Pixmap>();
        send_sync::<crate::PixmapRef<'static>>();
        send_sync::<crate::PixmapMut<'static>>();
        send_sync::<ScanCache>();
        let root = InvocationKey::root();
        assert_ne!(root, root.child(0, 0).unwrap());
        let a = root.child(1, 111).unwrap();
        let b = root.child(1, 222).unwrap();
        let cache = ScanCache::new(1024 * 1024, 16, Box::new(())).unwrap();
        let path = path();
        for y in [0, 7, 14, 21, 28] {
            draw(&cache, a, &path, [31, 37], y, Transform::identity()).unwrap();
            draw(&cache, b, &path, [31, 37], y, Transform::identity()).unwrap();
        }
        assert!(cache.status().is_ok());
    }
    #[test]
    fn repeats_gaps_source_matrix_and_frame_changes_permanently_refuse() {
        let source = path();
        let key = InvocationKey::root().child(3, 17).unwrap();
        for variant in 0..5 {
            let cache = ScanCache::new(1024 * 1024, 16, Box::new(())).unwrap();
            draw(&cache, key, &source, [31, 37], 0, Transform::identity()).unwrap();
            let changed =
                PathBuilder::from_rect(Rect::from_xywh(1.25, 2.375, 26.625, 31.25).unwrap());
            let result = match variant {
                0 => draw(&cache, key, &source, [31, 37], 0, Transform::identity()),
                1 => draw(&cache, key, &source, [31, 37], 8, Transform::identity()),
                2 => draw(&cache, key, &changed, [31, 37], 7, Transform::identity()),
                3 => draw(
                    &cache,
                    key,
                    &source,
                    [31, 37],
                    7,
                    Transform::from_translate(0.125, 0.0),
                ),
                _ => draw(&cache, key, &source, [33, 37], 7, Transform::identity()),
            };
            assert!(result.is_err(), "variant{variant}");
            assert!(draw(&cache, key, &source, [31, 37], 7, Transform::identity()).is_err());
        }
    }
}
/// Exact traversal ancestry. Keys are never hashed or serialized as authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct InvocationKey {
    sites: [Site; 72],
    length: u8,
}
impl InvocationKey {
    pub fn root() -> Self {
        Self {
            sites: [Site::default(); 72],
            length: 0,
        }
    }
    pub fn child(mut self, tag: u8, value: u64) -> Option<Self> {
        let index = self.length as usize;
        if index == self.sites.len() {
            return None;
        }
        self.sites[index] = Site { tag, value };
        self.length += 1;
        Some(self)
    }
}
struct Binding {
    path: Path,
    clip: [u32; 4],
    start: i32,
    stop: i32,
    shift: i32,
    contained: bool,
    rule: FillRule,
}
impl Binding {
    fn matches(
        &self,
        path: &Path,
        clip: &ScreenIntRect,
        start: i32,
        stop: i32,
        shift: i32,
        contained: bool,
        rule: FillRule,
    ) -> bool {
        if self.clip != [clip.x(), clip.y(), clip.width(), clip.height()]
            || self.start != start
            || self.stop != stop
            || self.shift != shift
            || self.contained != contained
            || self.rule != rule
            || self.path.verbs() != path.verbs()
            || self.path.points().len() != path.points().len()
        {
            return false;
        }
        let a = self.path.bounds();
        let b = path.bounds();
        if [
            a.x().to_bits(),
            a.y().to_bits(),
            a.width().to_bits(),
            a.height().to_bits(),
        ] != [
            b.x().to_bits(),
            b.y().to_bits(),
            b.width().to_bits(),
            b.height().to_bits(),
        ] {
            return false;
        }
        for (a, b) in self.path.points().iter().zip(path.points()) {
            tiny_skia_path::allocation::checkpoint();
            if a.x.to_bits() != b.x.to_bits() || a.y.to_bits() != b.y.to_bits() {
                return false;
            }
        }
        true
    }
}
struct Entry {
    key: InvocationKey,
    binding: Binding,
    program: Option<PreparedEdges>,
    next_band: u32,
}
struct State {
    entries: Vec<Entry>,
    charged: usize,
    error: Option<String>,
}
struct Owner {
    _memory: Mutex<Box<dyn tiny_skia_path::allocation::MemoryOwner>>,
}
/// One operation's bounded retained scan payloads. Actual caller reservation
/// outlives every retained allocation. No file, source or publication authority.
pub struct ScanCache {
    state: Mutex<State>,
    failed: AtomicBool,
    limit: usize,
    maximum: usize,
    _owner: Arc<Owner>,
}
impl ScanCache {
    pub fn new(
        bytes: usize,
        maximum: usize,
        memory: Box<dyn tiny_skia_path::allocation::MemoryOwner>,
    ) -> Result<Self, String> {
        if bytes < 4096 || bytes > 128 * 1024 * 1024 || maximum == 0 || maximum > 16384 {
            return Err("SVG retained scan admission is invalid".into());
        }
        tiny_skia_path::allocation::charge(4096);
        Ok(Self {
            state: Mutex::new(State {
                entries: Vec::new(),
                charged: 4096,
                error: None,
            }),
            failed: AtomicBool::new(false),
            limit: bytes,
            maximum,
            _owner: Arc::new(Owner {
                _memory: Mutex::new(memory),
            }),
        })
    }
    pub fn status(&self) -> Result<(), String> {
        let state = self
            .state
            .try_lock()
            .map_err(|_| "SVG retained scan state is unavailable")?;
        if self.failed.load(Ordering::Acquire) {
            return Err(state
                .error
                .clone()
                .unwrap_or_else(|| "SVG retained scan was refused".into()));
        }
        state.error.clone().map_or(Ok(()), Err)
    }
    pub fn refuse(&self, message: &str) {
        self.failed.store(true, Ordering::Release);
        if let Ok(mut state) = self.state.try_lock() {
            if state.error.is_none() {
                state.error = Some(message.into());
            }
        }
    }
    fn charge(&self, state: &mut State, bytes: usize) -> bool {
        if let Some(next) = state
            .charged
            .checked_add(bytes)
            .filter(|n| *n <= self.limit)
        {
            state.charged = next;
            true
        } else {
            state.error = Some("SVG retained scan memory admission exceeded".into());
            false
        }
    }
    pub(crate) fn draw(
        &self,
        key: InvocationKey,
        path: &Path,
        rule: FillRule,
        clip: &ScreenIntRect,
        start: i32,
        stop: i32,
        shift: i32,
        contained: bool,
        band: [u32; 2],
        blitter: &mut dyn Blitter,
    ) -> bool {
        if !tiny_skia_path::allocation::is_active() {
            self.refuse("SVG retained scan requires an admitted Rust scope");
            return false;
        }
        let Ok(mut state) = self.state.try_lock() else {
            self.failed.store(true, Ordering::Release);
            return false;
        };
        if state.error.is_some() || self.failed.load(Ordering::Acquire) {
            return false;
        }
        if band[0] >= band[1] || !(0..=2).contains(&shift) {
            state.error = Some("SVG retained scan band is invalid".into());
            return false;
        }
        let index = match state.entries.binary_search_by(|entry| {
            tiny_skia_path::allocation::checkpoint();
            entry.key.cmp(&key)
        }) {
            Ok(index) => {
                let entry = &state.entries[index];
                if entry.next_band != band[0]
                    || !entry
                        .binding
                        .matches(path, clip, start, stop, shift, contained, rule)
                {
                    state.error = Some("SVG retained scan source or ascending band changed".into());
                    return false;
                }
                index
            }
            Err(index) => {
                if state.entries.len() == self.maximum {
                    state.error = Some("SVG retained scan invocation admission exceeded".into());
                    return false;
                }
                let program = prepare_edges(path, rule, clip, start, stop, shift, contained);
                let path_bytes = path
                    .verbs()
                    .len()
                    .checked_mul(core::mem::size_of::<tiny_skia_path::PathVerb>())
                    .and_then(|n| {
                        n.checked_add(
                            path.points()
                                .len()
                                .checked_mul(core::mem::size_of::<crate::Point>())?,
                        )
                    });
                let program_bytes = program
                    .as_ref()
                    .map_or(Some(0), PreparedEdges::allocation_bytes);
                let Some(bytes) = path_bytes.and_then(|n| n.checked_add(program_bytes?)) else {
                    state.error = Some("SVG retained scan size overflow".into());
                    return false;
                };
                if !self.charge(&mut state, bytes) {
                    return false;
                }
                if state.entries.len() == state.entries.capacity() {
                    let capacity = state
                        .entries
                        .capacity()
                        .saturating_mul(2)
                        .max(4)
                        .min(self.maximum);
                    let Some(bytes) = capacity.checked_mul(core::mem::size_of::<Entry>()) else {
                        state.error = Some("SVG retained scan metadata overflow".into());
                        return false;
                    };
                    if !self.charge(&mut state, bytes) {
                        return false;
                    }
                    let additional = capacity - state.entries.len();
                    if state.entries.try_reserve_exact(additional).is_err()
                        || state.entries.capacity() > capacity
                    {
                        state.error = Some("SVG retained scan metadata allocation failed".into());
                        return false;
                    }
                }
                let binding = Binding {
                    path: path.clone(),
                    clip: [clip.x(), clip.y(), clip.width(), clip.height()],
                    start,
                    stop,
                    shift,
                    contained,
                    rule,
                };
                for _ in index..state.entries.len() {
                    tiny_skia_path::allocation::checkpoint();
                }
                state.entries.insert(
                    index,
                    Entry {
                        key,
                        binding,
                        program,
                        next_band: band[0],
                    },
                );
                index
            }
        };
        let entry = &mut state.entries[index];
        if let Some(program) = entry.program.as_mut() {
            program.advance(band[1] << shift, blitter);
        }
        entry.next_band = band[1];
        true
    }
}
