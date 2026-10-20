//! Finder Open Documents delivery without replacing winit's application delegate.
use crate::server::Shared;
use objc2::{
    class, define_class, msg_send, rc::Retained, runtime::AnyObject, sel, DefinedClass,
    MainThreadMarker, MainThreadOnly,
};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSObject, NSObjectProtocol};
use std::{
    ffi::{CStr, OsStr},
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    sync::OnceLock,
};

const CORE: u32 = u32::from_be_bytes(*b"aevt");
const OPEN: u32 = u32::from_be_bytes(*b"odoc");
const DIRECT: u32 = u32::from_be_bytes(*b"----");
static REPAINT: OnceLock<eframe::egui::Context> = OnceLock::new();

pub fn repaint_with(ctx: &eframe::egui::Context) {
    let _ = REPAINT.set(ctx.clone());
}

/// Decode the native boundary before enqueueing any file. NSURL preserves
/// filesystem bytes, including spaces, percent signs and non-ASCII filenames.
pub fn paths(event: &NSAppleEventDescriptor) -> Result<Vec<PathBuf>, String> {
    // Foundation's generated binding omits this Carbon-typed selector unless
    // an additional framework crate is enabled; AEKeyword is a four-byte code.
    let list: Option<Retained<NSAppleEventDescriptor>> =
        unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT] };
    let list = list.ok_or("Finder did not provide document files")?;
    let kind: u32 = unsafe { msg_send![&*list, descriptorType] };
    if kind != u32::from_be_bytes(*b"list") {
        return Err("Finder document parameter is not a native file list".into());
    }
    let count = list.numberOfItems();
    if !(1..=super::MAX_PENDING_FILES as isize).contains(&count) {
        return Err(format!(
            "Choose between 1 and {} files to open",
            super::MAX_PENDING_FILES
        ));
    }
    (1..=count)
        .map(|index| {
            let descriptor = list
                .descriptorAtIndex(index)
                .ok_or("Missing Finder document")?;
            // fileURLValue coerces text; only native file URL descriptors belong here.
            let kind: u32 = unsafe { msg_send![&*descriptor, descriptorType] };
            if kind != u32::from_be_bytes(*b"furl") {
                return Err("Finder document is not a native file URL".into());
            }
            let url = descriptor
                .fileURLValue()
                .ok_or("Finder document is not a local file")?;
            if !url.isFileURL() {
                return Err("Finder document is not a local file".into());
            }
            // The URL retains its NUL-terminated filesystem representation until
            // this owned PathBuf is created. No lossy UTF-8 conversion is involved.
            let bytes =
                unsafe { CStr::from_ptr(url.fileSystemRepresentation().as_ptr()) }.to_bytes();
            let path = PathBuf::from(OsStr::from_bytes(bytes));
            if !path.is_absolute() {
                return Err("Finder document needs an absolute file path".into());
            }
            Ok(path)
        })
        .collect()
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Shared]
    struct OpenDocuments;

    unsafe impl NSObjectProtocol for OpenDocuments {}

    impl OpenDocuments {
        // NSAppleEventManager requires exactly this two-descriptor signature.
        #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
        fn opened(&self, event: &NSAppleEventDescriptor, reply: Option<&NSAppleEventDescriptor>) {
            // Rust panics must never unwind through an Objective-C callback.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::enqueue(self.ivars(), paths(event)?)
            })).unwrap_or_else(|_| Err("Finder document delivery could not complete; current work remains open".into()));
            if let Err(error) = result {
                if let Ok(mut engine) = self.ivars().lock() {
                    engine.status = error;
                }
                if let Some(reply) = reply {
                    let error = NSAppleEventDescriptor::descriptorWithInt32(-10000);
                    unsafe { let _: () = msg_send![reply, setParamDescriptor: &*error, forKeyword: u32::from_be_bytes(*b"errn")]; }
                }
            }
            if let Some(ctx) = REPAINT.get() { ctx.request_repaint(); }
        }
    }
);

pub struct Registration(Retained<OpenDocuments>);

/// Register on the process main thread before the native event loop starts.
/// Keep the returned owner alive until run_native returns.
pub fn install(root: Shared) -> Result<Registration, String> {
    let marker = MainThreadMarker::new().ok_or("Finder activation requires the main thread")?;
    // Initialize AppKit's ordinary handlers first, then replace only odoc.
    // winit continues to own NSApplication's launch/termination delegate.
    unsafe {
        let _: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
    }
    let handler = OpenDocuments::alloc(marker).set_ivars(root);
    let handler: Retained<OpenDocuments> = unsafe { msg_send![super(handler), init] };
    let manager = NSAppleEventManager::sharedAppleEventManager();
    unsafe {
        let _: () = msg_send![&*manager, setEventHandler: &*handler,
            andSelector: sel!(handleOpenDocuments:withReplyEvent:), forEventClass: CORE, andEventID: OPEN];
    }
    Ok(Registration(handler))
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = &self.0; // Retain the callback owner until after unregistration.
        let manager = NSAppleEventManager::sharedAppleEventManager();
        unsafe {
            let _: () =
                msg_send![&*manager, removeEventHandlerForEventClass: CORE, andEventID: OPEN];
        }
    }
}
