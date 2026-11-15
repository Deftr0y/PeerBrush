//! A published source can retire only through a complete, exclusive root permit.
//! Failed attempts keep custody/debt and taint fresh source admission.
use super::*;
use crate::{large_document::DiskReservation, workspace::source_runtime::SourceRetirement};

pub(crate) struct RetiredSource {
    identity: (u64, u64),
    bytes: u64,
    budget: DiskBudget,
}
impl RetiredSource {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn into_parts(self) -> ((u64, u64), u64, DiskBudget) {
        (self.identity, self.bytes, self.budget)
    }
}
impl ParkedSource {
    pub(crate) fn retire(
        mut self,
        permit: &SourceRetirement<'_>,
        check: &dyn Fn() -> std::result::Result<(), String>,
    ) -> std::result::Result<(DiskReservation, RetiredSource), String> {
        if !cfg!(windows) {
            return Err("Handle-bound source retirement is unavailable; source retained".into());
        }
        let store = self.store.as_ref().unwrap();
        let disk = store
            .disk_budget()
            .ok_or("Source has no actual disk admission")?;
        let filesystem = store.filesystem_identity();
        let validate = || {
            permit.check(
                store.identity(),
                filesystem,
                &store.resident_budget(),
                &disk,
            )
        };
        check()?;
        validate()?;
        self.verify(&mut || {
            check()
                .and_then(|_| permit.check_namespace())
                .map_err(|_| Error::Cancelled)
        })
        .map_err(|e| e.to_string())?;
        let expected = store.state().map_err(|e| e.to_string())?.expected;
        let charge = store
            .0
            .disk
            .as_ref()
            .ok_or("Source disk owner unavailable")?;
        if charge.bytes().map_err(|e| e.to_string())? != expected.bytes {
            return Err("Source disk reservation differs from retained bytes".into());
        }
        let file = control(&store.0.path, false)?;
        let directory = control(&store.0.root, true)?;
        let parent = crate::large_document::root::Directory::open(
            store.0.root.parent().ok_or("Source parent unavailable")?,
        )?;
        if parent.identity() != permit.parent_identity()?
            || crate::file_safety::identity(&file)? != filesystem.1
            || crate::file_safety::identity(&directory)? != filesystem.0
            || file
                .metadata()
                .map_err(|e| e.to_string())?
                .permissions()
                .readonly()
        {
            return Err("Source retirement handles differ from retained ownership".into());
        }
        check()?;
        // No application callback after the final complete source/root checks.
        validate()?;
        store.check_source(expected).map_err(|e| e.to_string())?;
        parent.check()?;
        permit.attempt()?;
        crate::large_document::unlink_owned_at(&file, &store.0.directory, Path::new("blocks.pbt"))
            .map_err(|e| e.to_string())?;
        // POSIX disposition removes the namespace link on close of this DELETE
        // handle. Keep the independent original owner to verify zero links.
        // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntddk/ns-ntddk-_file_disposition_information_ex
        drop(file);
        if crate::file_safety::link_count(&store.0.file)? != 0 {
            return Err("Retired source retains a filesystem link; debt retained".into());
        }
        #[cfg(test)]
        permit.fail(crate::workspace::source_runtime::CollectionFault::File)?;
        // Delete the now-empty owned directory through its actual handle. A
        // replacement canonical directory can never become our deletion target.
        let named = open_directory(&store.0.root).map_err(|e| e.to_string())?;
        if crate::file_safety::identity(&named)? != filesystem.0
            || fs::read_dir(&store.0.root)
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
        {
            return Err("Retired source directory changed; handles and debt retained".into());
        }
        drop(named);
        crate::large_document::unlink_owned_at(
            &directory,
            parent.held(),
            Path::new(
                store
                    .0
                    .root
                    .file_name()
                    .ok_or("Invalid source directory component")?,
            ),
        )
        .map_err(|e| e.to_string())?;
        drop(directory);
        #[cfg(test)]
        permit.fail(crate::workspace::source_runtime::CollectionFault::Directory)?;
        permit.check_namespace()?;
        permit.close_observation()?;
        let store = self.store.take().unwrap();
        let inner =
            Arc::try_unwrap(store.0).unwrap_or_else(|_| unreachable!("Private source retirement"));
        let Backing {
            root,
            file: source_file,
            directory: source_directory,
            disk: charge,
            ..
        } = inner.into_backing();
        // All known handles close before any nominal receipt or quota settlement.
        drop(source_file);
        drop(source_directory);
        parent.check()?;
        crate::server::sync_parent(&root)?;
        if !matches!(fs::symlink_metadata(&root), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        {
            return Err("Retired source directory name remains; debt retained".into());
        }
        parent.check()?;
        permit.check_namespace()?;
        Ok((
            charge.ok_or("Retired source charge unavailable")?,
            RetiredSource {
                identity: filesystem.1,
                bytes: expected.bytes,
                budget: disk,
            },
        ))
    }
}
fn control(path: &Path, directory: bool) -> std::result::Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // DELETE | READ_ATTRIBUTES | SYNCHRONIZE; no second data writer.
        options
            .access_mode(0x00110080)
            .share_mode(7)
            .custom_flags(0x00200000 | if directory { 0x02000000 } else { 0 });
    }
    let file = options
        .open(path)
        .map_err(|_| "Cannot acquire source retirement handle; source retained")?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if metadata.is_dir() != directory || (!directory && !metadata.is_file()) {
        return Err("Source retirement handle has another kind".into());
    }
    Ok(file)
}
