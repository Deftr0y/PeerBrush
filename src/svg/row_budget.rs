//! Shared native admission for the isolated renderer prototype. Pure captured
//! cancellation is checked inside kernels; session/lease/source publication
//! checks remain with the caller before/after rows and final Engine adoption.
use crate::{
    engine::native::Cancellation,
    large_document::{Reservation, ResidentBudget},
};
use std::sync::Arc;
struct Lease {
    _values: Vec<Reservation>,
    _metadata: Reservation,
}
struct Cancel(Cancellation);
impl super::tiny_skia::allocation::Cancellation for Cancel {
    fn check(&self) -> Result<(), String> {
        self.0.check()
    }
}
pub(crate) struct Budget<'a> {
    pool: &'a ResidentBudget,
    cancellation: Cancellation,
}
impl<'a> Budget<'a> {
    pub(crate) fn new(pool: &'a ResidentBudget, cancellation: Cancellation) -> Self {
        Self { pool, cancellation }
    }
}
impl resvg::rows::RowBudget for Budget<'_> {
    fn check(&self) -> Result<(), String> {
        self.cancellation.check()
    }
    fn reserve(&self, bytes: usize) -> Result<Box<dyn resvg::rows::MemoryLease>, String> {
        self.cancellation.check()?;
        if bytes > 128 * 1024 * 1024 {
            return Err("SVG row working admission exceeds its bound".into());
        }
        let metadata = self
            .pool
            .reserve_bytes(1024, 1024)
            .map_err(|e| e.to_string())?;
        let count = bytes.div_ceil(64 * 1024 * 1024);
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| "SVG row reservation list allocation failed")?;
        if values.capacity() * std::mem::size_of::<Reservation>() > 512 {
            return Err("SVG row reservation list exceeded admission".into());
        }
        let mut left = bytes;
        while left != 0 {
            let part = left.min(64 * 1024 * 1024);
            values.push(
                self.pool
                    .reserve_bytes(part, part)
                    .map_err(|e| e.to_string())?,
            );
            left -= part;
        }
        self.cancellation.check()?;
        Ok(Box::new(Lease {
            _values: values,
            _metadata: metadata,
        }))
    }
    fn path_cancellation(&self) -> Arc<dyn super::tiny_skia::allocation::Cancellation> {
        Arc::new(Cancel(self.cancellation.clone()))
    }
}
