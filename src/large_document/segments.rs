//! Compatible mode1 output logs; no encoder-success or publication authority.
#[path = "segments/coverage.rs"]
mod coverage;
use super::{
    linear,
    store::{read_exact_at, stamp, write_all_at},
    ByteRole, DiskBudget, Error, LargeSourceTemporary, ResidentBudget, Result, ScratchBytes,
};
use coverage::Coverage;
use ring::digest::{Context, SHA256};
use std::path::Path;
type Digest = [u8; 32];
type Check<'a> = &'a mut dyn FnMut() -> Result<()>;
const CHUNK: usize = 65536;
const HEADER: u64 = 128;
const SEAL: u64 = 128;
const EVENT: u64 = 96;
const MAX_LOGICAL: u64 = 64 * 1024 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct SegmentLimits {
    pub output_bytes: u64,
    pub protocol_log_bytes: u64,
    pub records: u32,
    pub events: u32,
}
impl SegmentLimits {
    pub(crate) fn physical_ceiling(self) -> Result<u64> {
        if self.output_bytes == 0
            || self.output_bytes > MAX_LOGICAL
            || self.protocol_log_bytes == 0
            || self.protocol_log_bytes > MAX_LOGICAL
            || self.records == 0
            || self.records > 131072
            || self.events == 0
            || self.events > 1048576
            || self.events < self.records
        {
            return Err(Error::Budget);
        }
        HEADER
            .checked_add(SEAL)
            .and_then(|n| n.checked_add(self.protocol_log_bytes))
            .and_then(|n| n.checked_add(48 * u64::from(self.records)))
            .and_then(|n| n.checked_add(EVENT * u64::from(self.events)))
            .ok_or(Error::Budget)
    }
}
#[derive(Clone, Copy)]
pub(crate) enum SegmentEvent {
    Data {
        offset: u64,
        sha256: Digest,
    },
    Seek {
        position: u64,
    },
    Finalized {
        position: u64,
    },
    /// Ready, rectangle request or release; transport verifies their full fields.
    Observed {
        wire_kind: u32,
    },
    Done {
        bytes: u64,
        records: u32,
    },
}

