//! Blank-canvas input cadence, without GPU presentation. Run with --release.
use peerbrush::{
    engine::{Document, Engine},
    preview::Cache,
};
use serde_json::json;
use std::time::Instant;

fn main() {
    for depth in [8, 16] {
        for (width, height) in [(1024, 768), (2048, 2048)] {
            let doc = Document::new_depth(width, height, depth).unwrap();
            let layer = &doc.layers[0].id;
            let points: Vec<_> = (0..120)
                .map(|i| {
                    [
                        80. + i as f32 * 6.,
                        height as f32 * 0.5 + (i as f32 * 0.1).sin() * 60.,
                    ]
                })
                .collect();
            for smoothing in [0., 0.35] {
                let command = |count: usize| json!({"op":"paint","layer":layer,"points":&points[..count],"radius":18,"smoothing":smoothing,"color":[233,84,32,255]});
                // Reproduce the previous first-input strategy in the same warmed process.
                let mut full = Cache::default();
                full.render(&doc, "", None, 1536, None, false).unwrap();
                let start = Instant::now();
                let draft = full.edit(doc.clone(), &[command(1)], "stroke").unwrap();
                let reference = full
                    .render(&draft, "stroke", None, 1536, None, false)
                    .unwrap();
                let full_first = start.elapsed().as_secs_f64() * 1000.;
                let mut cache = Cache::default();
                // The native UI starts a new gesture key on the first input.
                cache.render(&doc, "", None, 1536, None, false).unwrap();
                let mut samples = Vec::new();
                let mut partial = 0;
                for count in 1..=points.len() {
                    let command = command(count);
                    let start = Instant::now();
                    let (draft, dirty) = cache
                        .edit_with_region(doc.clone(), std::slice::from_ref(&command), "stroke")
                        .unwrap();
                    let rendered = cache
                        .render(&draft, "stroke", dirty, 1536, None, false)
                        .unwrap();
                    samples.push(start.elapsed().as_secs_f64() * 1000.);
                    partial += usize::from(rendered.dirty.is_some());
                    if count == 1 {
                        assert_eq!(rendered.bytes, reference.bytes);
                    }
                    if count == 1 || count % 30 == 0 {
                        let reference = Engine::preview_edits(doc.clone(), &[command]).unwrap();
                        assert_eq!(
                            rendered.bytes,
                            reference.preview(None, 1536, None, false).unwrap().2
                        );
                        assert_eq!(draft.layers[0].pixels, reference.layers[0].pixels);
                    }
                }
                let first = samples[0];
                samples.sort_by(f64::total_cmp);
                println!("{width}x{height} native{depth}, smoothing {smoothing}: first full {full_first:.2}ms / retained {first:.2}ms; median {:.2}ms; p95 {:.2}ms; max {:.2}ms; {partial}/120 partial uploads (CPU replay+render only)", samples[60], samples[114], samples[119]);
                assert!(doc.layers[0].pixels.tiles.is_empty());
                assert!(doc.layers[0].pixels.samples16.is_empty());
                assert_eq!(doc.revision, 0);
            }
        }
    }
}
