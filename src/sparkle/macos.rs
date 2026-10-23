//! Credential-free native bridge probe. No start/check/install method is exposed.
use super::{require_supported_macos, Configuration};
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
        fn may_check(&self, _updater: &AnyObject, _kind: isize, error: *mut *mut NSError) -> Bool {
            if !error.is_null() {
                // Sparkle's NSError** is an autoreleasing out parameter; the
                // delegate owns the error for the controller's whole lifetime.
                unsafe { *error = Retained::as_ptr(&self.ivars().error).cast_mut(); }
            }
            Bool::NO
        }

        #[unsafe(method(updater:shouldProceedWithUpdate:updateCheck:error:))]
        fn may_proceed(&self, _updater: &AnyObject, _item: &AnyObject, _kind: isize, error: *mut *mut NSError) -> Bool {
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
        fn aborted(&self, _updater: &AnyObject, _error: &NSError) {
            self.ivars().invalidate("The update was interrupted. Automatic installation remains disabled.");
        }

        #[unsafe(method(updater:didFinishUpdateCycleForUpdateCheck:error:))]
        fn finished(&self, _updater: &AnyObject, _kind: isize, _error: Option<&NSError>) {
            self.ivars().invalidate("The update cycle ended. Automatic installation remains disabled.");
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
    /// Credential-free native component assertion. No continuation is invoked
    /// and no document closes. The passive application owner type holds Probe
    /// here; this is not a configured application update cycle.
    pub fn assert_application_guards(self) {
        let mut application = Application {
            controller: Some(self),
            message: "Prepared updates remain disabled",
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
                let updater: Retained<AnyObject> =
                    unsafe { msg_send![&*owner.controller, updater] };
                let item: Retained<AnyObject> =
                    unsafe { msg_send![class!(SUAppcastItem), emptyAppcastItem] };
                let postponed: Bool = unsafe {
                    msg_send![&*owner.delegate, updater: &*updater,
                    willInstallUpdateOnQuit: &*item, immediateInstallationBlock: &*block]
                };
                assert!(postponed.as_bool());
            }
            assert!(application.pending_installation());
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
                let updater: Retained<AnyObject> =
                    unsafe { msg_send![&*owner.controller, updater] };
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
        let updater: Retained<AnyObject> = unsafe { msg_send![&*self.controller, updater] };
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

    fn clear_unstarted_component_callbacks(&self) {
        // Component assertions only. Probe is constructed with startingUpdater:NO
        // and exposes no start method. This is not an application recovery API.
        let updater: Retained<AnyObject> = unsafe { msg_send![&*self.controller, updater] };
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
    let delegate = PreparationDelegate::alloc(marker).set_ivars(DelegateState {
        error,
        pending: RefCell::new(None),
        revision: Cell::new(0),
        invalidated: Cell::new(false),
        termination_inhibited: Cell::new(false),
        message: Cell::new(None),
    });
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

/// Application-owned passive lifetime. Configured app loading/update execution
/// is not implemented. The default owner loads nothing, preserves macOS 11 and
/// adds no codesign startup work; only the explicit native component owns Probe.
pub struct Application {
    controller: Option<Probe>,
    message: &'static str,
}

impl Application {
    pub fn dormant() -> Self {
        let supported = require_supported_macos(
            NSProcessInfo::processInfo()
                .operatingSystemVersion()
                .majorVersion,
        )
        .is_ok();
        Self {
            controller: None,
            message: if supported {
                ""
            } else {
                "Automatic updates are unavailable on macOS 11. PeerBrush remains supported."
            },
        }
    }

    pub fn message(&self) -> &'static str {
        self.controller
            .as_ref()
            .and_then(|owner| owner.delegate.ivars().message.get())
            .unwrap_or(self.message)
    }

    pub fn pending_installation(&self) -> bool {
        self.controller
            .as_ref()
            .is_some_and(|owner| owner.delegate.ivars().termination_inhibited.get())
    }

    pub fn defer(&self) {
        if let Some(owner) = &self.controller {
            owner.delegate.ivars().defer();
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
