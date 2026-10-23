//! App-owned information-only user driver. No Install or installer-block call exists.
use super::*;
use crate::sparkle::check::{self, Phase, Session};
use std::{cell::Ref, rc::Rc, time::Instant};

pub(super) struct State {
    pub session: RefCell<Session>,
    updater: Cell<*const AnyObject>,
    cancellation: RefCell<Option<RcBlock<dyn Fn()>>>,
    choice: RefCell<Option<RcBlock<dyn Fn(isize)>>>,
    cancellation_sent: Cell<bool>,
    choice_sent: Cell<bool>,
    download_cancelled: Cell<bool>,
    repaint: RefCell<Option<eframe::egui::Context>>,
    started: Cell<Option<Instant>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            session: RefCell::new(Session::default()),
            updater: Cell::new(std::ptr::null()),
            cancellation: RefCell::new(None),
            choice: RefCell::new(None),
            cancellation_sent: Cell::new(false),
            choice_sent: Cell::new(false),
            download_cancelled: Cell::new(false),
            repaint: RefCell::new(None),
            started: Cell::new(None),
        }
    }
}
impl State {
    pub fn view(&self) -> Ref<'_, Session> {
        self.session.borrow()
    }
    pub fn repaint(&self) {
        if let Some(ctx) = self.repaint.borrow().as_ref() {
            ctx.request_repaint();
        }
    }
    pub fn begin(&self, ctx: &eframe::egui::Context) -> Result<(), String> {
        self.session.borrow_mut().begin()?;
        *self.repaint.borrow_mut() = Some(ctx.clone());
        self.started.set(Some(Instant::now()));
        Ok(())
    }
    pub fn checking(&self, updater: &AnyObject) {
        self.updater.set(updater);
        self.session.borrow_mut().checking();
        self.repaint();
    }
    pub fn may_check(&self, updater: &AnyObject, kind: isize) -> bool {
        self.updater.get() == updater as *const AnyObject
            && self.session.borrow_mut().may_check(kind)
    }
    pub fn owns(&self, updater: &AnyObject) -> bool {
        self.updater.get() == updater as *const AnyObject && !self.updater.get().is_null()
    }
    pub fn has_owner(&self) -> bool {
        !self.updater.get().is_null()
    }
    pub fn may_proceed(&self, updater: &AnyObject, item: &AnyObject, kind: isize) -> bool {
        if kind != 0 || self.updater.get() != updater as *const AnyObject {
            return false;
        }
        let state = self.session.borrow();
        if state.phase != Phase::Checking || state.cancelled || state.quarantine {
            return false;
        }
        // A signed-feed failure can leave safe fallback items. They are never
        // displayed as authenticated release information by this driver.
        let signed: isize = unsafe { msg_send![item, signingValidationStatus] };
        signed == 1
    }
    pub fn cancel(&self) {
        self.session.borrow_mut().cancel();
        let cancellation = self.cancellation.borrow_mut().take();
        let choice = self.choice.borrow_mut().take();
        // These are check cancellation and Dismiss choices, never install/retry
        // handlers. Take ownership and release every borrow before native reentry.
        if let Some(block) = cancellation {
            if !self.cancellation_sent.replace(true) {
                block.call(());
            }
        }
        if let Some(block) = choice {
            self.dismiss(&block);
        }
        self.repaint();
    }
    fn dismiss(&self, block: &Block<dyn Fn(isize)>) {
        if !self.choice_sent.replace(true) {
            block.call((2_isize,));
        }
    }
    pub fn failed(&self) {
        self.session.borrow_mut().fail();
        self.repaint();
    }
    pub fn finished(&self) {
        let cancellation = self.cancellation.borrow_mut().take();
        let choice = self.choice.borrow_mut().take();
        self.session.borrow_mut().finished();
        self.cancellation_sent.set(true);
        self.choice_sent.set(true);
        self.started.set(None);
        drop(cancellation);
        drop(choice);
        self.repaint();
    }
    pub fn quarantine(&self) {
        self.session.borrow_mut().quarantine();
        self.repaint();
    }
    pub fn timed_out(&self) -> bool {
        matches!(self.view().phase, Phase::Verifying | Phase::Checking)
            && self
                .started
                .get()
                .is_some_and(|start| start.elapsed().as_secs() >= 30)
    }
}

