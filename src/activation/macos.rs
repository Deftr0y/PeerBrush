//! Finder Open Documents delivery without replacing winit's application delegate.
use crate::server::Shared;
use objc2::{
    class, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
    sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_core_foundation::CFURL;
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSData, NSObject, NSObjectProtocol,
    NSURLBookmarkResolutionOptions, NSURL,
};
use std::{
    ffi::{CStr, OsStr},
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    sync::OnceLock,
};

const CORE: u32 = u32::from_be_bytes(*b"aevt");
const OPEN: u32 = u32::from_be_bytes(*b"odoc");
const DIRECT: u32 = u32::from_be_bytes(*b"----");
const FILE_URL: u32 = u32::from_be_bytes(*b"furl");
const ALIAS: u32 = u32::from_be_bytes(*b"alis");
const BOOKMARK: u32 = u32::from_be_bytes(*b"bmrk");
/// Native file references are bounded before parsing; at most 16 are accepted.
pub const MAX_FILE_REFERENCE_BYTES: usize = 64 * 1024;
static REPAINT: OnceLock<eframe::egui::Context> = OnceLock::new();

pub fn repaint_with(ctx: &eframe::egui::Context) {
    let _ = REPAINT.set(ctx.clone());
}

fn bookmark_url(data: &NSData) -> Result<Retained<NSURL>, String> {
    if !(1..=MAX_FILE_REFERENCE_BYTES).contains(&data.length()) {
        return Err("Finder file reference is empty or too large".into());
    }
    let mut stale = Bool::NO;
    unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            data,
            NSURLBookmarkResolutionOptions::WithoutUI
                | NSURLBookmarkResolutionOptions::WithoutMounting,
            None,
            &mut stale,
        )
    }
    .map_err(|_| "Finder file reference could not be resolved without interaction".into())
}

fn file_url(descriptor: &NSAppleEventDescriptor) -> Result<Retained<NSURL>, String> {
    let kind: u32 = unsafe { msg_send![descriptor, descriptorType] };
    if kind == u32::from_be_bytes(*b"fsrf") {
        return Err("Finder FSRef document descriptors are not supported".into());
    }
    if ![FILE_URL, ALIAS, BOOKMARK].contains(&kind) {
        return Err("Finder document is not a supported native file reference".into());
    }
    let data = descriptor.data();
    if !(1..=MAX_FILE_REFERENCE_BYTES).contains(&data.length()) {
        return Err("Finder file reference is empty or too large".into());
    }
    if kind == FILE_URL {
        // Only the original furl type reaches Foundation's otherwise coercing getter.
        return descriptor
            .fileURLValue()
            .ok_or("Finder document is not a local file".into());
    }
    if kind == ALIAS {
        // Convert the native alias format without resolving it. Resolution below
        // forbids prompting or mounting volumes; no general descriptor coercion.
        #[allow(deprecated)]
        let bookmark =
            unsafe { CFURL::new_bookmark_data_from_alias_record(None, Some(data.as_ref())) }
                .ok_or("Finder alias could not be decoded")?;
        return bookmark_url((&*bookmark).as_ref());
    }
    bookmark_url(&data)
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
            let url = file_url(&descriptor)?;
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
