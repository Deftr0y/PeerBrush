//! Native access-rule preservation; no token privileges or SACL authority.
use crate::large_document::ResidentBudget;
use std::fs::File;
pub(in crate::file_safety) fn supported(file: &File) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file
            .metadata()
            .map_err(|_| "Native access metadata unavailable")?
            .file_attributes()
            & 0x4000
            != 0
        {
            return Err("Native encrypted-file staging is unsupported; original retained".into());
        }
    }
    let _ = file;
    Ok(())
}
pub(in crate::file_safety) fn preserve(
    source: &File,
    destination: &File,
    budget: &ResidentBudget,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    supported(source)?;
    supported(destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _acl_admission = budget
            .reserve_bytes(256 * 1024, 256 * 1024)
            .map_err(|e| e.to_string())?;
        #[cfg(target_os = "linux")]
        let acl_before = linux_acl(source)?;
        #[cfg(target_os = "macos")]
        let acl_before = macos_acl(source)?;
        let before = source
            .metadata()
            .map_err(|_| "Native source access metadata unavailable")?;
        super::permissions::preserve_held(source, destination)?;
        let after = source
            .metadata()
            .map_err(|_| "Native source access metadata unavailable")?;
        let saved = destination
            .metadata()
            .map_err(|_| "Native destination access metadata unavailable")?;
        if (before.uid(), before.gid(), before.mode()) != (after.uid(), after.gid(), after.mode())
            || (after.uid(), after.gid(), after.mode()) != (saved.uid(), saved.gid(), saved.mode())
        {
            return Err("Native access rules changed; original retained".into());
        }
        #[cfg(target_os = "linux")]
        if acl_before != linux_acl(source)? || acl_before != linux_acl(destination)? {
            return Err("Native held ACL readback differs; original retained".into());
        }
        #[cfg(target_os = "macos")]
        if acl_before != macos_acl(source)? || acl_before != macos_acl(destination)? {
            return Err("Native held ACL readback differs; original retained".into());
        }
    }
    #[cfg(windows)]
    windows::preserve(source, destination, budget)?;
    check()
}
#[cfg(target_os = "linux")]
fn linux_acl(file: &File) -> Result<Option<Vec<u8>>, String> {
    use std::{ffi::c_void, os::fd::AsRawFd};
    extern "C" {
        fn fgetxattr(fd: i32, name: *const i8, value: *mut c_void, size: usize) -> isize;
    }
    let name = b"system.posix_acl_access\0";
    let size = unsafe {
        fgetxattr(
            file.as_raw_fd(),
            name.as_ptr().cast(),
            std::ptr::null_mut(),
            0,
        )
    };
    if size < 0 {
        let error = std::io::Error::last_os_error();
        return if matches!(error.raw_os_error(), Some(61 | 95)) {
            Ok(None)
        } else {
            Err("Native ACL observation failed".into())
        };
    }
    if size > 65536 {
        return Err("Native ACL exceeds bounded admission".into());
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size as usize)
        .map_err(|_| "Native ACL allocation refused")?;
    if bytes.capacity() > 65536 {
        return Err("Native ACL allocation exceeds admission".into());
    }
    bytes.resize(size as usize, 0);
    if unsafe {
        fgetxattr(
            file.as_raw_fd(),
            name.as_ptr().cast(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    } != size
    {
        return Err("Native ACL changed while observing it".into());
    }
    Ok(Some(bytes))
}
#[cfg(target_os = "macos")]
fn macos_acl(file: &File) -> Result<Vec<u8>, String> {
    use std::{ffi::c_void, os::fd::AsRawFd};
    extern "C" {
        fn acl_get_fd_np(fd: i32, kind: u32) -> *mut c_void;
        fn acl_init(entries: i32) -> *mut c_void;
        fn acl_size(acl: *mut c_void) -> isize;
        fn acl_copy_ext(bytes: *mut c_void, acl: *mut c_void, size: isize) -> isize;
        fn acl_free(acl: *mut c_void) -> i32;
    }
    struct Acl(*mut c_void);
    impl Drop for Acl {
        fn drop(&mut self) {
            unsafe {
                acl_free(self.0);
            }
        }
    }
    let mut acl = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) };
    if acl.is_null() {
        if std::io::Error::last_os_error().raw_os_error() != Some(2) {
            return Err("Native held ACL observation unavailable".into());
        }
        // Darwin reports ENOENT for no ACL. Compare its canonical empty ACL,
        // the same object copied by the existing held-access preservation.
        acl = unsafe { acl_init(0) };
    }
    if acl.is_null() {
        return Err("Native empty ACL allocation refused".into());
    }
    let acl = Acl(acl);
    let size = unsafe { acl_size(acl.0) };
    if !(0..=65536).contains(&size) {
        return Err("Native ACL exceeds bounded admission".into());
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size as usize)
        .map_err(|_| "Native ACL allocation refused")?;
    if bytes.capacity() > 65536 {
        return Err("Native ACL allocation exceeds admission".into());
    }
    bytes.resize(size as usize, 0);
    if unsafe { acl_copy_ext(bytes.as_mut_ptr().cast(), acl.0, size) } != size {
        return Err("Native ACL external representation unavailable".into());
    }
    Ok(bytes)
}
#[cfg(windows)]
mod windows {
    use super::*;
    use std::{ffi::c_void, os::windows::io::AsRawHandle, ptr};
    #[link(name = "advapi32")]
    extern "system" {
        fn GetSecurityInfo(
            handle: *mut c_void,
            kind: u32,
            info: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn SetSecurityInfo(
            handle: *mut c_void,
            kind: u32,
            info: u32,
            owner: *mut c_void,
            group: *mut c_void,
            dacl: *mut c_void,
            sacl: *mut c_void,
        ) -> u32;
        fn GetSecurityDescriptorControl(
            descriptor: *mut c_void,
            control: *mut u16,
            revision: *mut u32,
        ) -> i32;
        fn IsValidSid(sid: *mut c_void) -> i32;
        fn GetLengthSid(sid: *mut c_void) -> u32;
        fn IsValidAcl(acl: *mut c_void) -> i32;
        fn GetAce(acl: *mut c_void, index: u32, ace: *mut *mut c_void) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    struct Rules {
        descriptor: *mut c_void,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        protected: bool,
        present: bool,
    }
    impl Drop for Rules {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.descriptor);
            }
        }
    }
    impl Rules {
        fn read(file: &File) -> Result<Self, String> {
            let mut r = Self {
                descriptor: ptr::null_mut(),
                owner: ptr::null_mut(),
                group: ptr::null_mut(),
                dacl: ptr::null_mut(),
                protected: false,
                present: false,
            };
            // OWNER|GROUP|DACL only. SACL access would require an unapproved
            // privilege change and is deliberately neither requested nor claimed.
            if unsafe {
                GetSecurityInfo(
                    file.as_raw_handle(),
                    1,
                    7,
                    &mut r.owner,
                    &mut r.group,
                    &mut r.dacl,
                    ptr::null_mut(),
                    &mut r.descriptor,
                )
            } != 0
            {
                return Err(
                    "Cannot observe native held access rules; use a new Save As path".into(),
                );
            }
            let (mut control, mut revision) = (0u16, 0u32);
            if r.descriptor.is_null()
                || unsafe {
                    GetSecurityDescriptorControl(r.descriptor, &mut control, &mut revision)
                } == 0
            {
                return Err("Native access descriptor control unavailable".into());
            }
            r.protected = control & 0x1000 != 0;
            r.present = control & 4 != 0;
            if !r.present && !r.dacl.is_null() {
                return Err("Native DACL presence is contradictory".into());
            }
            Ok(r)
        }
        fn sid(&self, pointer: *mut c_void) -> Result<&[u8], String> {
            if pointer.is_null() || unsafe { IsValidSid(pointer) } == 0 {
                return Err("Native access SID invalid".into());
            }
            let n = unsafe { GetLengthSid(pointer) } as usize;
            if n == 0 || n > 68 {
                return Err("Native access SID exceeds its native bound".into());
            }
            // The caller keeps this Rules allocation alive throughout comparison.
            Ok(unsafe { std::slice::from_raw_parts(pointer.cast(), n) })
        }
        fn acl(&self) -> Result<Option<&[u8]>, String> {
            if self.dacl.is_null() {
                return Ok(None);
            }
            if unsafe { IsValidAcl(self.dacl) } == 0 {
                return Err("Native access ACL invalid".into());
            }
            let size = unsafe { ptr::read_unaligned(self.dacl.cast::<u8>().add(2).cast::<u16>()) }
                as usize;
            if size < 8 {
                return Err("Native access ACL header invalid".into());
            }
            Ok(Some(unsafe {
                std::slice::from_raw_parts(self.dacl.cast(), size)
            }))
        }
        fn acl_equal(&self, other: &Self, normalize_origin: bool) -> Result<bool, String> {
            if self.present != other.present {
                return Ok(false);
            }
            let (left, right) = (self.acl()?, other.acl()?);
            let (Some(left), Some(right)) = (left, right) else {
                return Ok(left.is_none() && right.is_none());
            };
            // ACL reserved bytes and unused allocation capacity are not access
            // rules. Compare revision, ACE count, and every exact active ACE.
            if left[0] != right[0] {
                return Err("Native DACL revision changed".into());
            }
            if left[4..6] != right[4..6] {
                return Err("Native DACL active ACE count changed".into());
            }
            let count = u16::from_le_bytes([left[4], left[5]]) as u32;
            for index in 0..count {
                let ace = |rules: &Self, bytes: &[u8]| -> Result<(*const u8, usize), String> {
                    let mut pointer = ptr::null_mut();
                    if unsafe { GetAce(rules.dacl, index, &mut pointer) } == 0 || pointer.is_null()
                    {
                        return Err("Native active ACE observation failed".into());
                    }
                    let offset = (pointer as usize)
                        .checked_sub(rules.dacl as usize)
                        .ok_or("Native ACE offset invalid")?;
                    if offset.checked_add(4).is_none_or(|n| n > bytes.len()) {
                        return Err("Native ACE header invalid".into());
                    }
                    let size = u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
                    if size < 4 || offset.checked_add(size).is_none_or(|n| n > bytes.len()) {
                        return Err("Native ACE span invalid".into());
                    }
                    Ok((pointer.cast(), size))
                };
                let (lp, ln) = ace(self, left)?;
                let (rp, rn) = ace(other, right)?;
                let (l, r) = unsafe {
                    (
                        std::slice::from_raw_parts(lp, ln),
                        std::slice::from_raw_parts(rp, rn),
                    )
                };
                if !active_ace_equal(l, r, normalize_origin) {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        fn owners_equal(&self, other: &Self) -> Result<bool, String> {
            Ok(self.sid(self.owner)? == other.sid(other.owner)?
                && self.sid(self.group)? == other.sid(other.group)?)
        }
        fn equal(&self, other: &Self) -> Result<bool, String> {
            Ok(self.owners_equal(other)?
                && self.protected == other.protected
                && self.acl_equal(other, false)?)
        }
    }
    fn active_ace_equal(left: &[u8], right: &[u8], normalize_origin: bool) -> bool {
        if left.len() != right.len()
            || left.len() < 4
            || left.len() % 4 != 0
            || u16::from_le_bytes([left[2], left[3]]) as usize != left.len()
            || u16::from_le_bytes([right[2], right[3]]) as usize != right.len()
        {
            return false;
        }
        let origin = if normalize_origin { 0x10 } else { 0 };
        left[0] == right[0]
            && (left[1] & !origin) == (right[1] & !origin)
            && left[2..] == right[2..]
    }
    #[cfg(test)]
    mod rule_tests {
        use super::*;
        fn ace() -> [u8; 16] {
            [0, 0x13, 16, 0, 0x8f, 0, 0x12, 0, 1, 2, 3, 4, 5, 6, 7, 8]
        }
        #[test]
        fn owner_group_acl_presence_and_order_changes_are_not_normalized() {
            // Native SID wire layouts: Everyone and LocalSystem. No account
            // lookup, token change or filesystem-security mutation is involved.
            let mut everyone = [0u32; 3];
            let mut system = [0u32; 3];
            let eb =
                unsafe { std::slice::from_raw_parts_mut(everyone.as_mut_ptr().cast::<u8>(), 12) };
            eb.copy_from_slice(&[1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]);
            let sb =
                unsafe { std::slice::from_raw_parts_mut(system.as_mut_ptr().cast::<u8>(), 12) };
            sb.copy_from_slice(&[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0]);
            let (e, y) = (everyone.as_mut_ptr().cast(), system.as_mut_ptr().cast());
            let rules = |owner, group, dacl, present| Rules {
                descriptor: ptr::null_mut(),
                owner,
                group,
                dacl,
                protected: true,
                present,
            };
            let original = rules(e, e, ptr::null_mut(), true);
            assert!(original
                .owners_equal(&rules(e, e, ptr::null_mut(), true))
                .unwrap());
            assert!(!original
                .owners_equal(&rules(y, e, ptr::null_mut(), true))
                .unwrap());
            assert!(!original
                .owners_equal(&rules(e, y, ptr::null_mut(), true))
                .unwrap());
            assert!(!original
                .acl_equal(&rules(e, e, ptr::null_mut(), false), true)
                .unwrap());
            let mut empty = [0x00080002u32, 0];
            assert!(!original
                .acl_equal(&rules(e, e, empty.as_mut_ptr().cast(), true), true)
                .unwrap());
            let a = ace();
            let mut b = a;
            b[4] ^= 1;
            assert!(![a, b]
                .iter()
                .zip([b, a].iter())
                .all(|(l, r)| active_ace_equal(l, r, true)));
        }
        #[test]
        fn inheritance_origin_normalization_preserves_all_access_bytes() {
            let original = ace();
            let mut explicit = original;
            explicit[1] &= !0x10;
            assert!(!active_ace_equal(&original, &explicit, false));
            assert!(active_ace_equal(&original, &explicit, true));
            for (byte, bit) in [
                (0, 1),
                (1, 1),
                (1, 2),
                (1, 4),
                (1, 8),
                (1, 0x20),
                (1, 0x40),
                (1, 0x80),
                (4, 1),
                (6, 1),
                (8, 1),
                (15, 1),
            ] {
                let mut changed = explicit;
                changed[byte] ^= bit;
                assert!(
                    !active_ace_equal(&original, &changed, true),
                    "changed active ACE byte {}",
                    byte
                );
            }
            let mut malformed = explicit;
            malformed[2] = 15;
            assert!(!active_ace_equal(&original, &malformed, true));
            assert!(!active_ace_equal(&[], &[], true));
        }
    }
    pub(super) fn preserve(
        source: &File,
        destination: &File,
        budget: &ResidentBudget,
    ) -> Result<(), String> {
        // Three bounded descriptors may coexist. ACL length is native u16 and
        // owner/group SIDs are each at most68bytes; all release before this ticket.
        let _admission = budget
            .reserve_bytes(256 * 1024, 256 * 1024)
            .map_err(|e| e.to_string())?;
        let before = Rules::read(source)?;
        let current = Rules::read(destination)?;
        if !before.owners_equal(&current)? {
            return Err(
                "Native owner/group cannot be preserved without privilege changes; use Save As"
                    .into(),
            );
        }
        drop(current);
        // Backups reside in a separate directory. Freeze the original DACL
        // against new inheritance there, matching the established backup rule.
        // This is ordinary access preservation, not full descriptor copying.
        let protection = 0x80000000;
        if unsafe {
            SetSecurityInfo(
                destination.as_raw_handle(),
                1,
                4 | protection,
                ptr::null_mut(),
                ptr::null_mut(),
                before.dacl,
                ptr::null_mut(),
            )
        } != 0
        {
            return Err("Cannot preserve native held access rules; original retained".into());
        }
        let after = Rules::read(source)?;
        let saved = Rules::read(destination)?;
        if !before.equal(&after)? {
            return Err("Native source access rules changed during preservation".into());
        }
        if !before.owners_equal(&saved)? {
            return Err("Native destination owner/group readback differs".into());
        }
        if !saved.protected {
            return Err("Native backup DACL protection readback differs".into());
        }
        if !before.acl_equal(&saved, true)? {
            return Err("Native destination DACL readback differs".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};
    struct Fixture {
        root: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
            let id = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = parent.join(format!("pb-native-access-{}-{}", std::process::id(), id));
            fs::create_dir(&root).unwrap();
            assert_eq!(fs::canonicalize(&root).unwrap(), root);
            Self { root }
        }
        fn output(&self) -> File {
            let mut options = fs::OpenOptions::new();
            options.create_new(true).read(true).write(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.access_mode(0xC0060000);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(self.root.join("output")).unwrap()
        }
        fn finish(self) {
            assert!(self
                .root
                .starts_with(fs::canonicalize(std::env::temp_dir()).unwrap()));
            assert_eq!(fs::canonicalize(&self.root).unwrap(), self.root);
            fs::remove_file(self.root.join("source")).unwrap();
            fs::remove_file(self.root.join("output")).unwrap();
            fs::remove_dir(self.root).unwrap();
        }
    }
    #[test]
    fn held_ordinary_access_rules_round_trip_before_payload_without_privilege_changes() {
        let fixture = Fixture::new();
        let path = fixture.root.join("source");
        fs::write(&path, b"original stays").unwrap();
        let source = File::open(&path).unwrap();
        let mut output = fixture.output();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        supported(&source).unwrap();
        preserve(&source, &output, &pool, &|| Ok(())).unwrap();
        assert_eq!(pool.used(), 0);
        assert_eq!(output.metadata().unwrap().len(), 0);
        output.write_all(b"private copied payload").unwrap();
        output.sync_all().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"original stays");
        drop(output);
        drop(source);
        fixture.finish();
    }
    #[test]
    fn cancelled_or_unadmitted_access_copy_keeps_destination_empty() {
        let fixture = Fixture::new();
        let path = fixture.root.join("source");
        fs::write(&path, b"original stays").unwrap();
        let source = File::open(&path).unwrap();
        let output = fixture.output();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        assert_eq!(
            preserve(&source, &output, &pool, &|| Err(
                "exact access cancel".into()
            ))
            .unwrap_err(),
            "exact access cancel"
        );
        assert!(
            preserve(&source, &output, &ResidentBudget::new(1).unwrap(), &|| Ok(
                ()
            ))
            .is_err()
        );
        assert_eq!(output.metadata().unwrap().len(), 0);
        assert_eq!(pool.used(), 0);
        assert_eq!(fs::read(path).unwrap(), b"original stays");
        drop(output);
        drop(source);
        fixture.finish();
    }
}