pub(crate) struct SegmentLog {
    owned: linear::Owned,
    budget: ResidentBudget,
    header: [u8; 128],
    limits: SegmentLimits,
    coverage: Coverage,
    transcript: Context,
    written: u64,
    events: u32,
    records: u32,
    protocol_bytes: u64,
    position: u64,
    highest: u64,
    finalized: u64,
    done: bool,
    poisoned: bool,
}
pub(crate) struct CompletedSegments {
    log: SegmentLog,
    seal: [u8; 128],
    transcript_sha256: Digest,
}
pub(crate) struct SegmentReplay {
    pub bytes: u64,
    pub sha256: Digest,
}
impl SegmentLog {
    pub(crate) fn create(
        root: &Path,
        limits: SegmentLimits,
        nonce: [u8; 16],
        binding: Digest,
        budget: ResidentBudget,
        disk: DiskBudget,
    ) -> Result<Self> {
        let ceiling = limits.physical_ceiling()?;
        if nonce == [0; 16] || binding == [0; 32] {
            return Err(Error::Invalid("Output log lacks its operation binding"));
        }
        let coverage = Coverage::new(budget.clone(), limits.records)?;
        let mut header = [0; 128];
        header[..8].copy_from_slice(b"PBJXLOG1");
        header[8..12].copy_from_slice(&1u32.to_be_bytes());
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut header[16..32])
            .map_err(|_| Error::Invalid("Output log identity creation failed"))?;
        if header[16..32] == [0; 16] {
            return Err(Error::Invalid("Output log identity creation failed"));
        }
        header[32..48].copy_from_slice(&nonce);
        header[48..80].copy_from_slice(&binding);
        header[80..88].copy_from_slice(&limits.output_bytes.to_be_bytes());
        header[88..96].copy_from_slice(&limits.protocol_log_bytes.to_be_bytes());
        header[96..100].copy_from_slice(&limits.records.to_be_bytes());
        header[100..104].copy_from_slice(&limits.events.to_be_bytes());
        header[104..112].copy_from_slice(&ceiling.to_be_bytes());
        let owned = linear::Owned::create(root, "segments.pbj", &header, HEADER + SEAL, disk)?;
        let transcript = transcript_start(&header);
        Ok(Self {
            owned,
            budget,
            header,
            limits,
            coverage,
            transcript,
            written: HEADER,
            events: 0,
            records: 0,
            protocol_bytes: 0,
            position: 0,
            highest: 0,
            finalized: 0,
            done: false,
            poisoned: false,
        })
    }
    pub(crate) fn filesystem_identity(&self) -> ((u64, u64), (u64, u64)) {
        self.owned.filesystem_identity()
    }
    pub(crate) fn append(
        &mut self,
        sequence: u64,
        event: SegmentEvent,
        payload: Option<(&ScratchBytes, usize)>,
        check: Check<'_>,
    ) -> Result<()> {
        if self.poisoned {
            return Err(Error::Interrupted);
        }
        let result = self.append_inner(sequence, event, payload, check);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn append_inner(
        &mut self,
        sequence: u64,
        event: SegmentEvent,
        payload: Option<(&ScratchBytes, usize)>,
        check: Check<'_>,
    ) -> Result<()> {
        if self.done || sequence != u64::from(self.events) || self.events == self.limits.events {
            return Err(Error::Invalid(
                "Output event sequence is incomplete or exceeded",
            ));
        }
        if payload.is_some_and(|(p, n)| !p.uses_budget(&self.budget) || n > p.len() || n > CHUNK) {
            return Err(Error::Budget);
        }
        let bytes = payload.map(|(p, n)| &p[..n]).unwrap_or(&[]);
        let mut record = [0; 64];
        record[..8].copy_from_slice(&sequence.to_be_bytes());
        let mut placement = None;
        let (kind, position, value, digest) = match event {
            SegmentEvent::Data { offset, sha256 } => {
                if bytes.is_empty()
                    || bytes.len() > CHUNK
                    || self.records == self.limits.records
                    || offset != self.position
                    || offset < self.finalized
                    || offset
                        .checked_add(bytes.len() as u64)
                        .is_none_or(|n| n > self.limits.output_bytes)
                    || self
                        .protocol_bytes
                        .checked_add(bytes.len() as u64 + 48)
                        .is_none_or(|n| n > self.limits.protocol_log_bytes)
                {
                    return Err(Error::Budget);
                }
                if ring::digest::digest(&SHA256, bytes).as_ref() != sha256 {
                    return Err(Error::Corrupt);
                }
                placement = Some(self.coverage.prepare(offset, bytes.len() as u32)?);
                (1, offset, 0, sha256)
            }
            SegmentEvent::Seek { position } => {
                if position < self.finalized || position > self.limits.output_bytes {
                    return Err(Error::Invalid("Output seek crosses its admitted prefix"));
                }
                (2, position, 0, [0; 32])
            }
            SegmentEvent::Finalized { position } => {
                if position < self.finalized
                    || position > self.highest
                    || !self.coverage.covers(position)
                {
                    return Err(Error::Invalid(
                        "Output finalized prefix has a gap or moved backward",
                    ));
                }
                (3, position, 0, [0; 32])
            }
            SegmentEvent::Observed { wire_kind } => {
                if ![2, 3, 5].contains(&wire_kind) {
                    return Err(Error::Invalid("Unexpected observed output event"));
                }
                (4, 0, u64::from(wire_kind), [0; 32])
            }
            SegmentEvent::Done { bytes, records } => {
                if bytes == 0
                    || bytes != self.highest
                    || bytes != self.finalized
                    || records != self.records
                    || !self.coverage.covers(bytes)
                {
                    return Err(Error::Invalid(
                        "Output completion lacks exact gap-free coverage",
                    ));
                }
                (5, bytes, u64::from(records), [0; 32])
            }
        };
        if kind != 1 && payload.is_some() {
            return Err(Error::Invalid("Output controls cannot contain bytes"));
        }
        record[8..12].copy_from_slice(&(kind as u32).to_be_bytes());
        record[12..16].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
        record[16..24].copy_from_slice(&position.to_be_bytes());
        record[24..32].copy_from_slice(&value.to_be_bytes());
        record[32..64].copy_from_slice(&digest);
        let record_sha = record_digest(&self.header, &record, bytes);
        let added = EVENT + bytes.len() as u64;
        if self
            .written
            .checked_add(added)
            .and_then(|n| n.checked_add(SEAL))
            .is_none_or(|n| n > self.limits.physical_ceiling().unwrap_or(0))
        {
            return Err(Error::Budget);
        }
        check()?;
        self.owned.check()?;
        self.owned.grow(added)?;
        let record_offset = self.written;
        for part in [&record[..], bytes, &record_sha[..]] {
            if part.is_empty() {
                continue;
            }
            check()?;
            write_all_at(&self.owned.file, self.written, part)?;
            self.transcript.update(part);
            self.written += part.len() as u64;
        }
        let observed = stamp(&self.owned.file)?;
        if observed.bytes != self.written {
            return Err(Error::Changed);
        }
        self.owned.expected = observed;
        self.owned.check()?;
        check()?;
        self.owned.check()?;
        if let Some(placement) = placement {
            self.coverage.apply(placement, record_offset, record_sha);
            self.position = position + bytes.len() as u64;
            self.highest = self.highest.max(self.position);
            self.records += 1;
            self.protocol_bytes += bytes.len() as u64 + 48;
        } else {
            match event {
                SegmentEvent::Seek { position } => self.position = position,
                SegmentEvent::Finalized { position } => self.finalized = position,
                SegmentEvent::Done { .. } => self.done = true,
                _ => {}
            }
        }
        self.events += 1;
        Ok(())
    }
    pub(crate) fn finish(mut self, check: Check<'_>) -> Result<CompletedSegments> {
        if self.poisoned || !self.done {
            return Err(Error::Interrupted);
        }
        check()?;
        self.owned.check()?;
        let transcript_sha256 = finish(self.transcript.clone());
        let mut seal = [0; 128];
        seal[..8].copy_from_slice(b"PBJXLSE1");
        seal[8..16].copy_from_slice(&(self.written + SEAL).to_be_bytes());
        seal[16..24].copy_from_slice(&u64::from(self.events).to_be_bytes());
        seal[24..32].copy_from_slice(&u64::from(self.records).to_be_bytes());
        seal[32..40].copy_from_slice(&self.protocol_bytes.to_be_bytes());
        seal[40..48].copy_from_slice(&self.highest.to_be_bytes());
        seal[48..80].copy_from_slice(&transcript_sha256);
        seal[80..112].copy_from_slice(&self.header[48..80]);
        seal[112..128].copy_from_slice(&self.header[32..48]);
        write_all_at(&self.owned.file, self.written, &seal)?;
        self.owned.file.sync_all()?;
        self.owned.expected = stamp(&self.owned.file)?;
        if self.owned.expected.bytes != self.written + SEAL {
            return Err(Error::Changed);
        }
        let complete = CompletedSegments {
            log: self,
            seal,
            transcript_sha256,
        };
        complete.verify(check)?;
        Ok(complete)
    }
    pub(crate) fn abort(mut self, check: Check<'_>) -> Result<()> {
        check()?;
        self.owned.check_identity()?;
        let before = stamp(&self.owned.file)?;
        if before.bytes != self.written {
            return Err(Error::Changed);
        }
        let mut buffer = ScratchBytes::new(&self.budget, CHUNK, CHUNK)?;
        let mut hash = Context::new(&SHA256);
        hash.update(b"PeerBrushJxlOutputTranscript1\0");
        let mut at = 0;
        while at < self.written {
            check()?;
            let count = (self.written - at).min(CHUNK as u64) as usize;
            read_exact_at(&self.owned.file, at, &mut buffer[..count])?;
            hash.update(&buffer[..count]);
            at += count as u64;
        }
        if finish(hash) != finish(self.transcript) {
            return Err(Error::Corrupt);
        }
        check()?;
        self.owned.check_identity()?;
        if stamp(&self.owned.file)? != before {
            return Err(Error::Changed);
        }
        self.owned.expected = before;
        linear::retire_owned(self.owned, check)
    }
}
impl CompletedSegments {
    pub(crate) fn check_owner(&self) -> Result<()> {
        self.log.owned.check()
    }

