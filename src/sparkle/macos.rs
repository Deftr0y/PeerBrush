//! Owned manual information checks and the credential-free native component probe.
use super::{require_supported_macos, Configuration};
pub(crate) mod checkpoint;
mod coordinator;
mod process_identity;
#[doc(hidden)]
pub use coordinator::main_entry as coordinator_main_entry;
mod driver;
mod installed;
use block2::{Block, RcBlock};
use objc2::{
    class, define_class, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyObject, Bool},
    DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSProcessInfo, NSString};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::{
    collections::BTreeSet, ffi::c_void, fs, os::unix::fs::MetadataExt, path::Path, process::Command,
};
use std::{path::PathBuf, rc::Rc, sync::mpsc};

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
    verify_framework_with_notice(
        root,
        &root
            .parent()
            .ok_or("Sparkle notice directory is unavailable")?
            .join("LICENSE"),
    )
}

fn verify_framework_with_notice(root: &Path, notice: &Path) -> Result<(), String> {
    verify_inventory(root, notice, false)?;
    verify_upstream_signatures(root)
}

fn verify_inventory(root: &Path, notice: &Path, signed: bool) -> Result<(), String> {
    if !root.is_absolute() {
        return Err("Sparkle preparation requires an absolute external framework path".into());
    }
    let pin: Value = serde_json::from_slice(include_bytes!(
        "../../packaging/sparkle/sparkle-2.10.0.json"
    ))
    .map_err(|_| "Invalid compiled Sparkle pin")?;
    for ancestor in root.ancestors().chain(notice.ancestors()) {
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
    let notice_metadata =
        fs::symlink_metadata(&notice).map_err(|_| "Missing staged Sparkle license")?;
    if !notice_metadata.is_file()
        || notice_metadata.nlink() != 1
        || notice_metadata.len() > 64 * 1024
        || sha256(&installed::bounded(notice, 64 * 1024)?)?
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
        signed: bool,
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
            if metadata.nlink() != 1 || metadata.mode() & 0o7777 != 0o777 {
                return Err("Sparkle links require exact modes and one inode link".into());
            }
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
            let bytes = installed::bounded(path, 16 * 1024 * 1024)?;
            json!({"kind":"file", "mode":metadata.mode() & 0o7777, "bytes":bytes.len(), "sha256":sha256(&bytes)?})
        } else {
            return Err("Unexpected Sparkle entry type, hard link or file size".into());
        };
        let transformed = signed
            && (crate::sparkle::check::OBJECTS
                .iter()
                .any(|(_, code)| *code == name)
                || crate::sparkle::check::SEALS.contains(&name));
        let matches = if transformed {
            expected.get(name).is_some_and(|entry| {
                entry["kind"] == "file"
                    && actual["kind"] == "file"
                    && actual["mode"] == entry["mode"]
            })
        } else {
            expected.get(name) == Some(&actual)
        };
        if !matches || !visited.insert(name.into()) {
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
                    signed,
                )?;
            }
        }
        Ok(())
    }
    visit(root, root, expected, &mut visited, signed)?;
    if visited.len() != expected.len() {
        return Err("Incomplete Sparkle framework inventory".into());
    }
    Ok(())
}

fn verify_upstream_signatures(root: &Path) -> Result<(), String> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationKind {
    Relaunch,
    InstallOnQuit,
}

struct PendingCycle {
    updater: Retained<AnyObject>,
    item: Retained<AnyObject>,
    relaunch: Option<RcBlock<dyn Fn()>>,
    install_on_quit: Option<RcBlock<dyn Fn()>>,
}

impl PendingCycle {
    fn matches(&self, updater: &AnyObject, item: &AnyObject) -> bool {
        Retained::as_ptr(&self.updater) == updater as *const AnyObject
            && Retained::as_ptr(&self.item) == item as *const AnyObject
    }

    fn slot(&mut self, kind: ContinuationKind) -> &mut Option<RcBlock<dyn Fn()>> {
        match kind {
            ContinuationKind::Relaunch => &mut self.relaunch,
            ContinuationKind::InstallOnQuit => &mut self.install_on_quit,
        }
    }

    fn has(&self, kind: ContinuationKind) -> bool {
        match kind {
            ContinuationKind::Relaunch => self.relaunch.is_some(),
            ContinuationKind::InstallOnQuit => self.install_on_quit.is_some(),
        }
    }
}

struct DelegateState {
    checks: Rc<driver::State>,
    error: Retained<NSError>,
    pending: RefCell<Option<PendingCycle>>,
    revision: Cell<u64>,
    invalidated: Cell<bool>,
    termination_inhibited: Cell<bool>,
    message: Cell<Option<&'static str>>,
}

impl DelegateState {
    fn changed(&self) {
        if let Some(revision) = self.revision.get().checked_add(1) {
            self.revision.set(revision);
        } else {
            self.invalidated.set(true);
            self.message.set(Some("The update callback cannot be reviewed safely. Automatic installation remains disabled."));
        }
    }

