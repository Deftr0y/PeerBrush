//! Scoped admission of synchronous renderer temporaries. The caller holds its
//! shared memory reservation through every temporary's drop. No path allocation
//! may escape that operation; the strict row wrapper returns only charged pixels.
use alloc::vec::Vec;
#[cfg(feature = "std")]
mod scoped {
    use super::*;
    use alloc::{boxed::Box, rc::Rc, string::String, sync::Arc};
    use core::cell::{Cell, RefCell};
    use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
    /// Pure captured cancellation; no I/O, engine mutation or FFI unwinding.
    pub trait Cancellation: Send + Sync {
        fn check(&self) -> Result<(), String>;
    }
    /// Real caller-owned accounting token retained through returned data drop.
    pub trait MemoryOwner: Send {}
    impl<T: Send> MemoryOwner for T {}
    /// Explicit failure returned at the checked Rust boundary.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Refusal {
        Bytes,
        Elements,
        Work,
        Allocation,
        Cancelled(String),
        UnsupportedPanicProfile,
    }
    /// Allocation bytes are monotonic: old and replacement capacities remain
    /// counted together, even after an earlier temporary is freed.
    pub struct Limits {
        pub bytes: usize,
        pub elements: usize,
        pub work: usize,
        pub cancellation: Arc<dyn Cancellation>,
    }
    struct State {
        limits: Limits,
        bytes: Cell<usize>,
        work: Cell<usize>,
        error: RefCell<Option<Refusal>>,
        parent: Option<Rc<State>>,
        depth: usize,
        _memory: Box<dyn MemoryOwner>,
    }
    /// An allocation cannot be detached from its operation's real reservation.
    pub struct Bounded<T> {
        value: T,
        _state: Rc<State>,
    }
    impl<T> Bounded<T> {
        pub fn get(&self) -> &T {
            &self.value
        }
    }
    impl<T: core::fmt::Debug> core::fmt::Debug for Bounded<T> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_tuple("Bounded").field(&self.value).finish()
        }
    }
    /// Caller errors may own partial data, so their reservation is retained too.
    #[derive(Debug)]
    pub enum Failure<E> {
        Operation(Bounded<E>),
        Refused(Refusal),
    }
    struct Marker;
    std::thread_local! {static ACTIVE:RefCell<Option<Rc<State>>>=const{RefCell::new(None)};}
    struct Restore(Option<Rc<State>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let old = self.0.take();
            ACTIVE.with(|slot| *slot.borrow_mut() = old);
        }
    }
    fn record(state: &State, error: Refusal) {
        if state.error.borrow().is_none() {
            *state.error.borrow_mut() = Some(error.clone());
        }
        let mut parent = state.parent.clone();
        while let Some(ancestor) = parent {
            if ancestor.error.borrow().is_none() {
                *ancestor.error.borrow_mut() = Some(error.clone());
            }
            parent = ancestor.parent.clone();
        }
    }
    fn refuse(state: &State, error: Refusal) -> ! {
        record(state, error);
        // resume_unwind does not invoke or replace the global panic hook.
        // Only with_limits catches this exact private type.
        resume_unwind(Box::new(Marker));
    }
    fn current() -> Option<Rc<State>> {
        ACTIVE.with(|s| s.borrow().clone())
    }
    pub fn is_active() -> bool {
        current().is_some()
    }
    pub fn checkpoint() {
        if let Some(state) = current() {
            let mut at = Some(state.clone());
            while let Some(scope) = at {
                let error = scope.error.borrow().clone();
                if let Some(error) = error {
                    refuse(&state, error);
                }
                if scope
                    .work
                    .get()
                    .checked_add(1)
                    .is_none_or(|n| n > scope.limits.work)
                {
                    refuse(&state, Refusal::Work);
                }
                if let Err(error) = scope.limits.cancellation.check() {
                    refuse(&state, Refusal::Cancelled(error));
                }
                at = scope.parent.clone();
            }
            let mut at = Some(state);
            while let Some(scope) = at {
                scope.work.set(scope.work.get() + 1);
                at = scope.parent.clone();
            }
        }
    }
    pub fn charge(bytes: usize) {
        checkpoint();
        if let Some(state) = current() {
            let mut at = Some(state.clone());
            while let Some(scope) = at {
                if scope
                    .bytes
                    .get()
                    .checked_add(bytes)
                    .is_none_or(|n| n > scope.limits.bytes)
                {
                    refuse(&state, Refusal::Bytes);
                }
                at = scope.parent.clone();
            }
            let mut at = Some(state);
            while let Some(scope) = at {
                scope.bytes.set(scope.bytes.get() + bytes);
                at = scope.parent.clone();
            }
        }
    }
    pub fn reserve<T>(values: &mut Vec<T>, additional: usize) {
        let Some(state) = current() else {
            values.reserve(additional);
            return;
        };
        checkpoint();
        let mut elements = state.limits.elements;
        let mut parent = state.parent.clone();
        while let Some(scope) = parent {
            elements = elements.min(scope.limits.elements);
            parent = scope.parent.clone();
        }
        let needed = values
            .len()
            .checked_add(additional)
            .filter(|n| *n <= elements)
            .unwrap_or_else(|| refuse(&state, Refusal::Elements));
        if needed > values.capacity() {
            let target = needed
                .max(values.capacity().checked_mul(2).unwrap_or(needed))
                .min(elements);
            let bytes = target
                .checked_mul(core::mem::size_of::<T>())
                .unwrap_or_else(|| refuse(&state, Refusal::Bytes));
            charge(bytes);
            if values.try_reserve_exact(target - values.len()).is_err() {
                refuse(&state, Refusal::Allocation);
            }
            if values.capacity() > target {
                refuse(&state, Refusal::Allocation);
            }
        }
    }
    /// Catch only our exact private marker; resume every unrelated Rust panic.
    /// Nested scopes charge/check both ledgers and retain both real owners.
    /// Abort-profile builds refuse before entering the operation.
    pub fn with_limits<T, E>(
        limits: Limits,
        memory: Box<dyn MemoryOwner>,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<Bounded<T>, Failure<E>> {
        #[cfg(panic = "abort")]
        {
            let _ = (limits, memory, operation);
            return Err(Failure::Refused(Refusal::UnsupportedPanicProfile));
        }
        #[cfg(panic = "unwind")]
        {
            let previous = current();
            let depth = previous.as_ref().map_or(0, |s| s.depth + 1);
            if depth > 64 {
                if let Some(active) = previous.as_ref() {
                    record(active, Refusal::Work);
                }
                return Err(Failure::Refused(Refusal::Work));
            }
            let state = Rc::new(State {
                limits,
                bytes: Cell::new(0),
                work: Cell::new(0),
                error: RefCell::new(None),
                parent: previous.clone(),
                depth,
                _memory: memory,
            });
            ACTIVE.with(|slot| *slot.borrow_mut() = Some(state.clone()));
            let _restore = Restore(previous);
            match catch_unwind(AssertUnwindSafe(operation)) {
                Ok(value) => {
                    let error = state.error.borrow().clone();
                    match error {
                        Some(error) => {
                            drop(value);
                            Err(Failure::Refused(error))
                        }
                        None => match value {
                            Ok(value) => Ok(Bounded {
                                value,
                                _state: state.clone(),
                            }),
                            Err(value) => Err(Failure::Operation(Bounded {
                                value,
                                _state: state.clone(),
                            })),
                        },
                    }
                }
                Err(marker) => {
                    if marker.is::<Marker>() {
                        Err(Failure::Refused(
                            state.error.borrow().clone().unwrap_or(Refusal::Allocation),
                        ))
                    } else {
                        resume_unwind(marker)
                    }
                }
            }
        }
    }
}
#[cfg(feature = "std")]
pub use scoped::{
    charge, checkpoint, is_active, reserve, with_limits, Bounded, Cancellation, Failure, Limits,
    MemoryOwner, Refusal,
};
#[cfg(not(feature = "std"))]
pub fn checkpoint() {}
#[cfg(not(feature = "std"))]
pub fn charge(_: usize) {}
#[cfg(not(feature = "std"))]
pub fn is_active() -> bool {
    false
}
#[cfg(not(feature = "std"))]
pub fn reserve<T>(values: &mut Vec<T>, additional: usize) {
    values.reserve(additional);
}
pub fn with_capacity<T>(capacity: usize) -> Vec<T> {
    if !is_active() {
        return Vec::with_capacity(capacity);
    }
    let mut values = Vec::new();
    reserve(&mut values, capacity);
    values
}
pub fn clone_vec<T: Clone>(values: &[T]) -> Vec<T> {
    if !is_active() {
        return values.to_vec();
    }
    let mut result = with_capacity(values.len());
    result.extend_from_slice(values);
    result
}
pub fn push<T>(values: &mut Vec<T>, value: T) {
    reserve(values, 1);
    values.push(value);
}
pub fn extend_from_slice<T: Clone>(values: &mut Vec<T>, slice: &[T]) {
    reserve(values, slice.len());
    values.extend_from_slice(slice);
}
pub fn insert<T>(values: &mut Vec<T>, index: usize, value: T) {
    reserve(values, 1);
    values.insert(index, value);
}
pub fn filled<T: Clone>(value: T, count: usize) -> Vec<T> {
    if !is_active() {
        return alloc::vec![value;count];
    }
    let mut result = with_capacity(count);
    result.resize(count, value);
    result
}

