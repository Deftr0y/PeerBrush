//! Credential-free native bridge probe. No start/check/install method is exposed.
use super::{require_supported_macos, Configuration};
use objc2::{
    class, define_class, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyObject, Bool},
    DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSProcessInfo, NSString};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet, ffi::c_void, fs, os::unix::fs::MetadataExt, path::Path, process::Command,
};

// CommonCrypto is supplied by libSystem, already part of macOS. No crypto crate
// or build-time tool is added just to hash these small pinned framework files.
#[link(name = "System")]
unsafe extern "C" {
    fn CC_SHA256(data: *const c_void, length: u32, digest: *mut u8) -> *mut u8;
}

fn sha256(bytes: &[u8]) -> Result<String, String> {
    let length =
        u32::try_from(bytes.len()).map_err(|_| "Sparkle file exceeds the hashing budget")?;
    let mut digest = [0_u8; 32];
    // Buffers remain live for this synchronous system digest function.
    unsafe {
        CC_SHA256(bytes.as_ptr().cast(), length, digest.as_mut_ptr());
    }
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Verify exact code/resources/license and native signatures. External output,
/// private staging permissions and receipt validation belong to prepare_sparkle.py.
pub fn verify_framework(root: &Path) -> Result<(), String> {
    if !root.is_absolute() {
        return Err("Sparkle preparation requires an absolute external framework path".into());
    }
    let pin: Value = serde_json::from_slice(include_bytes!(
        "../../packaging/sparkle/sparkle-2.10.0.json"
    ))
    .map_err(|_| "Invalid compiled Sparkle pin")?;
    for ancestor in root.ancestors() {
        if fs::symlink_metadata(ancestor)
            .map_err(|_| "Sparkle framework path is unavailable")?
            .file_type()
            .is_symlink()
        {
            return Err("Sparkle framework ancestors must not be symlinks".into());
        }
    }
    let expected = pin["framework_entries"]
        .as_object()
        .ok_or("Missing compiled Sparkle inventory")?;
    let notice = root
        .parent()
        .ok_or("Sparkle notice directory is unavailable")?
        .join("LICENSE");
    let notice_metadata =
        fs::symlink_metadata(&notice).map_err(|_| "Missing staged Sparkle license")?;
    if !notice_metadata.is_file()
        || notice_metadata.nlink() != 1
        || notice_metadata.len() > 64 * 1024
        || sha256(&fs::read(notice).map_err(|_| "Unreadable Sparkle notice")?)?
            != pin["license_sha256"]
                .as_str()
                .ok_or("Missing compiled Sparkle notice hash")?
    {
        return Err("Staged Sparkle license differs from the committed notice".into());
    }
    let mut visited = BTreeSet::new();
    fn visit(
        root: &Path,
        path: &Path,
        expected: &serde_json::Map<String, Value>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "Invalid Sparkle framework path")?;
        let name = if relative.as_os_str().is_empty() {
            "."
        } else {
            relative
                .to_str()
                .ok_or("Unexpected Sparkle filesystem name")?
        };
        let metadata = fs::symlink_metadata(path).map_err(|_| "Missing Sparkle framework entry")?;
        let actual = if metadata.file_type().is_symlink() {
            let target = fs::read_link(path).map_err(|_| "Unreadable Sparkle symlink")?;
            if !path
                .canonicalize()
                .map_err(|_| "Dangling Sparkle symlink")?
                .starts_with(root)
            {
                return Err("Sparkle symlink escapes its framework".into());
            }
            json!({"kind":"symlink", "target":target.to_str().ok_or("Invalid Sparkle symlink")?})
        } else if metadata.is_dir() {
            json!({"kind":"directory", "mode":metadata.mode() & 0o7777})
        } else if metadata.is_file() && metadata.nlink() == 1 && metadata.len() <= 16 * 1024 * 1024
        {
            let bytes = fs::read(path).map_err(|_| "Unreadable Sparkle code/resource")?;
            json!({"kind":"file", "mode":metadata.mode() & 0o7777, "bytes":bytes.len(), "sha256":sha256(&bytes)?})
        } else {
            return Err("Unexpected Sparkle entry type, hard link or file size".into());
        };
        if expected.get(name) != Some(&actual) || !visited.insert(name.into()) {
            return Err("Sparkle framework differs from its exact committed pin".into());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path).map_err(|_| "Unreadable Sparkle directory")? {
                visit(
                    root,
                    &entry
                        .map_err(|_| "Unreadable Sparkle directory entry")?
                        .path(),
                    expected,
                    visited,
                )?;
            }
        }
        Ok(())
    }
    visit(root, root, expected, &mut visited)?;
    if visited.len() != expected.len() {
        return Err("Incomplete Sparkle framework inventory".into());
    }
    // Verify each precise pinned code object, never a wildcard or --deep signing.
    // The upstream signatures are ad hoc: this establishes integrity of pinned
    // CI inputs, not our Developer ID or distribution/notarization acceptance.
    for object in [
        "Versions/B/XPCServices/Installer.xpc",
        "Versions/B/XPCServices/Downloader.xpc",
        "Versions/B/Autoupdate",
        "Versions/B/Updater.app",
        ".",
    ] {
        let result = Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict"])
            .arg(root.join(object))
            .output()
            .map_err(|_| "Native Sparkle signature verification could not start")?;
        if !result.status.success() {
            return Err("Native Sparkle signature integrity verification failed".into());
        }
    }
    Ok(())
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Retained<NSError>]
    struct PreparationDelegate;

    unsafe impl NSObjectProtocol for PreparationDelegate {}

    impl PreparationDelegate {
        #[unsafe(method(updater:mayPerformUpdateCheck:error:))]
        fn may_check(&self, _updater: &AnyObject, _kind: isize, error: *mut *mut NSError) -> Bool {
            if !error.is_null() {
                // Sparkle's NSError** is an autoreleasing out parameter; the
                // delegate owns the error for the controller's whole lifetime.
                unsafe { *error = Retained::as_ptr(self.ivars()).cast_mut(); }
            }
            Bool::NO
        }

        #[unsafe(method(updater:shouldProceedWithUpdate:updateCheck:error:))]
        fn may_proceed(&self, updater: &AnyObject, _item: &AnyObject, kind: isize, error: *mut *mut NSError) -> Bool {
            self.may_check(updater, kind, error)
        }

        #[unsafe(method(updaterShouldRelaunchApplication:))]
        fn may_relaunch(&self, _updater: &AnyObject) -> Bool { Bool::NO }

        #[unsafe(method(updaterShouldPromptForPermissionToCheckForUpdates:))]
        fn may_prompt(&self, _updater: &AnyObject) -> Bool { Bool::NO }
    }
);

