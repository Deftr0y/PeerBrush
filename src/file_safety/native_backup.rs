//! Preserve under the already held inventory quota; never reacquire that lock.
use super::*;
use crate::large_document::ScratchBytes;
use ring::digest::{Context, SHA256};

impl NativeInventory<'_> {
    /// The caller must durably admit `prospective_bytes` before creating any
    /// external backup/output bytes. This guard authenticates actual files; it
    /// neither creates that journal nor grants final destination publication.
    /// Uncertain partial files remain present and charged by that admission.
    #[cfg(test)]
    pub(crate) fn preserve_current(
        &mut self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<PathBuf>, String> {
        let key = destination_key(self.lease.path());
        let name = OsString::from(format!(
            "{:016x}{:016x}-{}.{}",
            key[0],
            key[1],
            crate::engine::id(),
            self.lease
                .path()
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
        ));
        self.preserve_with_name(name, check)
    }
    pub(in crate::file_safety) fn preserve_planned(
        &mut self,
        plan: &super::super::native_outcome::BackupPlan,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<PathBuf>, String> {
        if plan.target() != self.lease.path() || plan.expected() != self.lease.expected() {
            return Err("Native backup plan belongs to another destination version".into());
        }
        match (self.lease.expected(), plan.name()) {
            (None, None) => {
                self.validate(check)?;
                Ok(None)
            }
            (Some(_), Some(name)) => self.preserve_with_name(name.into(), check),
            _ => Err("Native backup name differs from the admitted source".into()),
        }
    }
    fn preserve_with_name(
        &mut self,
        name: OsString,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<PathBuf>, String> {
        self.validate(check)?;
        let Some(expected) = self.lease.expected() else {
            return Ok(None);
        };
        if self.entries.len() >= MAX_ENTRIES
            || self
                .backup_bytes
                .checked_add(expected.bytes)
                .is_none_or(|n| n > FilePolicy::Source36.backup_bytes())
        {
            return Err("Native previous-save inventory cannot admit another backup".into());
        }
        let mut buffer =
            ScratchBytes::new(&self.budget, 65536, 65536).map_err(|e| e.to_string())?;
        if name.as_encoded_bytes().len() > 1024
            || !matches!(
                (
                    Path::new(&name).components().next(),
                    Path::new(&name).components().nth(1)
                ),
                (Some(std::path::Component::Normal(_)), None)
            )
        {
            return Err("Native backup plan contains an invalid component".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            if name
                .encode_wide()
                .any(|unit| unit == 0 || unit == b':' as u16)
            {
                return Err("Native backup plan contains a stream alias".into());
            }
        }
        let path = self.directory.join(&name);
        let mut source = open_native_read(self.lease.path()).map_err(|e| e.to_string())?;
        super::super::native_access::supported(&source)?;
        if identity(&source)? != expected.identity {
            return Err("Native previous source identity changed".into());
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC. Keep
            // read/delete sharing for identity observation and replacement tests.
            options
                .access_mode(0xC0060000)
                .share_mode(5)
                .custom_flags(0x00200000);
        }
        self.validate(check)?;
        let mut output = options.open(&path).map_err(|e| e.to_string())?;
        super::super::owned_rename::hold_write_stamp(&output)?;
        let output_id = identity(&output)?;
        let named = || -> Result<(), String> {
            regular_directory(&self.directory)?;
            if identity(&self.held_directory)? != self.directory_identity
                || identity(&open_directory(&self.directory)?)? != self.directory_identity
                || !regular(&path)
                || identity(&open_inventory_file(&path)?)? != output_id
            {
                return Err("Native backup owner changed; all names retained".into());
            }
            Ok(())
        };
        named()?;
        super::super::native_access::preserve(
            &source,
            &output,
            super::super::native_access::Destination::Backup,
            &self.budget,
            check,
        )?;
        let mut left = expected.bytes;
        let mut digest = Context::new(&SHA256);
        while left != 0 {
            check()?;
            named()?;
            let count = left.min(buffer.len() as u64) as usize;
            source
                .read_exact(&mut buffer[..count])
                .map_err(|e| e.to_string())?;
            digest.update(&buffer[..count]);
            output
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
            left -= count as u64;
        }
        if source.read(&mut [0; 1]).map_err(|e| e.to_string())? != 0
            || digest.finish().as_ref() != expected.fingerprint
        {
            return Err("Native previous file bytes changed; backup debt retained".into());
        }
        super::super::owned_rename::finish_write_stamp(&output)?;
        output.sync_all().map_err(|e| e.to_string())?;
        let sealed = owned_rename::Sealed::verify(
            &output,
            expected.bytes,
            expected.fingerprint,
            &self.budget,
            &mut || {
                check()?;
                named()
            },
        )?;
        // Keep the seal alive through final named/source checks. This does not
        // turn the backup into a publication receipt for the new document.
        let _sealed = sealed;
        self.lease.validate(check)?;
        check()?;
        named()?;
        let entry = Entry::capture(&self.directory, name)?;
        if entry.identity != output_id || entry.bytes != expected.bytes {
            return Err("Native backup changed after complete readback; files retained".into());
        }
        drop(output);
        self.entries.push(entry);
        self.entries.sort_by(|a, b| a.name.cmp(&b.name));
        self.backup_bytes += expected.bytes;
        crate::server::sync_parent(&path)?;
        self.validate(check)?;
        Ok(Some(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, time::Duration};
    fn setup() -> (PathBuf, PathBuf, ResidentBudget) {
        let dir = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("pb-native-held-backup-{}", crate::engine::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("source.psb");
        fs::write(
            &path,
            (0..200123).map(|i| (i * 29 + 17) as u8).collect::<Vec<_>>(),
        )
        .unwrap();
        (dir, path, ResidentBudget::new(16 * 1024 * 1024).unwrap())
    }
    fn cleanup(dir: &Path) {
        assert_eq!(
            dir.parent().unwrap(),
            fs::canonicalize(std::env::temp_dir()).unwrap()
        );
        assert_eq!(fs::canonicalize(dir).unwrap(), dir);
        assert!(dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pb-native-held-backup-"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn held_quota_preserves_exact_previous_bytes_without_reacquiring_it() {
        let (dir, path, budget) = setup();
        let cleanup_dir = dir.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
            let mut inventory = lease.freeze_inventory(&budget, &|| Ok(())).unwrap();
            let old = fs::read(&path).unwrap();
            let before = inventory.prospective_bytes(77).unwrap();
            let backup = inventory.preserve_current(&|| Ok(())).unwrap().unwrap();
            assert_eq!(fs::read(&backup).unwrap(), old);
            assert_eq!(fs::read(&path).unwrap(), old);
            assert_eq!(inventory.backup_bytes(), old.len() as u64);
            assert_eq!(inventory.external_bytes().unwrap() + 77, before);
            inventory.validate(&|| Ok(())).unwrap();
            let saved =
                Entry::capture(backup.parent().unwrap(), backup.file_name().unwrap().into())
                    .unwrap();
            saved.validate(backup.parent().unwrap()).unwrap();
            drop(saved);
            drop(inventory);
            assert_eq!(budget.used(), 0);
            send.send(()).unwrap();
        });
        receive
            .recv_timeout(Duration::from_secs(5))
            .expect("Held quota backup blocked on its own lock");
        worker.join().unwrap();
        cleanup(&cleanup_dir);
    }
    #[test]
    fn cancelled_partial_backup_stays_debt_and_cannot_be_adopted_as_complete() {
        let (dir, path, budget) = setup();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        let mut inventory = lease.freeze_inventory(&budget, &|| Ok(())).unwrap();
        let original = fs::read(&path).unwrap();
        let result = inventory.preserve_current(&|| {
            if fs::read_dir(dir.join(".peerbrush-versions"))
                .unwrap()
                .flatten()
                .any(|e| {
                    e.path().extension().is_some_and(|x| x == "psb")
                        && e.metadata().is_ok_and(|m| m.len() > 0)
                })
            {
                Err("cancel exact backup".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err(), "cancel exact backup");
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(inventory.validate(&|| Ok(())).is_err());
        assert!(fs::read_dir(dir.join(".peerbrush-versions"))
            .unwrap()
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "psb")));
        drop(inventory);
        drop(lease);
        cleanup(&dir);
    }
    #[test]
    fn foreign_backup_replacement_is_never_written_or_removed() {
        let (dir, path, budget) = setup();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        let mut inventory = lease.freeze_inventory(&budget, &|| Ok(())).unwrap();
        let replaced = Cell::new(false);
        let result = inventory.preserve_current(&|| {
            if !replaced.get() {
                if let Some(entry) = fs::read_dir(dir.join(".peerbrush-versions"))
                    .unwrap()
                    .flatten()
                    .find(|e| e.path().extension().is_some_and(|x| x == "psb"))
                {
                    fs::rename(entry.path(), dir.join("retained-owned-partial")).unwrap();
                    fs::write(entry.path(), b"foreign replacement stays").unwrap();
                    replaced.set(true);
                }
            }
            Ok(())
        });
        assert!(result.is_err());
        assert!(replaced.get());
        let foreign = fs::read_dir(dir.join(".peerbrush-versions"))
            .unwrap()
            .flatten()
            .find(|e| e.path().extension().is_some_and(|x| x == "psb"))
            .unwrap()
            .path();
        assert_eq!(fs::read(foreign).unwrap(), b"foreign replacement stays");
        assert!(dir.join("retained-owned-partial").exists());
        drop(inventory);
        drop(lease);
        cleanup(&dir);
    }

    #[test]
    fn changed_source_and_absent_destination_never_produce_a_false_backup_receipt() {
        let (dir, path, budget) = setup();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        let mut inventory = lease.freeze_inventory(&budget, &|| Ok(())).unwrap();
        let changed = Cell::new(false);
        assert!(inventory
            .preserve_current(&|| {
                if !changed.get()
                    && fs::read_dir(dir.join(".peerbrush-versions"))
                        .unwrap()
                        .flatten()
                        .any(|e| {
                            e.path().extension().is_some_and(|x| x == "psb")
                                && e.metadata().is_ok_and(|m| m.len() > 0)
                        })
                {
                    fs::write(&path, b"external source edit stays").unwrap();
                    changed.set(true);
                }
                Ok(())
            })
            .is_err());
        assert!(changed.get());
        assert_eq!(fs::read(&path).unwrap(), b"external source edit stays");
        drop(inventory);
        drop(lease);
        let absent = dir.join("save-as.psb");
        let lease = NativeLease::acquire(&absent, || Ok(())).unwrap();
        let mut inventory = lease.freeze_inventory(&budget, &|| Ok(())).unwrap();
        let before = inventory.names(&|| Ok(())).unwrap();
        assert!(inventory.preserve_current(&|| Ok(())).unwrap().is_none());
        assert_eq!(inventory.names(&|| Ok(())).unwrap(), before);
        assert!(!absent.exists());
        drop(inventory);
        drop(lease);
        cleanup(&dir);
    }
}
