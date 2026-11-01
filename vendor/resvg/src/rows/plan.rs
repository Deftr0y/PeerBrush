// Copyright 2026 the PeerBrush contributors.
// Same Apache-2.0 OR MIT terms as the containing pinned resvg package.
use super::{MemoryLease, RowBudget, RowImage};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
/// Explicit retained-memory and whole-operation work admission. These limits
/// do not admit the borrowed input tree or provide source/session authority.
pub struct PlanLimits {
    pub retained_bytes: usize,
    pub maximum_invocations: usize,
    pub work: u64,
}
struct WholeControl {
    calls: AtomicU64,
    maximum: u64,
    cancellation: Arc<dyn tiny_skia::allocation::Cancellation>,
}
impl tiny_skia::allocation::Cancellation for WholeControl {
    fn check(&self) -> Result<(), String> {
        self.calls
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                old.checked_add(1).filter(|next| *next <= self.maximum)
            })
            .map_err(|_| "SVG total operation work admission exceeded")?;
        self.cancellation.check()
    }
}
struct Budget<'a> {
    outer: &'a dyn RowBudget,
    control: Arc<WholeControl>,
}
impl RowBudget for Budget<'_> {
    fn check(&self) -> Result<(), String> {
        tiny_skia::allocation::Cancellation::check(&*self.control)?;
        self.outer.check()
    }
    fn reserve(&self, bytes: usize) -> Result<Box<dyn MemoryLease>, String> {
        self.check()?;
        self.outer.reserve(bytes)
    }
    fn path_cancellation(&self) -> Arc<dyn tiny_skia::allocation::Cancellation> {
        self.control.clone()
    }
}
/// One immutable tree/matrix/frame, consumed only in ascending complete bands.
/// Any refusal or unwinding attempt permanently closes this plan. Cached scan
/// payloads retain their actual reservation independently of row temporaries.
pub struct RowPlan<'a> {
    tree: &'a usvg::Tree,
    transform: tiny_skia::Transform,
    frame: [u32; 2],
    next: u32,
    failed: bool,
    budget: Budget<'a>,
    cache: tiny_skia::ScanCache,
}
impl<'a> RowPlan<'a> {
    pub fn new(
        tree: &'a usvg::Tree,
        transform: tiny_skia::Transform,
        frame: [u32; 2],
        limits: PlanLimits,
        budget: &'a dyn RowBudget,
    ) -> Result<Self, String> {
        #[cfg(panic = "abort")]
        {
            let _ = (tree, transform, frame, limits, budget);
            return Err("SVG bounded rendering requires an unwind-capable Rust profile".into());
        }
        if frame.iter().any(|n| *n == 0 || *n > 16000)
            || !transform.is_finite()
            || (transform.sx * transform.sy - transform.kx * transform.ky).abs() < 0.000001
            || limits.work == 0
            || !(4096..=128 * 1024 * 1024).contains(&limits.retained_bytes)
            || limits.maximum_invocations == 0
            || limits.maximum_invocations > 16384
        {
            return Err("SVG prepared operation admission is invalid".into());
        }
        budget.check()?;
        let retained = budget.reserve(limits.retained_bytes)?;
        let cache = tiny_skia::ScanCache::new(
            limits.retained_bytes,
            limits.maximum_invocations,
            Box::new(retained),
        )?;
        let control = Arc::new(WholeControl {
            calls: AtomicU64::new(0),
            maximum: limits.work,
            cancellation: budget.path_cancellation(),
        });
        let budget = Budget {
            outer: budget,
            control,
        };
        let working = budget.reserve(128 * 1024 * 1024)?;
        let checked = tiny_skia::allocation::with_limits(
            tiny_skia::allocation::Limits {
                bytes: 128 * 1024 * 1024,
                elements: 262144,
                work: 32_000_000,
                cancellation: budget.path_cancellation(),
            },
            Box::new(working),
            || {
                let mut nodes = 0;
                let mut geometry = 0;
                super::inspect(tree.root(), 0, &mut nodes, &mut geometry)
            },
        );
        match checked {
            Ok(owner) => drop(owner),
            Err(tiny_skia::allocation::Failure::Operation(error)) => {
                return Err(error.get().clone())
            }
            Err(tiny_skia::allocation::Failure::Refused(
                tiny_skia::allocation::Refusal::Cancelled(error),
            )) => return Err(error),
            Err(tiny_skia::allocation::Failure::Refused(error)) => {
                return Err(format!("SVG prepared admission refused: {error:?}"))
            }
        }
        Ok(Self {
            tree,
            transform,
            frame,
            next: 0,
            failed: false,
            cache,
            budget,
        })
    }
    pub fn next_y(&self) -> u32 {
        self.next
    }
    pub fn is_complete(&self) -> bool {
        !self.failed && self.next == self.frame[1]
    }
    pub fn work_used(&self) -> u64 {
        self.budget.control.calls.load(Ordering::Acquire)
    }
    pub fn next(&mut self, count: u32) -> Result<RowImage, String> {
        if self.failed || self.next == self.frame[1] {
            self.failed = true;
            return Err("SVG prepared operation is closed".into());
        }
        // Set before any callback: an unrelated panic cannot leave a reusable
        // plan whose earlier invocations have already advanced.
        self.failed = true;
        let image = super::render_rows_internal(
            self.tree,
            self.transform,
            self.frame,
            self.next,
            count,
            &self.budget,
            Some(&self.cache),
            false,
        )?;
        self.cache.status()?;
        self.next = self
            .next
            .checked_add(count)
            .ok_or("SVG prepared band overflow")?;
        self.failed = false;
        Ok(image)
    }
}
