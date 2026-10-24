//! App-owned user driver. Only an exact Found reply may start a download;
//! Ready/install/relaunch continuations remain held and are never invoked.
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
    item: RefCell<Option<Retained<AnyObject>>>,
    held_ready: RefCell<Option<HeldReady>>,
    ready_revision: Cell<u64>,
    ready_exhausted: Cell<bool>,
    found: RefCell<Option<HeldFound>>,
    found_revision: Cell<u64>,
    download_generation: Cell<Option<u64>>,
    download_item_seen: Cell<bool>,
    download_started: Cell<bool>,
    download_complete: Cell<bool>,
    expected_bytes: Cell<Option<u64>>,
    received_bytes: Cell<u64>,
    extraction: Cell<Option<f64>>,
    control_requested: Cell<bool>,
    prepared_generation: Cell<Option<u64>>,
    native_lifecycle_started: Cell<bool>,
    lifecycle_revision: Cell<u64>,
    installing: RefCell<Option<HeldInstalling>>,
    installed: RefCell<Option<HeldInstalled>>,
}
struct HeldFound {
    updater: Retained<AnyObject>,
    item: Retained<AnyObject>,
    generation: u64,
    downloadable: bool,
}
struct HeldReady {
    _updater: Retained<AnyObject>,
    _item: Option<Retained<AnyObject>>,
    _reply: RcBlock<dyn Fn(isize)>,
    generation: Option<u64>,
}
struct OwnedNativeCallback {
    updater: Retained<AnyObject>,
    item: Option<Retained<AnyObject>>,
    ready_generation: Option<u64>,
    handler: RcBlock<dyn Fn()>,
    revision: u64,
}
struct HeldInstalling {
    owner: OwnedNativeCallback,
    application_terminated: bool,
}
struct HeldInstalled {
    owner: OwnedNativeCallback,
    relaunched: bool,
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
            item: RefCell::new(None),
            held_ready: RefCell::new(None),
            ready_revision: Cell::new(0),
            ready_exhausted: Cell::new(false),
            found: RefCell::new(None),
            found_revision: Cell::new(0),
            download_generation: Cell::new(None),
            download_item_seen: Cell::new(false),
            download_started: Cell::new(false),
            download_complete: Cell::new(false),
            expected_bytes: Cell::new(None),
            received_bytes: Cell::new(0),
            extraction: Cell::new(None),
            control_requested: Cell::new(false),
            prepared_generation: Cell::new(None),
            native_lifecycle_started: Cell::new(false),
            lifecycle_revision: Cell::new(0),
            installing: RefCell::new(None),
            installed: RefCell::new(None),
        }
    }
}
impl State {
    pub(super) fn begin_in_owner(&self, updater: &AnyObject) -> Result<(), String> {
        self.session.borrow_mut().begin()?;
        self.started.set(Some(Instant::now()));
        self.checking(updater);
        self.native_starting()
    }
    fn remember_item(&self, item: &AnyObject) -> bool {
        if !self.has_owner() || !self.view().native_cycle {
            return false;
        }
        let old = self.item.borrow().as_ref().map(Retained::as_ptr);
        if old.is_some_and(|old| old != item as *const AnyObject) {
            self.quarantine();
            return false;
        }
        if old.is_none() {
            let Some(item) = (unsafe { Retained::retain((item as *const AnyObject).cast_mut()) })
            else {
                self.quarantine();
                return false;
            };
            let mut slot = self.item.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|current| Retained::as_ptr(current) != Retained::as_ptr(&item))
            {
                drop(slot);
                self.quarantine();
                return false;
            }
            let previous = slot.replace(item);
            drop(slot);
            drop(previous);
        }
        true
    }
    fn retain_ready(&self, reply: &Block<dyn Fn(isize)>) {
        // This records an actual callback; it grants no installation permission.
        self.quarantine();
        if !self.has_owner() || !self.view().native_cycle {
            return;
        }
        let item = self.item.borrow().as_ref().map(Retained::as_ptr);
        let generation = self.ready_revision.get().checked_add(1);
        if generation.is_none() {
            self.ready_exhausted.set(true);
        }
        self.ready_revision.set(generation.unwrap_or(u64::MAX));
        // An old check/Found handler is not cancellation authority for Ready.
        // Retire those callbacks without invoking them before native reentry.
        self.cancellation_sent.set(true);
        self.choice_sent.set(true);
        let cancellation = self.cancellation.borrow_mut().take();
        let choice = self.choice.borrow_mut().take();
        drop(cancellation);
        drop(choice);
        // Native copies/retains can reenter. Publish only if no newer callback won.
        let Some(reply) =
            (unsafe { RcBlock::copy((reply as *const Block<dyn Fn(isize)>).cast_mut()) })
        else {
            return;
        };
        let Some(updater) = (unsafe { Retained::retain(self.updater.get().cast_mut()) }) else {
            return;
        };
        let item = match item {
            Some(item) => Some(match unsafe { Retained::retain(item.cast_mut()) } {
                Some(item) => item,
                None => return,
            }),
            None => None,
        };
        if generation.is_some_and(|generation| {
            self.ready_exhausted.get() || self.ready_revision.get() != generation
        }) {
            return;
        }
        let previous = self.held_ready.borrow_mut().replace(HeldReady {
            _updater: updater,
            _item: item,
            _reply: reply,
            generation,
        });
        drop(previous);
    }
    pub(super) fn held_generation(&self) -> Option<u64> {
        if self.ready_exhausted.get() {
            return None;
        }
        self.held_ready
            .borrow()
            .as_ref()
            .and_then(|held| held._item.as_ref().and(held.generation))
    }
    pub(super) fn held_is_current(&self, generation: u64) -> bool {
        generation != 0
            && self.held_generation() == Some(generation)
            && self.view().native_cycle
            && !self.view().cancelled
            && !self.native_lifecycle_started.get()
            && self.held_ready.borrow().as_ref().is_some_and(|held| {
                Retained::as_ptr(&held._updater) == self.updater.get()
                    && held._item.as_ref().map(Retained::as_ptr)
                        == self.item.borrow().as_ref().map(Retained::as_ptr)
            })
    }
    pub(super) fn prepare_final_control(&self, generation: u64) -> Result<(), String> {
        if self.control_requested.replace(true)
            || self.started.get().is_none()
            || !self.held_is_current(generation)
        {
            return Err("The actual native Ready callback is no longer current".into());
        }
        self.prepared_generation.set(Some(generation));
        Ok(())
    }
    pub(super) fn final_control_current(&self, generation: u64) -> bool {
        self.prepared_generation.get() == Some(generation) && self.held_is_current(generation)
    }
    fn retain_native_lifecycle(&self, handler: &Block<dyn Fn()>) -> Option<OwnedNativeCallback> {
        // Native progress invalidates preparation; it never proves completion or
        // cancellation. No RefCell borrow spans copy/retain or release reentry.
        self.native_lifecycle_started.set(true);
        self.quarantine();
        let revision = self.lifecycle_revision.get().checked_add(1)?;
        self.lifecycle_revision.set(revision);
        let generation = self.held_generation();
        let item = self.item.borrow().as_ref().map(Retained::as_ptr);
        let handler = unsafe { RcBlock::copy((handler as *const Block<dyn Fn()>).cast_mut()) }?;
        let updater = unsafe { Retained::retain(self.updater.get().cast_mut()) }?;
        let item = match item {
            Some(item) => Some(unsafe { Retained::retain(item.cast_mut()) }?),
            None => None,
        };
        if self.lifecycle_revision.get() != revision
            || self.held_generation() != generation
            || Retained::as_ptr(&updater) != self.updater.get()
            || item.as_ref().map(Retained::as_ptr)
                != self.item.borrow().as_ref().map(Retained::as_ptr)
        {
            return None;
        }
        Some(OwnedNativeCallback {
            updater,
            item,
            ready_generation: generation,
            handler,
            revision,
        })
    }
    fn retain_installing(&self, application_terminated: bool, retry: &Block<dyn Fn()>) {
        let Some(owner) = self.retain_native_lifecycle(retry) else {
            return;
        };
        let previous = self.installing.borrow_mut().replace(HeldInstalling {
            owner,
            application_terminated,
        });
        drop(previous);
    }
    fn retain_installed(&self, relaunched: bool, acknowledgement: &Block<dyn Fn()>) {
        let Some(owner) = self.retain_native_lifecycle(acknowledgement) else {
            return;
        };
        let previous = self
            .installed
            .borrow_mut()
            .replace(HeldInstalled { owner, relaunched });
        drop(previous);
    }
    fn retain_found(&self, item: &AnyObject, reply: &Block<dyn Fn(isize)>, downloadable: bool) {
        if self.found.borrow().is_some() || self.choice_sent.get() || !self.has_owner() {
            self.quarantine();
            return;
        }
        let Some(generation) = self.found_revision.get().checked_add(1) else {
            self.quarantine();
            return;
        };
        self.found_revision.set(generation);
        // No RefCell borrow spans Objective-C retain/copy or callback reentry.
        let copied = unsafe { RcBlock::copy((reply as *const Block<dyn Fn(isize)>).cast_mut()) };
        let updater = unsafe { Retained::retain(self.updater.get().cast_mut()) };
        let item = unsafe { Retained::retain((item as *const AnyObject).cast_mut()) };
        let (Some(reply), Some(updater), Some(item)) = (copied, updater, item) else {
            self.quarantine();
            return;
        };
        if self.found_revision.get() != generation
            || self.choice_sent.get()
            || self.view().cancelled
            || self.view().quarantine
            || self.view().phase != Phase::Found
        {
            self.quarantine();
            return;
        }
        let previous = self.found.borrow_mut().replace(HeldFound {
            updater,
            item,
            generation,
            downloadable,
        });
        let choice = self.choice.borrow_mut().replace(reply);
        drop(previous);
        drop(choice);
    }
    pub(super) fn found_generation(&self) -> Option<u64> {
        if self.choice_sent.get()
            || self.choice.borrow().is_none()
            || self.view().phase != Phase::Found
            || self.view().cancelled
            || self.view().quarantine
        {
            return None;
        }
        self.found
            .borrow()
            .as_ref()
            .filter(|held| held.downloadable)
            .map(|held| held.generation)
    }
    fn take_download(&self, generation: u64) -> Result<RcBlock<dyn Fn(isize)>, String> {
        if self.found_generation() != Some(generation)
            || !self.view().native_cycle
            || self.download_generation.get().is_some()
            || self.held_ready.borrow().is_some()
        {
            return Err("The offered native update changed; download remains held".into());
        }
        let reply = self
            .choice
            .borrow_mut()
            .take()
            .ok_or("The offered native update has no owned reply")?;
        self.choice_sent.set(true);
        // Consume the exact generation before invoking a reentrant native block.
        self.download_generation.set(Some(generation));
        Ok(reply)
    }
    pub(super) fn download(&self, generation: u64) -> Result<(), String> {
        let (updater, item) = {
            let held = self.found.borrow();
            let held = held
                .as_ref()
                .filter(|held| held.generation == generation)
                .ok_or("The offered native update changed")?;
            (
                Retained::as_ptr(&held.updater),
                Retained::as_ptr(&held.item),
            )
        };
        // HeldFound is never retired by Cancel, Ready, EOF or cycle completion.
        // Its retained objects therefore survive these read-only SDK calls.
        // The actual Found callback admits only manual NotDownloaded. Pinned
        // SPUUIBasedUpdateDriver captures that stage in this exact reply block;
        // it cannot become a resumed/installing choice while being retained.
        let signed: isize = unsafe { msg_send![item, signingValidationStatus] };
        let information_only: Bool = unsafe { msg_send![item, isInformationOnlyUpdate] };
        if updater != self.updater.get() || signed != 1 || information_only.as_bool() {
            return Err("The offered native update cannot be downloaded".into());
        }
        let reply = self.take_download(generation)?;
        // Pinned Found+NotDownloaded choice1 starts download/extraction, unlike
        // Ready choice1 which can install. No Ready handler is invoked here.
        reply.call((1_isize,));
        self.repaint();
        Ok(())
    }
    pub(super) fn will_download(&self, updater: &AnyObject, item: &AnyObject) {
        let matches = self.found.borrow().as_ref().is_some_and(|held| {
            Retained::as_ptr(&held.updater) == updater as *const AnyObject
                && Retained::as_ptr(&held.item) == item as *const AnyObject
                && self.download_generation.get() == Some(held.generation)
        });
        if !matches
            || !self.owns(updater)
            || self.view().cancelled
            || self.view().quarantine
            || self.download_item_seen.replace(true)
        {
            // Sparkle may fall back to a secondary delta item without Found.
            // It cannot inherit the first item's explicit human selection.
            self.quarantine();
        }
    }
    pub(super) fn did_download(&self, updater: &AnyObject, item: &AnyObject) {
        let matches = self.found.borrow().as_ref().is_some_and(|held| {
            Retained::as_ptr(&held.item) == item as *const AnyObject
                && self.download_generation.get() == Some(held.generation)
        });
        if !self.owns(updater)
            || !matches
            || !self.download_started.get()
            || !self.download_item_seen.get()
            || self.view().cancelled
            || self.view().quarantine
        {
            self.quarantine();
            return;
        }
        self.download_complete.set(true);
    }
    fn downloading(&self) -> bool {
        self.download_generation.get().is_some()
            && self.download_item_seen.get()
            && self.download_started.get()
            && self.extraction.get().is_none()
            && !self.view().cancelled
            && !self.view().quarantine
    }
    pub(super) fn observed_status(&self) -> super::coordinator::NativeStatus {
        let state = self.view();
        super::coordinator::NativeStatus {
            phase: state.phase,
            version: state.version.clone(),
            busy: state.busy,
            cancelled: state.cancelled,
            quarantine: state.quarantine,
            native_cycle: state.native_cycle,
            held_generation: self.held_generation(),
            found_generation: self.found_generation(),
            download_generation: self.download_generation.get(),
            expected_bytes: self.expected_bytes.get(),
            received_bytes: self.received_bytes.get(),
            extraction: self.extraction.get(),
            download_complete: self.download_complete.get(),
            final_profile: None,
        }
    }
    pub(super) fn observe_owner(&self, status: super::coordinator::NativeStatus) {
        let mut state = self.session.borrow_mut();
        // A remote observation cannot prove absence or release the local hold.
        if state.native_cycle && !status.native_cycle {
            state.quarantine();
            return;
        }
        state.native_cycle |= status.native_cycle;
        state.cancelled |= status.cancelled;
        if status.quarantine || state.quarantine {
            state.quarantine();
        } else if !state.cancelled {
            state.phase = status.phase;
            state.version = status.version;
        }
        state.busy = status.busy;
    }
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
    pub fn native_starting(&self) -> Result<(), String> {
        self.session.borrow_mut().native_starting()
    }
    pub fn native_start_failed(&self) {
        self.session.borrow_mut().native_start_failed();
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
        self.prepared_generation.set(None);
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
        self.control_requested.set(true);
        self.prepared_generation.set(None);
        self.session.borrow_mut().fail();
        self.repaint();
    }
    pub fn finished(&self) {
        self.control_requested.set(true);
        self.prepared_generation.set(None);
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
        self.prepared_generation.set(None);
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
            if !state.remember_item(item) { state.quarantine(); return; }
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
                let information_only: Bool=unsafe { msg_send![item, isInformationOnlyUpdate] };
                state.retain_found(item,reply,!information_only.as_bool());
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
        fn download(&self, cancellation: &Block<dyn Fn()>) {
            let state=self.ivars();
            if state.download_generation.get().is_none() || !state.download_item_seen.get()
                || state.download_started.replace(true) || state.view().quarantine {
                state.quarantine();
                if !state.download_cancelled.replace(true) { cancellation.call(()); }
                return;
            }
            let copied=unsafe { RcBlock::copy((cancellation as *const Block<dyn Fn()>).cast_mut()) };
            let Some(copied)=copied else { state.quarantine(); if !state.download_cancelled.replace(true) { cancellation.call(()); } return; };
            state.cancellation_sent.set(false);
            let previous=state.cancellation.borrow_mut().replace(copied); drop(previous);
            if state.view().cancelled { state.cancel(); } else { state.repaint(); }
        }
        #[unsafe(method(showDownloadDidReceiveExpectedContentLength:))]
        fn length(&self, length: u64) {
            let state=self.ivars();
            if state.downloading() { state.expected_bytes.set((length!=0).then_some(length)); state.repaint(); }
            else if !state.view().cancelled { state.quarantine(); }
        }
        #[unsafe(method(showDownloadDidReceiveDataOfLength:))]
        fn data(&self, length: u64) {
            let state=self.ivars();
            if state.downloading() {
                if let Some(total)=state.received_bytes.get().checked_add(length) { state.received_bytes.set(total); state.repaint(); }
                else { state.quarantine(); }
            } else if !state.view().cancelled { state.quarantine(); }
        }
        #[unsafe(method(showDownloadDidStartExtractingUpdate))]
        fn extracting(&self) {
            let state=self.ivars();
            state.cancellation_sent.set(true);
            let cancellation=state.cancellation.borrow_mut().take(); drop(cancellation);
            if state.download_generation.get().is_none() || !state.download_complete.get()
                || state.extraction.replace(Some(0.)).is_some() || state.view().cancelled || state.view().quarantine {
                state.quarantine();
            }
            state.repaint();
        }
        #[unsafe(method(showExtractionReceivedProgress:))]
        fn extracting_progress(&self, progress: f64) {
            let state=self.ivars();
            if state.download_generation.get().is_none() || state.extraction.get().is_none()
                || !progress.is_finite() || !(0.0..=1.0).contains(&progress) { state.quarantine(); }
            else if !state.view().cancelled && !state.view().quarantine { state.extraction.set(Some(progress)); state.repaint(); }
        }
        #[unsafe(method(showReadyToInstallAndRelaunch:))]
        fn install(&self, reply: &Block<dyn Fn(isize)>) { self.ivars().retain_ready(reply); }
        #[unsafe(method(showInstallingUpdateWithApplicationTerminated:retryTerminatingApplication:))]
        fn installing(&self, terminated: Bool, retry: &Block<dyn Fn()>) { self.ivars().retain_installing(terminated.as_bool(), retry); }
        #[unsafe(method(showUpdateInstalledAndRelaunched:acknowledgement:))]
        fn installed(&self, relaunched: Bool, acknowledgement: &Block<dyn Fn()>) { self.ivars().retain_installed(relaunched.as_bool(), acknowledgement); }
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
    assert_eq!(
        state.held_generation(),
        None,
        "An unbound callback supplies no owner generation"
    );
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

/// Actual public selectors on an explicitly unstarted component. These retained
/// callback facts never become permission to install, terminate or retire evidence.
pub(super) fn assert_owned_final_control(marker: MainThreadMarker, updater: &AnyObject) {
    let calls = Rc::new(Cell::new(0));
    for change in [
        "none", "cancel", "fail", "finish", "reissue", "updater", "item",
    ] {
        let state = Rc::new(State::default());
        state.begin_in_owner(updater).unwrap();
        let driver = create(marker, state.clone());
        let item: Retained<AnyObject> =
            unsafe { msg_send![class!(SUAppcastItem), emptyAppcastItem] };
        assert!(state.remember_item(&item));
        let count = calls.clone();
        let ready = RcBlock::new(move |_choice: isize| count.set(count.get() + 1));
        unsafe {
            let _: () = msg_send![&*driver, showReadyToInstallAndRelaunch: &*ready];
        }
        state.prepare_final_control(1).unwrap();
        assert!(state.final_control_current(1));
        match change {
            "cancel" => state.cancel(),
            "fail" => state.failed(),
            "finish" => state.finished(),
            "reissue" => unsafe {
                let _: () = msg_send![&*driver, showReadyToInstallAndRelaunch: &*ready];
            },
            "updater" => {
                let foreign: Retained<AnyObject> = unsafe { msg_send![class!(NSObject), new] };
                state.updater.set(Retained::as_ptr(&foreign));
                assert!(!state.final_control_current(1));
                state.updater.set(updater);
                state.quarantine();
            }
            "item" => {
                let foreign: Retained<AnyObject> = unsafe { msg_send![class!(SUAppcastItem), new] };
                let old = state.item.borrow_mut().replace(foreign);
                drop(old);
            }
            _ => {}
        }
        assert_eq!(state.final_control_current(1), change == "none", "{change}");
        assert!(
            state.prepare_final_control(1).is_err(),
            "Every request remains one-shot"
        );
        assert!(state.view().native_cycle && state.view().quarantine);
        assert_eq!(calls.get(), 0);
    }
    // Failure or cycle completion before a request invalidates preparation
    // without releasing the unknown native owner or retained Ready callback.
    for failure in [false, true] {
        let ended = Rc::new(State::default());
        ended.begin_in_owner(updater).unwrap();
        let ended_driver = create(marker, ended.clone());
        let item: Retained<AnyObject> =
            unsafe { msg_send![class!(SUAppcastItem), emptyAppcastItem] };
        assert!(ended.remember_item(&item));
        let count = calls.clone();
        let ready = RcBlock::new(move |_choice: isize| count.set(count.get() + 1));
        unsafe {
            let _: () = msg_send![&*ended_driver, showReadyToInstallAndRelaunch: &*ready];
        }
        if failure {
            ended.failed();
        } else {
            ended.finished();
        }
        assert!(ended.prepare_final_control(1).is_err());
        assert!(ended.held_ready.borrow().is_some());
        assert!(ended.view().native_cycle && ended.view().quarantine);
    }
    struct CancelOnDrop(std::rc::Weak<State>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            if let Some(state) = self.0.upgrade() {
                state.cancel();
            }
        }
    }
    let state = Rc::new(State::default());
    state.begin_in_owner(updater).unwrap();
    let driver = create(marker, state.clone());
    let item: Retained<AnyObject> = unsafe { msg_send![class!(SUAppcastItem), emptyAppcastItem] };
    assert!(state.remember_item(&item));
    let count = calls.clone();
    let ready = RcBlock::new(move |_choice: isize| count.set(count.get() + 1));
    unsafe {
        let _: () = msg_send![&*driver, showReadyToInstallAndRelaunch: &*ready];
    }
    state.prepare_final_control(1).unwrap();
    let reenter = CancelOnDrop(Rc::downgrade(&state));
    let count = calls.clone();
    let retry = RcBlock::new(move || {
        let _ = &reenter;
        count.set(count.get() + 1);
    });
    unsafe {
        let _: () = msg_send![&*driver, showInstallingUpdateWithApplicationTerminated: Bool::NO, retryTerminatingApplication: &*retry];
    }
    assert!(!state.final_control_current(1));
    {
        let held = state.installing.borrow();
        let held = held.as_ref().unwrap();
        assert!(!held.application_terminated);
        assert_eq!(
            Retained::as_ptr(&held.owner.updater),
            updater as *const AnyObject
        );
        assert_eq!(
            held.owner.item.as_ref().map(Retained::as_ptr),
            Some(Retained::as_ptr(&item))
        );
        assert_eq!(held.owner.ready_generation, Some(1));
        assert_eq!(held.owner.revision, 1);
        assert!(std::ptr::eq::<Block<dyn Fn()>>(
            &*held.owner.handler,
            &*retry
        ));
    }
    drop(retry);
    let count = calls.clone();
    let replacement = RcBlock::new(move || count.set(count.get() + 1));
    unsafe {
        let _: () = msg_send![&*driver, showInstallingUpdateWithApplicationTerminated: Bool::YES, retryTerminatingApplication: &*replacement];
    }
    assert!(
        state.view().cancelled,
        "Old callback release reenters after publishing and releasing the slot borrow"
    );
    {
        let held = state.installing.borrow();
        let held = held.as_ref().unwrap();
        assert!(held.application_terminated);
        assert_eq!(held.owner.revision, 2);
        assert!(std::ptr::eq::<Block<dyn Fn()>>(
            &*held.owner.handler,
            &*replacement
        ));
    }
    let count = calls.clone();
    let acknowledgement = RcBlock::new(move || count.set(count.get() + 1));
    unsafe {
        let _: () = msg_send![&*driver, showUpdateInstalledAndRelaunched: Bool::YES, acknowledgement: &*acknowledgement];
    }
    {
        let held = state.installed.borrow();
        let held = held.as_ref().unwrap();
        assert!(held.relaunched);
        assert_eq!(
            Retained::as_ptr(&held.owner.updater),
            updater as *const AnyObject
        );
        assert_eq!(
            held.owner.item.as_ref().map(Retained::as_ptr),
            Some(Retained::as_ptr(&item))
        );
        assert_eq!(held.owner.ready_generation, Some(1));
        assert_eq!(held.owner.revision, 3);
        assert!(std::ptr::eq::<Block<dyn Fn()>>(
            &*held.owner.handler,
            &*acknowledgement
        ));
    }
    state.lifecycle_revision.set(u64::MAX);
    unsafe {
        let _: () = msg_send![&*driver, showInstallingUpdateWithApplicationTerminated: Bool::NO, retryTerminatingApplication: &*replacement];
    }
    assert!(
        state
            .installing
            .borrow()
            .as_ref()
            .unwrap()
            .application_terminated
    );
    state.cancel();
    state.finished();
    assert!(
        state.held_ready.borrow().is_some()
            && state.installing.borrow().is_some()
            && state.installed.borrow().is_some()
    );
    assert!(state.view().native_cycle && state.view().quarantine);
    assert!(!state.final_control_current(1));
    assert_eq!(
        calls.get(),
        0,
        "No Ready/retry/acknowledgement handler is invoked"
    );
    let checking: Bool = unsafe { msg_send![updater, canCheckForUpdates] };
    assert!(
        !checking.as_bool(),
        "Cleanup belongs only to the known-unstarted component"
    );
    let installing = state.installing.borrow_mut().take();
    let installed = state.installed.borrow_mut().take();
    let ready = state.held_ready.borrow_mut().take();
    drop(installing);
    drop(installed);
    drop(ready);
}

