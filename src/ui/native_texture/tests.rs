use super::*;
use std::time::{Duration, Instant};
fn wait_empty(pool: &ResidentBudget) {
    let end = Instant::now() + Duration::from_secs(5);
    while pool.used() != 0 {
        assert!(
            Instant::now() < end,
            "Image owner or retirement admission did not release"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn image(retirement: &Retirement, size: [usize; 2], value: u8) -> tiles::OwnedImage {
    tiles::OwnedImage::convert(size, retirement, |i| Ok([value, i as u8, 71, 199])).unwrap()
}
#[test]
fn actual_delta_clones_hold_pixel_charge_after_handle_presenter_and_context_teardown() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 8).unwrap();
    let baseline = pool.used();
    let ctx = egui::Context::default();
    let texture = image(&retirement, [3, 2], 43).upload(&ctx);
    let delta = ctx.tex_manager().write().take_delta();
    assert!(!delta.set.is_empty());
    let retained_renderer = delta.clone();
    drop(texture);
    drop(ctx);
    drop(delta);
    retirement.collect();
    assert!(
        pool.used() > baseline,
        "Renderer-held ColorImage must retain its exact charge"
    );
    // A presenter may disappear before deferred viewport/renderer deltas.
    drop(retirement);
    assert!(pool.used() > 0);
    drop(retained_renderer);
    wait_empty(&pool);
}
#[test]
fn retained_owner_waits_for_weak_upgrade_and_seals_future_resurrection_before_refund() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 8).unwrap();
    let baseline = pool.used();
    let ctx = egui::Context::default();
    let candidate = image(&retirement, [3, 2], 43);
    let weak = Arc::downgrade(&candidate.image);
    let texture = candidate.upload(&ctx);
    let upgraded = weak.upgrade().unwrap();
    drop(texture);
    drop(ctx);
    retirement.collect();
    assert!(pool.used() > baseline);
    drop(upgraded);
    retirement.collect();
    assert_eq!(pool.used(), baseline);
    assert!(weak.upgrade().is_none());
    drop(weak);
    drop(retirement);
    wait_empty(&pool);
}
#[test]
fn new_admission_retires_released_images_before_reserving_replacement_pixels() {
    let pool = ResidentBudget::new(1024 * 1024).unwrap();
    let (retirement, release) = retirement::paused(&pool).unwrap();
    let ctx = egui::Context::default();
    let texture = image(&retirement, [3, 2], 43).upload(&ctx);
    let delta = ctx.tex_manager().write().take_delta();
    let pressure = pool
        .reserve_bytes(pool.limit() - pool.used() - 100, pool.limit())
        .unwrap();
    drop(texture);
    drop(ctx);
    drop(delta);
    // The background reaper is gated, and132 bytes cannot fit until the old
    // actual image is synchronously destroyed by the admission path itself.
    let replacement = image(&retirement, [1, 1], 97);
    drop(replacement);
    drop(pressure);
    release.send(()).unwrap();
    drop(retirement);
    wait_empty(&pool);
}
#[test]
fn whole_overwrite_and_partial_delta_owners_retire_independently() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 8).unwrap();
    let baseline = pool.used();
    let ctx = egui::Context::default();
    let mut texture = image(&retirement, [4, 3], 11).upload(&ctx);
    let first = ctx.tex_manager().write().take_delta();
    let old_charge = pool.used() - baseline;
    let tiles::OwnedImage {
        image: replacement,
        ticket,
    } = image(&retirement, [4, 3], 97);
    ticket.track(&replacement);
    texture.set(
        egui::ImageData::Color(replacement),
        egui::TextureOptions::LINEAR,
    );
    let tiles::OwnedImage {
        image: partial,
        ticket,
    } = image(&retirement, [1, 1], 149);
    ticket.track(&partial);
    texture.set_partial(
        [1, 1],
        egui::ImageData::Color(partial),
        egui::TextureOptions::LINEAR,
    );
    let second = ctx.tex_manager().write().take_delta();
    assert_eq!(second.set.len(), 2);
    let renderer_color = second.set[1].1.image.clone();
    drop(texture);
    drop(ctx);
    drop(first);
    retirement.collect();
    assert!(pool.used() > baseline);
    assert_eq!(pool.used() - baseline, old_charge + 4 + 128);
    drop(second);
    retirement.collect();
    assert_eq!(pool.used() - baseline, 4 + 128);
    drop(renderer_color);
    retirement.collect();
    assert_eq!(pool.used(), baseline);
    drop(retirement);
    wait_empty(&pool);
}
#[test]
fn retirement_start_slots_and_pixel_admission_refuse_before_unowned_allocations() {
    let pool = ResidentBudget::new(1024 * 1024).unwrap();
    assert!(retirement::refused_start(&pool).is_err());
    assert_eq!(pool.used(), 0);
    let retirement = retirement::small(&pool, 2).unwrap();
    let baseline = pool.used();
    let a = image(&retirement, [1, 1], 1);
    let b = image(&retirement, [1, 1], 2);
    let before = pool.used();
    let mut called = false;
    assert!(tiles::OwnedImage::convert([1, 1], &retirement, |_| {
        called = true;
        Ok([0; 4])
    })
    .is_err());
    assert!(!called);
    assert_eq!(pool.used(), before);
    drop(a);
    drop(b);
    assert_eq!(pool.used(), baseline);
    assert!(
        tiles::OwnedImage::convert([MAX_PIXELS + 1, 1], &retirement, |_| panic!(
            "Pixel conversion before admission"
        ))
        .is_err()
    );
    let pressure = pool
        .reserve_bytes(pool.limit() - pool.used() - 100, pool.limit())
        .unwrap();
    assert!(tiles::OwnedImage::convert([1, 1], &retirement, |_| panic!(
        "Pixel conversion before admission"
    ))
    .is_err());
    drop(pressure);
    drop(retirement);
    wait_empty(&pool);
}
struct Bytes {
    width: u32,
    pixels: Vec<u8>,
}
impl Bytes {
    fn new(w: u32, h: u32) -> Self {
        let mut pixels = vec![];
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&[
                    (x * 13 + y * 3) as u8,
                    (x * 7 + y * 17) as u8,
                    71,
                    if (x + y) % 5 == 0 { 0 } else { 199 },
                ]);
            }
        }
        Self { width: w, pixels }
    }
}
impl tiles::Rows for Bytes {
    fn row(&self, y: u32) -> Result<&[u8]> {
        let stride = self.width as usize * 4;
        self.pixels
            .get(y as usize * stride..(y as usize + 1) * stride)
            .ok_or_else(|| "Invalid test row".into())
    }
}
fn linear(pixels: &[egui::Color32], size: [usize; 2], x: f64, y: f64) -> [f64; 4] {
    let x = x - 0.5;
    let y = y - 0.5;
    let left = x.floor();
    let top = y.floor();
    let tx = x - left;
    let ty = y - top;
    let mut result = [0.; 4];
    for (dy, wy) in [(0., 1. - ty), (1., ty)] {
        for (dx, wx) in [(0., 1. - tx), (1., tx)] {
            let sx = (left + dx).clamp(0., size[0] as f64 - 1.) as usize;
            let sy = (top + dy).clamp(0., size[1] as f64 - 1.) as usize;
            let value = pixels[sy * size[0] + sx].to_array();
            for c in 0..4 {
                result[c] += value[c] as f64 * wx * wy;
            }
        }
    }
    result
}
#[test]
fn bounded_halo_tiles_cover_full_grid_and_match_global_linear_samples_at_every_seam() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 64).unwrap();
    let bytes = Bytes::new(23, 17);
    let full: Vec<_> = bytes
        .pixels
        .chunks_exact(4)
        .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();
    let layout = tiles::Layout::new(23, 17, 9).unwrap();
    let mut covered = vec![0; 23 * 17];
    let tiles = tiles::Tiles::prepare(23, 17, 9, &bytes, &retirement, &mut || Ok(())).unwrap();
    assert_eq!(tiles.uploads.len(), layout.count);
    for upload in &tiles.uploads {
        let a = upload.area;
        let size = upload.pixels.image.size;
        assert!(size[0] <= 9 && size[1] <= 9 && size[0] * size[1] <= MAX_PIXELS);
        for y in a.inner[1]..a.inner[3] {
            for x in a.inner[0]..a.inner[2] {
                covered[(y * 23 + x) as usize] += 1;
            }
        }
        for y in [
            a.inner[1] as f64,
            a.inner[1] as f64 + 0.125,
            (a.inner[3] as f64 - 0.125).max(a.inner[1] as f64),
            a.inner[3] as f64,
        ] {
            for x in [
                a.inner[0] as f64,
                a.inner[0] as f64 + 0.125,
                (a.inner[2] as f64 - 0.125).max(a.inner[0] as f64),
                a.inner[2] as f64,
            ] {
                let actual = linear(
                    &upload.pixels.image.pixels,
                    size,
                    x - a.outer[0] as f64,
                    y - a.outer[1] as f64,
                );
                let expected = linear(&full, [23, 17], x, y);
                for c in 0..4 {
                    assert!(
                        (actual[c] - expected[c]).abs() < 1e-9,
                        "Halo changes global sample at {x},{y}"
                    );
                }
            }
        }
    }
    assert!(covered.iter().all(|n| *n == 1));
    drop(tiles);
    drop(retirement);
    wait_empty(&pool);
    for side in [8192, 4096, 256] {
        let layout = tiles::Layout::new(8192, 8192, side).unwrap();
        assert!(layout.count <= 4096);
        for a in layout.areas() {
            assert!(
                (a.outer[2] - a.outer[0]) as usize * (a.outer[3] - a.outer[1]) as usize
                    <= MAX_PIXELS
            );
        }
    }
    assert!(tiles::Layout::new(8192, 8192, 64).is_err());
}
#[test]
fn actual_painter_meshes_share_transformed_edges_and_crop_each_neighbor_halo() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 64).unwrap();
    let bytes = Bytes::new(13, 11);
    let ctx = egui::Context::default();
    let textures = tiles::Tiles::prepare(13, 11, 8, &bytes, &retirement, &mut || Ok(()))
        .unwrap()
        .publish(&ctx, &mut || Ok(()))
        .unwrap();
    let project = |uv: [f32; 2]| {
        egui::pos2(
            73. - uv[0] * 199. - uv[1] * 37.,
            41. + uv[0] * 51. - uv[1] * 131.,
        )
    };
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        textures.paint_with(
            &ctx.layer_painter(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new("native-mesh-test"),
            )),
            project,
            egui::Color32::WHITE,
        )
    });
    let meshes: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::Shape::Mesh(mesh) = &shape.shape {
                Some(mesh)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(meshes.len(), textures.tiles.len());
    let mut shared = std::collections::HashMap::new();
    for (tile, mesh) in textures.tiles.iter().zip(meshes) {
        assert_eq!(mesh.texture_id, tile.texture.id());
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices, [0, 1, 2, 0, 2, 3]);
        let a = tile.area;
        let points = [
            [a.inner[0], a.inner[1]],
            [a.inner[2], a.inner[1]],
            [a.inner[2], a.inner[3]],
            [a.inner[0], a.inner[3]],
        ];
        for ([x, y], vertex) in points.into_iter().zip(&mesh.vertices) {
            assert_eq!(vertex.pos, project([x as f32 / 13., y as f32 / 11.]));
            assert_eq!(
                vertex.uv,
                egui::pos2(
                    (x - a.outer[0]) as f32 / (a.outer[2] - a.outer[0]) as f32,
                    (y - a.outer[1]) as f32 / (a.outer[3] - a.outer[1]) as f32
                )
            );
            let bits = [vertex.pos.x.to_bits(), vertex.pos.y.to_bits()];
            if let Some(previous) = shared.insert([x, y], bits) {
                assert_eq!(previous, bits);
            }
        }
    }
    drop(output);
    drop(textures);
    drop(ctx);
    retirement.collect();
    drop(retirement);
    wait_empty(&pool);
}
#[test]
fn conversion_cancel_and_post_upload_cancel_keep_prior_complete_texture_set() {
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let retirement = retirement::small(&pool, 64).unwrap();
    let baseline = pool.used();
    let bytes = Bytes::new(13, 11);
    let ctx = egui::Context::default();
    let old = tiles::Tiles::prepare(13, 11, 8, &bytes, &retirement, &mut || Ok(()))
        .unwrap()
        .publish(&ctx, &mut || Ok(()))
        .unwrap();
    let ids: Vec<_> = old.tiles.iter().map(|t| t.texture.id()).collect();
    let before = pool.used();
    let mut checks = 0;
    assert!(
        tiles::Tiles::prepare(13, 11, 8, &bytes, &retirement, &mut || {
            checks += 1;
            if checks > 8 {
                Err("Cancelled upload".into())
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    assert_eq!(pool.used(), before);
    assert_eq!(
        ids,
        old.tiles.iter().map(|t| t.texture.id()).collect::<Vec<_>>()
    );
    let candidate = tiles::Tiles::prepare(13, 11, 8, &bytes, &retirement, &mut || Ok(())).unwrap();
    let mut checks = 0;
    assert!(candidate
        .publish(&ctx, &mut || {
            checks += 1;
            if checks == 3 {
                Err("Superseded after enqueue".into())
            } else {
                Ok(())
            }
        })
        .is_err());
    assert_eq!(
        ids,
        old.tiles.iter().map(|t| t.texture.id()).collect::<Vec<_>>()
    );
    drop(old);
    let delta = ctx.tex_manager().write().take_delta();
    drop(ctx);
    assert!(pool.used() > baseline);
    drop(delta);
    retirement.collect();
    assert_eq!(pool.used(), baseline);
    drop(retirement);
    wait_empty(&pool);
}
