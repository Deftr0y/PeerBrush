//! Authenticate the kernel-reported inherited socket peer, never a claimed PID.
use super::*;
use objc2_foundation::NSData;
use std::{
    ffi::{CStr, OsStr},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, net::UnixStream},
    },
};

const FAILED: &str = "App update process identity could not be authenticated";
// Public Security.framework/CoreFoundation APIs; no private Sparkle IPC.
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecGuestAttributeAudit: *const c_void;
    static kSecCodeInfoUnique: *const c_void;
    fn SecCodeCopySelf(flags: u32, code: *mut *const c_void) -> i32;
    fn SecCodeCopyGuestWithAttributes(
        host: *const c_void,
        attributes: *const c_void,
        flags: u32,
        code: *mut *const c_void,
    ) -> i32;
    fn SecCodeCheckValidity(code: *const c_void, flags: u32, requirement: *const c_void) -> i32;
    fn SecRequirementCreateWithString(
        value: *const c_void,
        flags: u32,
        requirement: *mut *const c_void,
    ) -> i32;
    fn SecCodeCopySigningInformation(
        code: *const c_void,
        flags: u32,
        information: *mut *const c_void,
    ) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
}
struct OwnedCF(*const c_void);
impl OwnedCF {
    fn returned(status: i32, value: *const c_void) -> Result<Self, String> {
        if status != 0 || value.is_null() {
            if !value.is_null() {
                unsafe {
                    CFRelease(value);
                }
            }
            return Err(FAILED.into());
        }
        Ok(Self(value))
    }
    fn valid(&self, requirement: &Self) -> Result<(), String> {
        // kSecCSStrictValidate; network access is not opted into.
        if unsafe { SecCodeCheckValidity(self.0, 1 << 4, requirement.0) } != 0 {
            return Err(FAILED.into());
        }
        Ok(())
    }
    fn unique(&self) -> Result<Vec<u8>, String> {
        let mut raw = std::ptr::null();
        let status = unsafe { SecCodeCopySigningInformation(self.0, 0, &mut raw) };
        let info = Self::returned(status, raw)?;
        let data = unsafe { CFDictionaryGetValue(info.0, kSecCodeInfoUnique) };
        if data.is_null() || unsafe { CFGetTypeID(data) != CFDataGetTypeID() } {
            return Err(FAILED.into());
        }
        let length = unsafe { CFDataGetLength(data) };
        if !(20..=64).contains(&length) {
            return Err(FAILED.into());
        }
        let bytes = unsafe { CFDataGetBytePtr(data) };
        if bytes.is_null() {
            return Err(FAILED.into());
        }
        Ok(unsafe { std::slice::from_raw_parts(bytes, length as usize) }.to_vec())
    }
}
impl Drop for OwnedCF {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.0);
        }
    }
}

fn peer_audit(socket: &UnixStream, expected: u32) -> Result<[u32; 8], String> {
    let mut token = [0_u32; 8];
    let mut size = std::mem::size_of_val(&token) as libc::socklen_t;
    let status = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            token.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0
        || size as usize != std::mem::size_of_val(&token)
        || expected <= 1
        || token[5] != expected
        || token[1] != unsafe { libc::geteuid() }
        || token[1] == 0
    {
        return Err(FAILED.into());
    }
    Ok(token)
}

pub(super) fn authenticate(socket: &UnixStream, expected: u32, team: &str) -> Result<(), String> {
    if team.len() != 10
        || !team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err(FAILED.into());
    }
    let audit = peer_audit(socket, expected)?;
    objc2::rc::autoreleasepool(|_| {
        let expression = NSString::from_str(&format!("identifier \"com.peerbrush.PeerBrush\" and anchor apple generic and certificate leaf[subject.OU] = \"{team}\" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"));
        let mut raw = std::ptr::null();
        let status = unsafe {
            SecRequirementCreateWithString((&*expression as *const NSString).cast(), 0, &mut raw)
        };
        let requirement = OwnedCF::returned(status, raw)?;
        raw = std::ptr::null();
        let status = unsafe { SecCodeCopySelf(0, &mut raw) };
        let this = OwnedCF::returned(status, raw)?;
        this.valid(&requirement)?;
        let bytes = NSData::with_bytes(bytemuck::cast_slice(&audit));
        let key = unsafe { &*kSecGuestAttributeAudit.cast::<AnyObject>() };
        let attributes: Retained<AnyObject> =
            unsafe { msg_send![class!(NSDictionary), dictionaryWithObject: &*bytes, forKey: key] };
        raw = std::ptr::null();
        let status = unsafe {
            SecCodeCopyGuestWithAttributes(
                std::ptr::null(),
                (&*attributes as *const AnyObject).cast(),
                0,
                &mut raw,
            )
        };
        let peer = OwnedCF::returned(status, raw)?;
        peer.valid(&requirement)?;
        if peer.unique()? != this.unique()? {
            return Err(FAILED.into());
        }
        this.valid(&requirement)?;
        peer.valid(&requirement)?;
        // The kernel token binds the generation, while this final query also
        // refuses an endpoint subsequently used by a different process.
        if peer_audit(socket, expected)? != audit {
            return Err(FAILED.into());
        }
        Ok(())
    })
}

pub(super) fn executable(pid: u32) -> Result<PathBuf, String> {
    let pid = i32::try_from(pid).map_err(|_| FAILED)?;
    if pid <= 1 {
        return Err(FAILED.into());
    }
    let mut bytes = vec![0_u8; 4096];
    let count = unsafe { libc::proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
    if count <= 0 || count as usize >= bytes.len() {
        return Err(FAILED.into());
    }
    let path = CStr::from_bytes_until_nul(&bytes).map_err(|_| FAILED)?;
    let path = PathBuf::from(OsStr::from_bytes(path.to_bytes()));
    if !path.is_absolute() {
        return Err(FAILED.into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn kernel_socket_peer_binds_actual_pid_uid_and_refuses_foreign_claim() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        sender.write_all(b"audit fixture").unwrap();
        let actual = peer_audit(&receiver, std::process::id());
        if unsafe { libc::geteuid() } == 0 {
            assert!(actual.is_err());
        } else {
            let token = actual.unwrap();
            assert_eq!(token[5], std::process::id());
            assert_eq!(token[1], unsafe { libc::geteuid() });
        }
        assert!(peer_audit(&receiver, std::process::id().saturating_add(1)).is_err());
        assert!(peer_audit(&receiver, 1).is_err());
    }
    #[test]
    fn incomplete_publisher_policy_never_enters_signature_authority() {
        let (_sender, receiver) = UnixStream::pair().unwrap();
        for team in ["", "unreviewed", "ABCD12345!", "ABCD1234567"] {
            assert!(authenticate(&receiver, std::process::id(), team).is_err());
        }
    }
}
