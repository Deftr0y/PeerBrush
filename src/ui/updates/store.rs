//! Read-only native provider availability for this package on its owner window thread.
use eframe::egui;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(25);
#[cfg(any(windows, test))]
const MAX_UPDATES: usize = 64;
#[cfg(any(windows, test))]
const MAX_IDENTITY: usize = 1024;

// Native direct identity/provider and installed acceptance remain unqualified.
// Public configuration alone cannot enable this prepared information-only path.
const APP_INSTALLER_CHECK_ENABLED: bool = false;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Provider {
    #[default]
    Store,
    AppInstaller,
}
impl Provider {
    fn text(self, store: &'static str, app_installer: &'static str) -> &'static str {
        match self {
            Self::Store => store,
            Self::AppInstaller => app_installer,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Availability {
    NoUpdate,
    Available { mandatory: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    #[cfg(any(not(windows), test))]
    Unsupported,
    #[cfg(any(windows, test))]
    Window,
    #[cfg(any(windows, test))]
    Identity,
    Service,
    #[cfg(any(windows, test))]
    Result,
    Timeout,
    Cancelled,
    Disabled,
}
impl Failure {
    fn message(self) -> &'static str {
        match self {
            #[cfg(any(not(windows), test))]
            Self::Unsupported => "Native Store checking is unavailable on this platform.",
            #[cfg(any(windows, test))]
            Self::Window => "Store checking requires this app's current Windows window and thread.",
            #[cfg(any(windows, test))]
            Self::Identity => "The current Store package identity could not be verified.",
            Self::Service => "Microsoft Store availability is unknown; its service could not complete the check.",
            #[cfg(any(windows, test))]
            Self::Result => "Microsoft Store returned an unsupported or ambiguous package result; availability is unknown.",
            Self::Timeout => "The Store check timed out; availability is unknown.",
            Self::Cancelled => "The Store check was cancelled; availability is unknown.",
            Self::Disabled => "App Installer checking is unavailable in this build.",
        }
    }
    fn for_provider(self, provider: Provider) -> &'static str {
        if provider == Provider::Store {
            return self.message();
        }
        match self {
            #[cfg(any(not(windows), test))]
            Self::Unsupported => "Native App Installer checking is unavailable on this platform.",
            #[cfg(any(windows, test))]
            Self::Window => {
                "App Installer checking requires this app's current Windows window and thread."
            }
            #[cfg(any(windows, test))]
            Self::Identity => {
                "The current App Installer package association could not be verified."
            }
            Self::Service => {
                "App Installer availability is unknown; its service could not complete the check."
            }
            #[cfg(any(windows, test))]
            Self::Result => {
                "App Installer returned an unsupported or failed result; availability is unknown."
            }
            Self::Timeout => "The App Installer check timed out; availability is unknown.",
            Self::Cancelled => "The App Installer check was cancelled; availability is unknown.",
            Self::Disabled => self.message(),
        }
    }
}

#[cfg(any(windows, test))]
fn app_installer_availability(value: i32, extended_error: i32) -> Result<Availability, Failure> {
    // Unknown, Error, future enum values and even nonzero success HRESULTs are
    // never interpreted as no update. Availability supplies no offered version.
    if extended_error != 0 {
        return Err(Failure::Result);
    }
    match value {
        1 => Ok(Availability::NoUpdate),
        2 => Ok(Availability::Available { mandatory: false }),
        3 => Ok(Availability::Available { mandatory: true }),
        _ => Err(Failure::Result),
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    // Exact UTF-16, never rendered, logged or written to preferences.
    full: Vec<u16>,
    family: Vec<u16>,
}
#[cfg(any(windows, test))]
impl Identity {
    fn valid(&self) -> bool {
        !self.full.is_empty()
            && self.full.len() <= MAX_IDENTITY
            && !self.family.is_empty()
            && self.family.len() <= MAX_IDENTITY
    }
}
#[cfg(any(windows, test))]
#[derive(Clone)]
struct Candidate {
    identity: Identity,
    mandatory: bool,
}
#[cfg(any(windows, test))]
fn availability(expected: &Identity, candidates: &[Candidate]) -> Result<Availability, Failure> {
    if !expected.valid() || candidates.len() > MAX_UPDATES {
        return Err(Failure::Result);
    }
    let mut found = None;
    for candidate in candidates {
        if !candidate.identity.valid() {
            return Err(Failure::Result);
        }
        if candidate.identity.full == expected.full || candidate.identity.family == expected.family
        {
            // The public Package contract does not promise installed vs offered version.
            // A changed full name within our family is unknown, never "no update".
            if candidate.identity != *expected || found.is_some() {
                return Err(Failure::Result);
            }
            found = Some(candidate.mandatory);
        }
    }
    Ok(match found {
        Some(mandatory) => Availability::Available { mandatory },
        None => Availability::NoUpdate,
    })
}

#[derive(Clone, Copy)]
enum Observation {
    Pending,
    Finished(Result<Availability, Failure>),
    Unavailable,
}
// One native operation boundary, also used by deterministic portable UI fixtures.
trait Operation {
    fn poll(&mut self) -> Observation;
    fn cancel(&mut self);
}
struct Pending {
    operation: Box<dyn Operation>,
    deadline: Instant,
    rejected: bool,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.operation.cancel();
    }
}

#[derive(Default)]
pub(super) struct Check {
    provider: Provider,
    pending: Option<Pending>,
    result: Option<Result<Availability, Failure>>,
    #[cfg(windows)]
    window: Option<native::Window>,
    #[cfg(test)]
    next: Option<Box<dyn Operation>>,
}
impl Check {
    pub(super) fn app_installer() -> Self {
        Self {
            provider: Provider::AppInstaller,
            ..Self::default()
        }
    }
    fn allowed(&self) -> bool {
        #[cfg(test)]
        if self.next.is_some() {
            return true;
        }
        self.provider == Provider::Store || APP_INSTALLER_CHECK_ENABLED
    }
    #[cfg(windows)]
    pub(super) fn window(&mut self, frame: &eframe::Frame) {
        use eframe::wgpu::rwh::{HasWindowHandle, RawWindowHandle};
        let window = frame.window_handle().ok().and_then(|handle| {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                Some(native::Window {
                    hwnd: handle.hwnd.get(),
                    thread: std::thread::current().id(),
                })
            } else {
                None
            }
        });
        if self.window != window {
            if self.pending.is_some() || self.result.is_some() {
                self.reject(Failure::Window);
            }
            self.window = window;
        }
    }
    fn begin(&mut self, operation: Box<dyn Operation>, now: Instant) {
        self.result = None;
        self.pending = Some(Pending {
            operation,
            deadline: now + TIMEOUT,
            rejected: false,
        });
    }
    fn start(&mut self, now: Instant) {
        if self.pending.is_some() {
            return;
        }
        self.result = None;
        #[cfg(test)]
        if let Some(operation) = self.next.take() {
            self.begin(operation, now);
            return;
        }
        if !self.allowed() {
            self.result = Some(Err(Failure::Disabled));
            return;
        }
        #[cfg(windows)]
        match self.window.ok_or(Failure::Window).and_then(
            |window| -> Result<Box<dyn Operation>, Failure> {
                match self.provider {
                    Provider::Store => {
                        native::Native::start(window).map(|value| Box::new(value) as _)
                    }
                    Provider::AppInstaller => {
                        native::AppInstaller::start(window).map(|value| Box::new(value) as _)
                    }
                }
            },
        ) {
            Ok(operation) => self.begin(operation, now),
            Err(error) => self.result = Some(Err(error)),
        }
        #[cfg(not(windows))]
        {
            let _ = now;
            self.result = Some(Err(Failure::Unsupported));
        }
    }
    fn reject(&mut self, error: Failure) {
        self.result = Some(Err(error));
        if let Some(pending) = &mut self.pending {
            if !pending.rejected {
                pending.rejected = true;
                pending.operation.cancel();
            }
        }
    }
    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn cancel(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.rejected)
        {
            self.reject(Failure::Cancelled);
        }
    }
    pub(super) fn tick(&mut self, now: Instant) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.rejected && now >= pending.deadline)
        {
            self.reject(Failure::Timeout);
        }
        let Some(pending) = &mut self.pending else {
            return;
        };
        let observation = pending.operation.poll();
        // Native calls themselves are synchronous COM boundaries, not a process
        // deadline. A late completed call still cannot publish a timed-out result.
        if !pending.rejected && Instant::now() >= pending.deadline {
            self.reject(Failure::Timeout);
        }
        let pending = self
            .pending
            .as_ref()
            .expect("Pending native provider operation");
        match observation {
            Observation::Pending => (),
            Observation::Unavailable => {
                if !pending.rejected {
                    self.reject(Failure::Service);
                }
            }
            Observation::Finished(result) => {
                if !pending.rejected {
                    self.result = Some(result);
                }
                self.pending = None;
            }
        }
    }
    pub(super) fn show(&mut self, ui: &mut egui::Ui) {
        ui.label(self.provider.text(
            "Installed Microsoft Store package",
            "Associated Windows App Installer",
        ));
        if !self.allowed() && self.provider == Provider::AppInstaller {
            ui.label(Failure::Disabled.message());
        }
        if ui
            .add_enabled(
                !self.pending() && self.allowed(),
                egui::Button::new(
                    self.provider
                        .text("Check Microsoft Store", "Check App Installer"),
                ),
            )
            .clicked()
        {
            self.start(Instant::now());
        }
        if let Some(pending) = &self.pending {
            if pending.rejected {
                ui.label(self.provider.text(
                    "Cancellation requested; waiting for Microsoft Store before another check.",
                    "Cancellation requested; waiting for App Installer before another check.",
                ));
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(self.provider.text(
                        "Checking this installed package with Microsoft Store…",
                        "Checking the associated App Installer for this installed package…",
                    ));
                });
                if ui
                    .button(
                        self.provider
                            .text("Cancel Store check", "Cancel App Installer check"),
                    )
                    .clicked()
                {
                    self.cancel();
                }
            }
        }
        if let Some(result) = self.result {
            match result {
                Ok(Availability::NoUpdate) => {
                    ui.label(self.provider.text("Microsoft Store reports no update for this installed app package.", "The associated App Installer reports no update for this installed app package."));
                }
                Ok(Availability::Available { mandatory }) => {
                    ui.label(self.provider.text("Microsoft Store reports an update for this installed app package.", "The associated App Installer reports an update for this installed app package."));
                    if mandatory {
                        ui.label(self.provider.text("Microsoft Store marks that update as mandatory; this check does not install it.", "App Installer marks that update as required; this check does not install it."));
                    }
                }
                Err(error) => {
                    ui.label(error.for_provider(self.provider));
                }
            }
        }
        ui.label(self.provider.text("Store results may be cached or delayed. This does not verify public listing or installation readiness.", "Update availability only. Installation is unavailable in this build."));
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use windows::{
        core::{Interface, HSTRING},
        ApplicationModel::{Package, PackageSignatureKind, PackageUpdateAvailabilityResult},
        Foundation::{AsyncStatus, Collections::IVectorView, IAsyncOperation},
        Management::Deployment::PackageManager,
        Services::Store::{StoreContext, StorePackageUpdate},
        Win32::{
            Foundation::HWND,
            System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_SINGLETHREADED},
            UI::Shell::IInitializeWithWindow,
        },
    };
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) struct Window {
        pub hwnd: isize,
        pub thread: std::thread::ThreadId,
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }
    pub(super) struct Native {
        operation: IAsyncOperation<IVectorView<StorePackageUpdate>>,
        _context: StoreContext,
        expected: Identity,
        thread: std::thread::ThreadId,
        // Declared last: COM objects are released before this balanced uninitialize.
        _apartment: Apartment,
    }
    fn bounded(value: HSTRING) -> Result<Vec<u16>, Failure> {
        if value.is_empty() || value.len() > MAX_IDENTITY {
            return Err(Failure::Identity);
        }
        Ok(value.as_wide().to_vec())
    }
    fn identity(package: &Package) -> Result<Identity, Failure> {
        let id = package.Id().map_err(|_| Failure::Identity)?;
        Ok(Identity {
            full: bounded(id.FullName().map_err(|_| Failure::Identity)?)?,
            family: bounded(id.FamilyName().map_err(|_| Failure::Identity)?)?,
        })
    }
    fn current() -> Result<Identity, Failure> {
        let package = Package::Current().map_err(|_| Failure::Identity)?;
        if package.SignatureKind().map_err(|_| Failure::Identity)? != PackageSignatureKind::Store
            || package.IsFramework().map_err(|_| Failure::Identity)?
            || package.IsResourcePackage().map_err(|_| Failure::Identity)?
        {
            return Err(Failure::Identity);
        }
        identity(&package)
    }
    impl Native {
        pub(super) fn start(window: Window) -> Result<Self, Failure> {
            if window.hwnd == 0 || std::thread::current().id() != window.thread {
                return Err(Failure::Window);
            }
            // eframe's root Frame supplies the live HWND on its UI thread. The Store
            // method requires that thread and IInitializeWithWindow before invocation:
            // https://learn.microsoft.com/en-us/uwp/api/windows.services.store.storecontext.getappandoptionalstorepackageupdatesasync
            unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }.map_err(|_| Failure::Window)?;
            let apartment = Apartment;
            let expected = current()?;
            let context = StoreContext::GetDefault().map_err(|_| Failure::Service)?;
            let initialize: IInitializeWithWindow = context.cast().map_err(|_| Failure::Window)?;
            // Only the borrowed root HWND is passed; no window is created or adopted.
            unsafe { initialize.Initialize(HWND(window.hwnd as *mut _)) }
                .map_err(|_| Failure::Window)?;
            let operation = context
                .GetAppAndOptionalStorePackageUpdatesAsync()
                .map_err(|_| Failure::Service)?;
            Ok(Self {
                operation,
                _context: context,
                expected,
                thread: window.thread,
                _apartment: apartment,
            })
        }
        fn result(&self) -> Result<Availability, Failure> {
            if current()? != self.expected {
                return Err(Failure::Identity);
            }
            let values = self.operation.GetResults().map_err(|_| Failure::Service)?;
            let count = values.Size().map_err(|_| Failure::Result)?;
            if count as usize > MAX_UPDATES {
                return Err(Failure::Result);
            }
            let mut candidates = Vec::with_capacity(count as usize);
            for index in 0..count {
                let update = values.GetAt(index).map_err(|_| Failure::Result)?;
                let package = update.Package().map_err(|_| Failure::Result)?;
                candidates.push(Candidate {
                    identity: identity(&package).map_err(|_| Failure::Result)?,
                    mandatory: update.Mandatory().map_err(|_| Failure::Result)?,
                });
            }
            let result = availability(&self.expected, &candidates)?;
            if current()? != self.expected {
                return Err(Failure::Identity);
            }
            Ok(result)
        }
    }
    impl Operation for Native {
        fn poll(&mut self) -> Observation {
            if std::thread::current().id() != self.thread {
                return Observation::Unavailable;
            }
            match self.operation.Status() {
                Ok(AsyncStatus::Started) => Observation::Pending,
                Ok(AsyncStatus::Completed) => Observation::Finished(self.result()),
                Ok(AsyncStatus::Canceled) => Observation::Finished(Err(Failure::Cancelled)),
                Ok(AsyncStatus::Error) => Observation::Finished(Err(Failure::Service)),
                _ => Observation::Unavailable,
            }
        }
        fn cancel(&mut self) {
            let _ = self.operation.Cancel();
        }
    }
    impl Drop for Native {
        fn drop(&mut self) {
            let _ = self.operation.Cancel();
            let _ = self.operation.Close();
        }
    }

    #[derive(PartialEq, Eq)]
    struct Association {
        identity: Identity,
        signature: i32,
        // Private native UTF-16 only; never displayed, persisted or logged.
        uri: Vec<u16>,
    }
    fn association(package: &Package) -> Result<Association, Failure> {
        let signature = package.SignatureKind().map_err(|_| Failure::Identity)?;
        if !matches!(
            signature,
            PackageSignatureKind::Developer | PackageSignatureKind::Enterprise
        ) || package.IsFramework().map_err(|_| Failure::Identity)?
            || package.IsResourcePackage().map_err(|_| Failure::Identity)?
            || package.IsOptional().map_err(|_| Failure::Identity)?
        {
            return Err(Failure::Identity);
        }
        let uri = package
            .GetAppInstallerInfo()
            .and_then(|value| value.Uri())
            .map_err(|_| Failure::Identity)?;
        if uri.SchemeName().map_err(|_| Failure::Identity)?.to_string() != "https"
            || uri.Host().map_err(|_| Failure::Identity)?.is_empty()
            || !uri.UserName().map_err(|_| Failure::Identity)?.is_empty()
            || !uri.Password().map_err(|_| Failure::Identity)?.is_empty()
            || !uri.Query().map_err(|_| Failure::Identity)?.is_empty()
            || !uri.Fragment().map_err(|_| Failure::Identity)?.is_empty()
        {
            return Err(Failure::Identity);
        }
        let uri = uri.AbsoluteUri().map_err(|_| Failure::Identity)?;
        if uri.is_empty() || uri.len() > 4096 || uri.as_wide().iter().any(|value| *value < 32) {
            return Err(Failure::Identity);
        }
        Ok(Association {
            identity: identity(package)?,
            signature: signature.0,
            uri: uri.as_wide().to_vec(),
        })
    }
    fn current_association() -> Result<Association, Failure> {
        association(&Package::Current().map_err(|_| Failure::Identity)?)
    }
    pub(super) struct AppInstaller {
        operation: IAsyncOperation<PackageUpdateAvailabilityResult>,
        _package: Package,
        expected: Association,
        thread: std::thread::ThreadId,
        _apartment: Apartment,
    }
    impl AppInstaller {
        pub(super) fn start(window: Window) -> Result<Self, Failure> {
            if !APP_INSTALLER_CHECK_ENABLED {
                return Err(Failure::Disabled);
            }
            if window.hwnd == 0 || std::thread::current().id() != window.thread {
                return Err(Failure::Window);
            }
            unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }.map_err(|_| Failure::Window)?;
            let apartment = Apartment;
            let expected = current_association()?;
            let full =
                HSTRING::from_wide(&expected.identity.full).map_err(|_| Failure::Identity)?;
            let manager = PackageManager::new().map_err(|_| Failure::Service)?;
            // Microsoft's Current-object AccessDenied workaround, current user
            // and exact full name only; never enumerate other users or packages.
            let package = manager
                .FindPackageByUserSecurityIdPackageFullName(&HSTRING::new(), &full)
                .map_err(|_| Failure::Identity)?;
            // Release the lookup manager before the apartment moves into result.
            drop(manager);
            if association(&package)? != expected || current_association()? != expected {
                return Err(Failure::Identity);
            }
            let operation = package
                .CheckUpdateAvailabilityAsync()
                .map_err(|_| Failure::Service)?;
            let result = Self {
                operation,
                _package: package,
                expected,
                thread: window.thread,
                _apartment: apartment,
            };
            if current_association()? != result.expected
                || association(&result._package)? != result.expected
            {
                return Err(Failure::Identity);
            }
            Ok(result)
        }
        fn result(&self) -> Result<Availability, Failure> {
            if current_association()? != self.expected
                || association(&self._package)? != self.expected
            {
                return Err(Failure::Identity);
            }
            let result = self.operation.GetResults().map_err(|_| Failure::Service)?;
            let availability = app_installer_availability(
                result.Availability().map_err(|_| Failure::Result)?.0,
                result.ExtendedError().map_err(|_| Failure::Result)?.0,
            )?;
            if current_association()? != self.expected
                || association(&self._package)? != self.expected
            {
                return Err(Failure::Identity);
            }
            Ok(availability)
        }
    }
    impl Operation for AppInstaller {
        fn poll(&mut self) -> Observation {
            if std::thread::current().id() != self.thread {
                return Observation::Unavailable;
            }
            match self.operation.Status() {
                Ok(AsyncStatus::Started) => Observation::Pending,
                Ok(AsyncStatus::Completed) => Observation::Finished(self.result()),
                Ok(AsyncStatus::Canceled) => Observation::Finished(Err(Failure::Cancelled)),
                Ok(AsyncStatus::Error) => Observation::Finished(Err(Failure::Service)),
                _ => Observation::Unavailable,
            }
        }
        fn cancel(&mut self) {
            let _ = self.operation.Cancel();
        }
    }
    impl Drop for AppInstaller {
        fn drop(&mut self) {
            let _ = self.operation.Cancel();
            let _ = self.operation.Close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    #[derive(Default)]
    struct Fake {
        observations: VecDeque<Observation>,
        polls: usize,
        cancels: usize,
        drops: usize,
    }
    struct FakeOperation(Rc<RefCell<Fake>>);
    impl Operation for FakeOperation {
        fn poll(&mut self) -> Observation {
            let mut fake = self.0.borrow_mut();
            fake.polls += 1;
            fake.observations
                .pop_front()
                .unwrap_or(Observation::Pending)
        }
        fn cancel(&mut self) {
            self.0.borrow_mut().cancels += 1;
        }
    }
    impl Drop for FakeOperation {
        fn drop(&mut self) {
            self.0.borrow_mut().drops += 1;
        }
    }
    fn operation() -> (Box<dyn Operation>, Rc<RefCell<Fake>>) {
        let fake = Rc::new(RefCell::new(Fake::default()));
        (Box::new(FakeOperation(fake.clone())), fake)
    }
    fn identity(full: &str, family: &str) -> Identity {
        Identity {
            full: full.encode_utf16().collect(),
            family: family.encode_utf16().collect(),
        }
    }

    #[test]
    fn app_installer_unknown_error_future_and_extended_failure_never_mean_up_to_date() {
        assert_eq!(app_installer_availability(1, 0), Ok(Availability::NoUpdate));
        assert_eq!(
            app_installer_availability(2, 0),
            Ok(Availability::Available { mandatory: false })
        );
        assert_eq!(
            app_installer_availability(3, 0),
            Ok(Availability::Available { mandatory: true })
        );
        for value in [i32::MIN, -1, 0, 4, 5, i32::MAX] {
            assert_eq!(app_installer_availability(value, 0), Err(Failure::Result));
        }
        for extended in [i32::MIN, -1, 1, i32::MAX] {
            for value in 0..=4 {
                assert_eq!(
                    app_installer_availability(value, extended),
                    Err(Failure::Result)
                );
            }
        }
        let mut check = Check::app_installer();
        assert!(!APP_INSTALLER_CHECK_ENABLED);
        check.start(Instant::now());
        assert_eq!(check.result, Some(Err(Failure::Disabled)));
        assert!(!check.pending());
    }

    #[test]
    fn app_installer_reuses_bounded_polling_quarantine_and_never_promotes_late_success() {
        for failure in [
            Failure::Cancelled,
            Failure::Timeout,
            Failure::Service,
            Failure::Window,
        ] {
            let mut check = Check::app_installer();
            let (first, fake) = operation();
            let now = Instant::now();
            check.begin(first, now);
            check.tick(now);
            match failure {
                Failure::Cancelled => check.cancel(),
                Failure::Timeout => check.tick(now + TIMEOUT),
                Failure::Service => {
                    fake.borrow_mut()
                        .observations
                        .push_back(Observation::Unavailable);
                    check.tick(now);
                }
                Failure::Window => check.reject(Failure::Window),
                _ => unreachable!(),
            }
            let (next, second) = operation();
            check.next = Some(next);
            check.start(now);
            assert_eq!(second.borrow().polls, 0);
            assert_eq!(fake.borrow().cancels, 1);
            fake.borrow_mut()
                .observations
                .push_back(Observation::Finished(Ok(Availability::NoUpdate)));
            check.tick(now);
            assert!(!check.pending());
            assert_eq!(check.result, Some(Err(failure)));
            check.start(now);
            assert_eq!(check.result, None);
            second
                .borrow_mut()
                .observations
                .push_back(Observation::Finished(Err(Failure::Result)));
            check.tick(now);
            assert_eq!(check.result, Some(Err(Failure::Result)));
        }
        let (operation, fake) = operation();
        fake.borrow_mut()
            .observations
            .push_back(Observation::Finished(Ok(Availability::NoUpdate)));
        let mut check = Check::app_installer();
        let backdated = Instant::now() - TIMEOUT - Duration::from_secs(1);
        check.begin(operation, backdated);
        check.tick(backdated);
        assert_eq!(check.result, Some(Err(Failure::Timeout)));
        assert!(!check.pending());
        assert_eq!((fake.borrow().polls, fake.borrow().drops), (1, 1));
    }

    #[test]
    fn exact_main_package_is_required_and_ambiguous_versions_never_mean_no_update() {
        let current = identity("PeerBrush_1.30.0.0_x64__publisher", "PeerBrush_publisher");
        let optional = Candidate {
            identity: identity("Optional_2.0_x64__publisher", "Optional_publisher"),
            mandatory: true,
        };
        assert_eq!(availability(&current, &[]), Ok(Availability::NoUpdate));
        assert_eq!(
            availability(&current, std::slice::from_ref(&optional)),
            Ok(Availability::NoUpdate)
        );
        for mandatory in [false, true] {
            let matched = Candidate {
                identity: current.clone(),
                mandatory,
            };
            assert_eq!(
                availability(&current, &[optional.clone(), matched.clone()]),
                Ok(Availability::Available { mandatory })
            );
            assert_eq!(
                availability(&current, &[matched.clone(), matched]),
                Err(Failure::Result)
            );
        }
        for full in [
            "PeerBrush_1.31.0.0_x64__publisher",
            "PeerBrush_1.29.0.0_x64__publisher",
            "PeerBrush_1.30.0.0_arm64__publisher",
        ] {
            assert_eq!(
                availability(
                    &current,
                    &[Candidate {
                        identity: identity(full, "PeerBrush_publisher"),
                        mandatory: false
                    }]
                ),
                Err(Failure::Result)
            );
        }
        assert_eq!(
            availability(
                &current,
                &[Candidate {
                    identity: identity("PeerBrush_1.30.0.0_x64__publisher", "Wrong_publisher"),
                    mandatory: false
                }]
            ),
            Err(Failure::Result)
        );
        assert_eq!(
            availability(&current, &vec![optional.clone(); MAX_UPDATES + 1]),
            Err(Failure::Result)
        );
        let mut malformed = optional;
        malformed.identity.full = vec![65; MAX_IDENTITY + 1];
        assert_eq!(availability(&current, &[malformed]), Err(Failure::Result));
        assert_eq!(
            availability(&identity("", "family"), &[]),
            Err(Failure::Result)
        );
    }

    #[test]
    fn cancel_timeout_and_status_failure_quarantine_one_operation_and_discard_late_results() {
        for failure in [
            Failure::Cancelled,
            Failure::Timeout,
            Failure::Service,
            Failure::Window,
        ] {
            let mut check = Check::default();
            let (first, fake) = operation();
            let now = Instant::now();
            check.begin(first, now);
            check.tick(now);
            match failure {
                Failure::Cancelled => check.cancel(),
                Failure::Timeout => check.tick(now + TIMEOUT),
                Failure::Service => {
                    fake.borrow_mut()
                        .observations
                        .push_back(Observation::Unavailable);
                    check.tick(now);
                }
                Failure::Window => check.reject(Failure::Window),
                _ => unreachable!(),
            }
            assert_eq!(check.result, Some(Err(failure)));
            assert!(check.pending());
            let (next, second) = operation();
            check.next = Some(next);
            check.start(now);
            assert_eq!(second.borrow().polls, 0);
            assert_eq!(fake.borrow().cancels, 1);
            fake.borrow_mut()
                .observations
                .push_back(Observation::Finished(Ok(Availability::Available {
                    mandatory: true,
                })));
            check.tick(now);
            assert!(!check.pending());
            assert_eq!(
                check.result,
                Some(Err(failure)),
                "Late success cannot clear rejection"
            );
            assert_eq!(fake.borrow().drops, 1);
            check.start(now);
            assert_eq!(
                check.result, None,
                "A fresh attempt clears an earlier result"
            );
            second
                .borrow_mut()
                .observations
                .push_back(Observation::Finished(Err(Failure::Identity)));
            check.tick(now);
            assert_eq!(check.result, Some(Err(Failure::Identity)));
            assert!(!check.pending());
        }
        // A synchronous COM call can return after the deadline even when the
        // supplied pre-poll observation still describes an earlier instant.
        let (completed, fake) = operation();
        fake.borrow_mut()
            .observations
            .push_back(Observation::Finished(Ok(Availability::Available {
                mandatory: false,
            })));
        let mut check = Check::default();
        let backdated = Instant::now() - TIMEOUT - Duration::from_secs(1);
        check.begin(completed, backdated);
        check.tick(backdated);
        assert_eq!(check.result, Some(Err(Failure::Timeout)));
        assert!(!check.pending());
        assert_eq!((fake.borrow().polls, fake.borrow().drops), (1, 1));

        let (pending, fake) = operation();
        let mut check = Check::default();
        check.begin(pending, Instant::now());
        drop(check);
        assert_eq!((fake.borrow().cancels, fake.borrow().drops), (1, 1));
    }

    #[test]
    fn terminal_provider_errors_replace_success_without_waiting_or_displaying_service_payloads() {
        let mut check = Check::default();
        for result in [
            Ok(Availability::NoUpdate),
            Ok(Availability::Available { mandatory: false }),
            Err(Failure::Result),
            Err(Failure::Service),
            Err(Failure::Identity),
        ] {
            let (next, fake) = operation();
            check.next = Some(next);
            check.start(Instant::now());
            assert_eq!(check.result, None);
            fake.borrow_mut()
                .observations
                .push_back(Observation::Finished(result));
            check.tick(Instant::now());
            assert_eq!(check.result, Some(result));
            assert!(!check.pending());
        }
        for failure in [
            Failure::Unsupported,
            Failure::Window,
            Failure::Identity,
            Failure::Service,
            Failure::Result,
            Failure::Timeout,
            Failure::Cancelled,
        ] {
            let message = failure.message();
            assert!(
                !message.contains("http") && !message.contains("0x") && !message.contains("\\")
            );
        }
    }

    fn label(output: &egui::FullOutput, expected: &str) -> Option<egui::Pos2> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == expected => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
    }
    fn click(point: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ]
    }
    fn frame(
        app: &mut crate::ui::PeerBrush,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1360., 1000.),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                app.updates_tick(ctx);
                app.updates_dialog(ctx);
            },
        )
    }
    fn snapshot(engine: &crate::engine::Engine) -> serde_json::Value {
        let history = |entries: &[crate::engine::History]| {
            entries.iter().map(|entry| serde_json::json!({"before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,"task":entry.task,"scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted})).collect::<Vec<_>>()
        };
        serde_json::json!({"doc":engine.doc,"png":engine.doc.export_png().unwrap(),"undo":history(&engine.undo),"redo":history(&engine.redo),"closed":engine.closed,"saved":engine.saved_revision,"path":engine.path})
    }

    #[test]
    fn actual_store_controls_keep_public_notes_separate_and_preserve_native_sources_and_history() {
        use crate::{
            engine::Document,
            updates::{self, Channel, Owner, Report},
        };
        for depth in [8, 16] {
            let directory =
                std::env::temp_dir().join(format!("pb-store-check-{}", crate::engine::id()));
            std::fs::create_dir(&directory).unwrap();
            let (mut app, ctx) = crate::ui::tests::fixture();
            app.updates = super::super::Panel::load(&directory);
            app.updates.owner = Owner::MicrosoftStore;
            app.updates.open = true;
            app.updates.result = Some(Ok(Report {
                current_version: env!("CARGO_PKG_VERSION").into(),
                channel: Channel::Stable,
                target: updates::Target::WindowsX64,
                release_version: Some("999.0.0".into()),
                release_url: None,
                notes: "Synthetic public notes".into(),
                assets: vec![],
                checked_at: 1,
            }));
            {
                let mut engine = app.shared.lock().unwrap();
                engine.doc = Document::new_depth(32, 24, depth).unwrap();
                let paint = engine.doc.layers[0].id.clone();
                let source = crate::source::Source {
                    width: 16,
                    height: 16,
                    matrix: [1., 0., 0., 1., 0., 0.],
                    content: crate::source::Content::Shape {
                        shape: "rectangle".into(),
                        bounds: [1., 1., 12., 12.],
                        points: vec![],
                        closed: true,
                        fill: [12501, 23003, 34507, 45611],
                        stroke: [0; 4],
                        stroke_width: 0.,
                    },
                };
                engine
                    .edit(
                        "human",
                        &[serde_json::json!({"op":"source.add","source":source,"x":8,"y":4})],
                        None,
                        None,
                        "Editable source",
                    )
                    .unwrap();
                engine.edit("human", &[serde_json::json!({"op":"paint","layer":paint,"points":[[4,4]],"radius":2,"color":[233,84,32,255]})], None, None, "Human stroke").unwrap();
                engine.undo("human").unwrap();
                assert!(engine.doc.layers.iter().any(|layer| layer.source.is_some()));
                if depth == 16 {
                    assert!(engine.doc.layers.iter().any(|layer| layer
                        .pixels
                        .rgba16()
                        .chunks_exact(4)
                        .any(|pixel| pixel[..3].iter().any(|value| value % 257 != 0))));
                }
                assert_eq!((engine.undo.len(), engine.redo.len()), (1, 1));
                let png = engine.doc.export_png().unwrap();
                assert_eq!(
                    image::load_from_memory(&png).unwrap().color(),
                    if depth == 16 {
                        image::ColorType::Rgba16
                    } else {
                        image::ColorType::Rgba8
                    }
                );
            }
            let before = snapshot(&app.shared.lock().unwrap());
            assert!(
                app.update_restart_blocker().is_some(),
                "Unsaved source remains protected"
            );
            let (first, fake) = operation();
            app.updates.store_check.next = Some(first);
            frame(&mut app, &ctx, vec![]);
            let output = frame(&mut app, &ctx, vec![]);
            assert!(label(&output, "Published version 999.0.0").is_some());
            assert!(
                app.updates.store_check.result.is_none(),
                "GitHub notes never become native availability"
            );
            frame(
                &mut app,
                &ctx,
                click(
                    label(&output, "Check Microsoft Store").expect("Actual native check control"),
                ),
            );
            assert!(app.updates.store_check.pending());
            fake.borrow_mut()
                .observations
                .push_back(Observation::Finished(Ok(Availability::NoUpdate)));
            frame(&mut app, &ctx, vec![]);
            let output = frame(&mut app, &ctx, vec![]);
            assert!(label(
                &output,
                "Microsoft Store reports no update for this installed app package."
            )
            .is_some());
            assert!(label(&output, "Published version 999.0.0").is_some());
            let (second, fake) = operation();
            app.updates.store_check.next = Some(second);
            frame(
                &mut app,
                &ctx,
                click(label(&output, "Check Microsoft Store").unwrap()),
            );
            fake.borrow_mut()
                .observations
                .push_back(Observation::Finished(Err(Failure::Service)));
            frame(&mut app, &ctx, vec![]);
            let output = frame(&mut app, &ctx, vec![]);
            assert!(label(&output, Failure::Service.message()).is_some());
            assert!(label(
                &output,
                "Microsoft Store reports no update for this installed app package."
            )
            .is_none());
            let (available, fake_available) = operation();
            app.updates.store_check.next = Some(available);
            frame(
                &mut app,
                &ctx,
                click(label(&output, "Check Microsoft Store").unwrap()),
            );
            fake_available
                .borrow_mut()
                .observations
                .push_back(Observation::Finished(Ok(Availability::Available {
                    mandatory: true,
                })));
            frame(&mut app, &ctx, vec![]);
            let output = frame(&mut app, &ctx, vec![]);
            assert!(label(
                &output,
                "Microsoft Store reports an update for this installed app package."
            )
            .is_some());
            assert!(label(
                &output,
                "Microsoft Store marks that update as mandatory; this check does not install it."
            )
            .is_some());
            assert!(label(&output, Failure::Service.message()).is_none());
            let (third, fake) = operation();
            app.updates.store_check.next = Some(third);
            frame(
                &mut app,
                &ctx,
                click(label(&output, "Check Microsoft Store").unwrap()),
            );
            frame(&mut app, &ctx, vec![]);
            let output = frame(&mut app, &ctx, vec![]);
            frame(
                &mut app,
                &ctx,
                click(label(&output, "Cancel Store check").expect("Actual cancellation control")),
            );
            assert_eq!(
                app.updates.store_check.result,
                Some(Err(Failure::Cancelled))
            );
            assert!(app.updates.store_check.pending());
            fake.borrow_mut()
                .observations
                .push_back(Observation::Finished(Ok(Availability::Available {
                    mandatory: true,
                })));
            frame(&mut app, &ctx, vec![]);
            assert_eq!(
                app.updates.store_check.result,
                Some(Err(Failure::Cancelled))
            );
            let (fourth, fake) = operation();
            app.updates.store_check.next = Some(fourth);
            let output = frame(&mut app, &ctx, vec![]);
            frame(
                &mut app,
                &ctx,
                click(label(&output, "Check Microsoft Store").unwrap()),
            );
            let output = frame(&mut app, &ctx, vec![]);
            frame(
                &mut app,
                &ctx,
                click(label(&output, "Later").expect("Actual panel deferral")),
            );
            assert!(!app.updates.open);
            assert!(fake.borrow().cancels > 0);
            fake.borrow_mut()
                .observations
                .push_back(Observation::Finished(Ok(Availability::NoUpdate)));
            frame(&mut app, &ctx, vec![]);
            assert_eq!(
                app.updates.store_check.result,
                Some(Err(Failure::Cancelled))
            );
            assert_eq!(snapshot(&app.shared.lock().unwrap()), before);
            assert!(app.update_restart_blocker().is_some());
            assert!(app.shared.lock().unwrap().file_version.is_none());
            assert!(
                app.updates.store.preferences.last_attempt.is_none(),
                "Manual native checking does not rewrite public-check preferences"
            );
            app.updates.owner = Owner::Direct;
            app.updates.open = true;
            frame(&mut app, &ctx, vec![]);
            let output = frame(&mut app, &ctx, vec![]);
            assert!(label(&output, "Check Microsoft Store").is_none());
            drop(app);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn actual_app_installer_controls_are_disabled_by_default_and_keep_sources_and_store_separate() {
        use crate::updates::Owner;
        for depth in [8, 16] {
            let (mut app, ctx) = crate::ui::workspace_tests::fixture(depth);
            app.updates.owner = Owner::WindowsAppInstaller;
            app.updates.open = true;
            let before = snapshot(&app.shared.lock().unwrap());
            frame(&mut app, &ctx, vec![]);
            let output = frame(&mut app, &ctx, vec![]);
            assert!(label(&output, "Check Microsoft Store").is_none());
            assert!(label(&output, Failure::Disabled.message()).is_some());
            frame(
                &mut app,
                &ctx,
                click(label(&output, "Check App Installer").unwrap()),
            );
            assert!(!app.updates.app_installer_check.pending());
            assert!(app.updates.store_check.result.is_none());
            let (first, fake) = operation();
            app.updates.app_installer_check.next = Some(first);
            let output = frame(&mut app, &ctx, vec![]);
            frame(
                &mut app,
                &ctx,
                click(label(&output, "Check App Installer").unwrap()),
            );
            assert!(app.updates.app_installer_check.pending());
            fake.borrow_mut()
                .observations
                .push_back(Observation::Finished(Ok(Availability::Available {
                    mandatory: true,
                })));
            frame(&mut app, &ctx, vec![]);
            let output = frame(&mut app, &ctx, vec![]);
            assert!(label(
                &output,
                "The associated App Installer reports an update for this installed app package."
            )
            .is_some());
            assert!(label(
                &output,
                "App Installer marks that update as required; this check does not install it."
            )
            .is_some());
            assert!(app.updates.store_check.result.is_none());
            let (next, fake) = operation();
            app.updates.app_installer_check.next = Some(next);
            let output = frame(&mut app, &ctx, vec![]);
            frame(
                &mut app,
                &ctx,
                click(label(&output, "Check App Installer").unwrap()),
            );
            // Observe each hover pass before egui swaps its widget tables.
            let mut output = frame(&mut app, &ctx, vec![]);
            let mut last = None;
            let (id, at) = (0..16)
                .find_map(|_| {
                    let at = label(&output, "Later")?;
                    let mut target = None;
                    output = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(ctx.screen_rect()),
                            events: vec![egui::Event::PointerMoved(at)],
                            ..Default::default()
                        },
                        |ctx| {
                            app.updates_tick(ctx);
                            app.updates_dialog(ctx);
                            let previous = last.take();
                            target = None;
                            let hovered: Vec<_> = ctx.interaction_snapshot(|state| {
                                state.hovered.iter().copied().collect()
                            });
                            if let Some(response) = hovered
                                .into_iter()
                                .filter_map(|id| ctx.read_response(id))
                                .find(|response| {
                                    response.enabled()
                                        && response.sense.senses_click()
                                        && !response.sense.senses_drag()
                                        && response.rect.contains(at)
                                        && response.interact_rect.contains_rect(response.rect)
                                        && ctx.screen_rect().contains_rect(response.rect)
                                        && response.layer_id.id
                                            == egui::Id::new("peerbrush_updates")
                                        && ctx.layer_id_at(at) == Some(response.layer_id)
                                })
                            {
                                let key = (response.id, response.rect, response.interact_rect);
                                last = Some(key);
                                target = (previous == Some(key)).then_some((response.id, at));
                            }
                        },
                    );
                    target
                })
                .expect("Later must be a stable, fully visible live button");
            frame(&mut app, &ctx, click(at));
            assert_eq!(
                ctx.interaction_snapshot(|state| state.clicked),
                Some(id),
                "The actual Later button must receive the click"
            );
            assert!(!app.updates.open);
            assert_eq!(
                app.updates.app_installer_check.result,
                Some(Err(Failure::Cancelled))
            );
            fake.borrow_mut()
                .observations
                .push_back(Observation::Finished(Ok(Availability::NoUpdate)));
            frame(&mut app, &ctx, vec![]);
            assert_eq!(
                app.updates.app_installer_check.result,
                Some(Err(Failure::Cancelled))
            );
            assert_eq!(snapshot(&app.shared.lock().unwrap()), before);
            assert!(app.update_restart_blocker().is_some());
            assert!(app.updates.store.preferences.last_attempt.is_none());
            app.updates.owner = Owner::MicrosoftStore;
            app.updates.open = true;
            let output = frame(&mut app, &ctx, vec![]);
            assert!(label(&output, "Check App Installer").is_none());
            assert!(label(&output, "Check Microsoft Store").is_some());
        }
    }
}