    pub(crate) fn payload_length(&self) -> u64 {
        self.log.highest
    }
    pub(crate) fn binding(&self) -> Digest {
        self.log.header[48..80].try_into().unwrap()
    }
    fn verify(&self, check: Check<'_>) -> Result<()> {
        check()?;
        self.log.owned.check()?;
        let mut buffer = ScratchBytes::new(&self.log.budget, CHUNK, CHUNK)?;
        let mut header = [0; 128];
        read_exact_at(&self.log.owned.file, 0, &mut header)?;
        if header != self.log.header {
            return Err(Error::Corrupt);
        }
        let mut transcript = transcript_start(&header);
        let mut at = HEADER;
        let mut records = 0u32;
        let mut protocol_bytes = 0u64;
        for sequence in 0..self.log.events {
            check()?;
            let mut record = [0; 64];
            read_exact_at(&self.log.owned.file, at, &mut record)?;
            let length = u32be(&record[12..16]) as usize;
            let kind = u32be(&record[8..12]);
            if u64be(&record[..8]) != u64::from(sequence)
                || !(1..=5).contains(&kind)
                || length > CHUNK
                || (kind == 1) == (length == 0)
                || at
                    .checked_add(EVENT + length as u64)
                    .is_none_or(|end| end > self.log.written)
            {
                return Err(Error::Corrupt);
            }
            read_exact_at(&self.log.owned.file, at + 64, &mut buffer[..length])?;
            let mut observed = [0; 32];
            read_exact_at(&self.log.owned.file, at + 64 + length as u64, &mut observed)?;
            if record_digest(&header, &record, &buffer[..length]) != observed {
                return Err(Error::Corrupt);
            }
            if kind == 1 {
                if ring::digest::digest(&SHA256, &buffer[..length]).as_ref() != &record[32..64] {
                    return Err(Error::Corrupt);
                }
                records += 1;
                protocol_bytes += length as u64 + 48;
            } else if record[32..64] != [0; 32] {
                return Err(Error::Corrupt);
            }
            if (kind == 5) != (sequence + 1 == self.log.events) {
                return Err(Error::Corrupt);
            }
            transcript.update(&record);
            transcript.update(&buffer[..length]);
            transcript.update(&observed);
            at += EVENT + length as u64;
        }
        if at != self.log.written
            || records != self.log.records
            || protocol_bytes != self.log.protocol_bytes
            || finish(transcript) != self.transcript_sha256
        {
            return Err(Error::Corrupt);
        }
        let mut seal = [0; 128];
        read_exact_at(&self.log.owned.file, at, &mut seal)?;
        if seal != self.seal || self.log.owned.expected.bytes != at + SEAL {
            return Err(Error::Corrupt);
        }
        check()?;
        self.log.owned.check()
    }
    /// Replay proves canonical bytes only; the caller still verifies encoder exit,
    /// input/profile fidelity, source revision and destination publication authority.
    pub(crate) fn replay(
        &self,
        output: &mut LargeSourceTemporary,
        check: Check<'_>,
    ) -> Result<SegmentReplay> {
        if output.payload_length() != self.payload_length()
            || output.role() != ByteRole::CompatibleJxlMode1
            || !output.uses_budgets(&self.log.budget, &self.log.owned.disk_budget())
        {
            return Err(Error::Invalid(
                "Canonical output has a foreign role, length or budget",
            ));
        }
        self.verify(check)?;
        let mut buffer = ScratchBytes::new(&self.log.budget, CHUNK, CHUNK)?;
        let mut hash = Context::new(&SHA256);
        let mut written = 0;
        for interval in self.log.coverage.intervals() {
            check()?;
            self.log.owned.check()?;
            let mut record = [0; 64];
            read_exact_at(&self.log.owned.file, interval.record_offset, &mut record)?;
            let count = u32be(&record[12..16]) as usize;
            let start = u64be(&record[16..24]);
            let end = start.checked_add(count as u64).ok_or(Error::Corrupt)?;
            if u32be(&record[8..12]) != 1
                || count == 0
                || count > CHUNK
                || interval.start != written
                || interval.start < start
                || interval.end > end
                || interval.end <= interval.start
            {
                return Err(Error::Corrupt);
            }
            read_exact_at(
                &self.log.owned.file,
                interval.record_offset + 64,
                &mut buffer[..count],
            )?;
            let mut trailer = [0; 32];
            read_exact_at(
                &self.log.owned.file,
                interval.record_offset + 64 + count as u64,
                &mut trailer,
            )?;
            if trailer != interval.record_sha256
                || record_digest(&self.log.header, &record, &buffer[..count]) != trailer
            {
                return Err(Error::Corrupt);
            }
            let bytes = &buffer[(interval.start - start) as usize..(interval.end - start) as usize];
            output.append(bytes, check)?;
            hash.update(bytes);
            written = interval.end;
            check()?;
            self.log.owned.check()?;
        }
        if written != self.payload_length() {
            return Err(Error::Corrupt);
        }
        check()?;
        self.log.owned.check()?;
        Ok(SegmentReplay {
            bytes: written,
            sha256: finish(hash),
        })
    }
    pub(crate) fn retire(self, check: Check<'_>) -> Result<()> {
        self.verify(check)?;
        linear::retire_owned(self.log.owned, check)
    }
}
fn transcript_start(header: &[u8; 128]) -> Context {
    let mut context = Context::new(&SHA256);
    context.update(b"PeerBrushJxlOutputTranscript1\0");
    context.update(header);
    context
}
fn record_digest(header: &[u8; 128], record: &[u8; 64], bytes: &[u8]) -> Digest {
    let mut hash = Context::new(&SHA256);
    hash.update(b"PeerBrushJxlOutputRecord1\0");
    hash.update(header);
    hash.update(record);
    hash.update(bytes);
    finish(hash)
}
fn finish(hash: Context) -> Digest {
    hash.finish().as_ref().try_into().unwrap()
}
fn u32be(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap())
}
fn u64be(bytes: &[u8]) -> u64 {
    u64::from_be_bytes(bytes.try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::super::store::{open_directory, validate_ancestors};
    use super::*;
    use std::{
        fs::{self, File},
        path::PathBuf,
    };
    struct Fixture {
        root: PathBuf,
        held: Option<File>,
        identity: (u64, u64),
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("peerbrush-segment-tests-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let held = open_directory(&root).unwrap();
            let identity = crate::file_safety::identity(&held).unwrap();
            Self {
                root,
                held: Some(held),
                identity,
            }
        }
        fn spool(&self) -> PathBuf {
            self.root.join("spool")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
            if self.root.starts_with(temp)
                && self.root.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with("peerbrush-segment-tests-")
                })
                && validate_ancestors(&self.root).is_ok()
                && fs::canonicalize(&self.root).is_ok_and(|path| path == self.root)
                && open_directory(&self.root)
                    .and_then(|file| {
                        crate::file_safety::identity(&file).map_err(|_| Error::Changed)
                    })
                    .is_ok_and(|id| id == self.identity)
                && self.held.as_ref().is_some_and(|file| {
                    crate::file_safety::identity(file).is_ok_and(|id| id == self.identity)
                })
            {
                drop(self.held.take());
                let _ = fs::remove_dir_all(&self.root);
            }
        }
    }

    fn okay() -> Result<()> {
        Ok(())
    }
    fn limits() -> SegmentLimits {
        SegmentLimits {
            output_bytes: 1024,
            protocol_log_bytes: 65536,
            records: 32,
            events: 128,
        }
    }
    fn log(f: &Fixture, pool: &ResidentBudget, disk: &DiskBudget) -> SegmentLog {
        SegmentLog::create(
            &f.spool(),
            limits(),
            [7; 16],
            [9; 32],
            pool.clone(),
            disk.clone(),
        )
        .unwrap()
    }
    fn data(log: &mut SegmentLog, sequence: u64, offset: u64, pool: &ResidentBudget, value: &[u8]) {
        let mut payload = ScratchBytes::new(pool, value.len(), CHUNK).unwrap();
        payload.copy_from_slice(value);
        log.append(
            sequence,
            SegmentEvent::Data {
                offset,
                sha256: ring::digest::digest(&SHA256, value)
                    .as_ref()
                    .try_into()
                    .unwrap(),
            },
            Some((&payload, payload.len())),
            &mut okay,
        )
        .unwrap();
    }
    fn complete(mut log: SegmentLog, pool: &ResidentBudget) -> CompletedSegments {
        data(&mut log, 0, 0, pool, &[3; 12]);
        log.append(1, SegmentEvent::Finalized { position: 12 }, None, &mut okay)
            .unwrap();
        log.append(
            2,
            SegmentEvent::Done {
                bytes: 12,
                records: 1,
            },
            None,
            &mut okay,
        )
        .unwrap();
        log.finish(&mut okay).unwrap()
    }
    #[test]
    fn seek_overlap_replays_current_canonical_bytes_and_retires_exact_debt() {
        let f = Fixture::new();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let mut log = log(&f, &pool, &disk);
        assert_eq!(disk.used().unwrap(), 256);
        log.append(0, SegmentEvent::Observed { wire_kind: 2 }, None, &mut okay)
            .unwrap();
        log.append(1, SegmentEvent::Seek { position: 8 }, None, &mut okay)
            .unwrap();
        data(&mut log, 2, 8, &pool, &[2; 4]);
        log.append(3, SegmentEvent::Seek { position: 0 }, None, &mut okay)
            .unwrap();
        data(&mut log, 4, 0, &pool, &[1; 8]);
        log.append(5, SegmentEvent::Seek { position: 2 }, None, &mut okay)
            .unwrap();
        data(&mut log, 6, 2, &pool, &[3; 4]);
        log.append(7, SegmentEvent::Finalized { position: 12 }, None, &mut okay)
            .unwrap();
        log.append(
            8,
            SegmentEvent::Done {
                bytes: 12,
                records: 3,
            },
            None,
            &mut okay,
        )
        .unwrap();
        let completed = log.finish(&mut okay).unwrap();
        assert_eq!(disk.used().unwrap(), 256 + 9 * 96 + 16);
        let retained = pool.used();
        assert!(retained > 0);
        let mut output = LargeSourceTemporary::create(
            &f.root.join("canonical"),
            12,
            ByteRole::CompatibleJxlMode1,
            pool.clone(),
            disk.clone(),
        )
        .unwrap();
        let replay = completed.replay(&mut output, &mut okay).unwrap();
        let output = output.finish(completed.binding(), &mut okay).unwrap();
        assert_eq!(replay.bytes, 12);
        assert_eq!(replay.sha256, output.payload_sha256());
        let mut bytes = ScratchBytes::new(&pool, 12, CHUNK).unwrap();
        output.read_range_into(0, &mut bytes, &mut okay).unwrap();
        assert_eq!(&bytes[..], &[1, 1, 3, 3, 3, 3, 1, 1, 2, 2, 2, 2]);
        drop(bytes);
        output.retire(&mut okay).unwrap();
        assert_eq!(pool.used(), retained);
        completed.retire(&mut okay).unwrap();
        assert_eq!(pool.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }
    #[test]
    fn gap_and_rewrite_refusals_latch_without_fabricating_pixels() {
        let f = Fixture::new();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let mut first = log(&f, &pool, &disk);
        first
            .append(0, SegmentEvent::Seek { position: 8 }, None, &mut okay)
            .unwrap();
        data(&mut first, 1, 8, &pool, &[2; 4]);
        assert!(first
            .append(2, SegmentEvent::Finalized { position: 12 }, None, &mut okay)
            .is_err());
        assert!(matches!(
            first.append(2, SegmentEvent::Seek { position: 0 }, None, &mut okay),
            Err(Error::Interrupted)
        ));
        first.abort(&mut okay).unwrap();
        assert_eq!(disk.used().unwrap(), 0);
        let mut second = SegmentLog::create(
            &f.root.join("second"),
            limits(),
            [7; 16],
            [9; 32],
            pool.clone(),
            disk.clone(),
        )
        .unwrap();
        data(&mut second, 0, 0, &pool, &[1; 12]);
        second
            .append(1, SegmentEvent::Finalized { position: 8 }, None, &mut okay)
            .unwrap();
        assert!(second
            .append(2, SegmentEvent::Seek { position: 7 }, None, &mut okay)
            .is_err());
        second.abort(&mut okay).unwrap();
        assert_eq!(pool.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }
    #[test]
    fn cancelled_known_header_prefix_can_retire_but_unknown_partial_bytes_cannot() {
        let f = Fixture::new();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let mut log = log(&f, &pool, &disk);
        let payload = ScratchBytes::new(&pool, 12, CHUNK).unwrap();
        let hash = ring::digest::digest(&SHA256, &payload)
            .as_ref()
            .try_into()
            .unwrap();
        let mut calls = 0;
        assert!(matches!(
            log.append(
                0,
                SegmentEvent::Data {
                    offset: 0,
                    sha256: hash
                },
                Some((&payload, payload.len())),
                &mut || {
                    calls += 1;
                    if calls == 3 {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                }
            ),
            Err(Error::Cancelled)
        ));
        assert_eq!(log.written, HEADER + 64);
        assert_eq!(disk.used().unwrap(), 256 + 96 + 12);
        drop(payload);
        log.abort(&mut okay).unwrap();
        assert_eq!(disk.used().unwrap(), 0);
        let mut unknown = SegmentLog::create(
            &f.root.join("unknown"),
            limits(),
            [7; 16],
            [9; 32],
            pool.clone(),
            disk.clone(),
        )
        .unwrap();
        write_all_at(&unknown.owned.file, HEADER, &[4]).unwrap();
        unknown.poisoned = true;
        assert!(unknown.abort(&mut okay).is_err());
        assert_eq!(disk.used().unwrap(), 256);
        assert_eq!(
            fs::metadata(f.root.join("unknown/segments.pbj"))
                .unwrap()
                .len(),
            129
        );
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn same_byte_foreign_replacement_is_never_retired_or_replayed() {
        let f = Fixture::new();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let log = log(&f, &pool, &disk);
        let name = f.spool().join("segments.pbj");
        let mut original = [0; 128];
        read_exact_at(&log.owned.file, 0, &mut original).unwrap();
        let moved = fs::rename(&name, f.spool().join("original-held"));
        #[cfg(windows)]
        if let Err(error) = &moved {
            assert!(matches!(error.raw_os_error(), Some(5 | 32)));
            eprintln!("Native kernel prevented replacing the held log name; original retained");
            log.owned.check().unwrap();
            let mut observed = [0; 128];
            read_exact_at(&log.owned.file, 0, &mut observed).unwrap();
            assert_eq!(observed, original);
            log.abort(&mut okay).unwrap();
            assert_eq!(disk.used().unwrap(), 0);
            return;
        }
        moved.unwrap();
        fs::write(&name, &original).unwrap();
        eprintln!("Same-byte foreign replacement created; held-identity refusal must preserve it");
        assert!(matches!(log.abort(&mut okay), Err(Error::Changed)));
        assert_eq!(fs::read(&name).unwrap(), original);
        assert_eq!(disk.used().unwrap(), 256);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn fresh_hashes_detect_late_bytes_even_when_modified_stamp_is_restored() {
        let f = Fixture::new();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let completed = complete(log(&f, &pool, &disk), &pool);
        let stamp = completed
            .log
            .owned
            .file
            .metadata()
            .unwrap()
            .modified()
            .unwrap();
        write_all_at(&completed.log.owned.file, HEADER + 64, &[99]).unwrap();
        completed
            .log
            .owned
            .file
            .set_times(std::fs::FileTimes::new().set_modified(stamp))
            .unwrap();
        completed.log.owned.check().unwrap();
        assert!(matches!(completed.verify(&mut okay), Err(Error::Corrupt)));
        assert!(matches!(completed.retire(&mut okay), Err(Error::Corrupt)));
        assert!(disk.used().unwrap() > 256);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn event_sequence_count_payload_hash_and_disk_refusals_preserve_debt() {
        for case in 0..5 {
            let f = Fixture::new();
            let pool = ResidentBudget::new(1024 * 1024).unwrap();
            let disk = DiskBudget::new(if case == 4 { 256 } else { 1024 * 1024 }, 4).unwrap();
            let spec = if case == 3 {
                SegmentLimits {
                    records: 1,
                    events: 1,
                    ..limits()
                }
            } else {
                limits()
            };
            let mut log = SegmentLog::create(
                &f.spool(),
                spec,
                [7; 16],
                [9; 32],
                pool.clone(),
                disk.clone(),
            )
            .unwrap();
            let payload = ScratchBytes::new(&pool, 1, CHUNK).unwrap();
            let result = match case {
                0 => log.append(1, SegmentEvent::Observed { wire_kind: 2 }, None, &mut okay),
                1 => log.append(0, SegmentEvent::Observed { wire_kind: 12 }, None, &mut okay),
                2 => log.append(
                    0,
                    SegmentEvent::Data {
                        offset: 0,
                        sha256: [0; 32],
                    },
                    Some((&payload, payload.len())),
                    &mut okay,
                ),
                3 => {
                    log.append(0, SegmentEvent::Observed { wire_kind: 2 }, None, &mut okay)
                        .unwrap();
                    log.append(1, SegmentEvent::Observed { wire_kind: 2 }, None, &mut okay)
                }
                _ => log.append(0, SegmentEvent::Observed { wire_kind: 2 }, None, &mut okay),
            };
            assert!(result.is_err());
            drop(payload);
            log.abort(&mut okay).unwrap();
            assert_eq!(pool.used(), 0);
            assert_eq!(disk.used().unwrap(), 0);
        }
        assert!(SegmentLimits {
            events: 1048577,
            ..limits()
        }
        .physical_ceiling()
        .is_err());
        assert!(SegmentLimits {
            records: 131073,
            ..limits()
        }
        .physical_ceiling()
        .is_err());
        assert!(SegmentLimits {
            output_bytes: MAX_LOGICAL + 1,
            ..limits()
        }
        .physical_ceiling()
        .is_err());
    }
    #[test]
    fn record_limit_and_foreign_buffer_pool_refuse_before_data_io() {
        let f = Fixture::new();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let other = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let mut log = log(&f, &pool, &disk);
        let payload = ScratchBytes::new(&other, 1, CHUNK).unwrap();
        let sha = ring::digest::digest(&SHA256, &payload)
            .as_ref()
            .try_into()
            .unwrap();
        assert!(matches!(
            log.append(
                0,
                SegmentEvent::Data {
                    offset: 0,
                    sha256: sha
                },
                Some((&payload, payload.len())),
                &mut okay
            ),
            Err(Error::Budget)
        ));
        assert_eq!(log.written, HEADER);
        assert_eq!(disk.used().unwrap(), 256);
        log.abort(&mut okay).unwrap();
        drop(payload);
        assert_eq!(pool.used(), 0);
        assert_eq!(other.used(), 0);
        let spec = SegmentLimits {
            records: 1,
            events: 8,
            ..limits()
        };
        let mut log = SegmentLog::create(
            &f.root.join("bounded"),
            spec,
            [7; 16],
            [9; 32],
            pool.clone(),
            disk.clone(),
        )
        .unwrap();
        data(&mut log, 0, 0, &pool, &[0]);
        let payload = ScratchBytes::new(&pool, 1, CHUNK).unwrap();
        assert!(matches!(
            log.append(
                1,
                SegmentEvent::Data {
                    offset: 1,
                    sha256: sha
                },
                Some((&payload, payload.len())),
                &mut okay
            ),
            Err(Error::Budget)
        ));
        assert_eq!(log.records, 1);
        assert_eq!(log.written, HEADER + 97);
        drop(payload);
        log.abort(&mut okay).unwrap();
        assert_eq!(disk.used().unwrap(), 0);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn interrupted_or_truncated_seal_never_becomes_replay_authority() {
        let f = Fixture::new();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let mut log = log(&f, &pool, &disk);
        data(&mut log, 0, 0, &pool, &[3; 12]);
        log.append(1, SegmentEvent::Finalized { position: 12 }, None, &mut okay)
            .unwrap();
        log.append(
            2,
            SegmentEvent::Done {
                bytes: 12,
                records: 1,
            },
            None,
            &mut okay,
        )
        .unwrap();
        let mut calls = 0;
        assert!(matches!(
            log.finish(&mut || {
                calls += 1;
                if calls == 2 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Cancelled)
        ));
        assert_eq!(pool.used(), 0);
        assert_eq!(
            fs::metadata(f.spool().join("segments.pbj")).unwrap().len(),
            disk.used().unwrap()
        );
        let retained = disk.used().unwrap();
        let second = SegmentLog::create(
            &f.root.join("truncated"),
            limits(),
            [7; 16],
            [9; 32],
            pool.clone(),
            disk.clone(),
        )
        .unwrap();
        let complete = complete(second, &pool);
        let before = complete.log.owned.file.metadata().unwrap().len();
        complete.log.owned.file.set_len(before - 1).unwrap();
        assert!(matches!(complete.check_owner(), Err(Error::Changed)));
        assert!(matches!(complete.verify(&mut okay), Err(Error::Changed)));
        assert!(matches!(complete.retire(&mut okay), Err(Error::Changed)));
        assert_eq!(pool.used(), 0);
        assert!(disk.used().unwrap() > retained);
        assert_eq!(
            fs::metadata(f.root.join("truncated/segments.pbj"))
                .unwrap()
                .len(),
            before - 1
        );
    }
    #[test]
    fn charged_receive_buffer_prefix_is_bounded_before_slicing_and_io() {
        let f = Fixture::new();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let mut receiver = ScratchBytes::new(&pool, CHUNK, CHUNK).unwrap();
        receiver.fill(99);
        receiver[..3].copy_from_slice(&[1, 2, 3]);
        let digest = ring::digest::digest(&SHA256, &receiver[..3])
            .as_ref()
            .try_into()
            .unwrap();
        let mut log = log(&f, &pool, &disk);
        log.append(
            0,
            SegmentEvent::Data {
                offset: 0,
                sha256: digest,
            },
            Some((&receiver, 3)),
            &mut okay,
        )
        .unwrap();
        assert_eq!(log.highest, 3);
        assert_eq!(log.protocol_bytes, 51);
        log.append(1, SegmentEvent::Finalized { position: 3 }, None, &mut okay)
            .unwrap();
        log.append(
            2,
            SegmentEvent::Done {
                bytes: 3,
                records: 1,
            },
            None,
            &mut okay,
        )
        .unwrap();
        log.finish(&mut okay).unwrap().retire(&mut okay).unwrap();
        let mut bad = SegmentLog::create(
            &f.root.join("invalid-prefix"),
            limits(),
            [7; 16],
            [9; 32],
            pool.clone(),
            disk.clone(),
        )
        .unwrap();
        assert!(matches!(
            bad.append(
                0,
                SegmentEvent::Data {
                    offset: 0,
                    sha256: digest
                },
                Some((&receiver, CHUNK + 1)),
                &mut okay
            ),
            Err(Error::Budget)
        ));
        assert_eq!(bad.written, HEADER);
        bad.abort(&mut okay).unwrap();
        let tiny = ScratchBytes::new(&pool, 1, CHUNK).unwrap();
        let mut bad = SegmentLog::create(
            &f.root.join("short-buffer"),
            limits(),
            [7; 16],
            [9; 32],
            pool.clone(),
            disk.clone(),
        )
        .unwrap();
        assert!(matches!(
            bad.append(
                0,
                SegmentEvent::Data {
                    offset: 0,
                    sha256: digest
                },
                Some((&tiny, 2)),
                &mut okay
            ),
            Err(Error::Budget)
        ));
        assert_eq!(bad.written, HEADER);
        bad.abort(&mut okay).unwrap();
        assert_eq!(disk.used().unwrap(), 0);
        assert_eq!(pool.used(), CHUNK + 1);
        drop((receiver, tiny));
        assert_eq!(pool.used(), 0);
    }
}
