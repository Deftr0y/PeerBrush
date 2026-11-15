//! A newly created marker and the exact prefix acknowledged by its writer.
use super::*;
use crate::large_document::{DiskBudget, DiskReservation};
use ring::digest::{Context, SHA256};
use std::path::Path;

pub(super) struct Pending {
    directory: Directory,
    from: String,
    to: String,
    file: File,
    charge: DiskReservation,
    admitted: u64,
    written: u64,
    hash: Context,
    session: [u8; 16],
}

/// Only this owned unpublished writer can mint a verified retirement receipt.
pub(crate) struct RetiredPendingMarker {
    identity: (u64, u64),
    admitted: u64,
    budget: DiskBudget,
}
impl RetiredPendingMarker {
    pub(crate) fn into_parts(self) -> ((u64, u64), u64, DiskBudget) {
        (self.identity, self.admitted, self.budget)
    }
}

impl Pending {
    pub(super) fn new(
        session: &SourceSession,
        snapshot: [u8; 16],
        bytes: u64,
    ) -> Result<Self, String> {
        let directory = Directory::open(&session.directory)?;
        let charge = session.disk().reserve(bytes).map_err(|e| e.to_string())?;
        let uuid = uuid::Uuid::from_bytes(snapshot);
        let from = format!("pending-{uuid}");
        let file = owned_rename::staging_options()
            .open(directory.path().join(&from))
            .map_err(|_| "Cannot create exclusive native recovery marker; admission retained")?;
        charge.bind(&file).map_err(|e| e.to_string())?;
        Ok(Self {
            directory,
            from,
            to: format!("recovery-v3-{uuid}.pbr"),
            file,
            charge,
            admitted: bytes,
            written: 0,
            hash: Context::new(&SHA256),
            session: session.id,
        })
    }
    pub(super) fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.admitted - self.written {
            return Err(io::Error::other(
                "Native recovery marker exceeds admitted length",
            ));
        }
        let count = self.file.write(&bytes[..bytes.len().min(64 * 1024)])?;
        self.hash.update(&bytes[..count]);
        self.written += count as u64;
        Ok(count)
    }
    pub(super) fn seal(
        &self,
        session: &SourceSession,
        sha256: [u8; 32],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<owned_rename::Sealed, String> {
        self.check_session(session)?;
        if self.written != self.admitted {
            return Err("Native recovery marker producer ended before its admitted length".into());
        }
        owned_rename::Sealed::verify(
            &self.file,
            self.admitted,
            sha256,
            &session.resident(),
            &mut || {
                check()?;
                session.check()
            },
        )
    }
    pub(super) fn publish(
        self,
        session: &SourceSession,
        proof: &owned_rename::Sealed,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<HeldFile, String> {
        self.check_session(session)?;
        owned_rename::publish_new(
            self.directory.path(),
            self.directory.held(),
            Path::new(&self.from),
            Path::new(&self.to),
            &self.file,
            proof,
            &mut || {
                check()?;
                session.check()
            },
        )
        .map_err(|error| {
            format!(
                "Native recovery publication is uncertain; files and admission retained: {error}"
            )
        })?;
        let identity = crate::file_safety::identity(&self.file)?;
        let path = self.directory.path().join(&self.to);
        // Close the writing handle before observing the final Windows write stamp.
        drop(self.file);
        let file = super::super::open_read(&path)?;
        if crate::file_safety::identity(&file)? != identity {
            return Err("Published recovery marker owner changed; bytes retained".into());
        }
        HeldFile::new(path, file)
    }
    pub(super) fn abort(self, session: &SourceSession, reason: String) -> String {
        match self.retire(session) {
            Ok(()) => reason,
            Err(cleanup) => format!(
                "{reason}; unpublished marker cleanup refused, bytes/admission retained: {cleanup}"
            ),
        }
    }
    fn retire(self, session: &SourceSession) -> Result<(), String> {
        self.check_session(session)?;
        if !crate::recovery::NATIVE_DISCARD_AVAILABLE {
            return Err("Handle-bound marker removal is unavailable on this platform".into());
        }
        session.cleanup_check()?;
        self.directory.check()?;
        // A failed write may have changed more bytes than it acknowledged. Only
        // the complete matching prefix can authorize removal; never truncate it.
        let hash = self.hash.clone().finish().as_ref().try_into().unwrap();
        let proof = owned_rename::Sealed::verify(
            &self.file,
            self.written,
            hash,
            &session.resident(),
            &mut || session.cleanup_check(),
        )?;
        let retiring = format!("pending-{}", uuid::Uuid::new_v4());
        owned_rename::publish_new(
            self.directory.path(),
            self.directory.held(),
            Path::new(&self.from),
            Path::new(&retiring),
            &self.file,
            &proof,
            &mut || session.cleanup_check(),
        )?;
        crate::large_document::unlink_owned_at(
            &self.file,
            self.directory.held(),
            Path::new(&retiring),
        )
        .map_err(|e| e.to_string())?;
        if crate::file_safety::link_count(&self.file)? != 0 {
            return Err("Unpublished marker retains a filesystem link".into());
        }
        let identity = crate::file_safety::identity(&self.file)?;
        drop(self.file);
        self.directory.check()?;
        let path = self.directory.path().join(retiring);
        crate::server::sync_parent(&path)?;
        if !matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == io::ErrorKind::NotFound)
        {
            return Err("Unpublished marker name remains after removal".into());
        }
        self.directory.check()?;
        self.charge
            .settle_pending_marker(RetiredPendingMarker {
                identity,
                admitted: self.admitted,
                budget: session.disk(),
            })
            .map_err(|e| e.to_string())
    }
    fn check_session(&self, session: &SourceSession) -> Result<(), String> {
        if self.session != session.id {
            return Err("Pending recovery marker belongs to another source operation".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::source_runtime::{
        tests::{limits, Fixture},
        SourceRuntime,
    };

    fn with_source(run: impl FnOnce(&SourceSession, &Arc<SourceRuntime>, &Path)) {
        let fixture = Fixture::new();
        let (_shared, connection) = fixture.connection();
        let context =
            crate::large_document::root::PersistentRootContext::from_explicit_state(&connection)
                .unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        run(&session, &runtime, &connection.state_dir);
    }
    use std::sync::Arc;

    #[test]
    fn refused_producers_remove_only_known_unpublished_prefixes_where_supported() {
        for failure in ["empty", "short", "overflow", "sha", "cancel"] {
            with_source(|session, runtime, _| {
                let baseline = runtime.disk().used().unwrap();
                let resident = runtime.resident().used();
                let id = *uuid::Uuid::new_v4().as_bytes();
                let bytes = [19; 96 * 1024];
                let length = bytes.len() as u64;
                let cancelled = std::cell::Cell::new(false);
                let error = session
                    .publish_recovery_marker(
                        id,
                        length,
                        [9; 32],
                        &mut |out| {
                            match failure {
                                "empty" => return Err("Empty producer refused".into()),
                                "short" => {
                                    out.write_all(&bytes[..13]).map_err(|e| e.to_string())?
                                }
                                "overflow" => out
                                    .write_all(&vec![5; bytes.len() + 1])
                                    .map_err(|e| e.to_string())?,
                                "sha" | "cancel" => {
                                    out.write_all(&bytes).map_err(|e| e.to_string())?
                                }
                                _ => unreachable!(),
                            }
                            cancelled.set(failure == "cancel");
                            Ok(())
                        },
                        &|| {
                            if cancelled.get() {
                                Err("Cancelled marker fixture".into())
                            } else {
                                Ok(())
                            }
                        },
                    )
                    .unwrap_err();
                assert!(session.open_recovery_marker(id).is_err());
                assert!(session
                    .recovery_marker_ids()
                    .unwrap()
                    .iter()
                    .next()
                    .is_none());
                let path = session
                    .directory
                    .join(format!("pending-{}", uuid::Uuid::from_bytes(id)));
                if cfg!(windows) {
                    assert!(!error.contains("cleanup refused"), "{error}");
                    assert!(!path.exists());
                    assert_eq!(runtime.disk().used().unwrap(), baseline);
                    assert_eq!(runtime.resident().used(), resident);
                    assert_eq!(
                        session
                            .checkpoint_census(&|| Ok(()))
                            .unwrap()
                            .registered_count(),
                        0
                    );
                } else {
                    assert!(error.contains("cleanup refused"));
                    assert!(path.is_file());
                    assert_eq!(runtime.disk().used().unwrap(), baseline + length);
                }
            });
        }
    }
    #[test]
    fn full_marker_inventory_refuses_before_creating_or_charging_another_file() {
        with_source(|session, runtime, _| {
            let bytes = b"bounded marker";
            let hash = ring::digest::digest(&SHA256, bytes)
                .as_ref()
                .try_into()
                .unwrap();
            // The root owner occupies the first admitted inventory entry.
            for _ in 1..runtime.limits.max_files {
                session
                    .publish_recovery_marker(
                        *uuid::Uuid::new_v4().as_bytes(),
                        bytes.len() as u64,
                        hash,
                        &mut |out| out.write_all(bytes).map_err(|e| e.to_string()),
                        &|| Ok(()),
                    )
                    .unwrap();
            }
            let baseline = runtime.disk().used().unwrap();
            let called = std::cell::Cell::new(false);
            let id = *uuid::Uuid::new_v4().as_bytes();
            let error = session
                .publish_recovery_marker(
                    id,
                    bytes.len() as u64,
                    hash,
                    &mut |_| {
                        called.set(true);
                        Ok(())
                    },
                    &|| Ok(()),
                )
                .unwrap_err();
            assert!(error.contains("inventory capacity exceeded"));
            assert!(!called.get());
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            assert!(!session
                .directory
                .join(format!("pending-{}", uuid::Uuid::from_bytes(id)))
                .exists());
            assert!(!session
                .directory
                .join(format!("recovery-v3-{}.pbr", uuid::Uuid::from_bytes(id)))
                .exists());
        });
    }
    #[test]
    fn changed_prefix_alias_replacement_and_wrong_session_keep_debt_and_foreign_data() {
        for failure in ["prefix", "unacknowledged", "link", "replacement", "session"] {
            with_source(|session, runtime, state_dir| {
                let baseline = runtime.disk().used().unwrap();
                let mut pending =
                    Pending::new(session, *uuid::Uuid::new_v4().as_bytes(), 64).unwrap();
                pending.write(b"known prefix").unwrap();
                let path = pending.directory.path().join(&pending.from);
                let retained = state_dir.join("retained-owned-marker.pbr");
                let identity = crate::file_safety::identity(&pending.file).unwrap();
                match failure {
                    "prefix" => {
                        let modified = pending.file.metadata().unwrap().modified().unwrap();
                        let mut writer = pending.file.try_clone().unwrap();
                        writer.seek(SeekFrom::Start(0)).unwrap();
                        writer.write_all(b"changed").unwrap();
                        writer
                            .set_times(std::fs::FileTimes::new().set_modified(modified))
                            .unwrap();
                    }
                    "unacknowledged" => pending.file.write_all(b"unacknowledged tail").unwrap(),
                    "link" => std::fs::hard_link(&path, &retained).unwrap(),
                    "replacement" => {
                        std::fs::rename(&path, &retained).unwrap();
                        std::fs::write(&path, b"known prefix").unwrap();
                    }
                    "session" => {}
                    _ => unreachable!(),
                }
                let other = runtime.begin_existing().unwrap();
                let error = pending.abort(
                    if failure == "session" {
                        &other
                    } else {
                        session
                    },
                    "Original producer failure".into(),
                );
                assert!(
                    error.starts_with(
                        "Original producer failure; unpublished marker cleanup refused"
                    ),
                    "{error}"
                );
                assert_eq!(runtime.disk().used().unwrap(), baseline + 64);
                assert!(path.is_file());
                if failure == "replacement" {
                    assert_eq!(std::fs::read(&path).unwrap(), b"known prefix");
                    assert_ne!(
                        crate::file_safety::identity(&File::open(path).unwrap()).unwrap(),
                        identity
                    );
                    assert_eq!(std::fs::read(retained).unwrap(), b"known prefix");
                }
            });
        }
    }
    #[test]
    fn a_publication_attempt_keeps_the_pending_owner_when_the_destination_exists() {
        with_source(|session, runtime, _| {
            let id = *uuid::Uuid::new_v4().as_bytes();
            let bytes = b"canonical marker";
            let hash = ring::digest::digest(&SHA256, bytes)
                .as_ref()
                .try_into()
                .unwrap();
            session
                .publish_recovery_marker(
                    id,
                    bytes.len() as u64,
                    hash,
                    &mut |out| out.write_all(bytes).map_err(|e| e.to_string()),
                    &|| Ok(()),
                )
                .unwrap();
            let mut marker = session.open_recovery_marker(id).unwrap();
            let baseline = runtime.disk().used().unwrap();
            let error = session
                .publish_recovery_marker(
                    id,
                    bytes.len() as u64,
                    hash,
                    &mut |out| out.write_all(bytes).map_err(|e| e.to_string()),
                    &|| Ok(()),
                )
                .unwrap_err();
            assert!(error.contains("publication is uncertain"));
            assert!(session.recovery_marker_ids().is_err());
            let mut actual = Vec::new();
            marker.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, bytes);
            assert_eq!(
                runtime.disk().used().unwrap(),
                baseline + bytes.len() as u64
            );
            assert_eq!(
                std::fs::read(
                    session
                        .directory
                        .join(format!("pending-{}", uuid::Uuid::from_bytes(id)))
                )
                .unwrap(),
                bytes
            );
        });
    }
    #[test]
    fn interruption_after_publication_cannot_report_an_empty_catalog() {
        with_source(|session, runtime, _| {
            let id = *uuid::Uuid::new_v4().as_bytes();
            let bytes = b"publication interrupted";
            let hash = ring::digest::digest(&SHA256, bytes)
                .as_ref()
                .try_into()
                .unwrap();
            let canonical = session
                .directory
                .join(format!("recovery-v3-{}.pbr", uuid::Uuid::from_bytes(id)));
            let baseline = runtime.disk().used().unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                session.publish_recovery_marker(
                    id,
                    bytes.len() as u64,
                    hash,
                    &mut |out| out.write_all(bytes).map_err(|e| e.to_string()),
                    &|| {
                        assert!(
                            session.runtime.inventory.try_lock().is_ok(),
                            "Application callback retained inventory lock"
                        );
                        if canonical.exists() {
                            panic!("Interrupted after native publication");
                        }
                        Ok(())
                    },
                )
            }));
            assert!(result.is_err());
            assert_eq!(std::fs::read(canonical).unwrap(), bytes);
            assert_eq!(
                runtime.disk().used().unwrap(),
                baseline + bytes.len() as u64
            );
            assert!(session.recovery_marker_ids().is_err());
            assert!(session.checkpoint_census(&|| Ok(())).is_err());
        });
    }
}
