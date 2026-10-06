#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use peerbrush::{engine::Engine, server, ui::PeerBrush};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn main() {
    if let Err(error) = run() {
        eprintln!("PeerBrush: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut state_dir = server::default_state_dir();
    if let Some(i) = args.iter().position(|a| a == "--state-dir") {
        if i + 1 >= args.len() {
            return Err("--state-dir needs a path".into());
        }
        state_dir = PathBuf::from(args.remove(i + 1));
        args.remove(i);
    }
    match args.first().map(String::as_str) {
        Some("mcp") => return server::stdio(&state_dir),
        Some("discover") => {
            let manifest = peerbrush::discovery::discover()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        Some("cli") => {
            let method = args.get(1).map(String::as_str).unwrap_or("observe");
            let params = if let Some(file) = args.get(2).filter(|s| s.starts_with('@')) {
                serde_json::from_slice::<Value>(
                    &std::fs::read(&file[1..]).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?
            } else {
                serde_json::from_str::<Value>(args.get(2).map(String::as_str).unwrap_or("{}"))
                    .map_err(|e| e.to_string())?
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
            println!("PeerBrush — You and your AI. Same canvas.\npeerbrush [--state-dir PATH] [--headless]\npeerbrush mcp [--state-dir PATH]\npeerbrush discover\npeerbrush cli METHOD [JSON | @file.json] [--state-dir PATH]");
            return Ok(());
        }
        _ => {}
    }
    if server::focus_existing(&state_dir) {
        return Ok(());
    }
    let shared = Arc::new(Mutex::new(Engine::new()));
    let connection = server::start(shared.clone(), state_dir)?;
    if args.iter().any(|a| a == "--headless") {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    let options = eframe::NativeOptions {
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
        Box::new(move |cc| Ok(Box::new(PeerBrush::new(cc, shared, connection)))),
    )
    .map_err(|e| e.to_string())
}