#[cfg(all(test, feature = "std", panic = "unwind"))]
mod tests {
    use super::*;
    use alloc::{boxed::Box, string::String, sync::Arc};
    use core::sync::atomic::{AtomicUsize, Ordering};
    struct Cancel {
        at: usize,
        calls: AtomicUsize,
    }
    impl Cancellation for Cancel {
        fn check(&self) -> Result<(), String> {
            if self.calls.fetch_add(1, Ordering::SeqCst) >= self.at {
                Err("path canceled".into())
            } else {
                Ok(())
            }
        }
    }
    struct Lease(Arc<AtomicUsize>);
    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn limits(bytes: usize, elements: usize, work: usize) -> Limits {
        Limits {
            bytes,
            elements,
            work,
            cancellation: Arc::new(Cancel {
                at: usize::MAX,
                calls: AtomicUsize::new(0),
            }),
        }
    }
    #[test]
    fn retained_success_and_partial_error_keep_real_owner_until_payload_drop() {
        let drops = Arc::new(AtomicUsize::new(0));
        let value = with_limits(
            limits(4096, 128, 100),
            Box::new(Lease(drops.clone())),
            || Ok::<_, ()>(with_capacity::<u32>(16)),
        )
        .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(value.get().capacity() >= 16);
        drop(value);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let error = with_limits(
            limits(4096, 128, 100),
            Box::new(Lease(drops.clone())),
            || Err::<(), _>(with_capacity::<u32>(16)),
        )
        .unwrap_err();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(error);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn byte_elements_work_and_cancel_refuse_before_successful_value() {
        for config in [
            limits(8, 128, 100),
            limits(4096, 2, 100),
            limits(4096, 128, 0),
        ] {
            let value = with_limits(config, Box::new(()), || {
                Ok::<_, ()>(with_capacity::<u32>(16))
            });
            assert!(matches!(value, Err(Failure::Refused(_))));
        }
        let mut config = limits(4096, 128, 100);
        config.cancellation = Arc::new(Cancel {
            at: 2,
            calls: AtomicUsize::new(0),
        });
        let result = with_limits(config, Box::new(()), || {
            let mut values = Vec::new();
            for n in 0..100 {
                push(&mut values, n);
            }
            Ok::<_, ()>(values)
        });
        assert!(
            matches!(result,Err(Failure::Refused(Refusal::Cancelled(ref m)))if m=="path canceled")
        );
    }
    #[test]
    fn replacement_capacity_and_both_nested_limits_are_enforced() {
        // The first4 words and replacement8 words coexist in admission.
        let result = with_limits(limits(40, 128, 100), Box::new(()), || {
            let mut v = with_capacity::<u32>(4);
            for n in 0..5 {
                push(&mut v, n);
            }
            Ok::<_, ()>(v)
        });
        assert!(matches!(result, Err(Failure::Refused(Refusal::Bytes))));
        for (outer, inner) in [(8, 4096), (4096, 8)] {
            let result = with_limits(limits(outer, 128, 100), Box::new(()), || {
                let _ignored = with_limits(limits(inner, 128, 100), Box::new(()), || {
                    Ok::<_, ()>(with_capacity::<u32>(16))
                });
                Ok::<_, ()>(())
            });
            assert!(matches!(result, Err(Failure::Refused(Refusal::Bytes))));
        }
    }
    #[test]
    fn unrelated_panic_identity_is_resumed_and_thread_scope_is_restored() {
        let payload = std::panic::catch_unwind(|| {
            let _ = with_limits(
                limits(4096, 128, 100),
                Box::new(()),
                || -> Result<(), ()> {
                    std::panic::resume_unwind(Box::new(731u32));
                },
            );
        })
        .unwrap_err();
        assert_eq!(*payload.downcast::<u32>().unwrap(), 731);
        let result = with_limits(limits(4096, 128, 100), Box::new(()), || {
            Ok::<_, ()>(with_capacity::<u32>(4))
        })
        .unwrap();
        drop(result);
        assert!(!is_active());
        std::thread::spawn(|| {
            assert!(!is_active());
            let v = with_limits(limits(4096, 128, 100), Box::new(()), || {
                Ok::<_, ()>(with_capacity::<u32>(4))
            })
            .unwrap();
            drop(v);
            assert!(!is_active());
        })
        .join()
        .unwrap();
    }
    #[test]
    fn escaped_nested_result_keeps_both_real_owners() {
        let outer = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(AtomicUsize::new(0));
        let value = with_limits(
            limits(4096, 128, 100),
            Box::new(Lease(outer.clone())),
            || {
                let nested = with_limits(
                    limits(4096, 128, 100),
                    Box::new(Lease(inner.clone())),
                    || Ok::<_, ()>(with_capacity::<u32>(8)),
                )
                .unwrap();
                Ok::<_, ()>(nested)
            },
        )
        .unwrap();
        assert_eq!(
            (outer.load(Ordering::SeqCst), inner.load(Ordering::SeqCst)),
            (0, 0)
        );
        assert_eq!(value.get().get().len(), 0);
        assert!(!is_active());
        drop(value);
        assert_eq!(
            (outer.load(Ordering::SeqCst), inner.load(Ordering::SeqCst)),
            (1, 1)
        );
    }
}

#[cfg(all(test, feature = "std", panic = "abort"))]
mod abort_tests {
    use super::*;
    use alloc::{boxed::Box, string::String, sync::Arc};
    struct Control;
    impl Cancellation for Control {
        fn check(&self) -> Result<(), String> {
            panic!("abort profile must refuse before callbacks")
        }
    }
    #[test]
    fn abort_profile_refuses_without_entering_operation_or_callback() {
        let result = with_limits(
            Limits {
                bytes: 4096,
                elements: 128,
                work: 100,
                cancellation: Arc::new(Control),
            },
            Box::new(()),
            || -> Result<(), ()> { panic!("operation was entered") },
        );
        assert!(matches!(
            result,
            Err(Failure::Refused(Refusal::UnsupportedPanicProfile))
        ));
    }
}
