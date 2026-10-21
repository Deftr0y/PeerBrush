//! Read-only Store availability for this running package, on its owner window thread.
use eframe::egui;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(25);
#[cfg(any(windows, test))]
const MAX_UPDATES: usize = 64;
#[cfg(any(windows, test))]
const MAX_IDENTITY: usize = 1024;

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
        }
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
    pending: Option<Pending>,
    result: Option<Result<Availability, Failure>>,
    #[cfg(windows)]
    window: Option<native::Window>,
    #[cfg(test)]
    next: Option<Box<dyn Operation>>,
}
impl Check {
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
        #[cfg(windows)]
        match self
            .window
            .ok_or(Failure::Window)
            .and_then(native::Native::start)
        {
            Ok(operation) => self.begin(Box::new(operation), now),
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
        let pending = self.pending.as_ref().expect("Pending Store operation");
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
        ui.label("Installed Microsoft Store package");
        if ui
            .add_enabled(!self.pending(), egui::Button::new("Check Microsoft Store"))
            .clicked()
        {
            self.start(Instant::now());
        }
        if let Some(pending) = &self.pending {
            if pending.rejected {
                ui.label(
                    "Cancellation requested; waiting for Microsoft Store before another check.",
                );
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking this installed package with Microsoft Store…");
                });
                if ui.button("Cancel Store check").clicked() {
                    self.cancel();
                }
            }
        }
        if let Some(result) = self.result {
            match result {
                Ok(Availability::NoUpdate) => {
                    ui.label("Microsoft Store reports no update for this installed app package.");
                }
                Ok(Availability::Available { mandatory }) => {
                    ui.label("Microsoft Store reports an update for this installed app package.");
                    if mandatory {
                        ui.label("Microsoft Store marks that update as mandatory; this check does not install it.");
                    }
                }
                Err(error) => {
                    ui.label(error.message());
                }
            }
        }
        ui.label("Store results may be cached or delayed. This does not verify public listing or installation readiness.");
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use windows::{
        core::{Interface, HSTRING},
        ApplicationModel::{Package, PackageSignatureKind},
        Foundation::{AsyncStatus, Collections::IVectorView, IAsyncOperation},
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
}
