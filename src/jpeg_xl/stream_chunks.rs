//! Private original-byte working cache. The closed broker alone supplies the
//! source identity, checks and frozen-index authenticated complete-chunk loader.
//! This component grants no source, file, encoder or publication authority.
use crate::large_document::{Reservation, ResidentBudget, ScratchBytes};

pub(super) const CHUNK: usize = 65536;
pub(super) const CACHE_BYTES: usize = 4 * CHUNK;
pub(super) const HEADERS: usize = 4096;
type Result<T> = std::result::Result<T, String>;
struct Entry {
    bytes: ScratchBytes,
    chunk: Option<u64>,
    touched: u64,
}
pub(super) struct OriginalChunks {
    entries: [Option<Entry>; 4],
    identity: [u8; 32],
    length: u64,
    pool: ResidentBudget,
    clock: u64,
    failed: bool,
    _headers: Reservation,
}
const _: () = assert!(std::mem::size_of::<OriginalChunks>() <= HEADERS);
impl OriginalChunks {
    pub(super) fn new(pool: &ResidentBudget, identity: [u8; 32], length: u64) -> Result<Self> {
        if length == 0 {
            return Err("Streaming original cache requires a sealed source".into());
        }
        let headers = pool
            .reserve_bytes(HEADERS, HEADERS)
            .map_err(|e| e.to_string())?;
        let mut entries = std::array::from_fn(|_| None);
        for entry in &mut entries {
            *entry = Some(Entry {
                bytes: ScratchBytes::new(pool, CHUNK, CHUNK).map_err(|e| e.to_string())?,
                chunk: None,
                touched: 0,
            });
        }
        Ok(Self {
            entries,
            identity,
            length,
            pool: pool.clone(),
            clock: 0,
            failed: false,
            _headers: headers,
        })
    }
    /// Loader must fill exactly the original complete chunk prefix and verify
    /// its immutable initial hash before success. Failed loads invalidate the
    /// slot before I/O; a later request cannot consume a provisional replacement.
    pub(super) fn copy(
        &mut self,
        identity: [u8; 32],
        start: u64,
        output: &mut ScratchBytes,
        check: &dyn Fn() -> Result<()>,
        mut load: impl FnMut(u64, usize, &mut ScratchBytes) -> Result<()>,
    ) -> Result<()> {
        let result = (|| {
            if self.failed
                || identity != self.identity
                || !output.uses_budget(&self.pool)
                || start
                    .checked_add(output.len() as u64)
                    .is_none_or(|n| n > self.length)
            {
                return Err("Streaming original cache identity, range or pool differs".into());
            }
            check()?;
            let mut copied = 0;
            while copied < output.len() {
                check()?;
                let at = start + copied as u64;
                let chunk = at / CHUNK as u64;
                let begin = chunk * CHUNK as u64;
                let length = (self.length - begin).min(CHUNK as u64) as usize;
                let hit = self
                    .entries
                    .iter()
                    .position(|e| e.as_ref().is_some_and(|e| e.chunk == Some(chunk)));
                let slot = hit.unwrap_or_else(|| {
                    self.entries
                        .iter()
                        .position(|e| e.as_ref().is_some_and(|e| e.chunk.is_none()))
                        .unwrap_or_else(|| {
                            self.entries
                                .iter()
                                .enumerate()
                                .min_by_key(|(_, e)| e.as_ref().map(|e| e.touched).unwrap_or(0))
                                .map(|(i, _)| i)
                                .unwrap_or(0)
                        })
                });
                let entry = self.entries[slot]
                    .as_mut()
                    .ok_or("Streaming cache slot missing")?;
                if hit.is_none() {
                    entry.chunk = None;
                    load(begin, length, &mut entry.bytes)?;
                    check()?;
                    entry.chunk = Some(chunk);
                }
                self.clock = self
                    .clock
                    .checked_add(1)
                    .ok_or("Streaming cache age overflow")?;
                entry.touched = self.clock;
                let offset = (at - begin) as usize;
                let count = (length - offset).min(output.len() - copied);
                output[copied..copied + count]
                    .copy_from_slice(&entry.bytes[offset..offset + count]);
                copied += count;
                check()?;
            }
            check()
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        sync::{Arc, Mutex},
    };
    fn pool() -> ResidentBudget {
        ResidentBudget::new(CACHE_BYTES + HEADERS + 3 * CHUNK).unwrap()
    }
    fn byte(at: u64) -> u8 {
        (at.wrapping_mul(71).wrapping_add(at / 97) & 255) as u8
    }
    #[test]
    fn original_ranges_cross_chunks_and_lru_eviction_never_reuse_wrong_bytes() {
        let pool = pool();
        let identity = [3; 32];
        let mut cache = OriginalChunks::new(&pool, identity, 7 * CHUNK as u64 + 13).unwrap();
        let loads = Cell::new(0usize);
        let mut physical = 0;
        let mut load = |at: u64, n: usize, out: &mut ScratchBytes| {
            loads.set(loads.get() + 1);
            physical += n;
            for (i, b) in out[..n].iter_mut().enumerate() {
                *b = byte(at + i as u64);
            }
            Ok(())
        };
        let mut output = ScratchBytes::new(&pool, 24, CHUNK).unwrap();
        for start in [
            CHUNK as u64 - 10,
            1,
            2 * CHUNK as u64,
            3 * CHUNK as u64,
            1,
            4 * CHUNK as u64,
            5 * CHUNK as u64,
            1,
            7 * CHUNK as u64 - 11,
        ] {
            cache
                .copy(identity, start, &mut output, &|| Ok(()), &mut load)
                .unwrap();
            for (i, b) in output.iter().enumerate() {
                assert_eq!(*b, byte(start + i as u64));
            }
        }
        drop(load);
        assert!(loads.get() < 12);
        assert_eq!(physical % CHUNK, 13);
        drop(output);
        drop(cache);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn hits_still_check_source_cancel_identity_and_exact_pool() {
        let pool = pool();
        let identity = [4; 32];
        let mut cache = OriginalChunks::new(&pool, identity, 100).unwrap();
        let mut out = ScratchBytes::new(&pool, 16, CHUNK).unwrap();
        let loads = Cell::new(0);
        let load = |_: u64, n: usize, b: &mut ScratchBytes| {
            loads.set(loads.get() + 1);
            b[..n].fill(8);
            Ok(())
        };
        cache.copy(identity, 0, &mut out, &|| Ok(()), load).unwrap();
        cache.copy(identity, 8, &mut out, &|| Ok(()), load).unwrap();
        assert_eq!(loads.get(), 1);
        assert!(cache
            .copy(
                identity,
                0,
                &mut out,
                &|| Err("source changed".into()),
                load
            )
            .is_err_and(|e| e == "source changed"));
        assert!(cache.copy(identity, 0, &mut out, &|| Ok(()), load).is_err());
        drop(cache);
        let mut cache = OriginalChunks::new(&pool, identity, 100).unwrap();
        assert!(cache.copy([5; 32], 0, &mut out, &|| Ok(()), load).is_err());
        drop(cache);
        let other = ResidentBudget::new(CHUNK).unwrap();
        let mut foreign = ScratchBytes::new(&other, 16, CHUNK).unwrap();
        let mut cache = OriginalChunks::new(&pool, identity, 100).unwrap();
        assert!(cache
            .copy(identity, 0, &mut foreign, &|| Ok(()), load)
            .is_err());
    }
    #[test]
    fn failed_replacement_latches_and_all_cache_charges_follow_retained_owner() {
        let pool = pool();
        let cache = Arc::new(Mutex::new(
            OriginalChunks::new(&pool, [1; 32], CHUNK as u64).unwrap(),
        ));
        let retained = cache.clone();
        let before = pool.used();
        let mut out = ScratchBytes::new(&pool, 10, CHUNK).unwrap();
        assert!(cache
            .lock()
            .unwrap()
            .copy([1; 32], 0, &mut out, &|| Ok(()), |_, _, b| {
                b[0] = 99;
                Err("failed authenticated load".into())
            })
            .is_err_and(|e| e == "failed authenticated load"));
        assert!(cache
            .lock()
            .unwrap()
            .copy([1; 32], 0, &mut out, &|| Ok(()), |_, _, _| panic!(
                "failed cache retried"
            ))
            .is_err());
        drop(out);
        drop(cache);
        assert_eq!(pool.used(), before);
        drop(retained);
        assert_eq!(pool.used(), 0);
        let too_small = ResidentBudget::new(HEADERS + CHUNK).unwrap();
        assert!(OriginalChunks::new(&too_small, [1; 32], 100).is_err());
        assert_eq!(too_small.used(), 0);
    }
}