    fn postpone(
        &self,
        updater: &AnyObject,
        item: &AnyObject,
        kind: ContinuationKind,
        block: &Block<dyn Fn()>,
    ) {
        self.termination_inhibited.set(true);
        self.changed();
        // Each native kind owns its own copied block. Reissued callbacks replace
        // only that kind and invalidate any in-progress native callback review.
        let Some(block) = (unsafe { RcBlock::copy((block as *const Block<dyn Fn()>).cast_mut()) })
        else {
            self.invalidate("The update callback could not be retained. Automatic installation remains disabled.");
            return;
        };
        // Retain before borrowing state: native ownership operations may reenter.
        let Some(retained_updater) =
            (unsafe { Retained::retain((updater as *const AnyObject).cast_mut()) })
        else {
            self.invalidate(
                "The update owner could not be retained. Automatic installation remains disabled.",
            );
            return;
        };
        let Some(retained_item) =
            (unsafe { Retained::retain((item as *const AnyObject).cast_mut()) })
        else {
            self.invalidate(
                "The update item could not be retained. Automatic installation remains disabled.",
            );
            return;
        };
        let mut stale = None;
        let mut pending = self.pending.borrow_mut();
        if pending
            .as_ref()
            .is_some_and(|cycle| !cycle.matches(updater, item))
        {
            stale = pending.take();
            self.invalidated.set(true);
            self.message.set(Some(
                "The pending update changed. Automatic installation remains disabled.",
            ));
        }
        if pending.is_none() {
            // Retaining the identities prevents pointer reuse from reassigning
            // a handler to another updater/item while a callback is postponed.
            *pending = Some(PendingCycle {
                updater: retained_updater,
                item: retained_item,
                relaunch: None,
                install_on_quit: None,
            });
        }
        let previous = pending.as_mut().unwrap().slot(kind).replace(block);
        drop(pending);
        // Native release/deallocation may reenter a delegate. Do it only after
        // publishing the replacement and releasing the RefCell borrow.
        drop(previous);
        drop(stale);
    }

    fn invalidate(&self, message: &'static str) {
        self.changed();
        // Dropping a block never invokes it. Keep the termination hold after
        // an observed install callback: cycle completion alone does not prove
        // that Sparkle has no install-on-quit work scheduled.
        let stale = self.pending.borrow_mut().take();
        self.invalidated.set(true);
        self.message.set(Some(message));
        drop(stale);
    }

    fn defer(&self) {
        self.changed();
        if self.termination_inhibited.get() {
            self.message.set(Some(
                "Update postponed. Automatic installation remains disabled.",
            ));
        }
    }