/// Known-unstarted component injection, not a configured update or Ready result.
/// The real constructed controller and native selector own the exact retained
/// block/item; no callback is invoked and no SDK check is started.
pub(super) fn assert_owned_ready(marker: MainThreadMarker, updater: &AnyObject) {
    let state = Rc::new(State::default());
    state.begin_in_owner(updater).unwrap();
    let driver = create(marker, state.clone());
    let item: Retained<AnyObject> = unsafe { msg_send![class!(SUAppcastItem), emptyAppcastItem] };
    assert!(state.remember_item(&item));
    let calls = Rc::new(Cell::new(0));
    let counter = calls.clone();
    let ready = RcBlock::new(move |_choice: isize| counter.set(counter.get() + 1));
    let unbound_item = Rc::new(State::default());
    unbound_item.begin_in_owner(updater).unwrap();
    let unbound_driver = create(marker, unbound_item.clone());
    unsafe {
        let _: () = msg_send![&*unbound_driver, showReadyToInstallAndRelaunch: &*ready];
    }
    unbound_item.cancel();
    unbound_item.finished();
    assert!(
        unbound_item.held_ready.borrow().is_some(),
        "Missing item identity must retain the real callback as unknown"
    );
    assert_eq!(
        unbound_item.held_generation(),
        None,
        "An unknown item cannot produce a bound generation"
    );
    assert!(unbound_item.view().native_cycle && unbound_item.view().quarantine);
    let old_calls = Rc::new(Cell::new(0));
    let counter = old_calls.clone();
    let checking = RcBlock::new(move || counter.set(counter.get() + 1));
    unsafe {
        let _: () = msg_send![&*driver, showUserInitiatedUpdateCheckWithCancellation: &*checking];
    }
    let counter = old_calls.clone();
    *state.choice.borrow_mut() = Some(RcBlock::new(move |_choice: isize| {
        counter.set(counter.get() + 1)
    }));
    for generation in [1, 2] {
        unsafe {
            let _: () = msg_send![&*driver, showReadyToInstallAndRelaunch: &*ready];
        }
        assert_eq!(state.held_generation(), Some(generation));
        assert_eq!(state.held_is_current(generation), generation == 1);
        assert!(!state.held_is_current(0));
        assert!(!state.held_is_current(generation + 1));
        {
            let held = state.held_ready.borrow();
            let held = held.as_ref().unwrap();
            assert_eq!(
                Retained::as_ptr(&held._updater),
                updater as *const AnyObject
            );
            assert_eq!(
                held._item.as_ref().map(Retained::as_ptr),
                Some(Retained::as_ptr(&item))
            );
        }
        state.cancel();
        state.finished();
        assert!(!state.held_is_current(generation));
        assert!(state.view().native_cycle && state.view().quarantine);
        assert_eq!(state.held_generation(), Some(generation));
        assert_eq!(calls.get(), 0);
        assert_eq!(
            old_calls.get(),
            0,
            "Ready retains its own handler; old check/Found handlers cannot cancel it"
        );
    }
    // The SDK's emptyAppcastItem is a singleton; alloc/init is required for a
    // genuinely different native identity in this explicitly unsigned fixture.
    let other: Retained<AnyObject> = unsafe { msg_send![class!(SUAppcastItem), new] };
    assert_ne!(Retained::as_ptr(&other), Retained::as_ptr(&item));
    assert!(!state.remember_item(&other));
    assert_eq!(
        state.item.borrow().as_ref().map(Retained::as_ptr),
        Some(Retained::as_ptr(&item))
    );
    state.ready_revision.set(u64::MAX);
    unsafe {
        let _: () = msg_send![&*driver, showReadyToInstallAndRelaunch: &*ready];
    }
    assert_eq!(state.held_generation(), None);
    assert!(
        state.held_ready.borrow().is_some(),
        "Counter exhaustion retains the actual callback as unknown"
    );
    assert_eq!(calls.get(), 0);
    let can_check: Bool = unsafe { msg_send![updater, canCheckForUpdates] };
    assert!(
        !can_check.as_bool(),
        "Only this explicitly unstarted component may clean up its injected blocks"
    );
    let unknown = unbound_item.held_ready.borrow_mut().take();
    drop(unknown);
    let held = state.held_ready.borrow_mut().take();
    let item = state.item.borrow_mut().take();
    drop(held);
    drop(item);
    assert_eq!(calls.get(), 0);
}

