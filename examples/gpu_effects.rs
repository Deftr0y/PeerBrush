//! Native parity and complete transfer+compute+readback timings, with no global backend installation.
use peerbrush::{
    effects::{self, Image},
    gpu,
};
use serde_json::json;
use std::time::Instant;
fn main() -> Result<(), String> {
    let edge: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024);
    if !(1..=4096).contains(&edge) {
        return Err("Use an image edge between 1 and 4096".into());
    }
    let backend = gpu::headless()?;
    println!(
        "{}",
        json!({"adapter":backend.adapter,"edge":edge,"profile":if cfg!(debug_assertions) { "debug" } else { "release" }})
    );
    let mut bytes = Vec::with_capacity(edge as usize * edge as usize * 4);
    let mut state = 0x18fa90c3u32;
    for pixel in 0..edge * edge {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let mut rgba = state.to_le_bytes();
        rgba[3] = [0, 1, 17, 63, 128, 254, 255][pixel as usize % 7];
        if pixel % 17 == 0 {
            rgba[..3].fill(255);
        }
        bytes.extend(rgba);
    }
    for (kind, settings) in [
        ("blur", json!({"radius":32})),
        (
            "bloom",
            json!({"threshold":0.65,"spread":16,"strength":1.5}),
        ),
        ("levels", json!({"black":0.1,"white":0.9,"gamma":1.4})),
        ("hsl", json!({"hue":47,"saturation":0.4,"lightness":0.1})),
        (
            "color_balance",
            json!({"shadows":[0.5,-0.1,0.2],"midtones":[0.1,0.2,-0.4],"highlights":[-0.2,0.3,0.1]}),
        ),
    ] {
        let mut cpu = Image {
            width: edge,
            height: edge,
            bytes: bytes.clone(),
        };
        let start = Instant::now();
        effects::apply_cpu(&mut cpu, kind, &settings)?;
        let cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut timings = Vec::new();
        let mut max_error = 0u8;
        for pass in 0..4 {
            let mut computed = Image {
                width: edge,
                height: edge,
                bytes: bytes.clone(),
            };
            let start = Instant::now();
            if !backend.apply(&mut computed, kind, &settings)? {
                return Err(format!("GPU did not dispatch {kind}"));
            }
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if pass > 0 {
                timings.push(elapsed);
            }
            max_error = max_error.max(
                cpu.bytes
                    .iter()
                    .zip(&computed.bytes)
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap_or(0),
            );
        }
        timings.sort_by(f64::total_cmp);
        let gpu_ms = timings[1];
        let tolerance = if kind == "blur" || kind == "levels" {
            0
        } else {
            1
        };
        if max_error > tolerance {
            return Err(format!("{kind}: GPU differs by {max_error} bytes"));
        }
        println!(
            "{}",
            json!({"effect":kind,"cpu_ms":cpu_ms,"gpu_ms":gpu_ms,"speedup":cpu_ms/gpu_ms,"max_byte_error":max_error,"includes":"upload + dispatch + readback"})
        );
    }
    Ok(())
}
