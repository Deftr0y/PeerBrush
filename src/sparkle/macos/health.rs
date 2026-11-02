//! Probe the installed editor in a new private HOME. No updater or user projects
//! run in that editor; a receipt file alone can never authorize normal startup.
use super::super::installed::{Scratch, Verified};
use super::*;
use eframe::egui;
use std::os::unix::fs::DirBuilderExt;

pub(super) const ENTRY: &str = "--peerbrush-installed-editor-health";
const LIMIT: Duration = Duration::from_secs(90);

pub(super) fn probe(
    app: PathBuf,
    target: Verified,
    team: String,
    armed: checkpoint::ArmedCheck,
    profile: UpdateProfile,
    binding: checkpoint::FinalProfileBinding,
) -> Result<checkpoint::HealthProfile, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let scratch = Scratch::new(cancel.clone())?;
    let root = &scratch.path;
    let config = root.join("Library/Application Support/PeerBrush/Config");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&config)
        .map_err(|_| FAILED)?;
    let original = armed.health_profile(&profile, &binding, &config.join("settings-v1"))?;
    let (mut socket, inherited) = UnixStream::pair().map_err(|_| FAILED)?;
    no_sigpipe(&socket)?;
    no_sigpipe(&inherited)?;
    let fd: OwnedFd = inherited.into();
    let exe = app.join("Contents/MacOS/peerbrush");
    let release = target.release.as_ref().ok_or(FAILED)?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let deadline = Instant::now() + LIMIT;
    target.recheck()?;
    let mut child = Command::new(&exe)
        .arg(ENTRY)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("TMPDIR", root)
        .current_dir(root)
        .stdin(Stdio::from(fd))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| FAILED)?;
    let result = (|| {
        send(&mut socket, &nonce, deadline, &cancel)?;
        let (returned, build): (String, String) = receive(&mut socket, deadline, &cancel)?;
        let expected: serde_json::Value = serde_json::from_str(&build).map_err(|_| FAILED)?;
        if returned != nonce
            || expected["commit"] != release.binary_source_commit
            || expected["version"] != release.version
            || expected["tree"] != "clean"
            || process_identity::executable(child.id())? != exe
        {
            return Err(FAILED.into());
        }
        process_identity::authenticate_peer(
            &socket,
            child.id(),
            &team,
            false,
            Some(&target.cdhash),
        )?;
        target.recheck()?;
        send(&mut socket, &nonce, deadline, &cancel)?;
        loop {
            if let Some(status) = child.try_wait().map_err(|_| FAILED)? {
                if !status.success() {
                    return Err(FAILED.into());
                }
                break;
            }
            remaining(deadline, &cancel)?;
            std::thread::sleep(Duration::from_millis(20));
        }
        target.recheck()?;
        Ok(original)
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

struct Editor {
    app: crate::ui::PeerBrush,
    frames: u32,
    rendered: Arc<AtomicBool>,
    deadline: Instant,
}
impl eframe::App for Editor {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let rendered = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(event, egui::Event::Screenshot { image, user_data, .. }
                if user_data.data.as_ref().is_some_and(|d| d.downcast_ref::<u32>() == Some(&3))
                    && image.width() >= 320 && image.height() >= 200
                    && image.pixels.windows(2).any(|pair| pair[0] != pair[1]))
            })
        });
        crate::ui::isolate_probe_input(ctx);
        eframe::App::update(&mut self.app, ctx, frame);
        self.frames = self.frames.saturating_add(1);
        if self.frames == 3 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                3_u32,
            )));
        }
        if rendered {
            self.rendered.store(true, Ordering::Release);
        }
        if rendered || Instant::now() >= self.deadline {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
    fn on_exit(&mut self) {
        eframe::App::on_exit(&mut self.app);
    }
}
pub(in crate::sparkle::macos) fn render(root: &Path, deadline: Instant) -> Result<(), String> {
    let locations = crate::settings::Locations::user()?;
    if locations.config != root.join("Library/Application Support/PeerBrush/Config") {
        return Err(FAILED.into());
    }
    let profile = crate::settings::Profile::open(locations, &root.join("no-legacy"))?;
    profile.update_profile().validate()?;
    let prefs = profile.locations.profile();
    let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
    let connection = crate::server::start_with_preferences(
        shared.clone(),
        root.join("state"),
        &prefs,
        &profile.locations.cache,
    )?;
    connection.configure_native_sources(Some(&profile), false)?;
    let rendered = Arc::new(AtomicBool::new(false));
    let observed = rendered.clone();
    eframe::run_native(
        "PeerBrush update verification",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([980., 640.]),
            ..Default::default()
        },
        Box::new(move |cc| {
            let mut app =
                crate::ui::PeerBrush::new_with_preferences(cc, shared, connection, &prefs);
            app.suspend_updates_for_health();
            Ok(Box::new(Editor {
                app,
                frames: 0,
                rendered: observed,
                deadline,
            }))
        }),
    )
    .map_err(|_| FAILED)?;
    profile.update_profile().validate()?;
    if !rendered.load(Ordering::Acquire) {
        return Err(FAILED.into());
    }
    Ok(())
}
pub(super) fn run() -> Result<(), String> {
    let configuration = crate::sparkle::check::compiled()?.ok_or(FAILED)?;
    let fd = unsafe { libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 3) };
    if fd < 0 {
        return Err(FAILED.into());
    }
    let mut socket = unsafe { UnixStream::from_raw_fd(fd) };
    no_sigpipe(&socket)?;
    let parent = u32::try_from(unsafe { libc::getppid() }).map_err(|_| FAILED)?;
    let deadline = Instant::now() + LIMIT;
    let cancel = AtomicBool::new(false);
    let nonce: String = receive(&mut socket, deadline, &cancel)?;
    let verify_owner = || {
        process_identity::authenticate_peer(
            &socket,
            parent,
            &configuration.publisher_team_id,
            false,
            None,
        )?;
        if !checkpoint::preserved_app(&process_identity::executable(parent)?)
            || unsafe { libc::getppid() } as u32 != parent
        {
            return Err(FAILED.to_string());
        }
        Ok(())
    };
    verify_owner()?;
    let root = std::env::current_dir().map_err(|_| FAILED)?;
    if uuid::Uuid::parse_str(&nonce)
        .map_err(|_| FAILED)?
        .to_string()
        != nonce
        || !root
            .file_name()
            .and_then(|v| v.to_str())
            .and_then(|v| v.strip_prefix("peerbrush-sparkle-inspect-"))
            .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok())
        || std::env::var_os("TMPDIR").as_deref() != Some(root.as_os_str())
    {
        return Err(FAILED.into());
    }
    render(&root, deadline)?;
    verify_owner()?;
    send(
        &mut socket,
        &(&nonce, crate::build_info::json()),
        deadline,
        &cancel,
    )?;
    if receive::<String>(&mut socket, deadline, &cancel)? != nonce {
        return Err(FAILED.into());
    }
    Ok(())
}