/// Known-unstarted callback/ownership fixture. It invokes only a synthetic Found
/// counter, never an SDK Found/Ready reply or a native check/download/install.
pub(super) fn assert_owned_download(marker: MainThreadMarker, root: &std::path::Path) {
    for mode in [
        "complete",
        "cancel-before-callback",
        "foreign",
        "overflow",
        "progress",
    ] {
        let state = Rc::new(State::default());
        let owner = super::construct_with_state(root, marker, state.clone()).unwrap();
        state.begin_in_owner(&owner.controller).unwrap();
        let item: Retained<AnyObject> =
            unsafe { msg_send![class!(SUAppcastItem), emptyAppcastItem] };
        assert!(state.remember_item(&item));
        assert!(state
            .session
            .borrow_mut()
            .found("0.30.1".into(), "component only".into()));
        let selections = Rc::new(Cell::new(0));
        let count = selections.clone();
        let weak = Rc::downgrade(&state);
        let found = RcBlock::new(move |choice: isize| {
            assert_eq!(choice, 1);
            assert!(weak.upgrade().unwrap().take_download(1).is_err());
            count.set(count.get() + 1);
        });
        state.retain_found(&item, &found, true);
        assert_eq!(state.found_generation(), Some(1));
        assert!(state.take_download(2).is_err());
        assert!(
            state.download(1).is_err(),
            "An unsigned SDK fixture cannot pass native selection validation"
        );
        assert_eq!(state.found_generation(), Some(1));
        assert_eq!(selections.get(), 0);
        let selected = state.take_download(1).unwrap();
        selected.call((1,));
        assert_eq!(selections.get(), 1);
        assert!(state.take_download(1).is_err());
        assert_eq!(state.found_generation(), None);
        let other: Retained<AnyObject> = unsafe { msg_send![class!(SUAppcastItem), new] };
        // This unused argument is an opaque component object, not a network request.
        let download_item = if mode == "foreign" { &*other } else { &*item };
        unsafe {
            let _: () = msg_send![&*owner.delegate, updater: &*owner.controller,
                willDownloadUpdate: download_item, withRequest: &*item];
        }
        if mode == "cancel-before-callback" {
            state.cancel();
        }
        let cancellations = Rc::new(Cell::new(0));
        let count = cancellations.clone();
        let cancellation = RcBlock::new(move || count.set(count.get() + 1));
        unsafe {
            let _: () =
                msg_send![&*owner._driver, showDownloadInitiatedWithCancellation: &*cancellation];
        }
        if matches!(mode, "foreign" | "cancel-before-callback") {
            assert_eq!(cancellations.get(), 1);
            assert!(!state.download_complete.get());
        } else {
            unsafe {
                let _: () =
                    msg_send![&*owner._driver, showDownloadDidReceiveExpectedContentLength: 0_u64];
                let _: () = msg_send![&*owner._driver, showDownloadDidReceiveExpectedContentLength: 4096_u64];
                let _: () =
                    msg_send![&*owner._driver, showDownloadDidReceiveDataOfLength: 2048_u64];
            }
            assert_eq!(state.expected_bytes.get(), Some(4096));
            assert_eq!(state.received_bytes.get(), 2048);
            if mode == "overflow" {
                unsafe {
                    let _: () =
                        msg_send![&*owner._driver, showDownloadDidReceiveDataOfLength: u64::MAX];
                }
                assert!(state.view().quarantine);
            } else {
                unsafe {
                    let _: () =
                        msg_send![&*owner._driver, showDownloadDidReceiveDataOfLength: 2048_u64];
                    let _: () = msg_send![&*owner.delegate, updater: &*owner.controller, didDownloadUpdate: &*item];
                    let _: () = msg_send![&*owner._driver, showDownloadDidStartExtractingUpdate];
                    let _: () = msg_send![&*owner._driver, showExtractionReceivedProgress: 0.5_f64];
                }
                assert!(state.download_complete.get());
                assert_eq!(state.extraction.get(), Some(0.5));
                if mode == "progress" {
                    unsafe {
                        let _: () =
                            msg_send![&*owner._driver, showExtractionReceivedProgress: f64::NAN];
                    }
                    assert!(state.view().quarantine);
                }
                if mode != "complete" {
                    state.cancel();
                }
                assert_eq!(
                    cancellations.get(),
                    0,
                    "Download cancellation is invalid after extraction begins"
                );
            }
        }
        let installs = Rc::new(Cell::new(0));
        let count = installs.clone();
        let ready = RcBlock::new(move |_choice: isize| count.set(count.get() + 1));
        unsafe {
            let _: () = msg_send![&*owner._driver, showReadyToInstallAndRelaunch: &*ready];
        }
        let generation = state.held_generation().unwrap();
        state.cancel();
        state.finished();
        assert_eq!(state.held_generation(), Some(generation));
        assert!(state.view().quarantine && state.view().native_cycle);
        assert!(state.session.borrow_mut().begin().is_err());
        assert_eq!(installs.get(), 0);
        assert_eq!(selections.get(), 1);
        let can_check: Bool = unsafe { msg_send![&*owner.controller, canCheckForUpdates] };
        assert!(
            !can_check.as_bool(),
            "Only these constructed, never-started component callbacks may be released"
        );
        let held = state.held_ready.borrow_mut().take();
        let choice = state.choice.borrow_mut().take();
        let cancellation = state.cancellation.borrow_mut().take();
        let found = state.found.borrow_mut().take();
        let item = state.item.borrow_mut().take();
        drop(held);
        drop(choice);
        drop(cancellation);
        drop(found);
        drop(item);
    }
}
