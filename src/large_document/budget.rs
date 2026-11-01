use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use super::{Error, Result};

/// Shared capacity accounting for native byte buffers, not a process RAM claim.
/// Engine metadata, allocator overhead and render scratch require their own admission.
#[derive(Clone)]
pub struct ResidentBudget(Arc<Budget>);

struct Budget {
    limit: usize,
    used: AtomicUsize,
}

/// A reservation for an external buffer; retain it until that allocation is
/// released or transferred to a separately enforced admission policy.
pub struct Reservation {
    budget: ResidentBudget,
    bytes: usize,
}
pub(super) type Charge = Reservation;

impl Reservation {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    /// External frozen owners only: all replaced growth buffers must already be
    /// dropped, and `bytes` must still cover every surviving allocation capacity.
    /// Payload fields must drop before this charge; no growth or escaping owner
    /// is permitted after the final reservation is reduced.
    pub(crate) fn shrink_to(&mut self, bytes: usize) -> Result<()> {
        let release = self.bytes.checked_sub(bytes).ok_or(Error::Budget)?;
        self.budget
            .0
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_sub(release)
            })
            .map_err(|_| Error::Corrupt)?;
        self.bytes = bytes;
        Ok(())
    }
}

impl ResidentBudget {
    pub fn new(limit: usize) -> Result<Self> {
        if limit == 0 {
            return Err(Error::Invalid("Resident byte budget must be positive"));
        }
        Ok(Self(Arc::new(Budget {
            limit,
            used: AtomicUsize::new(0),
        })))
    }

    pub fn used(&self) -> usize {
        self.0.used.load(Ordering::Acquire)
    }

    pub fn limit(&self) -> usize {
        self.0.limit
    }

    pub fn same_pool(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn reserve_bytes(&self, length: usize, maximum: usize) -> Result<Reservation> {
        if maximum > 64 * 1024 * 1024 || length > maximum {
            return Err(Error::Budget);
        }
        self.reserve(length)
    }

    pub(super) fn reserve(&self, bytes: usize) -> Result<Charge> {
        self.0
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|next| *next <= self.0.limit)
            })
            .map_err(|_| Error::Budget)?;
        Ok(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
}

pub(super) fn allocate(bytes: usize) -> Result<Box<[u8]>> {
    #[cfg(test)]
    if REFUSE_NEXT.with(|refuse| refuse.replace(false)) {
        return Err(Error::Budget);
    }
    if bytes == 0 {
        return Ok(Box::default());
    }
    let layout = std::alloc::Layout::array::<u8>(bytes).map_err(|_| Error::Budget)?;
    // SAFETY: Layout is nonzero and validated. The global allocator is paired
    // with Box's global deallocator below; all u8 values are initialized to zero.
    let pointer =
        std::ptr::NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) }).ok_or(Error::Budget)?;
    let slice = std::ptr::slice_from_raw_parts_mut(pointer.as_ptr(), bytes);
    // SAFETY: This uniquely owned allocation has precisely the layout of [u8]
    // with this length. No reference or other owner exists before this Box.
    Ok(unsafe { Box::from_raw(slice) })
}

#[cfg(test)]
std::thread_local! { static REFUSE_NEXT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[cfg(test)]
pub(super) fn refuse_next_allocation() {
    REFUSE_NEXT.with(|refuse| refuse.set(true));
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.0.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_reservations_refuse_overflow_and_return_capacity_on_every_exit() {
        let budget = ResidentBudget::new(1024).unwrap();
        let first = budget.reserve(512).unwrap();
        let clone = budget.clone();
        assert!(budget.same_pool(&clone));
        assert!(!budget.same_pool(&ResidentBudget::new(1024).unwrap()));
        assert_eq!(clone.used(), 512);
        assert!(matches!(clone.reserve(513), Err(Error::Budget)));
        assert!(matches!(clone.reserve(usize::MAX), Err(Error::Budget)));
        let second = budget.reserve(256).unwrap();
        assert_eq!(clone.used(), 768);
        drop(second);
        assert_eq!(clone.used(), 512);
        drop(first);
        assert_eq!(budget.used(), 0);
        assert_eq!(Arc::strong_count(&budget.0), 2);
    }

    #[test]
    fn byte_allocations_have_exact_lengths_and_reject_invalid_layouts() {
        assert!(allocate(0).unwrap().is_empty());
        for length in [1, 63, 65537, 512 * 1024] {
            let bytes = allocate(length).unwrap();
            assert_eq!(bytes.len(), length);
            assert!(bytes.iter().all(|b| *b == 0));
        }
        assert!(matches!(allocate(usize::MAX), Err(Error::Budget)));
    }

    #[test]
    fn frozen_external_owner_can_release_only_its_unused_peak_reservation() {
        let budget = ResidentBudget::new(1024).unwrap();
        let shared = budget.clone();
        let mut peak = budget.reserve_bytes(768, 1024).unwrap();
        let other = shared.reserve_bytes(128, 128).unwrap();
        assert_eq!(shared.used(), 896);
        assert!(matches!(peak.shrink_to(769), Err(Error::Budget)));
        assert_eq!(peak.bytes(), 768);
        assert_eq!(shared.used(), 896);
        peak.shrink_to(256).unwrap();
        assert_eq!(peak.bytes(), 256);
        assert_eq!(shared.used(), 384);
        peak.shrink_to(256).unwrap();
        assert_eq!(shared.used(), 384);
        let released = shared.reserve_bytes(640, 640).unwrap();
        assert_eq!(shared.used(), 1024);
        assert!(matches!(shared.reserve_bytes(1, 1), Err(Error::Budget)));
        assert!(matches!(peak.shrink_to(usize::MAX), Err(Error::Budget)));
        drop(released);
        drop(other);
        assert_eq!(shared.used(), 256);
        drop(peak);
        assert_eq!(shared.used(), 0);
    }
}
