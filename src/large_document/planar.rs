//! Owned linear canonical channel planes. Completion is storage integrity only.
#[cfg(test)]
use super::store::{open_directory, read_exact_at, stamp, validate_ancestors, write_all_at};
use super::{DiskBudget, Error, ResidentBudget, Result, ScratchBytes};
#[cfg(test)]
use ring::digest::{Context, SHA256};
#[cfg(test)]
use std::{
    fs::{self, File},
    path::PathBuf,
};
use std::{path::Path, sync::Arc};

const CHUNK: usize = 64 * 1024;
const HEADER: u64 = 64;
const SEAL: usize = 144;
type Digest = [u8; 32];
type Check<'a> = &'a mut dyn FnMut() -> Result<()>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanarSpec {
    width: u32,
    height: u32,
    depth: u16,
}
impl PlanarSpec {
    pub fn new(width: u32, height: u32, depth: u16) -> Result<Self> {
        if width == 0 || height == 0 || width > 16000 || height > 16000 || ![8, 16].contains(&depth)
        {
            return Err(Error::Invalid(
                "Invalid explicitly admitted planar geometry",
            ));
        }
        Ok(Self {
            width,
            height,
            depth,
        })
    }
    pub fn width(self) -> u32 {
        self.width
    }
    pub fn height(self) -> u32 {
        self.height
    }
    pub fn depth(self) -> u16 {
        self.depth
    }
    pub fn row_bytes(self) -> usize {
        self.width as usize * (self.depth as usize / 8)
    }
    pub fn payload_bytes(self) -> u64 {
        self.row_bytes() as u64 * self.height as u64 * 4
    }
    fn chunks(self) -> usize {
        self.payload_bytes().div_ceil(CHUNK as u64) as usize
    }
    pub fn encoded_bytes(self) -> u64 {
        HEADER + self.payload_bytes() + self.chunks() as u64 * 32 + SEAL as u64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PlanarRole {
    ImportWhiteMatte = 1,
    SaveCurrentMatte = 2,
}

use super::linear;
pub struct PlanarScratch {
    inner: linear::Scratch,
}
impl PlanarScratch {
    pub fn create(
        root: &Path,
        spec: PlanarSpec,
        role: PlanarRole,
        budget: ResidentBudget,
        disk: DiskBudget,
    ) -> Result<Self> {
        let layout = linear::Spec {
            width: spec.width,
            height: spec.height,
            depth: spec.depth,
            channels: 4,
            kind: linear::Kind::Native(role),
        };
        Ok(Self {
            inner: linear::Scratch::create(root, layout, budget, disk)?,
        })
    }
    pub fn spec(&self) -> PlanarSpec {
        PlanarSpec {
            width: self.inner.spec.width,
            height: self.inner.spec.height,
            depth: self.inner.spec.depth,
        }
    }
    pub fn uses_budgets(&self, ram: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.inner.uses_budgets(ram, disk)
    }
    pub fn append_rows(
        &mut self,
        channel: u16,
        y: u32,
        bytes: &[u8],
        check: Check<'_>,
    ) -> Result<()> {
        self.inner.append_rows(channel, y, bytes, check)
    }
    pub fn finish(self, binding: Digest, check: Check<'_>) -> Result<CompletedPlanar> {
        Ok(CompletedPlanar {
            inner: self.inner.finish(binding, check)?,
        })
    }
}
#[derive(Clone)]
pub struct CompletedPlanar {
    inner: Arc<linear::Complete>,
}
impl CompletedPlanar {
    pub fn spec(&self) -> PlanarSpec {
        PlanarSpec {
            width: self.inner.spec.width,
            height: self.inner.spec.height,
            depth: self.inner.spec.depth,
        }
    }
    pub fn role(&self) -> PlanarRole {
        match self.inner.spec.kind {
            linear::Kind::Native(role) => role,
            linear::Kind::Scalar => unreachable!("Native wrapper contains a scalar field"),
        }
    }
    pub fn binding(&self) -> Digest {
        self.inner.binding
    }
    pub fn uses_budgets(&self, ram: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.inner.uses_budgets(ram, disk)
    }
    pub fn read_into(
        &self,
        c: u16,
        y: u32,
        rows: u32,
        out: &mut ScratchBytes,
        check: Check<'_>,
    ) -> Result<()> {
        self.inner.read_into(c, y, rows, out, check)
    }
    pub fn read_stripe(&self, c: u16, y: u32, rows: u32, check: Check<'_>) -> Result<ScratchBytes> {
        self.inner.read_stripe(c, y, rows, check)
    }
    pub fn retire(self, check: Check<'_>) -> Result<()> {
        linear::retire(self.inner, check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf, Option<File>, (u64, u64));
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("peerbrush-planar-tests-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            let path = fs::canonicalize(path).unwrap();
            let held = open_directory(&path).unwrap();
            let identity = crate::file_safety::identity(&held).unwrap();
            Self(path, Some(held), identity)
        }
        fn root(&self) -> PathBuf {
            self.0.join("scratch")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
            if self.0.starts_with(temp)
                && self.0.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with("peerbrush-planar-tests-")
                })
                && validate_ancestors(&self.0).is_ok()
                && fs::canonicalize(&self.0).is_ok_and(|path| path == self.0)
                && open_directory(&self.0)
                    .and_then(|file| {
                        crate::file_safety::identity(&file).map_err(|_| Error::Changed)
                    })
                    .is_ok_and(|identity| identity == self.2)
                && self.1.as_ref().is_some_and(|file| {
                    crate::file_safety::identity(file).is_ok_and(|identity| identity == self.2)
                })
            {
                drop(self.1.take());
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }
    fn okay() -> Result<()> {
        Ok(())
    }
    fn rows(
        spec: PlanarSpec,
        channel: u16,
        y: u32,
        count: u32,
        budget: &ResidentBudget,
    ) -> ScratchBytes {
        let mut data =
            ScratchBytes::new(budget, spec.row_bytes() * count as usize, 64 * 1024 * 1024).unwrap();
        for row in 0..count {
            for x in 0..spec.width {
                let value = ((channel as u32 * 997 + y + row) * 193 + x * 7) as u16;
                let at = row as usize * spec.row_bytes() + x as usize * (spec.depth as usize / 8);
                if spec.depth == 16 {
                    data[at..at + 2].copy_from_slice(&value.to_be_bytes());
                } else {
                    data[at] = value as u8;
                }
            }
        }
        data
    }
    fn fill(scratch: &mut PlanarScratch) {
        for channel in 0..4 {
            let mut y = 0;
            while y < scratch.spec().height {
                let count = (scratch.spec().height - y).min(256);
                let data = rows(scratch.spec(), channel, y, count, &scratch.inner.budget);
                scratch.append_rows(channel, y, &data, &mut okay).unwrap();
                y += count;
            }
        }
    }

    #[test]
    fn native_planar_stripes_cross_chunks_and_share_one_charged_completion() {
        for depth in [8, 16] {
            let fixture = Fixture::new();
            let budget = ResidentBudget::new(8 * 1024 * 1024).unwrap();
            let spec = PlanarSpec::new(257, 260, depth).unwrap();
            let disk = DiskBudget::new(spec.encoded_bytes(), 2).unwrap();
            let mut scratch = PlanarScratch::create(
                &fixture.root(),
                spec,
                PlanarRole::ImportWhiteMatte,
                budget.clone(),
                disk.clone(),
            )
            .unwrap();
            assert_eq!(budget.used(), spec.chunks() * 32);
            assert!(scratch.uses_budgets(&budget, &disk));
            fill(&mut scratch);
            let completed = scratch.finish([33; 32], &mut okay).unwrap();
            assert_eq!(completed.spec(), spec);
            assert_eq!(completed.role(), PlanarRole::ImportWhiteMatte);
            assert_eq!(completed.binding(), [33; 32]);
            assert!(completed.uses_budgets(&budget, &disk));
            assert!(!completed
                .uses_budgets(&budget, &DiskBudget::new(spec.encoded_bytes(), 2).unwrap()));
            assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
            for channel in 0..4 {
                let actual = completed.read_stripe(channel, 127, 129, &mut okay).unwrap();
                let expected = rows(spec, channel, 127, 129, &budget);
                assert_eq!(&actual[..], &expected[..]);
            }
            let mut foreign = ScratchBytes::new(
                &ResidentBudget::new(1024 * 1024).unwrap(),
                spec.row_bytes(),
                1024 * 1024,
            )
            .unwrap();
            assert!(matches!(
                completed.read_into(0, 0, 1, &mut foreign, &mut okay),
                Err(Error::Budget)
            ));
            assert!(matches!(
                completed.clone().retire(&mut okay),
                Err(Error::Busy)
            ));
            let mut readers = Vec::new();
            for channel in 0..4 {
                let reader = completed.clone();
                readers.push(std::thread::spawn(move || {
                    for _ in 0..4 {
                        let stripe = reader.read_stripe(channel, 4, 256, &mut okay).unwrap();
                        assert_eq!(stripe.len(), 256 * spec.row_bytes());
                    }
                }));
            }
            for reader in readers {
                reader.join().unwrap();
            }
            completed.retire(&mut okay).unwrap();
            assert_eq!(disk.used().unwrap(), 0);
            assert_eq!(budget.used(), 0);
            assert!(!fixture.root().join("planes.pbp").exists());
        }
    }

    #[test]
    fn invalid_row_order_and_incomplete_seals_never_produce_completion() {
        let fixture = Fixture::new();
        let spec = PlanarSpec::new(17, 260, 16).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = PlanarScratch::create(
            &fixture.root(),
            spec,
            PlanarRole::SaveCurrentMatte,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        let one = rows(spec, 0, 0, 1, &budget);
        for (channel, y, bytes) in [
            (1, 0, &one[..]),
            (0, 1, &one[..]),
            (0, 0, &one[..one.len() - 1]),
        ] {
            assert!(scratch.append_rows(channel, y, bytes, &mut okay).is_err());
            assert_eq!(scratch.inner.owned.file.metadata().unwrap().len(), HEADER);
        }
        scratch.append_rows(0, 0, &one, &mut okay).unwrap();
        drop(one);
        assert!(scratch.finish([7; 32], &mut okay).is_err());
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn canceled_partial_append_poison_preserves_file_and_disk_charge() {
        let fixture = Fixture::new();
        let spec = PlanarSpec::new(257, 260, 16).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = PlanarScratch::create(
            &fixture.root(),
            spec,
            PlanarRole::SaveCurrentMatte,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        let data = rows(spec, 0, 0, 256, &budget);
        let probe = scratch.inner.owned.file.try_clone().unwrap();
        assert!(matches!(
            scratch.append_rows(0, 0, &data, &mut || {
                if probe.metadata()?.len() > HEADER {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Cancelled)
        ));
        assert!(probe.metadata().unwrap().len() > HEADER);
        assert!(matches!(
            scratch.append_rows(0, 0, &data, &mut okay),
            Err(Error::Interrupted)
        ));
        drop(data);
        drop(scratch);
        drop(probe);
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        assert!(fixture.root().join("planes.pbp").exists());
    }

    #[test]
    fn chunk_corruption_is_rejected_even_after_matching_attribute_observation() {
        let fixture = Fixture::new();
        let spec = PlanarSpec::new(257, 260, 16).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = PlanarScratch::create(
            &fixture.root(),
            spec,
            PlanarRole::ImportWhiteMatte,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        fill(&mut scratch);
        let mut completed = scratch.finish([7; 32], &mut okay).unwrap();
        let owned = &mut Arc::get_mut(&mut completed.inner).unwrap().owned;
        write_all_at(&owned.file, HEADER + 17, b"corrupt").unwrap();
        owned.expected = stamp(&owned.file).unwrap();
        let before = budget.used();
        assert!(matches!(
            completed.read_stripe(0, 0, 1, &mut okay),
            Err(Error::Corrupt)
        ));
        assert_eq!(budget.used(), before);
        drop(completed);
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
    }

    #[test]
    fn damaged_payload_or_index_cannot_be_sealed() {
        for damage_index in [false, true] {
            let fixture = Fixture::new();
            let spec = PlanarSpec::new(257, 3, 16).unwrap();
            let budget = ResidentBudget::new(1024 * 1024).unwrap();
            let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
            let mut scratch = PlanarScratch::create(
                &fixture.root(),
                spec,
                PlanarRole::ImportWhiteMatte,
                budget.clone(),
                disk.clone(),
            )
            .unwrap();
            fill(&mut scratch);
            if damage_index {
                scratch.inner.index[0] ^= 1;
            } else {
                write_all_at(&scratch.inner.owned.file, HEADER + 17, b"bad").unwrap();
                scratch.inner.owned.expected = stamp(&scratch.inner.owned.file).unwrap();
            }
            assert!(matches!(
                scratch.finish([7; 32], &mut okay),
                Err(Error::Corrupt)
            ));
            assert_eq!(budget.used(), 0);
            assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        }
    }

    #[test]
    fn foreign_replacement_is_preserved_on_read_and_explicit_retirement() {
        let fixture = Fixture::new();
        let spec = PlanarSpec::new(17, 3, 8).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = PlanarScratch::create(
            &fixture.root(),
            spec,
            PlanarRole::SaveCurrentMatte,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        fill(&mut scratch);
        let completed = scratch.finish([7; 32], &mut okay).unwrap();
        let path = fixture.root().join("planes.pbp");
        let moved = fixture.0.join("owned-moved");
        fs::rename(&path, &moved).unwrap();
        fs::write(&path, b"foreign replacement").unwrap();
        assert!(completed.read_stripe(0, 0, 1, &mut okay).is_err());
        assert!(completed.retire(&mut okay).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"foreign replacement");
        assert_eq!(fs::metadata(moved).unwrap().len(), spec.encoded_bytes());
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn a_complete_but_canceled_seal_does_not_return_a_completion_token() {
        let fixture = Fixture::new();
        let spec = PlanarSpec::new(17, 3, 16).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = PlanarScratch::create(
            &fixture.root(),
            spec,
            PlanarRole::SaveCurrentMatte,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        fill(&mut scratch);
        let file = scratch.inner.owned.file.try_clone().unwrap();
        assert!(matches!(
            scratch.finish([7; 32], &mut || {
                if file.metadata()?.len() == spec.encoded_bytes() {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Cancelled)
        ));
        assert_eq!(file.metadata().unwrap().len(), spec.encoded_bytes());
        drop(file);
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
    }

    #[test]
    #[ignore = "Opt-in 16K native8/16 planar component; writes about3GB, not full editor/PSB qualification"]
    fn full_16000_native8_and_native16_planes_use_bounded_shared_buffers_and_retire() {
        for depth in [8, 16] {
            let started = std::time::Instant::now();
            let fixture = Fixture::new();
            let spec = PlanarSpec::new(16000, 16000, depth).unwrap();
            let budget = ResidentBudget::new(12 * 1024 * 1024).unwrap();
            let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
            let mut data =
                ScratchBytes::new(&budget, spec.row_bytes() * 256, 64 * 1024 * 1024).unwrap();
            let mut scratch = PlanarScratch::create(
                &fixture.root(),
                spec,
                PlanarRole::SaveCurrentMatte,
                budget.clone(),
                disk.clone(),
            )
            .unwrap();
            let value =
                |channel: u16, y: u32, x: u32| ((channel as u32 * 997 + y) * 193 + x * 7) as u16;
            for channel in 0..4 {
                let mut y = 0;
                while y < spec.height {
                    let count = (spec.height - y).min(256);
                    for row in 0..count {
                        for x in 0..spec.width {
                            let sample = value(channel, y + row, x);
                            let at =
                                row as usize * spec.row_bytes() + x as usize * (depth as usize / 8);
                            if depth == 16 {
                                data[at..at + 2].copy_from_slice(&sample.to_be_bytes());
                            } else {
                                data[at] = sample as u8;
                            }
                        }
                    }
                    scratch
                        .append_rows(
                            channel,
                            y,
                            &data[..spec.row_bytes() * count as usize],
                            &mut okay,
                        )
                        .unwrap();
                    y += count;
                }
                println!("16K planar component depth={depth} channel={channel} payload_written={} resident_bytes={}",scratch.inner.owned.file.metadata().unwrap().len()-HEADER,budget.used());
            }
            let completed = scratch.finish([55; 32], &mut okay).unwrap();
            for channel in 0..4 {
                completed
                    .read_into(channel, 15744, 256, &mut data, &mut okay)
                    .unwrap();
                for row in 0..256 {
                    for x in 0..spec.width {
                        let at =
                            row as usize * spec.row_bytes() + x as usize * (depth as usize / 8);
                        let expected = value(channel, 15744 + row, x);
                        if depth == 16 {
                            assert_eq!(&data[at..at + 2], &expected.to_be_bytes());
                        } else {
                            assert_eq!(data[at], expected as u8);
                        }
                    }
                }
            }
            let mut hash = Context::new(&SHA256);
            let mut io = ScratchBytes::new(&budget, CHUNK, CHUNK).unwrap();
            let mut at = 0;
            while at < spec.encoded_bytes() {
                let count = (spec.encoded_bytes() - at).min(CHUNK as u64) as usize;
                read_exact_at(&completed.inner.owned.file, at, &mut io[..count]).unwrap();
                hash.update(&io[..count]);
                at += count as u64;
            }
            let sha = hash
                .finish()
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            drop(io);
            assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
            let resident = budget.used();
            completed.retire(&mut okay).unwrap();
            assert_eq!(disk.used().unwrap(), 0);
            drop(data);
            assert_eq!(budget.used(), 0);
            println!("16K planar component PASS depth={depth} payload_bytes={} encoded_bytes={} charged_resident_bytes={resident} elapsed_ms={} file_sha256={sha}; no total RAM/editor/PSB/history qualification",spec.payload_bytes(),spec.encoded_bytes(),started.elapsed().as_millis());
        }
    }
}
