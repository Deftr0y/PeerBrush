//! Bounded owned transport bytes. Every payload and descriptor stays admitted.
use crate::large_document::{PinnedBytes, Reservation, ResidentBudget, ScratchBytes};
use std::io::{self, Read, Write};

const CHUNK: usize = 1024 * 1024;
pub(crate) const MAX_BYTES: usize = 512 * 1024 * 1024;
pub(crate) struct Segments {
    chunks: Vec<PinnedBytes>,
    length: usize,
    pool: ResidentBudget,
    _index: Reservation,
}
impl Segments {
    pub(crate) fn len(&self) -> usize {
        self.length
    }
    pub(crate) fn retained_bytes(&self) -> usize {
        self._index.bytes()
            + self
                .chunks
                .iter()
                .map(PinnedBytes::allocated_bytes)
                .sum::<usize>()
    }
    pub(crate) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.pool.same_pool(pool) && self.chunks.iter().all(|bytes| bytes.uses_budget(pool))
    }
    pub(crate) fn parts(&self) -> impl Iterator<Item = &[u8]> {
        self.chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| &chunk[..self.length.saturating_sub(index * CHUNK).min(CHUNK)])
    }
    pub(crate) fn reader(self) -> SegmentReader {
        SegmentReader {
            bytes: self,
            chunk: 0,
            at: 0,
        }
    }
}
pub(crate) struct SegmentReader {
    bytes: Segments,
    chunk: usize,
    at: usize,
}
impl Read for SegmentReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.chunk >= self.bytes.chunks.len() {
            return Ok(0);
        }
        let logical = self
            .bytes
            .length
            .saturating_sub(self.chunk * CHUNK)
            .min(CHUNK);
        let count = output.len().min(logical - self.at);
        output[..count].copy_from_slice(&self.bytes.chunks[self.chunk][self.at..self.at + count]);
        self.at += count;
        if self.at == logical {
            self.chunk += 1;
            self.at = 0;
        }
        Ok(count)
    }
}
pub(crate) struct SegmentWriter<'a> {
    bytes: Segments,
    current: Option<ScratchBytes>,
    used: usize,
    maximum: usize,
    failed: bool,
    check: &'a mut dyn FnMut() -> Result<(), String>,
}
impl<'a> SegmentWriter<'a> {
    pub(crate) fn new(
        pool: &ResidentBudget,
        maximum: usize,
        check: &'a mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if maximum == 0 || maximum > MAX_BYTES {
            return Err("Transport byte admission exceeds its bound".into());
        }
        let count = maximum.div_ceil(CHUNK);
        let index_bytes = count
            .checked_mul(std::mem::size_of::<PinnedBytes>())
            .and_then(|n| n.checked_add(128))
            .ok_or("Transport descriptor admission overflow")?;
        let index = pool
            .reserve_bytes(index_bytes, index_bytes)
            .map_err(|e| e.to_string())?;
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(count)
            .map_err(|_| "Transport descriptor allocation refused")?;
        if chunks.capacity() * std::mem::size_of::<PinnedBytes>() > index.bytes() {
            return Err("Transport descriptor capacity exceeds admission".into());
        }
        Ok(Self {
            bytes: Segments {
                chunks,
                length: 0,
                pool: pool.clone(),
                _index: index,
            },
            current: None,
            used: 0,
            maximum,
            failed: false,
            check,
        })
    }
    pub(crate) fn finish(mut self) -> Result<Segments, String> {
        if self.failed {
            return Err("Transport writer is failed".into());
        }
        (self.check)()?;
        if let Some(current) = self.current.take() {
            if self.used > 0 {
                self.bytes.chunks.push(current.freeze());
            }
        }
        Ok(self.bytes)
    }
    fn append(&mut self, mut input: &[u8]) -> Result<(), String> {
        (self.check)()?;
        if self.failed
            || self
                .bytes
                .length
                .checked_add(input.len())
                .is_none_or(|n| n > self.maximum)
        {
            return Err("Transport bytes exceed their admitted bound".into());
        }
        while !input.is_empty() {
            (self.check)()?;
            if self.current.is_none() {
                self.current = Some(
                    ScratchBytes::new(&self.bytes.pool, CHUNK, CHUNK).map_err(|e| e.to_string())?,
                );
                self.used = 0;
            }
            let count = input.len().min(CHUNK - self.used);
            self.current.as_mut().unwrap()[self.used..self.used + count]
                .copy_from_slice(&input[..count]);
            self.used += count;
            self.bytes.length += count;
            input = &input[count..];
            if self.used == CHUNK {
                self.bytes
                    .chunks
                    .push(self.current.take().unwrap().freeze());
                self.used = 0;
            }
        }
        Ok(())
    }
}
impl Write for SegmentWriter<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        match self.append(input) {
            Ok(()) => Ok(input.len()),
            Err(reason) => {
                self.failed = true;
                Err(io::Error::other(reason))
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other("Transport writer is failed"));
        }
        if let Err(reason) = (self.check)() {
            self.failed = true;
            return Err(io::Error::other(reason));
        }
        Ok(())
    }
}
