//! Owned native Quit check; only synthetic projects and a private visible CI window.
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macOS native Quit component check requires macOS");
}

#[cfg(target_os = "macos")]
fn main() {
    use std::{
        fs,
        io::Write,
        os::unix::fs::DirBuilderExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--native-child") {
        assert_eq!(args.len(), 2);
        native::child(std::path::PathBuf::from(&args[1]));
        return;
    }
    assert!(args.is_empty());
    let directory =
        std::env::temp_dir().join(format!("peerbrush-native-quit-{}", peerbrush::engine::id()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let log = fs::File::create_new(directory.join("child.log")).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--native-child")
        .arg(&directory)
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("native Quit owned child exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // terminate: may exit a process with status zero before Rust cleanup. Only
    // a receipt written after run_native returns establishes normal completion.
    assert!(status.success(), "native Quit owned child failed");
    let receipt = directory.join("complete.json");
    let metadata =
        fs::symlink_metadata(&receipt).expect("native Quit child returned through normal cleanup");
    assert!(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 512);
    let completion = fs::read(receipt).unwrap();
    assert_eq!(completion, br#"{"native_depths":[8,16],"delegate_preserved":true,"dirty_cancel_preserved":true,"normal_exit_returned":true}"#);
    fs::remove_dir_all(&directory).unwrap();
    std::io::stdout().write_all(b"macOS native Quit cancellation, delegate preservation and guarded normal exit passed\n").unwrap();
}

#[cfg(target_os = "macos")]
mod native {
    use eframe::{egui, App};
    use objc2::{
        class, msg_send,
        rc::Retained,
        runtime::{AnyClass, AnyObject},
        sel,
    };
    use peerbrush::{
        activation,
        engine::{Document, Engine},
        server::{self, Shared},
        ui::PeerBrush,
        workspace,
    };
    use serde_json::{json, Value};
    use std::{
        cell::Cell,
        fs,
        io::Write,
        path::PathBuf,
        rc::Rc,
        sync::{Arc, Mutex},
    };

    fn snapshot(root: &Shared) -> Value {
        let engine = root.lock().unwrap();
        json!({"document": engine.doc, "undo": engine.undo.len(), "redo": engine.redo.len(), "saved_revision": engine.saved_revision})
    }

    fn dirty(root: &Shared, name: &str) {
        let mut engine = root.lock().unwrap();
        let layer = engine.doc.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"name":name})],
                None,
                None,
                name,
            )
            .unwrap();
    }

    fn methods(class: &AnyClass) -> Vec<(objc2::runtime::Sel, usize)> {
        class
            .instance_methods()
            .iter()
            .map(|method| (method.name(), method.implementation() as *const () as usize))
            .collect()
    }

    struct Probe {
        app: PeerBrush,
        root: Shared,
        second: Shared,
        application: Retained<AnyObject>,
        delegate: Retained<AnyObject>,
        original_methods: Vec<(objc2::runtime::Sel, usize)>,
        before: [Value; 2],
        directory: PathBuf,
        phase: u8,
        complete: Rc<Cell<bool>>,
    }

    impl Probe {
        fn unchanged_delegate(&self) {
            let current: Retained<AnyObject> = unsafe { msg_send![&*self.application, delegate] };
            assert!(std::ptr::eq(&*current, &*self.delegate));
            assert_eq!(current.class().name(), c"WinitApplicationDelegate");
            for (selector, implementation) in &self.original_methods {
                assert_eq!(
                    current
                        .class()
                        .instance_method(*selector)
                        .unwrap()
                        .implementation() as *const () as usize,
                    *implementation
                );
            }
        }

        fn quit(&self) {
            // This actual native call must return even while an engine is locked.
            let _engine = self.root.lock().unwrap();
            unsafe {
                let _: () = msg_send![&*self.application, terminate: std::ptr::null::<AnyObject>()];
            }
            self.unchanged_delegate();
        }
    }

    impl App for Probe {
        fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            if self.phase >= 3 && self.complete.get() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            match self.phase {
                0 => {
                    self.before = [snapshot(&self.root), snapshot(&self.second)];
                    self.quit();
                    self.quit();
                    let registry = workspace::attach(&self.root);
                    assert!(activation::take_quit(&registry));
                    assert!(!activation::take_quit(&registry));
                    self.quit();
                }
                1 => ctx.input_mut(|input| {
                    input.events.push(egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: Some(egui::Key::Escape),
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    })
                }),
                2 => {
                    assert_eq!(snapshot(&self.root), self.before[0]);
                    assert_eq!(snapshot(&self.second), self.before[1]);
                    assert!(
                        !self.root.lock().unwrap().closed && !self.second.lock().unwrap().closed
                    );
                    ctx.input_mut(|input| {
                        input.events.push(egui::Event::Key {
                            key: egui::Key::Escape,
                            physical_key: Some(egui::Key::Escape),
                            pressed: false,
                            repeat: false,
                            modifiers: Default::default(),
                        })
                    });
                    server::save(&self.root, &self.directory.join("native8.psd")).unwrap();
                    server::save(&self.second, &self.directory.join("native16.psb")).unwrap();
                    self.quit();
                }
                _ => {}
            }
            self.app.update(ctx, frame);
            self.unchanged_delegate();
            if self.phase >= 2
                && self.root.lock().unwrap().closed
                && self.second.lock().unwrap().closed
            {
                self.complete.set(true);
            }
            self.phase = self.phase.saturating_add(1);
            ctx.request_repaint();
        }
    }

    pub fn child(directory: PathBuf) {
        assert!(
            directory.is_absolute()
                && directory.is_dir()
                && !directory.join("complete.json").exists()
        );
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(32, 24, 8).unwrap();
        let root = Arc::new(Mutex::new(engine));
        let second = workspace::new_project(&root, 32, 24, 16, None).unwrap();
        root.lock().unwrap().doc.layers[0]
            .pixels
            .set16(1, 1, [12001, 23003, 34007, 45009]);
        second.lock().unwrap().doc.layers[0]
            .pixels
            .set16(1, 1, [12001, 23003, 34007, 45009]);
        dirty(&root, "Native8 unsaved work");
        dirty(&second, "Native16 unsaved work");
        activation::enable(&root);
        let complete = Rc::new(Cell::new(false));
        let reported = complete.clone();
        let captured = [root.clone(), second.clone()];
        let state = directory.clone();
        eframe::run_native(
            "PeerBrush native Quit synthetic fixture",
            eframe::NativeOptions::default(),
            Box::new(move |cc| {
                let application: Retained<AnyObject> =
                    unsafe { msg_send![class!(NSApplication), sharedApplication] };
                let delegate: Retained<AnyObject> = unsafe { msg_send![&*application, delegate] };
                let original_methods = methods(delegate.class());
                assert!(delegate
                    .class()
                    .instance_method(sel!(applicationShouldTerminate:))
                    .is_none());
                activation::macos::install_termination(&root, &cc.egui_ctx)?;
                assert!(activation::macos::install_termination(&root, &cc.egui_ctx).is_err());
                let connection = server::start(root.clone(), state.clone())?;
                let app = PeerBrush::new_with_preferences(cc, root.clone(), connection, &state);
                Ok(Box::new(Probe {
                    app,
                    root,
                    second,
                    application,
                    delegate,
                    original_methods,
                    before: [Value::Null, Value::Null],
                    directory: state,
                    phase: 0,
                    complete,
                }))
            }),
        )
        .unwrap();
        assert!(
            reported.get(),
            "native Quit returned before guarded final exit"
        );
        for (shared, depth) in captured.into_iter().zip([8, 16]) {
            let engine = shared.lock().unwrap();
            assert!(engine.closed);
            assert_eq!(engine.doc.bit_depth, depth);
            assert_eq!(engine.doc.revision, engine.saved_revision);
        }
        fs::File::create_new(directory.join("complete.json")).unwrap()
            .write_all(br#"{"native_depths":[8,16],"delegate_preserved":true,"dirty_cancel_preserved":true,"normal_exit_returned":true}"#).unwrap();
    }
}
