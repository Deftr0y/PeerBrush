//! Finite f64 derived fields, distinct from native raster/source authority.
use super::{linear, DiskBudget, Error, ResidentBudget, Result, ScratchBytes};
use std::{path::Path, sync::Arc};
type Digest = [u8; 32];
type Check<'a> = &'a mut dyn FnMut() -> Result<()>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalarSpec {
    width: u32,
    height: u32,
}
impl ScalarSpec {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 || width > 16000 || height > 16000 {
            return Err(Error::Invalid("Invalid derived scalar frame"));
        }
        Ok(Self { width, height })
    }
    pub fn frame(self) -> [u32; 2] {
        [self.width, self.height]
    }
    pub fn row_bytes(self) -> usize {
        self.width as usize * 8
    }
    pub fn payload_bytes(self) -> u64 {
        self.row_bytes() as u64 * self.height as u64
    }
    pub fn encoded_bytes(self) -> u64 {
        self.layout().encoded_bytes()
    }
    fn layout(self) -> linear::Spec {
        linear::Spec {
            width: self.width,
            height: self.height,
            depth: 64,
            channels: 1,
            kind: linear::Kind::Scalar,
        }
    }
}

pub struct ScalarScratch {
    inner: linear::Scratch,
}
impl ScalarScratch {
    pub fn abort(self, check: Check<'_>) -> Result<()> {
        self.inner.abort(check)
    }
    pub fn create(
        root: &Path,
        spec: ScalarSpec,
        resident: ResidentBudget,
        disk: DiskBudget,
    ) -> Result<Self> {
        Ok(Self {
            inner: linear::Scratch::create(root, spec.layout(), resident, disk)?,
        })
    }
    pub fn frame(&self) -> [u32; 2] {
        [self.inner.spec.width, self.inner.spec.height]
    }
    pub fn uses_budgets(&self, resident: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.inner.uses_budgets(resident, disk)
    }
    pub fn append_rows(&mut self, y: u32, bytes: &[u8], check: Check<'_>) -> Result<()> {
        finite(bytes, check)?;
        self.inner.append_rows(0, y, bytes, check)
    }
    /// Stage binding and value SHA describe derived arithmetic only. They never
    /// authorize a Source36 raster, editable document or recovery checkpoint.
    pub fn finish(self, stage_binding: Digest, check: Check<'_>) -> Result<CompletedScalar> {
        Ok(CompletedScalar {
            inner: self.inner.finish(stage_binding, check)?,
        })
    }
}

#[derive(Clone)]
pub struct CompletedScalar {
    inner: Arc<linear::Complete>,
}
impl CompletedScalar {
    pub fn frame(&self) -> [u32; 2] {
        [self.inner.spec.width, self.inner.spec.height]
    }
    pub fn bounds(&self) -> [i32; 4] {
        [
            0,
            0,
            self.inner.spec.width as i32,
            self.inner.spec.height as i32,
        ]
    }
    pub fn stage_binding(&self) -> Digest {
        self.inner.binding
    }
    /// SHA256 of canonical BE f64 values, independent of the private file nonce.
    pub fn value_sha256(&self) -> Digest {
        self.inner.content_sha256
    }
    pub fn uses_budgets(&self, resident: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.inner.uses_budgets(resident, disk)
    }
    pub fn read_into(
        &self,
        y: u32,
        rows: u32,
        output: &mut ScratchBytes,
        check: Check<'_>,
    ) -> Result<()> {
        self.inner.read_into(0, y, rows, output, check)?;
        finite(output, check)?;
        check()?;
        self.inner.owned.check()
    }
    pub fn read_rect(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        output: &mut ScratchBytes,
        check: Check<'_>,
    ) -> Result<()> {
        self.inner.read_rect(x, y, width, height, output, check)?;
        finite(output, check)?;
        check()?;
        self.inner.owned.check()
    }
    pub fn retire(self, check: Check<'_>) -> Result<()> {
        linear::retire(self.inner, check)
    }
}

