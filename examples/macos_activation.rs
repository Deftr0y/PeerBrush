//! Native component check; only private synthetic files, no Finder/default changes.
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macOS Apple Event component check requires macOS");
}

#[cfg(target_os = "macos")]
fn main() {
    use objc2::{class, msg_send, rc::Retained, runtime::AnyObject};
    use objc2_foundation::{
        NSAppleEventDescriptor as Descriptor, NSAppleEventSendOptions, NSData, NSString,
        NSURLBookmarkCreationOptions, NSURL,
    };
    use peerbrush::{activation, engine::Engine, server::Shared, workspace};
    use serde_json::{json, Value};
    use std::{
        fs::{self, File},
        io::Write,
        os::unix::fs::DirBuilderExt,
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn kind(descriptor: &Descriptor) -> u32 {
        unsafe { msg_send![descriptor, descriptorType] }
    }

    fn native_descriptor(kind: u32, data: &NSData) -> Retained<Descriptor> {
        let descriptor: Option<Retained<Descriptor>> = unsafe {
            msg_send![class!(NSAppleEventDescriptor), descriptorWithDescriptorType: kind, data: data]
        };
        descriptor.expect("native fixture descriptor")
    }

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
        assert_eq!(kind(&list), u32::from_be_bytes(*b"list"));
        parameter_event(&list)
    }

    fn snapshot(root: &Shared) -> Value {
        let mut engine = root.lock().unwrap();
        let mut state = engine.state();
        // Rejection updates the presentation status, never document or history.
        state.as_object_mut().unwrap().remove("file_status");
        json!({"state": state, "source": engine.doc, "undo": engine.undo.len(),
               "redo": engine.redo.len(), "saved_revision": engine.saved_revision})
    }

    fn rejected(event: &Descriptor, root: &Shared, before: &Value) {
        assert!(activation::macos::paths(event).is_err());
        match event.sendEventWithOptions_timeout_error(NSAppleEventSendOptions::WaitForReply, 5.0) {
            Ok(reply) => {
                let error: Option<Retained<Descriptor>> = unsafe {
                    msg_send![&*reply, paramDescriptorForKeyword: u32::from_be_bytes(*b"errn")]
                };
                assert_eq!(error.expect("rejection reply error").int32Value(), -10000);
            }
            Err(error) => {
                assert_eq!(
                    error.code(),
                    -10000,
                    "only the handler's rejection is accepted"
                );
                assert_eq!(error.domain().to_string(), "NSOSStatusErrorDomain");
            }
        }
        assert!(
            !root.lock().unwrap().focus_requested,
            "no file from a rejected event may queue"
        );
        assert_eq!(
            &snapshot(root),
            before,
            "rejected events preserve dirty source and history"
        );
    }

    objc2::rc::autoreleasepool(|_| {
        let root = Arc::new(Mutex::new(Engine::new()));
        root.lock().unwrap().doc.revision = 7;
        activation::enable(&root);
        let before = snapshot(&root);
        assert_eq!(before["state"]["dirty"], true);
        let app: *mut AnyObject = unsafe { msg_send![class!(NSApplication), sharedApplication] };
        let delegate: *mut AnyObject = unsafe { msg_send![app, delegate] };
        let owner = activation::macos::install(root.clone()).unwrap();
        let after_delegate: *mut AnyObject = unsafe { msg_send![app, delegate] };
        assert_eq!(
            delegate, after_delegate,
            "activation must preserve the existing application delegate"
        );
        let directory = std::env::temp_dir().join(format!(
            "peerbrush-native-files-{}",
            peerbrush::engine::id()
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .expect("exclusive private fixture directory");
        let fixture = Fixture(directory);
        // Alias resolution can canonicalize /var to /private/var on macOS.
        let directory = fs::canonicalize(&fixture.0).unwrap();
        let paths = ["PeerBrush 作品 with spaces.psd", "100% literal #?.psb"]
            .map(|name| directory.join(name));
        let mut files = Vec::new();
        let mut expected = Vec::new();
        for path in &paths {
            File::create_new(path)
                .unwrap()
                .write_all(b"SYNTHETIC_NATIVE_REFERENCE_FIXTURE\n")
                .unwrap();
            let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().unwrap()));
            let file = Descriptor::descriptorWithFileURL(&url);
            assert_eq!(kind(&file), u32::from_be_bytes(*b"furl"));
            // Only the fixture asks the OS to create a real Carbon alias.
            let alias: Option<Retained<Descriptor>> =
                unsafe { msg_send![&*file, coerceToDescriptorType: u32::from_be_bytes(*b"alis")] };
            let alias = alias.expect("OS-created native alias for existing fixture file");
            assert_eq!(kind(&alias), u32::from_be_bytes(*b"alis"));
            let bookmark = url
                .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                    NSURLBookmarkCreationOptions::empty(),
                    None,
                    None,
                )
                .expect("OS-created native bookmark for existing fixture file");
            let bookmark = native_descriptor(u32::from_be_bytes(*b"bmrk"), &bookmark);
            assert_eq!(kind(&bookmark), u32::from_be_bytes(*b"bmrk"));
            for reference in [file, alias, bookmark] {
                assert_eq!(
                    activation::macos::paths(&event(&[reference.clone()])).unwrap(),
                    [path.clone()]
                );
                files.push(reference);
                expected.push(path.clone());
            }
        }
        let opened = event(&files);
        assert_eq!(activation::macos::paths(&opened).unwrap(), expected);
        rejected(&event(&[]), &root, &before);
        let record = Descriptor::recordDescriptor();
        unsafe {
            let _: () = msg_send![&*record, setDescriptor: &*files[0], forKeyword: u32::from_be_bytes(*b"file")];
        }
        for parameter in [&*record, &*files[0]] {
            rejected(&parameter_event(parameter), &root, &before);
        }
        for text in [
            "https://example.invalid/image.psd",
            "file:///tmp/PeerBrush%20text.psd",
            "/tmp/PeerBrush text.psd",
        ] {
            let descriptor = Descriptor::descriptorWithString(&NSString::from_str(text));
            rejected(&event(&[descriptor.clone()]), &root, &before);
            rejected(&event(&[files[0].clone(), descriptor]), &root, &before);
        }
        for native_type in [*b"furl", *b"alis", *b"bmrk"] {
            for bytes in [
                Vec::new(),
                b"invalid native reference".to_vec(),
                vec![0; activation::macos::MAX_FILE_REFERENCE_BYTES + 1],
            ] {
                let descriptor =
                    native_descriptor(u32::from_be_bytes(native_type), &NSData::with_bytes(&bytes));
                rejected(&event(&[files[1].clone(), descriptor]), &root, &before);
            }
        }
        for reference in [&files[1], &files[2]] {
            let bytes = reference.data().to_vec();
            let truncated = native_descriptor(kind(reference), &NSData::with_bytes(&bytes[..1]));
            rejected(&event(&[files[0].clone(), truncated]), &root, &before);
        }
        for (native_type, bytes) in [
            (*b"furl", &b"https://example.invalid/image.psd"[..]),
            (*b"fsrf", &b"unsupported FSRef"[..]),
            (*b"obj ", &b"unsupported object specifier"[..]),
        ] {
            let descriptor =
                native_descriptor(u32::from_be_bytes(native_type), &NSData::with_bytes(bytes));
            rejected(&event(&[files[2].clone(), descriptor]), &root, &before);
        }
        rejected(
            &event(&vec![files[0].clone(); activation::MAX_PENDING_FILES + 1]),
            &root,
            &before,
        );
        assert!(!root.lock().unwrap().focus_requested);

        // A real same-process Apple Event exercises the installed Objective-C
        // selector and mixed native references without opening or saving a project.
        opened
            .sendEventWithOptions_timeout_error(NSAppleEventSendOptions::WaitForReply, 5.0)
            .expect("same-process Open Documents delivery");
        assert!(root.lock().unwrap().focus_requested);
        assert_eq!(
            snapshot(&root),
            before,
            "native delivery only queues; dirty work stays open"
        );
        assert_eq!(workspace::handles(&root).len(), 1);
        let after_delegate: *mut AnyObject = unsafe { msg_send![app, delegate] };
        assert_eq!(delegate, after_delegate);
        drop(owner);
        drop(fixture);
        println!("macOS native URL/alias/bookmark decoding, atomic rejection, dirty-source and delegate preservation passed; Finder acceptance remains separate");
    });
}