/// Main-thread lifetime owner. The controller is dropped before its weak delegate.
pub struct Probe {
    controller: Retained<AnyObject>,
    delegate: Retained<PreparationDelegate>,
    _framework: Retained<NSBundle>,
}

impl Probe {
    /// Exercise real selectors while asserting all operations stay vetoed.
    pub fn assert_disabled(&self) {
        let updater: Retained<AnyObject> = unsafe { msg_send![&*self.controller, updater] };
        let ready: Bool = unsafe { msg_send![&*updater, canCheckForUpdates] };
        assert!(
            !ready.as_bool(),
            "The preparation controller must not offer checks"
        );
        let mut error: *mut NSError = std::ptr::null_mut();
        let allowed: Bool = unsafe {
            msg_send![&*self.delegate, updater: &*updater, mayPerformUpdateCheck: 0_isize, error: &mut error]
        };
        assert!(!allowed.as_bool() && !error.is_null());
        let item: Retained<AnyObject> =
            unsafe { msg_send![class!(SUAppcastItem), emptyAppcastItem] };
        error = std::ptr::null_mut();
        let proceed: Bool = unsafe {
            msg_send![&*self.delegate, updater: &*updater,
            shouldProceedWithUpdate: &*item, updateCheck: 0_isize, error: &mut error]
        };
        assert!(!proceed.as_bool() && !error.is_null());
        let relaunch: Bool =
            unsafe { msg_send![&*self.delegate, updaterShouldRelaunchApplication: &*updater] };
        assert!(!relaunch.as_bool());
        let prompt: Bool = unsafe {
            msg_send![&*self.delegate,
            updaterShouldPromptForPermissionToCheckForUpdates: &*updater]
        };
        assert!(!prompt.as_bool());
    }
}

/// Only used by the explicit native component example; the app never calls it.
pub fn probe(root: &Path, configuration: &Configuration) -> Result<Probe, String> {
    configuration.validate()?;
    let marker =
        MainThreadMarker::new().ok_or("Sparkle preparation requires the process main thread")?;
    require_supported_macos(
        NSProcessInfo::processInfo()
            .operatingSystemVersion()
            .majorVersion,
    )?;
    verify_framework(root)?;
    unsafe {
        let _: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
    }
    let path = NSString::from_str(
        root.to_str()
            .ok_or("Sparkle preparation needs a Unicode staging path")?,
    );
    let framework = NSBundle::bundleWithPath(&path).ok_or("Invalid Sparkle framework bundle")?;
    // All executable/resource bytes, symlink targets and native code signatures
    // above are verified before this explicit native component load.
    unsafe {
        framework
            .loadAndReturnError()
            .map_err(|_| "Verified Sparkle framework could not load")?;
    }
    let controller_class = framework
        .classNamed(&NSString::from_str("SPUStandardUpdaterController"))
        .ok_or("Pinned Sparkle standard controller is unavailable")?;
    let domain = NSString::from_str("com.peerbrush.SparklePreparation");
    let message = NSString::from_str("Sparkle updates remain disabled until feed, public key and native safety acceptance are configured and verified.");
    let key = NSString::from_str("NSLocalizedDescription");
    let info: Retained<AnyObject> =
        unsafe { msg_send![class!(NSDictionary), dictionaryWithObject: &*message, forKey: &*key] };
    let error: Retained<NSError> = unsafe {
        msg_send![class!(NSError), errorWithDomain: &*domain, code: 1_isize, userInfo: &*info]
    };
    let delegate = PreparationDelegate::alloc(marker).set_ivars(error);
    let delegate: Retained<PreparationDelegate> = unsafe { msg_send![super(delegate), init] };
    let allocated: Allocated<AnyObject> = unsafe { msg_send![controller_class, alloc] };
    let controller: Option<Retained<AnyObject>> = unsafe {
        msg_send![allocated,
        initWithStartingUpdater: Bool::NO, updaterDelegate: &*delegate,
        userDriverDelegate: std::ptr::null::<AnyObject>()]
    };
    Ok(Probe {
        controller: controller.ok_or("Sparkle preparation controller could not initialize")?,
        delegate,
        _framework: framework,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn system_sha256_matches_the_standard_vectors() {
        assert_eq!(
            super::sha256(b"").unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            super::sha256(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