pub(super) fn no_update(error: &NSError) -> bool {
    // Exact pinned SUErrors.h/SUConstants.m, not an arbitrary NSError code1001.
    let code: isize = unsafe { msg_send![error, code] };
    let domain: Retained<NSString> = unsafe { msg_send![error, domain] };
    code == 1001
        && domain.length() == "SUSparkleErrorDomain".len()
        && domain.to_string() == "SUSparkleErrorDomain"
}

fn text(value: Option<Retained<NSString>>, limit: usize) -> Option<String> {
    let value = value?;
    if value.length() > limit {
        return None;
    }
    let value = value.to_string();
    (value.len() <= limit && !value.contains('\0')).then_some(value)
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Rc<State>]
    pub(super) struct CheckDriver;
    unsafe impl NSObjectProtocol for CheckDriver {}
    impl CheckDriver {
        #[unsafe(method(showUpdatePermissionRequest:reply:))]
        fn permission(&self, _request: &AnyObject, _reply: &Block<dyn Fn(*mut AnyObject)>) {
            // The verified plist/delegate never requests permission. Refuse an
            // unexpected path without changing the user's automatic preferences.
            self.ivars().quarantine();
        }
        #[unsafe(method(showUserInitiatedUpdateCheckWithCancellation:))]
        fn checking(&self, cancellation: &Block<dyn Fn()>) {
            let state=self.ivars();
            if state.cancellation_sent.get() { return; }
            if state.view().phase != Phase::Checking && !state.view().cancelled { state.quarantine(); return; }
            if state.cancellation.borrow().is_some() { state.quarantine(); state.cancel(); return; }
            let block=unsafe { RcBlock::copy((cancellation as *const Block<dyn Fn()>).cast_mut()) };
            let old=std::mem::replace(&mut *state.cancellation.borrow_mut(), block); drop(old);
            if state.view().cancelled { state.cancel(); } else { state.repaint(); }
        }
        #[unsafe(method(showUpdateFoundWithAppcastItem:state:reply:))]
        fn found(&self, item: &AnyObject, native_state: &AnyObject, reply: &Block<dyn Fn(isize)>) {
            let state=self.ivars();
            let stage: isize=unsafe { msg_send![native_state, stage] };
            let manual: Bool=unsafe { msg_send![native_state, userInitiated] };
            let signed: isize=unsafe { msg_send![item, signingValidationStatus] };
            if stage != 0 || !manual.as_bool() || signed != 1 {
                // Existing downloaded/installing sessions require the unfinished
                // termination/recovery flow. Never dismiss an installing session
                // as if that proved it could not install on ordinary termination.
                state.quarantine(); return;
            }
            let version: Option<Retained<NSString>>=unsafe { msg_send![item, displayVersionString] };
            let format: Option<Retained<NSString>>=unsafe { msg_send![item, itemDescriptionFormat] };
            let description: Option<Retained<NSString>>=unsafe { msg_send![item, itemDescription] };
            let format=text(format, 32);
            let notes=if format.as_deref()==Some("plain-text") { text(description, check::NOTES_LIMIT).unwrap_or_default() } else { String::new() };
            let accepted=state.session.borrow_mut().found(text(version,128).unwrap_or_default(),notes);
            let cancellation=state.cancellation.borrow_mut().take(); drop(cancellation);
            if accepted {
                let copied=unsafe { RcBlock::copy((reply as *const Block<dyn Fn(isize)>).cast_mut()) };
                let old=std::mem::replace(&mut *state.choice.borrow_mut(),copied); drop(old);
                if state.choice.borrow().is_none() { state.quarantine(); }
            } else if state.view().phase==Phase::Found { state.quarantine(); }
            else { state.failed(); state.dismiss(reply); }
            state.repaint();
        }
        #[unsafe(method(showUpdateReleaseNotesWithDownloadData:))]
        fn notes(&self, _download: &AnyObject) { self.ivars().quarantine(); }
        #[unsafe(method(showUpdateReleaseNotesFailedToDownloadWithError:))]
        fn notes_failed(&self, _error: &NSError) { self.ivars().quarantine(); }
        #[unsafe(method(showUpdateNotFoundWithError:acknowledgement:))]
        fn not_found(&self, error: &NSError, acknowledgement: &Block<dyn Fn()>) {
            let state=self.ivars();
            if state.has_owner() && no_update(error) { state.session.borrow_mut().not_found(); }
            else { state.failed(); }
            acknowledgement.call(()); state.repaint();
        }
        #[unsafe(method(showUpdaterError:acknowledgement:))]
        fn failed(&self, _error: &NSError, acknowledgement: &Block<dyn Fn()>) { self.ivars().failed(); acknowledgement.call(()); }
        #[unsafe(method(showDownloadInitiatedWithCancellation:))]
        fn download(&self, cancellation: &Block<dyn Fn()>) { self.ivars().quarantine(); if !self.ivars().download_cancelled.replace(true) { cancellation.call(()); } }
        #[unsafe(method(showDownloadDidReceiveExpectedContentLength:))]
        fn length(&self, _length: u64) { self.ivars().quarantine(); }
        #[unsafe(method(showDownloadDidReceiveDataOfLength:))]
        fn data(&self, _length: u64) { self.ivars().quarantine(); }
        #[unsafe(method(showDownloadDidStartExtractingUpdate))]
        fn extracting(&self) { self.ivars().quarantine(); }
        #[unsafe(method(showExtractionReceivedProgress:))]
        fn extracting_progress(&self, _progress: f64) { self.ivars().quarantine(); }
        #[unsafe(method(showReadyToInstallAndRelaunch:))]
        fn install(&self, _reply: &Block<dyn Fn(isize)>) { self.ivars().quarantine(); }
        #[unsafe(method(showInstallingUpdateWithApplicationTerminated:retryTerminatingApplication:))]
        fn installing(&self, _terminated: Bool, _retry: &Block<dyn Fn()>) { self.ivars().quarantine(); }
        #[unsafe(method(showUpdateInstalledAndRelaunched:acknowledgement:))]
        fn installed(&self, _relaunched: Bool, _acknowledgement: &Block<dyn Fn()>) { self.ivars().quarantine(); }
        #[unsafe(method(dismissUpdateInstallation))]
        fn dismissed(&self) { self.ivars().repaint(); }
        #[unsafe(method(showUpdateInFocus))]
        fn focus(&self) { self.ivars().repaint(); }
    }
);

