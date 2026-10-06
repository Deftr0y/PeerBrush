use peerbrush::{
    brush::{self, Settings},
    engine::{Document, Engine},
    raster::Raster,
};
use serde_json::json;
use std::time::Instant;
fn main() {
    for edge in [2048, 4096] {
        let points = (0..128)
            .map(|i| {
                [
                    120.0 + i as f32 * 10.0,
                    edge as f32 * 0.5 + (i as f32 * 0.08).sin() * 120.0,
                ]
            })
            .collect::<Vec<_>>();
        let settings = Settings {
            radius: 64.0,
            hardness: 0.65,
            flow: 0.4,
            opacity: 0.7,
            ..Default::default()
        };
        let mut samples = vec![];
        for _ in 0..3 {
            let mut pixels = Raster::new(edge, edge);
            let start = Instant::now();
            brush::paint(
                &mut pixels,
                &points,
                settings,
                [233, 84, 32, 255],
                false,
                None,
            )
            .unwrap();
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        let mut doc = Document::new(edge, edge).unwrap();
        doc.layers[0].pixels = Raster::new(edge, edge);
        let layer = doc.layers[0].id.clone();
        let start = Instant::now();
        for count in [16, 32, 64, 96, 128] {
            let commands = [
                json!({"op":"paint","layer":layer,"points":&points[..count],"radius":64,"hardness":0.65,"flow":0.4,"opacity":0.7}),
            ];
            let preview = Engine::preview_edits(doc.clone(), &commands).unwrap();
            std::hint::black_box(preview.preview(None, 1536, None, false).unwrap());
        }
        println!(
            "{edge}x{edge}: stroke median {:.2} ms; five full preview updates {:.2} ms",
            samples[1],
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
