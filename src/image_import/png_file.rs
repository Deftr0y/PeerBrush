//! PNG validation over the shared held, hash-bound image transport.
use super::{
    container_stream, decode_input, ColorPolicy, Complete, Header, PassRow, SourceBinding,
};
pub(crate) use crate::image_import::source_file::FileLimits;
use crate::{image_import::source_file::FileSource, workspace::source_runtime::SourceSession};
use ring::digest::{Context, SHA256};
use std::{
    io::{self, BufRead, Read, Seek, SeekFrom},
    path::Path,
};

pub(crate) struct FilePng<'a> {
    source: FileSource<'a>,
    facts: (u32, u32, bool),
    color: ColorPolicy,
    embedded: Option<(u64, u64, [u8; 32])>,
}
impl<'a> FilePng<'a> {
    pub(crate) fn open(
        session: &'a SourceSession,
        path: &Path,
        limits: FileLimits,
        color: ColorPolicy,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        if color == ColorPolicy::ToSrgb {
            return Err("Held native PNG sources require raw color policy".into());
        }
        let mut source = FileSource::open(session, path, limits, "png", check)?;
        source.start_read()?;
        let length = source.length();
        let facts = container_stream(
            &mut source,
            length,
            limits.edge,
            limits.pixels,
            color,
            check,
        )?;
        source.admit(check)?;
        check()?;
        Ok(Self {
            source,
            facts,
            color,
            embedded: None,
        })
    }
    /// The container retains its own whole-file proof; PNG rows bind the exact
    /// selected payload, including ignored chunks, rather than another variant.
    pub(in crate::image_import) fn embedded(
        mut source: FileSource<'a>,
        offset: u64,
        length: u64,
        limits: FileLimits,
        color: ColorPolicy,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        if color == ColorPolicy::ToSrgb
            || length == 0
            || offset
                .checked_add(length)
                .is_none_or(|end| end > source.length())
        {
            return Err("Invalid embedded PNG range or color policy".into());
        }
        source.start_read()?;
        source.skip(offset, check)?;
        let mut range = Range::new(&mut source, length);
        let facts = container_stream(&mut range, length, limits.edge, limits.pixels, color, check)?;
        let hash = range.finish(check)?;
        source.finish(check)?;
        Ok(Self {
            source,
            facts,
            color,
            embedded: Some((offset, length, hash)),
        })
    }
    pub(in crate::image_import) fn binding(
        &self,
        session: &SourceSession,
    ) -> Result<SourceBinding, String> {
        let binding = self.source.binding(session)?;
        Ok(self.embedded.map_or(binding, |(_, _, hash)| {
            SourceBinding::file(hash, self.source.pool())
        }))
    }
    pub(crate) fn recheck(&self) -> Result<(), String> {
        self.source.recheck()
    }
    /// Rehash the held source after a caller has sampled its staged pixels.
    pub(crate) fn verify(
        &mut self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.source.start_read()?;
        self.source.finish(check)
    }
    pub(in crate::image_import) fn decode_pass_rows(
        &mut self,
        check: &mut dyn FnMut() -> Result<(), String>,
        row: impl FnMut(Header, PassRow, &[u8]) -> Result<(), String>,
    ) -> Result<Complete, String> {
        self.source.start_read()?;
        let pool = self.source.pool();
        let complete = if let Some((offset, length, hash)) = self.embedded {
            self.source.skip(offset, check)?;
            let mut range = Range::new(&mut self.source, length);
            let complete = decode_input(
                &mut range,
                self.facts,
                SourceBinding::file(hash, pool.clone()),
                self.color,
                &pool,
                check,
                row,
            )?;
            if range.finish(check)? != hash {
                return Err("Embedded PNG differs from its admitted payload".into());
            }
            complete
        } else {
            let binding = self.source.source_binding()?;
            decode_input(
                &mut self.source,
                self.facts,
                binding,
                self.color,
                &pool,
                check,
                row,
            )?
        };
        self.source.finish(check)?;
        check()?;
        Ok(complete)
    }
    pub(in crate::image_import) fn color_policy(&self) -> ColorPolicy {
        self.color
    }
}

/// A monotonic, relative-position PNG view. Hash only consumed payload bytes;
/// the enclosing FileSource separately hashes every original container byte.
struct Range<'a, 's> {
    source: &'a mut FileSource<'s>,
    remaining: u64,
    position: u64,
    digest: Context,
}
impl<'a, 's> Range<'a, 's> {
    fn new(source: &'a mut FileSource<'s>, length: u64) -> Self {
        Self {
            source,
            remaining: length,
            position: 0,
            digest: Context::new(&SHA256),
        }
    }
    fn finish(mut self, check: &mut dyn FnMut() -> Result<(), String>) -> Result<[u8; 32], String> {
        while self.remaining != 0 {
            check()?;
            let count = self.fill_buf().map_err(|e| e.to_string())?.len();
            if count == 0 {
                return Err("Truncated embedded PNG".into());
            }
            self.consume(count);
        }
        Ok(self.digest.finish().as_ref().try_into().unwrap())
    }
}
impl BufRead for Range<'_, '_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.remaining == 0 {
            return Ok(&[]);
        }
        let bytes = self.source.fill_buf()?;
        Ok(&bytes[..bytes.len().min(self.remaining as usize)])
    }
    fn consume(&mut self, amount: usize) {
        // consume follows a successful fill_buf; FileSource cannot refill here.
        let bytes = self.source.buffered();
        let count = amount.min(bytes.len()).min(self.remaining as usize);
        self.digest.update(&bytes[..count]);
        self.source.consume(count);
        self.remaining -= count as u64;
        self.position += count as u64;
    }
}
impl Read for Range<'_, '_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = self.fill_buf()?;
        let count = bytes.len().min(output.len());
        output[..count].copy_from_slice(&bytes[..count]);
        self.consume(count);
        Ok(count)
    }
}
impl Seek for Range<'_, '_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        match to {
            SeekFrom::Current(0) => Ok(self.position),
            SeekFrom::Start(at) if at == self.position => Ok(at),
            _ => Err(io::Error::other(
                "Embedded PNG requested an unadmitted seek",
            )),
        }
    }
}