pub(super) fn create(marker: MainThreadMarker, state: Rc<State>) -> Retained<CheckDriver> {
    let allocated = CheckDriver::alloc(marker).set_ivars(state);
    unsafe { msg_send![super(allocated), init] }
}

/// Native selector/ownership assertions, reached only by the explicit component
/// probe. This constructs no updater and never starts a network/install cycle.
pub(super) fn assert_guarded(marker: MainThreadMarker) {
    let state = Rc::new(State::default());
    state.session.borrow_mut().begin().unwrap();
    state.session.borrow_mut().checking();
    let driver = create(marker, state.clone());
    let cancellations = Rc::new(Cell::new(0));
    let count = cancellations.clone();
    let block = RcBlock::new(move || count.set(count.get() + 1));
    unsafe {
        let _: () = msg_send![&*driver, showUserInitiatedUpdateCheckWithCancellation: &*block];
    }
    state.cancel();
    state.cancel();
    unsafe {
        let _: () = msg_send![&*driver, showUserInitiatedUpdateCheckWithCancellation: &*block];
    }
    assert_eq!(cancellations.get(), 1);
    let installs = Rc::new(Cell::new(0));
    let count = installs.clone();
    let installer = RcBlock::new(move |_choice: isize| count.set(count.get() + 1));
    unsafe {
        let _: () = msg_send![&*driver, showReadyToInstallAndRelaunch: &*installer];
    }
    assert!(state.view().quarantine);
    assert_eq!(installs.get(), 0);
    let choices = Rc::new(Cell::new(0));
    let count = choices.clone();
    let choice = RcBlock::new(move |value: isize| {
        assert_eq!(value, 2);
        count.set(count.get() + 1);
    });
    *state.choice.borrow_mut() = Some(choice);
    state.cancel();
    state.cancel();
    state.finished();
    assert_eq!(choices.get(), 1);
    assert_eq!(installs.get(), 0);
}
