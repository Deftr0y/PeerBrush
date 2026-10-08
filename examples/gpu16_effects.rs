//! Full native16 upload + tiled dispatch + readback timing against the native CPU oracle.
use peerbrush::{
    depth16::{color, Image16},
    gpu,
};
use serde_json::json;
use std::time::Instant;
fn main() -> Result<(), String> {
    let edge: u32 = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(2048);
    if !(1..=4096).contains(&edge) {
        return Err("Use a1..4096 image edge".into());
    }
    let backend = gpu::headless()?;
    println!(
        "{}",
        json!({"adapter":backend.adapter,"edge":edge,"precision":16,"profile":if cfg!(debug_assertions) {"debug"} else {"release"}})
    );
    let mut state = 0x513decafu32;
    let mut words = Vec::with_capacity(edge as usize * edge as usize * 4);
    for n in 0..edge * edge {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let r = (state & 65535) as u16;
        let g = (state >> 16) as u16;
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let b = (state & 65535) as u16;
        words.extend([r, g, b, [0, 1, 33, 12345, 65535][n as usize % 5]]);
    }
    for (kind, settings) in [
        ("hsl", json!({"hue":47,"saturation":0.4,"lightness":0.1})),
        (
            "color_balance",
            json!({"shadows":[0.5,-0.1,0.2],"midtones":[0.1,0.2,-0.4],"highlights":[-0.2,0.3,0.1]}),
        ),
        (
            "adjust",
            json!({"brightness":0.13,"contrast":1.4,"saturation":2.1}),
        ),
    ] {
        let mut cpu = Image16 {
            width: edge,
            height: edge,
            words: words.clone(),
        };
        let start = Instant::now();
        color::apply_cpu(&mut cpu, kind, &settings)?;
        let cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut times = Vec::new();
        let mut max_error = 0;
        for pass in 0..4 {
            let mut computed = Image16 {
                width: edge,
                height: edge,
                words: words.clone(),
            };
            let start = Instant::now();
            if !backend.apply16(&mut computed, kind, &settings)? {
                return Err(format!("GPU did not dispatch native16 {kind}"));
            }
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if pass > 0 {
                times.push(elapsed);
            }
            max_error = max_error.max(
                cpu.words
                    .iter()
                    .zip(&computed.words)
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap_or(0),
            );
        }
        if max_error > 1 {
            return Err(format!("{kind}: GPU differs by{max_error} native words"));
        }
        times.sort_by(f64::total_cmp);
        let gpu_ms = times[1];
        println!(
            "{}",
            json!({"effect":kind,"precision":16,"cpu_ms":cpu_ms,"gpu_ms":gpu_ms,"speedup":cpu_ms/gpu_ms,"max_word_error":max_error,"includes":"upload + tiled dispatch + readback"})
        );
    }
    Ok(())
}
