//! Real shared-engine replay + preview timings. Run with --release, without a GUI.
use peerbrush::{
    effects::Effect,
    engine::{id, Document, Engine, Layer},
    preview::{self, Cache},
};
use serde_json::json;
use std::time::Instant;

fn fixture(edge: u32, effects: bool, depth: u16) -> Document {
    let mut doc = Document::new(edge, edge).unwrap();
    doc.bit_depth = depth;
    doc.layers.clear();
    let mut active = Layer::new("Active paint", "paint", edge, edge);
    if effects {
        active.effects = vec![
            Effect {
                id: id(),
                kind: "levels".into(),
                enabled: true,
                settings: json!({"gamma":0.9}),
            },
            Effect {
                id: id(),
                kind: "blur".into(),
                enabled: true,
                settings: json!({"radius":4}),
            },
        ];
    }
    doc.layers.push(active);
    let folder = Layer::new("Reference group", "group", edge, edge);
    let folder_id = folder.id.clone();
    doc.layers.push(folder);
    for i in 0..6 {
        let mut layer = Layer::new("Sparse art", "paint", edge, edge);
        layer.parent = Some(folder_id.clone());
        layer.opacity = 0.7;
        for y in 0..64 {
            for x in 0..128 {
                layer.pixels.set(
                    edge as i32 / 2 + x + i * 15,
                    edge as i32 / 2 + y + i * 9,
                    [80 + i as u8 * 15, 170, 220, 210],
                );
            }
        }
        doc.layers.push(layer);
    }
    let mut backdrop = Layer::new("Backdrop", "fill", edge, edge);
    backdrop.color = [35, 31, 38, 255];
    doc.layers.push(backdrop);
    if depth == 16 {
        for layer in &mut doc.layers {
            layer.pixels.promote16();
        }
    }
    doc
}
fn elapsed(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
fn main() {
    let native = std::env::args().any(|arg| arg == "--native16" || arg == "--effects16");
    let native_effects = std::env::args().any(|arg| arg == "--effects16");
    let depth = if native { 16 } else { 8 };
    for edge in [2048, 4096] {
        for heavy in [false, true] {
            // Native effects have dedicated fidelity tests. This flag measures native live
            // painting; default runs both byte scenes, including cold CPU effect fallback.
            if native && heavy && !native_effects {
                continue;
            }
            let doc = fixture(edge, heavy, depth);
            let target = &doc.layers[0].id;
            let points = (0..128)
                .map(|i| {
                    [
                        120. + i as f32 * 10.,
                        edge as f32 * 0.5 + (i as f32 * 0.08).sin() * 120.,
                    ]
                })
                .collect::<Vec<_>>();
            let command = |count: usize| json!({"op":"paint","layer":target,"points":&points[..count],"radius":64,"hardness":0.65,"flow":0.4,"opacity":0.7});
            // Warm derived caches before comparing equivalent renders.
            let image = Engine::preview_edits(doc.clone(), &[command(128)]).unwrap();
            std::hint::black_box(image.preview(None, 1536, None, false).unwrap());
            let start = Instant::now();
            let old = if native {
                native_reference(&image, 1536)
            } else {
                preview::reference(&image, 1536).unwrap()
            };
            let reference = elapsed(start);
            let start = Instant::now();
            let new = image.preview(None, 1536, None, false).unwrap();
            let compiled = elapsed(start);
            assert_eq!(old.2, new.2);
            let mut full_replay = 0.;
            let mut full_render = 0.;
            let mut expected = Vec::new();
            for count in [16, 32, 64, 96, 128] {
                let start = Instant::now();
                let image = Engine::preview_edits(doc.clone(), &[command(count)]).unwrap();
                full_replay += elapsed(start);
                let start = Instant::now();
                let reference = image.preview(None, 1536, None, false).unwrap();
                full_render += elapsed(start);
                expected.push(reference.2);
            }
            let mut cache = Cache::default();
            cache
                .render(&doc, "stroke", None, 1536, None, false)
                .unwrap();
            let mut cached_replay = 0.;
            let mut cached_render = 0.;
            let mut partial = 0;
            for (sample, count) in [16, 32, 64, 96, 128].into_iter().enumerate() {
                let start = Instant::now();
                let image = cache
                    .edit(doc.clone(), &[command(count)], "stroke")
                    .unwrap();
                cached_replay += elapsed(start);
                let dirty = Some([
                    54,
                    edge as i32 / 2 - 190,
                    (points[count - 1][0] + 67.) as i32,
                    edge as i32 / 2 + 190,
                ]);
                let start = Instant::now();
                let result = cache
                    .render(&image, "stroke", dirty, 1536, None, false)
                    .unwrap();
                cached_render += elapsed(start);
                assert_eq!(
                    result.bytes, expected[sample],
                    "retained update {count} must match full rendering"
                );
                partial += usize::from(result.dirty.is_some());
            }
            println!("{edge}² {depth}-bit {}: {} {reference:.2}ms / full render {compiled:.2}ms; five replay {full_replay:.2}ms +render {full_render:.2}ms; incremental replay {cached_replay:.2}ms +dirty render {cached_render:.2}ms ({partial}/5 partial)",if heavy{"levels+blur"}else{"effect-free 9 layers"},if native{"serial native compositor"}else{"legacy compositor"});
        }
    }
}

fn native_reference(doc: &Document, edge: u32) -> (u32, u32, Vec<u8>) {
    let scale = (edge as f64 / doc.width.max(doc.height) as f64).min(1.);
    let width = ((doc.width as f64 * scale).round() as u32).max(1);
    let height = ((doc.height as f64 * scale).round() as u32).max(1);
    let plan = peerbrush::depth16::Plan16::new(doc).unwrap();
    let mut bytes = vec![0; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let pixel = peerbrush::depth16::display_pixel(plan.sample(
                0,
                (x as f64 / scale) as i32,
                (y as f64 / scale) as i32,
            ));
            let at = ((y * width + x) * 4) as usize;
            bytes[at..at + 4].copy_from_slice(&pixel);
        }
    }
    (width, height, bytes)
}
