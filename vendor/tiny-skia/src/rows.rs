// PeerBrush bounded-row prototype, based on the pinned tiny-skia renderer.
// Upstream copyright and BSD notices remain in LICENSE and original modules.
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "std")]
#[path = "rows/cache.rs"]
mod cache;
#[cfg(feature = "std")]
pub use cache::{InvocationKey, ScanCache};

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Rows {
    pub start: u32,
    pub count: u32,
}
impl Rows {
    pub fn full(height: u32) -> Self {
        Self {
            start: 0,
            count: height,
        }
    }
    pub fn end(self) -> u32 {
        self.start + self.count
    }
    pub fn contains(self, y: u32) -> bool {
        y >= self.start && y < self.end()
    }
    pub fn intersect(self, top: u32, bottom: u32) -> Option<Self> {
        let start = self.start.max(top);
        let end = self.end().min(bottom);
        (start < end).then_some(Self {
            start,
            count: end.saturating_sub(start),
        })
    }
}
#[derive(Clone, Default)]
pub(crate) struct Fault(Option<Arc<AtomicBool>>);
impl Fault {
    pub fn tracked() -> Self {
        Self(Some(Arc::new(AtomicBool::new(false))))
    }
    pub fn borrowed(&self) -> FaultRef<'_> {
        FaultRef(self.0.as_deref())
    }
    pub fn from_ref(value: FaultRef<'_>) -> Self {
        if value.0.is_some() {
            let result = Self::tracked();
            if value.failed() {
                result.borrowed().refuse();
            }
            result
        } else {
            Self::default()
        }
    }
}
impl PartialEq for Fault {
    fn eq(&self, other: &Self) -> bool {
        self.borrowed().failed() == other.borrowed().failed()
    }
}
#[derive(Clone, Copy, Default)]
pub(crate) struct FaultRef<'a>(Option<&'a AtomicBool>);
impl FaultRef<'_> {
    pub fn tracked(self) -> bool {
        self.0.is_some()
    }
    pub fn failed(self) -> bool {
        self.0.is_some_and(|f| f.load(Ordering::Relaxed))
    }
    pub fn refuse(self) {
        if let Some(f) = self.0 {
            f.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unprepared_source_row_is_observable_and_never_an_accepted_transparent_sample() {
        let image = crate::Pixmap::new_rows(19, 31, 9, 2).unwrap();
        assert!(image.pixel(3, 9).is_some());
        assert!(!image.row_storage_failed());
        assert!(image.pixel(3, 8).is_none());
        assert!(image.row_storage_failed());
        let source = crate::Pixmap::new_rows(19, 31, 9, 2).unwrap();
        let mut destination = crate::Pixmap::new_rows(19, 31, 11, 1).unwrap();
        destination.draw_pixmap(
            0,
            0,
            source.as_ref(),
            &crate::PixmapPaint::default(),
            crate::Transform::identity(),
            None,
        );
        assert!(source.row_storage_failed());
    }
}
impl PartialEq for FaultRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.failed() == other.failed()
    }
}