    fn ready_kind(&self, kind: ContinuationKind) -> Result<(), String> {
        if self.invalidated.get() {
            return Err(self
                .message
                .get()
                .unwrap_or("The pending update was invalidated.")
                .into());
        }
        if !self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|cycle| cycle.has(kind))
        {
            return Err("No matching owned update continuation is pending".into());
        }
        Ok(())
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateState]
    struct PreparationDelegate;

    unsafe impl NSObjectProtocol for PreparationDelegate {}

    impl PreparationDelegate {
        #[unsafe(method(updater:mayPerformUpdateCheck:error:))]
        fn may_check(&self, updater: &AnyObject, kind: isize, error: *mut *mut NSError) -> Bool {
            if self.ivars().checks.may_check(updater, kind) { return Bool::YES; }
            if !error.is_null() {
                // Sparkle's NSError** is an autoreleasing out parameter; the
                // delegate owns the error for the controller's whole lifetime.
                unsafe { *error = Retained::as_ptr(&self.ivars().error).cast_mut(); }
            }
            Bool::NO
        }

        #[unsafe(method(updater:shouldProceedWithUpdate:updateCheck:error:))]
        fn may_proceed(&self, updater: &AnyObject, item: &AnyObject, kind: isize, error: *mut *mut NSError) -> Bool {
            if self.ivars().checks.may_proceed(updater, item, kind) { return Bool::YES; }
            if !error.is_null() {
                unsafe { *error = Retained::as_ptr(&self.ivars().error).cast_mut(); }
            }
            Bool::NO
        }

        #[unsafe(method(updater:shouldPostponeRelaunchForUpdate:untilInvokingBlock:))]
        fn postpone_relaunch(&self, updater: &AnyObject, item: &AnyObject, block: &Block<dyn Fn()>) -> Bool {
            self.ivars().postpone(updater, item, ContinuationKind::Relaunch, block);
            Bool::YES
        }

        #[unsafe(method(updater:willInstallUpdateOnQuit:immediateInstallationBlock:))]
        fn postpone_quit(&self, updater: &AnyObject, item: &AnyObject, block: &Block<dyn Fn()>) -> Bool {
            self.ivars().postpone(updater, item, ContinuationKind::InstallOnQuit, block);
            Bool::YES
        }

        #[unsafe(method(updater:didAbortWithError:))]
        fn aborted(&self, updater: &AnyObject, error: &NSError) {
            let checks=&self.ivars().checks;
            if checks.has_owner() && !checks.owns(updater) { return; }
            // The user driver's no-update acknowledgement causes Sparkle to
            // abort with SUNoUpdateError before its normal finish callback.
            let no_update=checks.owns(updater)
                && checks.view().phase==crate::sparkle::check::Phase::NoUpdate
                && driver::no_update(error);
            if checks.owns(updater) && !no_update { checks.failed(); }
            self.ivars().invalidate(if no_update { "No compatible signed app update was found." }
                else { "The update was interrupted. Automatic installation remains disabled." });
        }

        #[unsafe(method(updater:didFinishUpdateCycleForUpdateCheck:error:))]
        fn finished(&self, updater: &AnyObject, _kind: isize, _error: Option<&NSError>) {
            if self.ivars().checks.has_owner() && !self.ivars().checks.owns(updater) { return; }
            self.ivars().checks.finished();
            self.ivars().invalidate("The update cycle ended. Automatic installation remains disabled.");
        }

        #[unsafe(method(updater:shouldDownloadReleaseNotesForUpdate:))]
        fn download_notes(&self, _updater: &AnyObject, _item: &AnyObject) -> Bool { Bool::NO }

        #[unsafe(method_id(allowedSystemProfileKeysForUpdater:))]
        fn profile_keys(&self, _updater: &AnyObject) -> Retained<AnyObject> { unsafe { msg_send![class!(NSArray), array] } }

        #[unsafe(method_id(feedURLStringForUpdater:))]
        fn feed_url(&self, updater: &AnyObject) -> Option<Retained<NSString>> {
            self.ivars().checks.owns(updater).then(|| NSString::from_str(crate::sparkle::PROPOSED_FEED_URL))
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
    _driver: Retained<driver::CheckDriver>,
    _framework: Retained<NSBundle>,
    _host_bundle: Retained<NSBundle>,
    _application_bundle: Retained<NSBundle>,
}

impl Probe {
    /// Credential-free native component assertion. No continuation is invoked
    /// and no document closes. The application owner type holds Probe
    /// here; this is not a configured application update cycle.
    pub fn assert_application_guards(self) {
        let checks = self.delegate.ivars().checks.clone();
        let mut application = Application {
            controller: Some(self),
            message: "Prepared updates remain disabled",
            configuration: None,
            verification: None,
            profile: None,
            checkpoint: None,
            checks,
        };
        for depth in [8, 16] {
            let mut engine = crate::engine::Engine::new();
            engine.doc = crate::engine::Document::new_depth(2, 2, depth).unwrap();
            if depth == 16 {
                engine.doc.layers[0]
                    .pixels
                    .set16(0, 0, [1001, 3003, 5007, 65535]);
            } else {
                engine.doc.layers[0].pixels.set(0, 0, [17, 93, 207, 255]);
            }
            let shared = std::sync::Arc::new(std::sync::Mutex::new(engine));
            let workspace = crate::workspace::attach(&shared);
            let calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let counter = calls.clone();
            let block = RcBlock::new(move || counter.set(counter.get() + 1));
            {
                let owner = application.controller.as_ref().unwrap();
                let updater: Retained<AnyObject> = owner.controller.clone();
                let item: Retained<AnyObject> =
                    unsafe { msg_send![class!(SUAppcastItem), emptyAppcastItem] };
                let postponed: Bool = unsafe {
                    msg_send![&*owner.delegate, updater: &*updater,
                    willInstallUpdateOnQuit: &*item, immediateInstallationBlock: &*block]
                };
                assert!(postponed.as_bool());
            }
            assert!(application.pending_installation());
            assert!(application.retire_profile_capture().is_err());
            assert!(application.pending_installation());
            assert!(application.controller.is_some());
            assert!(application
                .controller
                .as_ref()
                .unwrap()
                .delegate
                .ivars()
                .ready_kind(ContinuationKind::Relaunch)
                .is_err());
            for dirty in [false, true] {
                if dirty {
                    shared.lock().unwrap().doc.revision += 1;
                }
                let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
                let undo = shared.lock().unwrap().undo.len();
                for blocked in [true, false, false] {
                    assert!(application
                        .request_installation(&workspace, blocked, ContinuationKind::InstallOnQuit)
                        .is_err());
                    let engine = shared.lock().unwrap();
                    assert!(!engine.closed);
                    assert_eq!(engine.doc.bit_depth, depth);
                    assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
                    assert_eq!(engine.undo.len(), undo);
                    assert_eq!(calls.get(), 0);
                }
            }
            {
                let owner = application.controller.as_ref().unwrap();
                let updater: Retained<AnyObject> = owner.controller.clone();
                let error = Retained::as_ptr(&owner.delegate.ivars().error);
                unsafe {
                    let _: () =
                        msg_send![&*owner.delegate, updater: &*updater, didAbortWithError: &*error];
                }
            }
            let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
            assert!(
                application.pending_installation(),
                "Aborted install callbacks must keep termination inhibited"
            );
            assert!(application
                .request_installation(&workspace, false, ContinuationKind::InstallOnQuit)
                .is_err());
            assert!(!shared.lock().unwrap().closed);
            assert_eq!(
                serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
                before
            );
            assert_eq!(calls.get(), 0);
            application
                .controller
                .as_ref()
                .unwrap()
                .clear_unstarted_component_callbacks();
            assert!(!application.pending_installation());
            assert_eq!(calls.get(), 0);
        }
    }

    /// Exercise real selectors while asserting all operations stay vetoed.
    pub fn assert_disabled(&self) {
        driver::assert_guarded(MainThreadMarker::new().expect("Main-thread component probe"));
        self.assert_no_update_sequence();
        self.assert_native_start_guard();
        self.assert_external_bundle_constructor();
        coordinator::assert_native_termination_refusal(
            MainThreadMarker::new().expect("Main-thread component probe"),
        );
        let updater: Retained<AnyObject> = self.controller.clone();
        let profiles: Retained<AnyObject> = objc2::rc::autoreleasepool(|_| unsafe {
            msg_send![&*self.delegate, allowedSystemProfileKeysForUpdater: &*updater]
        });
        let count: usize = unsafe { msg_send![&*profiles, count] };
        assert_eq!(
            count, 0,
            "The retained result remains valid after its return pool drains"
        );
        let absent_feed: Option<Retained<NSString>> =
            unsafe { msg_send![&*self.delegate, feedURLStringForUpdater: &*updater] };
        assert!(
            absent_feed.is_none(),
            "An unbound updater has no feed override"
        );
        let actual_delegate: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*updater, delegate] };
        assert_eq!(
            actual_delegate
                .as_ref()
                .map(|value| Retained::as_ptr(value)),
            Some(Retained::as_ptr(&self.delegate).cast()),
            "Native controller must retain the selected weak delegate identity through its owner"
        );
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
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = calls.clone();
        let relaunch_block = RcBlock::new(move || counter.set(counter.get() + 1));
        let quit_calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = quit_calls.clone();
        let block = RcBlock::new(move || counter.set(counter.get() + 1));
        for _ in 0..2 {
            let postponed: Bool = unsafe {
                msg_send![&*self.delegate, updater: &*updater,
                shouldPostponeRelaunchForUpdate: &*item, untilInvokingBlock: &*relaunch_block]
            };
            let quitting: Bool = unsafe {
                msg_send![&*self.delegate, updater: &*updater,
                willInstallUpdateOnQuit: &*item, immediateInstallationBlock: &*block]
            };
            assert!(postponed.as_bool() && quitting.as_bool());
        }
        let state = self.delegate.ivars();
        {
            let pending = state.pending.borrow();
            let cycle = pending.as_ref().unwrap();
            assert!(cycle.has(ContinuationKind::Relaunch));
            assert!(cycle.has(ContinuationKind::InstallOnQuit));
            assert!(!std::ptr::eq(
                &**cycle.relaunch.as_ref().unwrap(),
                &**cycle.install_on_quit.as_ref().unwrap()
            ));
        }
        let counter = calls.clone();
        let replacement = RcBlock::new(move || counter.set(counter.get() + 1));
        let before_replacement = state.revision.get();
        let held: Bool = unsafe {
            msg_send![&*self.delegate, updater: &*updater,
            shouldPostponeRelaunchForUpdate: &*item, untilInvokingBlock: &*replacement]
        };
        assert!(held.as_bool());
        assert_ne!(state.revision.get(), before_replacement);
        {
            let pending = state.pending.borrow();
            let cycle = pending.as_ref().unwrap();
            assert!(std::ptr::eq(
                &**cycle.relaunch.as_ref().unwrap(),
                &*replacement
            ));
            assert!(std::ptr::eq(
                &**cycle.install_on_quit.as_ref().unwrap(),
                &*block
            ));
        }
        // The pinned emptyAppcastItem factory is a singleton. Match its alloc/
        // init construction to obtain a distinct empty item for identity refusal.
        let allocated: Allocated<AnyObject> = unsafe { msg_send![class!(SUAppcastItem), alloc] };
        let other_item: Retained<AnyObject> = unsafe { msg_send![allocated, init] };
        assert_ne!(Retained::as_ptr(&item), Retained::as_ptr(&other_item));
        let changed: Bool = unsafe {
            msg_send![&*self.delegate, updater: &*updater,
            willInstallUpdateOnQuit: &*other_item, immediateInstallationBlock: &*block]
        };
        assert!(changed.as_bool());
        assert!(state.invalidated.get());
        assert!(state.ready_kind(ContinuationKind::InstallOnQuit).is_err());
        assert_eq!(quit_calls.get(), 0);
        let revision = state.revision.get();
        state.defer();
        assert_ne!(state.revision.get(), revision);
        assert!(state.termination_inhibited.get());
        assert_eq!(calls.get(), 0);
        let error = Retained::as_ptr(&state.error);
        unsafe {
            let _: () = msg_send![&*self.delegate, updater: &*updater, didAbortWithError: &*error];
        }
        assert!(state.pending.borrow().is_none());
        assert!(state.invalidated.get() && state.termination_inhibited.get());
        assert!(state.ready_kind(ContinuationKind::InstallOnQuit).is_err());
        assert_eq!(calls.get(), 0);
        // A late callback after abort remains quarantined, never silently adopted.
        let held: Bool = unsafe {
            msg_send![&*self.delegate, updater: &*updater,
            willInstallUpdateOnQuit: &*item, immediateInstallationBlock: &*block]
        };
        assert!(held.as_bool());
        assert!(state.ready_kind(ContinuationKind::InstallOnQuit).is_err());
        unsafe {
            let _: () = msg_send![&*self.delegate, updater: &*updater,
            didFinishUpdateCycleForUpdateCheck: 0_isize, error: std::ptr::null::<NSError>()];
        }
        assert!(state.pending.borrow().is_none());
        assert!(state.termination_inhibited.get());
        assert_eq!(calls.get(), 0);
        assert_eq!(quit_calls.get(), 0);
        self.clear_unstarted_component_callbacks();
    }

    fn assert_no_update_sequence(&self) {
        use crate::sparkle::check::Phase;
        let framework = PathBuf::from(self._framework.bundlePath().to_string());
        for (shown_domain, abort_domain, abort_code, cancelled, expected) in [
            (
                "SUSparkleErrorDomain",
                "SUSparkleErrorDomain",
                1001_isize,
                false,
                Phase::NoUpdate,
            ),
            (
                "SUSparkleErrorDomain",
                "OtherErrorDomain",
                1001,
                false,
                Phase::Failed,
            ),
            (
                "SUSparkleErrorDomain",
                "SUSparkleErrorDomain",
                1002,
                false,
                Phase::Failed,
            ),
            (
                "OtherErrorDomain",
                "SUSparkleErrorDomain",
                1001,
                false,
                Phase::Failed,
            ),
            (
                "SUSparkleErrorDomain",
                "SUSparkleErrorDomain",
                1001,
                true,
                Phase::Cancelled,
            ),
        ] {
            let marker = MainThreadMarker::new().expect("Main-thread component probe");
            let checks = Rc::new(driver::State::default());
            let owner = construct_with_state(&framework, marker, checks.clone()).unwrap();
            let error = |domain: &str, code: isize| -> Retained<NSError> {
                unsafe {
                    msg_send![class!(NSError), errorWithDomain: &*NSString::from_str(domain), code: code, userInfo: std::ptr::null::<AnyObject>()]
                }
            };
            let shown = error(shown_domain, 1001);
            let abort = error(abort_domain, abort_code);
            checks.session.borrow_mut().begin().unwrap();
            checks.checking(&owner.controller);
            let feed: Retained<NSString> = objc2::rc::autoreleasepool(|_| {
                let value: Option<Retained<NSString>> = unsafe {
                    msg_send![&*owner.delegate, feedURLStringForUpdater: &*owner.controller]
                };
                value.expect("The exact bound updater receives its source-defined feed override")
            });
            assert_eq!(feed.to_string(), crate::sparkle::PROPOSED_FEED_URL);
            let foreign_feed: Option<Retained<NSString>> =
                unsafe { msg_send![&*owner.delegate, feedURLStringForUpdater: &*self.controller] };
            assert!(foreign_feed.is_none());
            // Another updater cannot abort/finish this owned manual cycle.
            unsafe {
                let _: () = msg_send![&*owner.delegate, updater: &*self.controller, didAbortWithError: &*abort];
                let _: () = msg_send![&*owner.delegate, updater: &*self.controller, didFinishUpdateCycleForUpdateCheck: 0_isize, error: &*abort];
            }
            assert_eq!(checks.view().phase, Phase::Checking);
            assert!(checks.view().busy);
            if cancelled {
                checks.cancel();
            }
            let calls = Rc::new(Cell::new(0));
            let count = calls.clone();
            let delegate = owner.delegate.clone();
            let updater = owner.controller.clone();
            let acknowledgement = RcBlock::new(move || {
                count.set(count.get() + 1);
                // Reproduce the real user-driver acknowledgement -> abort ->
                // finish selector order without starting any native updater.
                unsafe {
                    let _: () =
                        msg_send![&*delegate, updater: &*updater, didAbortWithError: &*abort];
                    let _: () = msg_send![&*delegate, updater: &*updater, didFinishUpdateCycleForUpdateCheck: 0_isize, error: &*abort];
                }
            });
            unsafe {
                let _: () = msg_send![&*owner._driver, showUpdateNotFoundWithError: &*shown, acknowledgement: &*acknowledgement];
            }
            assert_eq!(calls.get(), 1);
            assert_eq!(checks.view().phase, expected);
            assert!(!checks.view().busy);
            let ready: Bool = unsafe { msg_send![&*owner.controller, canCheckForUpdates] };
            assert!(!ready.as_bool());
        }
    }

    /// ABI/component-only: fake unsigned target metadata proves argument routing,
    /// never publisher validation, installed application roles or update startup.
    fn assert_external_bundle_constructor(&self) {
        let marker = MainThreadMarker::new().expect("Main-thread component probe");
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("peerbrush-owner-bundles-{}", uuid::Uuid::new_v4(),));
        let target = base.join("Target.app");
        std::fs::create_dir_all(target.join("Contents")).unwrap();
        std::fs::write(target.join("Contents/Info.plist"), br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.peerbrush.OwnerBundleFixture</string><key>CFBundleVersion</key><string>0.30.0</string><key>CFBundleShortVersionString</key><string>0.30.0</string><key>SUEnableAutomaticChecks</key><false/><key>SUAutomaticallyUpdate</key><false/></dict></plist>"#).unwrap();
        let host = NSBundle::bundleWithPath(&NSString::from_str(target.to_str().unwrap())).unwrap();
        let copied_owner = base.join("CopiedOwner.app");
        std::fs::create_dir_all(copied_owner.join("Contents")).unwrap();
        std::fs::copy(
            target.join("Contents/Info.plist"),
            copied_owner.join("Contents/Info.plist"),
        )
        .unwrap();
        let copied_bundle =
            NSBundle::bundleWithPath(&NSString::from_str(copied_owner.to_str().unwrap())).unwrap();
        assert_eq!(copied_bundle.bundleIdentifier(), host.bundleIdentifier());
        let same: Bool = unsafe { msg_send![&*host, isEqualTo: &*copied_bundle] };
        assert!(
            !same.as_bool(),
            "Distinct bundle locations must keep external-owner roles even with equal identifiers"
        );
        let framework = PathBuf::from(self._framework.bundlePath().to_string());
        let owner = construct_with_bundles(
            &framework,
            marker,
            Rc::new(driver::State::default()),
            host.clone(),
            host,
        )
        .unwrap();
        let actual: Retained<NSBundle> = unsafe { msg_send![&*owner.controller, hostBundle] };
        assert_eq!(
            PathBuf::from(actual.bundlePath().to_string())
                .canonicalize()
                .unwrap(),
            target
        );
        assert_eq!(
            PathBuf::from(owner._host_bundle.bundlePath().to_string())
                .canonicalize()
                .unwrap(),
            target
        );
        assert_eq!(
            PathBuf::from(owner._application_bundle.bundlePath().to_string())
                .canonicalize()
                .unwrap(),
            target
        );
        assert_ne!(
            NSBundle::mainBundle().bundlePath(),
            owner._host_bundle.bundlePath()
        );
        let class = owner
            ._framework
            .classNamed(&NSString::from_str("SPUUpdater"))
            .unwrap();
        require_class_bundle(class, &framework).unwrap();
        assert!(require_class_bundle(class, &base.join("Foreign.framework")).is_err());
        let can_check: Bool = unsafe { msg_send![&*owner.controller, canCheckForUpdates] };
        let session: Bool = unsafe { msg_send![&*owner.controller, sessionInProgress] };
        assert!(!can_check.as_bool() && !session.as_bool());
        drop(owner);
        std::fs::remove_dir_all(base).unwrap();
    }

    fn assert_native_start_guard(&self) {
        let framework = PathBuf::from(self._framework.bundlePath().to_string());
        for (fresh, kind) in [(false, 0_isize), (true, 1), (true, 0)] {
            let marker = MainThreadMarker::new().expect("Main-thread component probe");
            let checks = Rc::new(driver::State::default());
            let owner = construct_with_state(&framework, marker, checks.clone()).unwrap();
            checks.session.borrow_mut().begin().unwrap();
            checks.checking(&owner.controller);
            checks.native_starting().unwrap();
            assert!(checks.view().native_cycle);
            if fresh {
                let cancellation = RcBlock::new(|| {});
                unsafe {
                    let _: () = msg_send![&*owner._driver, showUserInitiatedUpdateCheckWithCancellation: &*cancellation];
                }
                assert!(checks.view().native_cycle);
            }
            unsafe {
                let _: () = msg_send![&*owner.delegate, updater: &*owner.controller, didFinishUpdateCycleForUpdateCheck: kind, error: std::ptr::null::<NSError>()];
            }
            assert!(checks.view().native_cycle && checks.view().quarantine);
            assert!(checks.session.borrow_mut().begin().is_err());
        }
    }

    fn clear_unstarted_component_callbacks(&self) {
        // Component assertions only. Probe is constructed without calling startUpdater
        // and exposes no start method. This is not an application recovery API.
        let updater: Retained<AnyObject> = self.controller.clone();
        let ready: Bool = unsafe { msg_send![&*updater, canCheckForUpdates] };
        assert!(!ready.as_bool());
        let state = self.delegate.ivars();
        let stale = state.pending.borrow_mut().take();
        state.invalidated.set(false);
        state.termination_inhibited.set(false);
        state.message.set(None);
        state.changed();
        drop(stale);
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
    construct(root, marker)
}

