//! Latest-write output coverage. No file, source or publication authority.
use super::super::{Error, Reservation, ResidentBudget, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Interval {
    pub start: u64,
    pub end: u64,
    pub record_offset: u64,
    pub record_sha256: [u8; 32],
}

pub(super) struct Coverage {
    intervals: Vec<Interval>,
    // Entries and their allocation drop before the charge.
    charge: Option<Reservation>,
    budget: ResidentBudget,
    maximum: usize,
}

#[derive(Clone, Copy)]
pub(super) struct Placement {
    start: u64,
    end: u64,
    first: usize,
    last: usize,
    left: Option<Interval>,
    right: Option<Interval>,
}
impl Coverage {
    pub fn new(budget: ResidentBudget, maximum_records: u32) -> Result<Self> {
        if maximum_records == 0 || maximum_records > 131072 {
            return Err(Error::Budget);
        }
        let maximum = (maximum_records as usize)
            .checked_mul(2)
            .and_then(|n| n.checked_add(1))
            .ok_or(Error::Budget)?;
        Ok(Self {
            intervals: Vec::new(),
            charge: None,
            budget,
            maximum,
        })
    }
    pub fn intervals(&self) -> &[Interval] {
        &self.intervals
    }
    /// Reserve the complete replacement footprint before a physical append.
    /// The returned placement is used only by the same exclusively held writer.
    pub fn prepare(&mut self, start: u64, length: u32) -> Result<Placement> {
        if length == 0 || length > 65536 {
            return Err(Error::Invalid(
                "Segment payload must be bounded and nonempty",
            ));
        }
        let end = start.checked_add(u64::from(length)).ok_or(Error::Budget)?;
        let first = self.intervals.partition_point(|item| item.end <= start);
        let last = self.intervals.partition_point(|item| item.start < end);
        let left = self.intervals.get(first).copied().and_then(|mut item| {
            if item.start < start {
                item.end = start;
                Some(item)
            } else {
                None
            }
        });
        let right = last.checked_sub(1).and_then(|at| {
            self.intervals.get(at).copied().and_then(|mut item| {
                if item.end > end {
                    item.start = end;
                    Some(item)
                } else {
                    None
                }
            })
        });
        let needed = self.intervals.len() - (last - first)
            + usize::from(left.is_some())
            + 1
            + usize::from(right.is_some());
        self.reserve(needed)?;
        Ok(Placement {
            start,
            end,
            first,
            last,
            left,
            right,
        })
    }
    /// No allocation or fallible operation follows the physical append.
    pub fn apply(&mut self, placement: Placement, record_offset: u64, record_sha256: [u8; 32]) {
        let Placement {
            start,
            end,
            first,
            last,
            left,
            right,
        } = placement;
        self.intervals.drain(first..last);
        let new = Interval {
            start,
            end,
            record_offset,
            record_sha256,
        };
        let mut at = first;
        for item in [left, Some(new), right].into_iter().flatten() {
            self.intervals.insert(at, item);
            at += 1;
        }
    }
    pub fn covers(&self, end: u64) -> bool {
        let mut at = 0;
        for interval in &self.intervals {
            if at >= end {
                break;
            }
            if interval.start != at || interval.end <= interval.start {
                return false;
            }
            at = interval.end;
        }
        at >= end
    }
    fn reserve(&mut self, needed: usize) -> Result<()> {
        if needed > self.maximum {
            return Err(Error::Budget);
        }
        if needed <= self.intervals.capacity() {
            return Ok(());
        }
        let capacity = self
            .intervals
            .capacity()
            .checked_mul(2)
            .ok_or(Error::Budget)?
            .max(needed)
            .min(self.maximum);
        let admitted = capacity
            .checked_mul(std::mem::size_of::<Interval>())
            .and_then(|n| n.checked_mul(2))
            .ok_or(Error::Budget)?;
        if admitted > 64 * 1024 * 1024 {
            return Err(Error::Budget);
        }
        let charge = self.budget.reserve_index(admitted)?;
        let mut intervals = Vec::new();
        intervals
            .try_reserve_exact(capacity)
            .map_err(|_| Error::Budget)?;
        if intervals
            .capacity()
            .checked_mul(std::mem::size_of::<Interval>())
            .is_none_or(|n| n > admitted)
        {
            return Err(Error::Budget);
        }
        intervals.extend_from_slice(&self.intervals);
        let old_intervals = std::mem::replace(&mut self.intervals, intervals);
        let old_charge = self.charge.replace(charge);
        drop(old_intervals);
        drop(old_charge);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn put(coverage: &mut Coverage, start: u64, length: u32, identity: u8) {
        let placement = coverage.prepare(start, length).unwrap();
        coverage.apply(placement, identity as u64, [identity; 32]);
    }
    #[test]
    fn overlap_splits_survivors_and_forward_coverage_does_not_fill_gaps() {
        let pool = ResidentBudget::new(32768).unwrap();
        let mut coverage = Coverage::new(pool.clone(), 32).unwrap();
        put(&mut coverage, 8, 4, 1);
        assert!(!coverage.covers(12));
        put(&mut coverage, 0, 8, 2);
        assert!(coverage.covers(12));
        put(&mut coverage, 2, 4, 3);
        assert_eq!(
            coverage
                .intervals()
                .iter()
                .map(|i| (i.start, i.end, i.record_offset))
                .collect::<Vec<_>>(),
            [(0, 2, 2), (2, 6, 3), (6, 8, 2), (8, 12, 1)]
        );
        put(&mut coverage, 1, 10, 4);
        assert_eq!(
            coverage
                .intervals()
                .iter()
                .map(|i| (i.start, i.end, i.record_offset))
                .collect::<Vec<_>>(),
            [(0, 1, 2), (1, 11, 4), (11, 12, 1)]
        );
        assert!(coverage.covers(12));
        assert!(!coverage.covers(13));
        assert!(pool.used() > 0);
        drop(coverage);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn capacity_refusal_preserves_previous_index_and_exact_charge() {
        let size = std::mem::size_of::<Interval>();
        let pool = ResidentBudget::new(2 * size).unwrap();
        let mut coverage = Coverage::new(pool.clone(), 2).unwrap();
        put(&mut coverage, 0, 4, 1);
        let baseline = pool.used();
        assert_eq!(baseline, 2 * size);
        assert!(matches!(coverage.prepare(1, 1), Err(Error::Budget)));
        assert_eq!(pool.used(), baseline);
        assert_eq!(coverage.intervals().len(), 1);
        assert_eq!(coverage.intervals()[0].start, 0);
        assert_eq!(coverage.intervals()[0].end, 4);
        drop(coverage);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn varied_small_transcripts_match_bytewise_last_writer_oracle() {
        let pool = ResidentBudget::new(2 * 1024 * 1024).unwrap();
        let mut coverage = Coverage::new(pool.clone(), 2000).unwrap();
        let mut reference = [None; 257];
        let mut seed = 0x69cb_18e2u32;
        for number in 1..=2000u32 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let start = seed as usize % reference.len();
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let length = (seed as usize % 37 + 1).min(reference.len() - start);
            let placement = coverage.prepare(start as u64, length as u32).unwrap();
            coverage.apply(placement, number as u64, [0; 32]);
            reference[start..start + length].fill(Some(number as u64));
            for (position, expected) in reference.iter().enumerate() {
                let actual = coverage
                    .intervals()
                    .iter()
                    .find(|i| i.start <= position as u64 && i.end > position as u64)
                    .map(|i| i.record_offset);
                assert_eq!(actual, *expected, "write {number}, offset {position}");
            }
            for end in [0, 1, 16, 128, 257] {
                assert_eq!(
                    coverage.covers(end),
                    reference[..end as usize].iter().all(Option::is_some)
                );
            }
        }
        drop(coverage);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn bounds_and_noop_reservation_do_not_leak() {
        let pool = ResidentBudget::new(4096).unwrap();
        assert!(Coverage::new(pool.clone(), 0).is_err());
        assert!(Coverage::new(pool.clone(), 131073).is_err());
        let mut coverage = Coverage::new(pool.clone(), 1).unwrap();
        assert!(coverage.prepare(0, 0).is_err());
        assert!(coverage.prepare(0, 65537).is_err());
        assert!(coverage.prepare(u64::MAX, 1).is_err());
        assert_eq!(pool.used(), 0);
        let prepared = coverage.prepare(0, 1).unwrap();
        assert!(coverage.intervals().is_empty());
        assert!(pool.used() > 0);
        coverage.apply(prepared, 0, [0; 32]);
        let baseline = pool.used();
        put(&mut coverage, 0, 1, 2);
        assert_eq!(pool.used(), baseline);
        drop(coverage);
        assert_eq!(pool.used(), 0);
    }
}
