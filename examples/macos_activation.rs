//! Native component check; no Finder/default-app changes or document writes.
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macOS Apple Event component check requires macOS");
}

#[cfg(target_os = "macos")]
fn main() {
    use objc2::{class, msg_send, rc::Retained, runtime::AnyObject};
    use objc2_foundation::{
        NSAppleEventDescriptor as Descriptor, NSAppleEventSendOptions, NSString, NSURL,
    };
    use peerbrush::{activation, engine::Engine, workspace};
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    fn parameter_event(parameter: &Descriptor) -> Retained<Descriptor> {
        let target = Descriptor::currentProcessDescriptor();
        let event: Retained<Descriptor> = unsafe {
            msg_send![class!(NSAppleEventDescriptor),
            appleEventWithEventClass: u32::from_be_bytes(*b"aevt"),
            eventID: u32::from_be_bytes(*b"odoc"), targetDescriptor: &*target,
            returnID: -1_i16, transactionID: 0_i32]
        };
        unsafe {
            let _: () = msg_send![&*event, setParamDescriptor: parameter, forKeyword: u32::from_be_bytes(*b"----")];
        }
        event
    }

    fn event(files: &[Retained<Descriptor>]) -> Retained<Descriptor> {
        let list = Descriptor::listDescriptor();
        for (index, file) in files.iter().enumerate() {
            list.insertDescriptor_atIndex(file, (index + 1) as isize);
        }
        let kind: u32 = unsafe { msg_send![&*list, descriptorType] };
        assert_eq!(kind, u32::from_be_bytes(*b"list"));
        parameter_event(&list)
    }

    objc2::rc::autoreleasepool(|_| {
        let root = Arc::new(Mutex::new(Engine::new()));
        root.lock().unwrap().doc.revision = 7;
        let before = root.lock().unwrap().state();
        activation::enable(&root);
        let app: *mut AnyObject = unsafe { msg_send![class!(NSApplication), sharedApplication] };
        let delegate: *mut AnyObject = unsafe { msg_send![app, delegate] };
        let owner = activation::macos::install(root.clone()).unwrap();
        let after_delegate: *mut AnyObject = unsafe { msg_send![app, delegate] };
        assert_eq!(
            delegate, after_delegate,
            "activation must preserve the existing application delegate"
        );
        let paths = [
            "/tmp/PeerBrush 作品 with spaces.psd",
            "/tmp/100% literal #?.psb",
        ]
        .map(PathBuf::from);
        let files: Vec<_> = paths
            .iter()
            .map(|path| {
                let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().unwrap()));
                Descriptor::descriptorWithFileURL(&url)
            })
            .collect();
        for file in &files {
            let kind: u32 = unsafe { msg_send![&**file, descriptorType] };
            assert_eq!(kind, u32::from_be_bytes(*b"furl"));
        }
        let opened = event(&files);
        assert_eq!(activation::macos::paths(&opened).unwrap(), paths);
        assert!(activation::macos::paths(&event(&[])).is_err());
        let record = Descriptor::recordDescriptor();
        unsafe {
            let _: () = msg_send![&*record, setDescriptor: &*files[0], forKeyword: u32::from_be_bytes(*b"file")];
        }
        for parameter in [&*record, &*files[0]] {
            assert!(activation::macos::paths(&parameter_event(parameter)).is_err());
        }
        let malformed = Descriptor::descriptorWithString(&NSString::from_str(
            "https://example.invalid/image.psd",
        ));
        assert!(activation::macos::paths(&event(&[files[0].clone(), malformed])).is_err());
        for text in [
            "file:///tmp/PeerBrush%20text.psd",
            "/tmp/PeerBrush text.psd",
        ] {
            let descriptor = Descriptor::descriptorWithString(&NSString::from_str(text));
            assert!(activation::macos::paths(&event(&[descriptor.clone()])).is_err());
            assert!(activation::macos::paths(&event(&[files[0].clone(), descriptor])).is_err());
        }
        assert!(activation::macos::paths(&event(&vec![
            files[0].clone();
            activation::MAX_PENDING_FILES + 1
        ]))
        .is_err());
        assert!(!root.lock().unwrap().focus_requested);

        // A real same-process Apple Event exercises the installed Objective-C
        // selector and queue, without scripting another app or touching files.
        opened
            .sendEventWithOptions_timeout_error(NSAppleEventSendOptions::WaitForReply, 5.0)
            .expect("same-process Open Documents delivery");
        assert!(root.lock().unwrap().focus_requested);
        assert_eq!(
            root.lock().unwrap().state(),
            before,
            "native delivery only queues; dirty work stays open"
        );
        assert_eq!(workspace::handles(&root).len(), 1);
        drop(owner);
        println!("macOS Apple Event decoding, bounded queue delivery and delegate preservation passed; Finder acceptance remains separate");
    });
}