fn construct(root: &Path, marker: MainThreadMarker) -> Result<Probe, String> {
    construct_with_state(root, marker, Rc::new(driver::State::default()))
}

fn construct_with_state(
    root: &Path,
    marker: MainThreadMarker,
    checks: Rc<driver::State>,
) -> Result<Probe, String> {
    let host = NSBundle::mainBundle();
    construct_with_bundles(root, marker, checks, host.clone(), host)
}

fn require_class_bundle(class: &objc2::runtime::AnyClass, root: &Path) -> Result<(), String> {
    let actual = unsafe { NSBundle::bundleForClass(class) };
    if Path::new(&actual.bundlePath().to_string()) != root {
        return Err("A different Sparkle class image is already loaded".into());
    }
    Ok(())
}

/// All callers verify code/resources before this explicit load. Construction
/// owns the delegate/driver/bundles but exposes no start or installation method.
fn construct_with_bundles(
    root: &Path,
    marker: MainThreadMarker,
    checks: Rc<driver::State>,
    host: Retained<NSBundle>,
    application: Retained<NSBundle>,
) -> Result<Probe, String> {
    if let Some(class) = objc2::runtime::AnyClass::get(c"SPUUpdater") {
        require_class_bundle(class, root)?;
    }
    unsafe {
        let _: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
    }
    let path = NSString::from_str(
        root.to_str()
            .ok_or("Sparkle preparation needs a Unicode staging path")?,
    );
    let framework = NSBundle::bundleWithPath(&path).ok_or("Invalid Sparkle framework bundle")?;
    if Path::new(&framework.bundlePath().to_string()) != root {
        return Err("Sparkle framework bundle path changed".into());
    }
    // All executable/resource bytes, symlink targets and native code signatures
    // above are verified before this explicit native component load.
    unsafe {
        framework
            .loadAndReturnError()
            .map_err(|_| "Verified Sparkle framework could not load")?;
    }
    let controller_class = framework
        .classNamed(&NSString::from_str("SPUUpdater"))
        .ok_or("Pinned Sparkle updater is unavailable")?;
    require_class_bundle(controller_class, root)?;
    let domain = NSString::from_str("com.peerbrush.SparklePreparation");
    let message = NSString::from_str("Sparkle updates remain disabled until feed, public key and native safety acceptance are configured and verified.");
    let key = NSString::from_str("NSLocalizedDescription");
    let info: Retained<AnyObject> =
        unsafe { msg_send![class!(NSDictionary), dictionaryWithObject: &*message, forKey: &*key] };
    let error: Retained<NSError> = unsafe {
        msg_send![class!(NSError), errorWithDomain: &*domain, code: 1_isize, userInfo: &*info]
    };
    let delegate = PreparationDelegate::alloc(marker).set_ivars(DelegateState {
        checks: checks.clone(),
        error,
        pending: RefCell::new(None),
        revision: Cell::new(0),
        invalidated: Cell::new(false),
        termination_inhibited: Cell::new(false),
        message: Cell::new(None),
    });
    let delegate: Retained<PreparationDelegate> = unsafe { msg_send![super(delegate), init] };
    let allocated: Allocated<AnyObject> = unsafe { msg_send![controller_class, alloc] };
    let user_driver = driver::create(marker, checks);
    let controller: Option<Retained<AnyObject>> = unsafe {
        msg_send![allocated, initWithHostBundle: &*host, applicationBundle: &*application,
        userDriver: &*user_driver, delegate: &*delegate]
    };
    let controller = controller.ok_or("Sparkle preparation controller could not initialize")?;
    let actual_host: Retained<NSBundle> = unsafe { msg_send![&*controller, hostBundle] };
    if actual_host.bundlePath() != host.bundlePath() {
        return Err("Sparkle targeted a different host bundle".into());
    }
    Ok(Probe {
        controller,
        delegate,
        _driver: user_driver,
        _framework: framework,
        _host_bundle: host,
        _application_bundle: application,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn public_configuration_cannot_activate_native_startup_before_safe_termination() {
        let checks = std::rc::Rc::new(super::driver::State::default());
        let mut app = super::Application {
            controller: None,
            message: "",
            configuration: Some(crate::sparkle::check::ApprovedCheck {
                feed_url: crate::sparkle::PROPOSED_FEED_URL.into(),
                public_ed_key: "Public configuration is not a startup authority".into(),
                publisher_team_id: "ABCD123456".into(),
                objects: std::collections::BTreeMap::new(),
            }),
            verification: None,
            profile: None,
            checkpoint: None,
            checks: checks.clone(),
        };
        assert!(app.configured());
        assert!(!app.startup_enabled());
        let shared = std::sync::Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
        let workspace = crate::workspace::attach(&shared);
        assert!(app
            .check(&eframe::egui::Context::default(), &workspace)
            .unwrap_err()
            .contains("not available"));
        assert!(app.controller.is_none() && app.verification.is_none());
        assert_eq!(checks.view().phase, crate::sparkle::check::Phase::Idle);
        assert!(!checks.view().busy && !app.pending_installation());
    }
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

/// Explicit credential-free component route for an inspected unsigned app ZIP.
/// It intentionally requires unchanged upstream bytes; transformed production
/// frameworks need the separately reviewed native policy before any load route.
pub fn probe_bundled(app: &Path, configuration: &Configuration) -> Result<Probe, String> {
    configuration.validate()?;
    let marker =
        MainThreadMarker::new().ok_or("Bundled Sparkle preparation requires the main thread")?;
    require_supported_macos(
        NSProcessInfo::processInfo()
            .operatingSystemVersion()
            .majorVersion,
    )?;
    let framework = app.join("Contents/Frameworks/Sparkle.framework");
    verify_framework_with_notice(
        &framework,
        &app.join("Contents/Resources/licenses/sparkle-2.10.0/LICENSE"),
    )?;
    construct(&framework, marker)
}

/// Main-thread application owner. Configured native startup remains disabled
/// until safe termination/cancellation/recovery is implemented and verified.
struct Verification {
    receiver: mpsc::Receiver<Result<checkpoint::Prepared, String>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub struct Application {
    controller: Option<Probe>,
    message: &'static str,
    configuration: Option<crate::sparkle::check::ApprovedCheck>,
    verification: Option<Verification>,
    checks: Rc<driver::State>,
    profile: Option<crate::settings::UpdateProfile>,
    checkpoint: Option<checkpoint::Prepared>,
}

impl Application {
    pub fn load() -> Self {
        let supported = require_supported_macos(
            NSProcessInfo::processInfo()
                .operatingSystemVersion()
                .majorVersion,
        )
        .is_ok();
        let configuration = if supported {
            crate::sparkle::check::compiled().ok().flatten()
        } else {
            None
        };
        Self {
            controller: None,
            message: if supported {
                ""
            } else {
                "Automatic updates are unavailable on macOS 11. PeerBrush remains supported."
            },
            configuration,
            verification: None,
            profile: None,
            checkpoint: None,
            checks: Rc::new(driver::State::default()),
        }
    }
    pub(crate) fn set_update_profile(&mut self, profile: Option<crate::settings::UpdateProfile>) {
        self.profile = profile;
    }
    pub(crate) fn configured(&self) -> bool {
        self.configuration.is_some()
    }
    pub(crate) fn startup_enabled(&self) -> bool {
        // Public feed/key/native facts cannot prove absence or safe cancellation
        // of a prior installer. Complete termination/recovery must precede startup.
        false
    }
    pub(crate) fn view(&self) -> std::cell::Ref<'_, crate::sparkle::check::Session> {
        self.checks.view()
    }
    pub fn message(&self) -> &'static str {
        self.controller
            .as_ref()
            .and_then(|owner| owner.delegate.ivars().message.get())
            .unwrap_or(self.message)
    }
    pub fn pending_installation(&self) -> bool {
        self.checks.view().native_cycle
            || self.checks.view().quarantine
            || self
                .controller
                .as_ref()
                .is_some_and(|owner| owner.delegate.ivars().termination_inhibited.get())
    }
    pub(crate) fn profile_capture_pending(&self) -> bool {
        self.verification.is_some() && !self.checks.view().cancelled
    }
    /// Ordinary on_exit will still merge/flush tool preferences. Retire the
    /// capture-only current-profile observation before any of those writes.
    pub(crate) fn retire_profile_capture(&mut self) -> Result<(), String> {
        if self.pending_installation() {
            return Err("An app update remains held; its preservation owner was retained".into());
        }
        if let Some(prepared) = &self.checkpoint {
            prepared.retire_profile_capture()?;
        }
        self.checkpoint = None;
        Ok(())
    }
    pub fn defer(&self) {
        if let Some(work) = &self.verification {
            work.cancel
                .store(true, std::sync::atomic::Ordering::Release);
        }
        self.checks.cancel();
        if let Some(owner) = &self.controller {
            owner.delegate.ivars().defer();
        }
    }
    pub(crate) fn check(
        &mut self,
        ctx: &eframe::egui::Context,
        workspace: &crate::workspace::Registry,
    ) -> Result<(), String> {
        if !self.startup_enabled() {
            return Err("App update checking is not available in this development build.".into());
        }
        let configuration = self
            .configuration
            .clone()
            .ok_or("App update checking is not available in this build")?;
        if self.verification.is_some() || self.checks.view().busy || self.pending_installation() {
            return Err("The current app update check must finish first".into());
        }
        if let Some(owner) = &self.controller {
            let busy: Bool = unsafe { msg_send![&*owner.controller, sessionInProgress] };
            if busy.as_bool() {
                return Err("The current app update check must finish first".into());
            }
        }
        self.retire_profile_capture()?;
        let app = PathBuf::from(NSBundle::mainBundle().bundlePath().to_string());
        let executable = std::env::current_exe().map_err(|_| "Running app path unavailable")?;
        if !app.is_absolute() || executable != app.join("Contents/MacOS/peerbrush") {
            return Err("App update checking requires the running installed bundle".into());
        }
        crate::workspace::lifecycle::update_readiness(workspace)?;
        let profile = self
            .profile
            .clone()
            .ok_or("App updates require the locked default settings profile")?;
        let checks = Rc::new(driver::State::default());
        checks.begin(ctx)?;
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        std::thread::Builder::new()
            .name("peerbrush-app-update-verification".into())
            .spawn(move || {
                let result = checkpoint::prepare(app, configuration, profile, worker_cancel);
                let _ = sender.send(result);
                repaint.request_repaint();
            })
            .map_err(|_| "Could not start app update verification")?;
        self.controller = None;
        self.checks = checks;
        self.verification = Some(Verification { receiver, cancel });
        Ok(())
    }
    pub(crate) fn tick(
        &mut self,
        ctx: &eframe::egui::Context,
        workspace: &crate::workspace::Registry,
        ui_blocked: bool,
    ) {
        if !self.startup_enabled() {
            if self.verification.is_some() {
                self.defer();
                self.verification = None;
                self.checks.finished();
            }
            return;
        }
        if self.checks.timed_out() && !self.checks.view().cancelled {
            self.defer();
        }
        let result = self
            .verification
            .as_ref()
            .map(|work| work.receiver.try_recv());
        if let Some(result) = result {
            match result {
                Ok(result) => {
                    self.verification = None;
                    if self.checks.view().cancelled {
                        self.checks.finished();
                    } else {
                        let result = result.and_then(|prepared| {
                            if ui_blocked { return Err("Finish or cancel the current edit/file operation before app update checking".into()); }
                            // Retain the actual owner before publication can create
                            // an observation, including on every later error.
                            self.checkpoint = Some(prepared);
                            let prepared = self.checkpoint.as_mut().unwrap();
                            prepared.publish(workspace)?;
                            // Capture-only publication cannot authorize loading a
                            // mutable live app/framework after worker verification.
                            checkpoint::require_captured_handoff()?;
                            prepared.recheck_metadata()?;
                            let marker = MainThreadMarker::new()
                                .ok_or("App update checking requires the main thread")?;
                            let owner = construct_with_state(
                                &prepared.verified.framework,
                                marker,
                                self.checks.clone(),
                            )?;
                            prepared.recheck_metadata()?;
                            self.checks.checking(&owner.controller);
                            // Publish intent durably before Sparkle can resume any
                            // previous installer. Every ambiguous outcome stays held.
                            prepared.arm_native_intent()?;
                            self.checks.native_starting()?;
                            self.controller = Some(owner);
                            let owner = self.controller.as_ref().unwrap();
                            let mut error: *mut NSError = std::ptr::null_mut();
                            let started: Bool =
                                unsafe { msg_send![&*owner.controller, startUpdater: &mut error] };
                            if !started.as_bool() {
                                self.checkpoint.as_ref().unwrap().not_started()?;
                                self.checks.native_start_failed();
                                return Err("App update checking could not start".into());
                            }
                            unsafe {
                                let _: () = msg_send![&*owner.controller, checkForUpdates];
                            }
                            Ok(())
                        });
                        if result.is_err() {
                            self.checks.failed();
                            self.checks.finished();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.verification = None;
                    self.checks.failed();
                    self.checks.finished();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.verification.is_some() || self.checks.view().busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    /// Read-only preparation guard. The application has no installation,
    /// relaunch or recovery executor; no project closes and no block is invoked.
    pub fn request_installation(
        &mut self,
        workspace: &crate::workspace::Registry,
        ui_blocked: bool,
        kind: ContinuationKind,
    ) -> Result<(), String> {
        if ui_blocked {
            return Err(
                "Finish or cancel the current edit/file operation before restarting.".into(),
            );
        }
        if self.checks.view().quarantine {
            return Err(
                "App updating was interrupted. Quitting is paused to protect your projects.".into(),
            );
        }
        if self.checks.view().native_cycle
            && !self.checks.view().quarantine
            && !self
                .controller
                .as_ref()
                .is_some_and(|owner| owner.delegate.ivars().termination_inhibited.get())
        {
            self.defer();
            return Err(
                "App update checking is paused. Quitting is blocked to protect your projects."
                    .into(),
            );
        }
        let owner = self
            .controller
            .as_ref()
            .ok_or("No owned update controller is available")?;
        let state = owner.delegate.ivars();
        state.ready_kind(kind)?;
        let revision = state.revision.get();
        crate::workspace::lifecycle::update_readiness(workspace)?;
        if state.revision.get() != revision {
            return Err("The pending native update changed during review".into());
        }
        state.ready_kind(kind)?;
        Err("Automatic updates are not available in this development build.".into())
    }
}

#[cfg(test)]
impl Application {
    pub(crate) fn native_starting_for_test(&self) {
        let mut state = self.checks.session.borrow_mut();
        state.begin().unwrap();
        state.checking();
        state.native_starting().unwrap();
    }
    pub(crate) fn queue_verification_for_test(&mut self, ctx: &eframe::egui::Context) {
        self.checks.begin(ctx).unwrap();
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Err("Synthetic stopped verification".into()))
            .unwrap();
        self.verification = Some(Verification {
            receiver,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
    }
    pub(crate) fn verification_pending_for_test(&self) -> bool {
        self.verification.is_some()
    }
}

impl Drop for Application {
    fn drop(&mut self) {
        self.defer();
    }
}
