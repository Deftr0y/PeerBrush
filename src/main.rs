#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use peerbrush::{activation, engine::Engine, server, ui::PeerBrush};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn main() {
    if let Err(error) = run() {
        eprintln!("PeerBrush: {error}");
        // File-association launches have no console. Keep failures visible without
        // interfering with the stdio MCP bridge or command-line clients.
        #[cfg(windows)]
        {
            let native = activation::native_error_dialog(std::env::args_os().skip(1));
            if native {
                rfd::MessageDialog::new()
                    .set_title("PeerBrush could not open")
                    .set_description(&error)
                    .set_level(rfd::MessageLevel::Error)
                    .show();
            }
        }
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--build-info" {
        println!("{}", peerbrush::build_info::json());
        return Ok(());
    }
    let (state_dir, args, local_preferences) = activation::storage_arguments(
        arguments,
        peerbrush::packaged::default_state_dir(server::default_state_dir())?,
    )?;
    if !peerbrush::packaged::external_config_is_visible(&state_dir)? {
        return Err("The state directory would be hidden from external AI clients by Windows package isolation. Choose a writable --state-dir outside AppData.".into());
    }
    match args.first().and_then(|a| a.to_str()) {
        Some("--version" | "-V") => {
            println!("PeerBrush {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("mcp") => return server::stdio(&state_dir),
        Some("updates") => {
            let channel = match args.as_slice() {
                [_, action] if action == "check" => peerbrush::updates::Channel::Stable,
                [_, action, option] if action == "check" && option == "--preview" => {
                    peerbrush::updates::Channel::Preview
                }
                _ => return Err("Use updates check [--preview]".into()),
            };
            let report =
                peerbrush::updates::check(channel, peerbrush::updates::Target::current()?, |_| {})?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        Some("settings") => {
            let locations = peerbrush::settings::Locations::user()?;
            match args.get(1).and_then(|s| s.to_str()) {
                Some("paths") if args.len() == 2 => {}
                Some("migrate") if args.len() == 4 && args[2] == "--from" => {
                    let source = std::path::absolute(std::path::PathBuf::from(&args[3]))
                        .map_err(|e| e.to_string())?;
                    let _profile =
                        peerbrush::settings::Profile::migrate(locations.clone(), &source)?;
                }
                _ => {
                    return Err(
                        "Use settings paths or settings migrate --from LEGACY_STATE_DIR".into(),
                    )
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&locations).map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        Some("discover") => {
            let manifest = peerbrush::discovery::discover()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        Some("cli") => {
            let method = args
                .get(1)
                .map(|a| a.to_str().ok_or("CLI method must be valid text"))
                .transpose()?
                .unwrap_or("observe");
            let raw = args
                .get(2)
                .map(|a| a.to_str().ok_or("CLI parameters must be valid text"))
                .transpose()?;
            let params = if let Some(file) = raw.filter(|s| s.starts_with('@')) {
                serde_json::from_slice::<Value>(
                    &std::fs::read(&file[1..]).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?
            } else {
                serde_json::from_str::<Value>(raw.unwrap_or("{}")).map_err(|e| e.to_string())?
            };
            let mut reply =
                server::client(&state_dir, "rpc", &json!({"method":method,"params":params}))?;
            if let Some(images) = reply["result"]["images"].as_array_mut() {
                use base64::Engine as _;
                std::fs::create_dir_all(state_dir.join("previews")).map_err(|e| e.to_string())?;
                for img in images {
                    let data = img["data"].as_str().ok_or("Missing image data")?;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .map_err(|e| e.to_string())?;
                    let path = state_dir
                        .join("previews")
                        .join(format!("{}.png", peerbrush::engine::id()));
                    server::atomic_write(&path, &bytes)?;
                    img.as_object_mut().unwrap().remove("data");
                    img["path"] = json!(path);
                }
            }
            println!("{}", serde_json::to_string_pretty(&reply).unwrap());
            if reply["ok"] == false {
                return Err(reply["error"].as_str().unwrap_or("Command failed").into());
            }
            return Ok(());
        }
        Some("--help") => {
            println!("PeerBrush — You and your AI. Same canvas.\npeerbrush [--state-dir PATH] [FILE ...]\npeerbrush [--state-dir PATH] --open FILE [--open FILE ...]\npeerbrush [--state-dir PATH] -- FILE ...\npeerbrush [--state-dir PATH] --headless\npeerbrush mcp [--state-dir PATH]\npeerbrush --build-info\npeerbrush discover\npeerbrush cli METHOD [JSON | @file.json] [--state-dir PATH]\npeerbrush updates check [--preview]\npeerbrush settings paths\npeerbrush settings migrate --from LEGACY_STATE_DIR\nFiles open in independent project tabs, preserving existing work.\nDefault launches use per-user preferences; --state-dir retains instance-local preferences.");
            return Ok(());
        }
        _ => {}
    }
    let launch = activation::Launch::parse(args)?;
    let files = launch.absolute_files()?;
    if server::activate_existing(&state_dir, &files)? {
        return Ok(());
    }
    let profile = if local_preferences {
        None
    } else {
        match peerbrush::settings::Profile::open(
            peerbrush::settings::Locations::user()?,
            &state_dir,
        ) {
            Ok(profile) => Some(profile),
            Err(error) => {
                // A simultaneous default launch can own the profile before its
                // listener is ready. File activation still reaches that window.
                if error.starts_with("Settings are in use") {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                    while std::time::Instant::now() < deadline {
                        if server::activate_existing(&state_dir, &files)? {
                            return Ok(());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
                return Err(error);
            }
        }
    };
    let preferences = profile
        .as_ref()
        .map_or_else(|| state_dir.clone(), |p| p.locations.profile());
    let cache = profile
        .as_ref()
        .map_or_else(|| state_dir.clone(), |p| p.locations.cache.clone());
    let shared = Arc::new(Mutex::new(Engine::new()));
    if !launch.headless {
        activation::enable(&shared);
        if !files.is_empty() {
            activation::enqueue(&shared, files.clone())?;
        }
    }
    let connection = match server::start_with_preferences(
        shared.clone(),
        state_dir.clone(),
        &preferences,
        &cache,
    ) {
        Ok(connection) => connection,
        Err(error) => {
            // Another Explorer launch can acquire the instance lock before its
            // connection file is ready. Wait briefly for that same instance.
            if error.starts_with("PeerBrush is already running for this workspace") {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                while std::time::Instant::now() < deadline {
                    if server::activate_existing(&state_dir, &files)? {
                        return Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            return Err(error);
        }
    };
    if launch.headless {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    let options = eframe::NativeOptions {
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            present_mode: if cfg!(target_os = "windows") {
                eframe::wgpu::PresentMode::AutoNoVsync
            } else {
                eframe::wgpu::PresentMode::AutoVsync
            },
            desired_maximum_frame_latency: Some(1),
            ..Default::default()
        },
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("PeerBrush")
            .with_icon({
                let mark = image::load_from_memory(include_bytes!("../assets/peerbrush-logo.png"))
                    .map_err(|e| e.to_string())?
                    .resize(256, 256, image::imageops::FilterType::Lanczos3)
                    .to_rgba8();
                eframe::egui::IconData {
                    width: mark.width(),
                    height: mark.height(),
                    rgba: mark.into_raw(),
                }
            })
            .with_inner_size([1360.0, 900.0])
            .with_min_inner_size([980.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "PeerBrush",
        options,
        Box::new(move |cc| {
            Ok(Box::new(PeerBrush::new_with_preferences(
                cc,
                shared,
                connection,
                &preferences,
            )))
        }),
    )
    .map_err(|e| e.to_string())
}
