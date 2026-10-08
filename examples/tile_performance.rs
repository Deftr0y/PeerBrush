//! Actual-device CPU/GPU preview comparison, including cold upload and bounded eviction.
use peerbrush::{
    engine::{Document, Layer},
    gpu::{self, composite::Compositor},
};
use std::{sync::Arc, time::Instant};
fn main() {
    let backend = gpu::headless().unwrap();
    println!("adapter: {}", backend.adapter);
    for edge in [2048u32, 4096] {
        let mut doc = Document::new(edge, edge).unwrap();
        doc.layers.clear();
        for n in 0..3 {
            let mut layer = Layer::new("Dense normal", "paint", edge, edge);
            layer.opacity = [0.731, 0.613, 1.0][n];
            for ty in 0..edge.div_ceil(256) {
                for tx in 0..edge.div_ceil(256) {
                    let mut tile = vec![0; 256 * 256 * 4];
                    for (i, p) in tile.chunks_exact_mut(4).enumerate() {
                        let x = tx * 256 + i as u32 % 256;
                        let y = ty * 256 + i as u32 / 256;
                        p.copy_from_slice(&[
                            (x * 37 + y * 11 + n as u32 * 53) as u8,
                            (x * 13 + y * 23) as u8,
                            (x * 7 + y * 43) as u8,
                            ((x * 3 + y * 5) % 256) as u8,
                        ]);
                    }
                    layer.pixels.tiles.insert((tx, ty), Arc::new(tile));
                }
            }
            doc.layers.push(layer);
        }
        let scale = 1536.0 / edge as f32;
        let coords: Vec<_> = (0..1536).map(|x| (x as f32 / scale) as i32).collect();
        let colors = vec![None; 3];
        let start = Instant::now();
        let cpu = doc.preview(None, 1536, None, false).unwrap().2;
        let cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut compositor = Compositor::new(&backend).unwrap();
        let start = Instant::now();
        let cold = compositor.render(&doc, &colors, &coords, &coords).unwrap();
        let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let warm = compositor.render(&doc, &colors, &coords, &coords).unwrap();
        let warm_ms = start.elapsed().as_secs_f64() * 1000.0;
        let max = cold
            .iter()
            .chain(&warm)
            .zip(cpu.iter().cycle())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(max <= 1, "GPU preview error {max}");
        println!("{edge}²: CPU {cpu_ms:.2}ms, GPU cold {cold_ms:.2}ms, repeated {warm_ms:.2}ms; max error {max}, resident {} MiB, upload {} MiB, hits {}",compositor.resident_bytes()/1024/1024,compositor.uploaded/1024/1024,compositor.hits);
    }
}