fn finite(bytes: &[u8], check: Check<'_>) -> Result<()> {
    if bytes.len() % 8 != 0 {
        return Err(Error::Invalid(
            "Scalar values require complete BE f64 samples",
        ));
    }
    for (number, word) in bytes.chunks_exact(8).enumerate() {
        if number % 8192 == 0 {
            check()?;
        }
        if !f64::from_be_bytes(word.try_into().unwrap()).is_finite() {
            return Err(Error::Invalid(
                "Derived scalar fields require finite f64 values",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{
        store::{open_directory, read_exact_at, stamp, validate_ancestors, write_all_at},
        PlanarSpec,
    };
    use super::*;
    use ring::digest::{Context, SHA256};
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
                .join(format!("peerbrush-scalar-tests-{}", uuid::Uuid::new_v4()));
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
        fn field(&self) -> PathBuf {
            self.root.join("field")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
            if self.root.starts_with(temp)
                && self
                    .root
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("peerbrush-scalar-tests-"))
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
    fn value(x: u32, y: u32) -> f64 {
        match (x + y) % 5 {
            0 => -0.0,
            1 => 1.0e-310,
            2 => -1.25,
            _ => (y as f64 * 509.0 + x as f64) * 0.125,
        }
    }
    fn fill(scratch: &mut ScalarScratch, budget: &ResidentBudget) -> Digest {
        let [width, height] = scratch.frame();
        let mut hash = Context::new(&SHA256);
        let mut y = 0;
        while y < height {
            let rows = (height - y).min(256);
            let mut bytes =
                ScratchBytes::new(budget, width as usize * rows as usize * 8, 64 * 1024 * 1024)
                    .unwrap();
            for row in 0..rows {
                for x in 0..width {
                    let at = (row as usize * width as usize + x as usize) * 8;
                    bytes[at..at + 8].copy_from_slice(&value(x, y + row).to_be_bytes());
                }
            }
            hash.update(&bytes);
            scratch.append_rows(y, &bytes, &mut okay).unwrap();
            y += rows;
        }
        hash.finish().as_ref().try_into().unwrap()
    }

    #[test]
    fn scalar_field_preserves_f64_bits_full_emission_hash_and_bounded_rectangles() {
        let fixture = Fixture::new();
        let spec = ScalarSpec::new(509, 260).unwrap();
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch =
            ScalarScratch::create(&fixture.field(), spec, budget.clone(), disk.clone()).unwrap();
        let emitted = fill(&mut scratch, &budget);
        let completed = scratch.finish([19; 32], &mut okay).unwrap();
        assert_eq!(completed.frame(), [509, 260]);
        assert_eq!(completed.bounds(), [0, 0, 509, 260]);
        assert_eq!(completed.stage_binding(), [19; 32]);
        assert_eq!(completed.value_sha256(), emitted);
        assert!(completed.uses_budgets(&budget, &disk));
        let mut output = ScratchBytes::new(&budget, 256 * 256 * 8, 64 * 1024 * 1024).unwrap();
        completed
            .read_rect(101, 3, 256, 256, &mut output, &mut okay)
            .unwrap();
        for row in 0..256 {
            for x in 0..256 {
                let at = (row * 256 + x) as usize * 8;
                assert_eq!(&output[at..at + 8], &value(101 + x, 3 + row).to_be_bytes());
            }
        }
        let mut row = ScratchBytes::new(&budget, spec.row_bytes(), 64 * 1024 * 1024).unwrap();
        completed.read_into(259, 1, &mut row, &mut okay).unwrap();
        for x in 0..509 {
            assert_eq!(&row[x * 8..x * 8 + 8], &value(x as u32, 259).to_be_bytes());
        }
        let mut magic = [0; 8];
        read_exact_at(&completed.inner.owned.file, 0, &mut magic).unwrap();
        assert_eq!(&magic, b"PBF64L01");
        assert!(PlanarSpec::new(509, 260, 64).is_err());
        assert!(matches!(
            completed.clone().retire(&mut okay),
            Err(Error::Busy)
        ));
        drop(row);
        drop(output);
        completed.retire(&mut okay).unwrap();
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }

    #[test]
    fn nonfinite_or_partial_scalar_samples_never_enter_the_owned_file() {
        let fixture = Fixture::new();
        let spec = ScalarSpec::new(3, 1).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch =
            ScalarScratch::create(&fixture.field(), spec, budget.clone(), disk.clone()).unwrap();
        for invalid in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            let mut bytes = [0; 24];
            bytes[..8].copy_from_slice(&invalid.to_be_bytes());
            assert!(scratch.append_rows(0, &bytes, &mut okay).is_err());
            assert_eq!(scratch.inner.owned.file.metadata().unwrap().len(), 64);
        }
        assert!(scratch.append_rows(0, &[0; 23], &mut okay).is_err());
        scratch.append_rows(0, &[0; 24], &mut okay).unwrap();
        scratch
            .finish([9; 32], &mut okay)
            .unwrap()
            .retire(&mut okay)
            .unwrap();
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }

    #[test]
    fn scalar_rectangles_refuse_foreign_pools_bounds_and_late_cancellation() {
        let fixture = Fixture::new();
        let spec = ScalarSpec::new(509, 260).unwrap();
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch =
            ScalarScratch::create(&fixture.field(), spec, budget.clone(), disk.clone()).unwrap();
        fill(&mut scratch, &budget);
        let completed = scratch.finish([17; 32], &mut okay).unwrap();
        let mut foreign = ScratchBytes::new(&ResidentBudget::new(1024).unwrap(), 8, 8).unwrap();
        assert!(matches!(
            completed.read_rect(0, 0, 1, 1, &mut foreign, &mut okay),
            Err(Error::Budget)
        ));
        let mut output = ScratchBytes::new(&budget, 8, 8).unwrap();
        for (x, y, w, h) in [
            (509, 0, 1, 1),
            (0, 260, 1, 1),
            (0, 0, 0, 1),
            (0, 0, 1, 257),
            (u32::MAX, 0, 1, 1),
        ] {
            assert!(completed
                .read_rect(x, y, w, h, &mut output, &mut okay)
                .is_err());
        }
        let before = budget.used();
        assert!(matches!(
            completed.read_rect(0, 0, 1, 1, &mut output, &mut || Err(Error::Cancelled)),
            Err(Error::Cancelled)
        ));
        assert_eq!(budget.used(), before);
        drop(output);
        completed.retire(&mut okay).unwrap();
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }

    #[test]
    fn scalar_corruption_is_not_returned_as_a_value_or_released_quota() {
        let fixture = Fixture::new();
        let spec = ScalarSpec::new(17, 3).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch =
            ScalarScratch::create(&fixture.field(), spec, budget.clone(), disk.clone()).unwrap();
        fill(&mut scratch, &budget);
        let mut completed = scratch.finish([13; 32], &mut okay).unwrap();
        let owned = &mut Arc::get_mut(&mut completed.inner).unwrap().owned;
        write_all_at(&owned.file, 64, &f64::NAN.to_be_bytes()).unwrap();
        owned.expected = stamp(&owned.file).unwrap();
        let mut output = ScratchBytes::new(&budget, 8, 8).unwrap();
        let before = budget.used();
        assert!(matches!(
            completed.read_rect(0, 0, 1, 1, &mut output, &mut okay),
            Err(Error::Corrupt)
        ));
        assert_eq!(budget.used(), before);
        drop(output);
        drop(completed);
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
    }

    #[test]
    fn scalar_foreign_replacement_survives_retirement_refusal() {
        let fixture = Fixture::new();
        let spec = ScalarSpec::new(17, 3).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch =
            ScalarScratch::create(&fixture.field(), spec, budget.clone(), disk.clone()).unwrap();
        fill(&mut scratch, &budget);
        let completed = scratch.finish([13; 32], &mut okay).unwrap();
        let path = fixture.field().join("field.pbf");
        let moved = fixture.root.join("owned-moved");
        fs::rename(&path, &moved).unwrap();
        fs::write(&path, b"foreign scalar path").unwrap();
        assert!(completed.retire(&mut okay).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"foreign scalar path");
        assert_eq!(fs::metadata(moved).unwrap().len(), spec.encoded_bytes());
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
    }

    #[test]
    fn explicit_abort_reclaims_only_a_fully_verified_known_prefix() {
        let fixture = Fixture::new();
        let spec = ScalarSpec::new(16000, 2).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch =
            ScalarScratch::create(&fixture.field(), spec, budget.clone(), disk.clone()).unwrap();
        let row = ScratchBytes::new(&budget, spec.row_bytes(), spec.row_bytes()).unwrap();
        let probe = scratch.inner.owned.file.try_clone().unwrap();
        assert!(matches!(
            scratch.append_rows(0, &row, &mut || {
                if probe.metadata()?.len() > 64 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Cancelled)
        ));
        assert_eq!(probe.metadata().unwrap().len(), 64 + 65536);
        drop(probe);
        drop(row);
        scratch.abort(&mut okay).unwrap();
        assert!(!fixture.field().join("field.pbf").exists());
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }

    #[test]
    fn abort_preserves_unknown_partial_bytes_and_mismatched_prefixes() {
        for extra in [false, true] {
            let fixture = Fixture::new();
            let spec = ScalarSpec::new(16000, 2).unwrap();
            let budget = ResidentBudget::new(1024 * 1024).unwrap();
            let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
            let mut scratch =
                ScalarScratch::create(&fixture.field(), spec, budget.clone(), disk.clone())
                    .unwrap();
            let row = ScratchBytes::new(&budget, spec.row_bytes(), spec.row_bytes()).unwrap();
            let probe = scratch.inner.owned.file.try_clone().unwrap();
            assert!(scratch
                .append_rows(0, &row, &mut || {
                    let length = probe.metadata()?.len();
                    if length > 64 {
                        write_all_at(&probe, if extra { length } else { 64 }, b"unknown")?;
                        Err(Error::Io(std::io::ErrorKind::StorageFull.into()))
                    } else {
                        Ok(())
                    }
                })
                .is_err());
            let length = probe.metadata().unwrap().len();
            drop(probe);
            drop(row);
            let result = scratch.abort(&mut okay);
            assert!(matches!(result, Err(Error::Changed | Error::Corrupt)));
            assert_eq!(
                fs::metadata(fixture.field().join("field.pbf"))
                    .unwrap()
                    .len(),
                length
            );
            assert_eq!(budget.used(), 0);
            assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        }
    }

    #[test]
    fn abort_rechecks_late_changes_and_foreign_names_before_deleting() {
        for foreign in [false, true] {
            let fixture = Fixture::new();
            let spec = ScalarSpec::new(17, 2).unwrap();
            let budget = ResidentBudget::new(1024 * 1024).unwrap();
            let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
            let mut scratch =
                ScalarScratch::create(&fixture.field(), spec, budget.clone(), disk.clone())
                    .unwrap();
            scratch.append_rows(0, &[0; 17 * 8], &mut okay).unwrap();
            let path = fixture.field().join("field.pbf");
            if foreign {
                fs::rename(&path, fixture.root.join("original")).unwrap();
                fs::write(&path, b"foreign abort path").unwrap();
                assert!(scratch.abort(&mut okay).is_err());
                assert_eq!(fs::read(&path).unwrap(), b"foreign abort path");
            } else {
                let probe = scratch.inner.owned.file.try_clone().unwrap();
                let mut checks = 0;
                assert!(matches!(
                    scratch.abort(&mut || {
                        checks += 1;
                        if checks == 3 {
                            let length = probe.metadata()?.len();
                            write_all_at(&probe, length, b"late")?;
                        }
                        Ok(())
                    }),
                    Err(Error::Changed)
                ));
                drop(probe);
                assert!(path.exists());
            }
            assert_eq!(budget.used(), 0);
            assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        }
    }

    #[test]
    #[ignore = "Opt-in full 2.048GB derived scalar field; storage component only"]
    fn full_16000_scalar_field_preserves_value_hash_rectangles_and_retirement() {
        let started = std::time::Instant::now();
        let fixture = Fixture::new();
        let spec = ScalarSpec::new(16000, 16000).unwrap();
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch =
            ScalarScratch::create(&fixture.field(), spec, budget.clone(), disk.clone()).unwrap();
        let mut bytes = ScratchBytes::new(&budget, spec.row_bytes() * 8, 64 * 1024 * 1024).unwrap();
        let mut values = Context::new(&SHA256);
        for y in (0..16000).step_by(8) {
            for row in 0..8 {
                for x in 0..16000 {
                    let at = (row as usize * 16000 + x as usize) * 8;
                    bytes[at..at + 8].copy_from_slice(&value(x, y + row).to_be_bytes());
                }
            }
            values.update(&bytes);
            scratch.append_rows(y, &bytes, &mut okay).unwrap();
            if (y + 8) % 4000 == 0 {
                println!(
                    "scalar component rows={} payload_written={} resident_bytes={}",
                    y + 8,
                    scratch.inner.owned.file.metadata().unwrap().len() - 64,
                    budget.used()
                );
            }
        }
        let expected: Digest = values.finish().as_ref().try_into().unwrap();
        let completed = scratch.finish([31; 32], &mut okay).unwrap();
        assert_eq!(completed.value_sha256(), expected);
        completed
            .read_into(15992, 8, &mut bytes, &mut okay)
            .unwrap();
        for row in 0..8 {
            for x in 0..16000 {
                let at = (row as usize * 16000 + x as usize) * 8;
                assert_eq!(&bytes[at..at + 8], &value(x, 15992 + row).to_be_bytes());
            }
        }
        let mut rect = ScratchBytes::new(&budget, 256 * 256 * 8, 64 * 1024 * 1024).unwrap();
        completed
            .read_rect(137, 15744, 256, 256, &mut rect, &mut okay)
            .unwrap();
        for row in 0..256 {
            for x in 0..256 {
                let at = (row as usize * 256 + x as usize) * 8;
                assert_eq!(
                    &rect[at..at + 8],
                    &value(137 + x, 15744 + row).to_be_bytes()
                );
            }
        }
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        let resident = budget.used();
        let digest = expected
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        drop(rect);
        drop(bytes);
        completed.retire(&mut okay).unwrap();
        assert_eq!(disk.used().unwrap(), 0);
        assert_eq!(budget.used(), 0);
        println!("scalar component PASS frame=16000x16000 finite_f64_BE payload_bytes={} encoded_bytes={} charged_resident_bytes={resident} elapsed_ms={} value_sha256={digest}; derived storage only, no operator/editor/source authority or total RAM qualification",spec.payload_bytes(),spec.encoded_bytes(),started.elapsed().as_millis());
    }
}
