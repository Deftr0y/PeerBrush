//! Caller-provided body storage must already have its actual admission. No
//! body allocation, source lookup or document publication occurs in this codec.
use super::*;
use ring::digest::{Context, SHA256};
use std::io::{Read, Write};

const MAGIC: &[u8; 8] = b"PBPCOD01";
const FIXED: usize = 76;
const CHUNK: usize = 64 * 1024;

pub(in crate::code::worker) struct Frame<'a> {
    pub(in crate::code::worker) message: Message,
    pub(in crate::code::worker) body: &'a [u8],
}
fn hash(control: &[u8], body: &[u8]) -> [u8; 32] {
    let mut hash = Context::new(&SHA256);
    hash.update(control);
    hash.update(body);
    hash.finish().as_ref().try_into().unwrap()
}
fn read(reader: &mut impl Read, bytes: &mut [u8], check: &dyn Fn() -> Result<()>) -> Result<()> {
    for chunk in bytes.chunks_mut(CHUNK) {
        check()?;
        reader.read_exact(chunk).map_err(|_| FAILED.to_owned())?;
    }
    check()
}
fn write(writer: &mut impl Write, bytes: &[u8], check: &dyn Fn() -> Result<()>) -> Result<()> {
    for chunk in bytes.chunks(CHUNK) {
        check()?;
        writer.write_all(chunk).map_err(|_| FAILED.to_owned())?;
    }
    check()
}
impl State {
    fn body(&self, message: &Message, body: &[u8]) -> Result<()> {
        match message {
            Message::Start { metadata_bytes, .. } => {
                let at = *metadata_bytes as usize;
                let metadata = body.get(..at).ok_or(FAILED)?;
                let script = body.get(at..).ok_or(FAILED)?;
                std::str::from_utf8(metadata).map_err(|_| FAILED)?;
                let script = std::str::from_utf8(script).map_err(|_| FAILED)?;
                if script.trim().is_empty() {
                    return Err(FAILED.into());
                }
            }
            Message::Words { .. } if self.config.ok_or(FAILED)?.bit_depth == 8 => {
                if body.chunks_exact(2).any(|word| word[1] != 0) {
                    return Err(FAILED.into());
                }
            }
            Message::Command { .. } => {
                std::str::from_utf8(body).map_err(|_| FAILED)?;
            }
            _ => (),
        }
        Ok(())
    }
    pub(in crate::code::worker) fn send(
        &mut self,
        message: &Message,
        body: &[u8],
        writer: &mut impl Write,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        check()?;
        let nonce = self.nonce.ok_or(FAILED)?;
        let next = self.plan(self.side, nonce, self.sequence, message, body.len())?;
        self.body(message, body)?;
        let mut control = [0u8; HEADER_LIMIT];
        let mut encoded = std::io::Cursor::new(&mut control[..]);
        serde_json::to_writer(&mut encoded, message).map_err(|_| FAILED)?;
        let length = encoded.position() as usize;
        let mut header = [0u8; FIXED];
        header[..8].copy_from_slice(MAGIC);
        header[8..24].copy_from_slice(&nonce);
        header[24..32].copy_from_slice(&self.sequence.to_le_bytes());
        header[32..36].copy_from_slice(&(length as u32).to_le_bytes());
        header[36..44].copy_from_slice(&(body.len() as u64).to_le_bytes());
        header[44..].copy_from_slice(&hash(&control[..length], body));
        // Once writing starts, failure is ambiguous to the peer. No retry can
        // reuse this state, even when a late flush/check reported the failure.
        self.poisoned = true;
        write(writer, &header, check)?;
        write(writer, &control[..length], check)?;
        write(writer, body, check)?;
        writer.flush().map_err(|_| FAILED)?;
        check()?;
        *self = next;
        Ok(())
    }
    pub(in crate::code::worker) fn receive<'a>(
        &mut self,
        reader: &mut impl Read,
        storage: &'a mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Frame<'a>> {
        let previous = self.clone();
        self.poisoned = true;
        if previous.poisoned || previous.expected() != Some(previous.side.other()) {
            return Err(FAILED.into());
        }
        let mut header = [0u8; FIXED];
        read(reader, &mut header, check)?;
        if &header[..8] != MAGIC {
            return Err(FAILED.into());
        }
        let nonce = header[8..24].try_into().unwrap();
        let sequence = u64::from_le_bytes(header[24..32].try_into().unwrap());
        if nonce == [0; 16]
            || previous.nonce.is_some_and(|expected| expected != nonce)
            || sequence != previous.sequence
            || sequence >= FRAME_LIMIT
        {
            return Err(FAILED.into());
        }
        let length = u32::from_le_bytes(header[32..36].try_into().unwrap()) as usize;
        let bytes = u64::from_le_bytes(header[36..44].try_into().unwrap());
        let bytes = usize::try_from(bytes).map_err(|_| FAILED)?;
        if length == 0 || length > HEADER_LIMIT || bytes > MAX_BYTES || bytes > storage.len() {
            return Err(FAILED.into());
        }
        let mut control = [0u8; HEADER_LIMIT];
        read(reader, &mut control[..length], check)?;
        let message: Message = serde_json::from_slice(&control[..length]).map_err(|_| FAILED)?;
        let next = previous.plan(previous.side.other(), nonce, sequence, &message, bytes)?;
        let body = &mut storage[..bytes];
        read(reader, body, check)?;
        if header[44..] != hash(&control[..length], body) {
            return Err(FAILED.into());
        }
        previous.body(&message, body)?;
        check()?;
        *self = next;
        Ok(Frame { message, body })
    }
    /// EOF is a transport requirement. The parent must separately observe a
    /// successful actual child exit before it can publish any collected edit.
    pub(in crate::code::worker) fn exact_eof(
        &mut self,
        reader: &mut impl Read,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        if self.side != Side::Parent || self.poisoned || !matches!(self.phase, Phase::End(true)) {
            return Err(FAILED.into());
        }
        self.poisoned = true;
        check()?;
        let mut byte = [0; 1];
        if reader.read(&mut byte).map_err(|_| FAILED)? != 0 {
            return Err(FAILED.into());
        }
        check()?;
        self.poisoned = false;
        self.eof = true;
        Ok(())
    }
}
