//! macOS owns the confirmation; no defaults change before the user's click.
use super::*;
use objc2::{
    class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
    sel,
};
use objc2_foundation::{NSBundle, NSError, NSString, NSURL};

#[link(name = "UniformTypeIdentifiers", kind = "framework")]
extern "C" {}

fn application() -> Option<Retained<NSURL>> {
    let bundle = NSBundle::mainBundle();
    if bundle.bundleIdentifier()?.to_string() != "com.peerbrush.PeerBrush" {
        return None;
    }
    let url = bundle.bundleURL();
    let path = PathBuf::from(url.path()?.to_string());
    let parent = path.parent()?;
    let user_apps = std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications"));
    if path.file_name()? != "PeerBrush.app"
        || !(parent == Path::new("/Applications") || user_apps.as_deref() == Some(parent))
    {
        return None;
    }
    Some(url)
}

pub(super) fn installed() -> bool {
    application().is_some()
}

pub(super) fn request(ctx: &egui::Context) -> Result<mpsc::Receiver<Result<(), String>>, String> {
    let application = application().ok_or("Open the installed PeerBrush in Applications first.")?;
    let workspace: Retained<AnyObject> = unsafe { msg_send![class!(NSWorkspace), sharedWorkspace] };
    let available: Bool = unsafe {
        msg_send![&*workspace, respondsToSelector: sel!(setDefaultApplicationAtURL:toOpenContentType:completionHandler:)]
    };
    if !available.as_bool() {
        return Err("On this macOS version, use Finder → Get Info → Open with → PeerBrush → Change All, for PSD and PSB separately.".into());
    }
    // Resolve both types before requesting either change. Other apps may have
    // registered a different canonical PSB identifier on this machine.
    let mut types = Vec::new();
    for extension in ["psd", "psb"] {
        let kind: Option<Retained<AnyObject>> = unsafe {
            msg_send![class!(UTType), typeWithFilenameExtension: &*NSString::from_str(extension)]
        };
        types.push(
            kind.ok_or("macOS could not resolve PSD/PSB file types. Reopen the installed app.")?,
        );
    }
    let (send, receive) = mpsc::channel();
    for (extension, kind) in ["PSD", "PSB"].into_iter().zip(types) {
        let send = send.clone();
        let ctx = ctx.clone();
        let completion = block2::RcBlock::new(move |error: *mut NSError| {
            let result = if error.is_null() {
                Ok(())
            } else {
                let message = unsafe { (&*error).localizedDescription() }.to_string();
                Err(format!("{extension} default was not changed: {message}"))
            };
            let _ = send.send(result);
            ctx.request_repaint();
        });
        unsafe {
            let _: () = msg_send![&*workspace, setDefaultApplicationAtURL: &*application,
                toOpenContentType: &*kind, completionHandler: &*completion];
        }
    }
    Ok(receive)
}
