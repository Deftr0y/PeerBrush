//! Rename only the file object whose identity was checked, with no replacement.
use super::*;
use ::windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO},
};
use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};

pub(super) fn rename(path: &Path, identity: (u64, u64), destination: &Path) -> Result<()> {
    move_owned(path, identity, destination, true)
}
pub(super) fn rename_file(path: &Path, identity: (u64, u64), destination: &Path) -> Result<()> {
    move_owned(path, identity, destination, false)
}
fn move_owned(
    path: &Path,
    identity: (u64, u64),
    destination: &Path,
    is_directory: bool,
) -> Result<()> {
    let parent = Directory::open(destination.parent().ok_or(STAGE_HOLD)?)?;
    if !destination.is_absolute() || fs::symlink_metadata(destination).is_ok() {
        return Err(STAGE_HOLD.into());
    }
    if is_directory {
        directory(path)?;
    } else {
        regular(path)?;
    }
    let file = OpenOptions::new()
        .access_mode(0x00010000 | 0x00000080)
        .share_mode(3)
        .custom_flags(0x02000000 | 0x00200000)
        .open(path)
        .map_err(|_| STAGE_HOLD)?;
    if crate::file_safety::identity(&file)? != identity {
        return Err(STAGE_HOLD.into());
    }
    let name: Vec<_> = destination.as_os_str().encode_wide().collect();
    if name.is_empty() || name.len() > 32760 {
        return Err(STAGE_HOLD.into());
    }
    let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let length = offset.checked_add(name.len() * 2).ok_or(STAGE_HOLD)?;
    let mut aligned = vec![0u64; length.div_ceil(8)];
    let info = aligned.as_mut_ptr() as *mut FILE_RENAME_INFO;
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false.into();
        (*info).RootDirectory = HANDLE::default();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileRenameInfo,
            info as _,
            length as u32,
        )
        .map_err(|_| STAGE_HOLD)?;
    }
    parent.check()?;
    // The DELETE-capable rename handle remains alive. A readback must share
    // that access and support directories; the ordinary metadata helper does
    // not request FILE_FLAG_BACKUP_SEMANTICS.
    let renamed = OpenOptions::new()
        .access_mode(0)
        .share_mode(7)
        .custom_flags(0x02000000 | 0x00200000)
        .open(destination)
        .map_err(|_| STAGE_HOLD)?;
    if crate::file_safety::identity(&renamed)? != identity {
        return Err(STAGE_HOLD.into());
    }
    crate::server::sync_parent(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_retirement_preserves_foreign_replacements_and_repeated_archives() {
        let root =
            std::env::temp_dir().join(format!("peerbrush-retirement-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap(); // also exercise verbatim Windows paths
        for cycle in 0..3 {
            let source = root.join("current");
            fs::create_dir(&source).unwrap();
            fs::write(source.join("signed-archive"), format!("cycle {cycle}")).unwrap();
            let held = Directory::open(&source).unwrap();
            let identity = held.identity;
            drop(held);
            let archived = root.join(format!("archive-{cycle}"));
            rename(&source, identity, &archived).unwrap();
            assert_eq!(
                fs::read_to_string(archived.join("signed-archive")).unwrap(),
                format!("cycle {cycle}")
            );
            fs::create_dir(&source).unwrap();
            let foreign = Directory::open(&source).unwrap();
            let other = foreign.identity;
            drop(foreign);
            assert_ne!(identity, other);
            assert!(rename(&source, identity, &root.join("wrong")).is_err());
            assert!(source.is_dir());
            assert!(rename(&source, other, &archived).is_err());
            assert_eq!(
                fs::read_to_string(archived.join("signed-archive")).unwrap(),
                format!("cycle {cycle}")
            );
            fs::remove_dir(&source).unwrap();
        }
        let marker = root.join("active.json");
        fs::write(&marker, b"completed-owner").unwrap();
        let identity = crate::file_safety::identity(&File::open(&marker).unwrap()).unwrap();
        let archived = root.join("completed.json");
        rename_file(&marker, identity, &archived).unwrap();
        assert_eq!(fs::read(&archived).unwrap(), b"completed-owner");
        fs::write(&marker, b"foreign-owner").unwrap();
        assert!(rename_file(&marker, identity, &root.join("wrong.json")).is_err());
        assert_eq!(fs::read(&marker).unwrap(), b"foreign-owner");
        fs::remove_dir_all(root).unwrap();
    }
}
