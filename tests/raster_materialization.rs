use peerbrush::{
    effects::{self, Effect, Image},
    engine::{id, Document, Layer},
    raster::{blend, Raster, TILE},
};
use serde_json::json;
use std::sync::Arc;

fn pixel_reference(raster: &Raster) -> Vec<u8> {
    (0..raster.height)
        .flat_map(|y| (0..raster.width).flat_map(move |x| raster.get(x as i32, y as i32)))
        .collect()
}
fn identity_effect() -> Effect {
    Effect {
        weight: 1.0,
        id: id(),
        kind: "hsl".into(),
        enabled: true,
        settings: json!({}),
    }
}

#[test]
fn sparse_tile_rows_preserve_pixels_and_ignore_edge_padding_and_external_tiles() {
    let mut raster = Raster::new(513, 259);
    for (x, y, pixel) in [
        (0, 0, [7, 9, 11, 0]),
        (255, 255, [100, 80, 60, 127]),
        (256, 255, [40, 200, 3, 255]),
        (512, 258, [17, 39, 91, 1]),
    ] {
        raster.set(x, y, pixel);
    }
    let edge = Arc::make_mut(raster.tiles.get_mut(&(2, 1)).unwrap());
    for y in 0..TILE as usize {
        for x in 0..TILE as usize {
            if x > 0 || y >= 3 {
                edge[(y * TILE as usize + x) * 4..(y * TILE as usize + x) * 4 + 4]
                    .copy_from_slice(&[255, 1, 255, 255]);
            }
        }
    }
    raster.tiles.insert(
        (u32::MAX, u32::MAX),
        Arc::new(vec![255; (TILE * TILE * 4) as usize]),
    );
    raster
        .tiles
        .insert((0, 2), Arc::new(vec![123; (TILE * TILE * 4) as usize]));
    let history = raster.clone();
    let expected = pixel_reference(&raster);
    assert_eq!(raster.rgba(), expected);
    assert_eq!(history.rgba(), expected);
    for (key, tile) in &raster.tiles {
        assert!(Arc::ptr_eq(tile, &history.tiles[key]));
    }
    assert_eq!(Raster::new(257, 1).rgba(), vec![0; 257 * 4]);
    assert!(Raster::new(0, 0).rgba().is_empty());
}

#[test]
fn dense_partial_tiles_roundtrip_without_row_or_channel_changes() {
    let (w, h) = (271, 267);
    let mut bytes = vec![0; w * h * 4];
    for (index, pixel) in bytes.chunks_exact_mut(4).enumerate() {
        pixel.copy_from_slice(&[
            (index % 251) as u8,
            ((index / 7) % 256) as u8,
            ((index / 31) % 256) as u8,
            [0, 1, 127, 255][index % 4],
        ]);
    }
    let raster = Raster::from_rgba(w as u32, h as u32, &bytes).unwrap();
    assert_eq!(raster.rgba(), bytes);
}

#[test]
fn sparse_import_keeps_hidden_rgb_and_zero_padding_without_allocating_empty_tiles() {
    let (w, h) = (513usize, 259usize);
    let mut bytes = vec![0; w * h * 4];
    bytes[(257 * w + 512) * 4..(257 * w + 512) * 4 + 4].copy_from_slice(&[17, 29, 41, 0]);
    let raster = Raster::from_rgba(w as u32, h as u32, &bytes).unwrap();
    assert_eq!(raster.tiles.len(), 1);
    assert!(raster.tiles.contains_key(&(2, 1)));
    assert_eq!(raster.get(512, 257), [17, 29, 41, 0]);
    assert_eq!(raster.rgba(), bytes);
    let tile = &raster.tiles[&(2, 1)];
    for (index, pixel) in tile.chunks_exact(4).enumerate() {
        if index != TILE as usize {
            assert_eq!(pixel, [0; 4]);
        }
    }
    assert!(Raster::from_rgba(513, 259, &vec![0; 513 * 259 * 4])
        .unwrap()
        .tiles
        .is_empty());
    assert!(Raster::from_rgba(2, 2, &[0; 15]).is_err());
    assert!(Raster::from_rgba(0, 1, &[]).is_err());
}

#[test]
#[ignore = "Manual release-mode materialization benchmark with scalar byte oracle"]
fn benchmark_large_dense_import_and_materialization() {
    use std::time::Instant;
    let (w, h) = (2048u32, 1536u32);
    let mut bytes = vec![0; w as usize * h as usize * 4];
    for (i, pixel) in bytes.chunks_exact_mut(4).enumerate() {
        if (i / (w as usize * 256)) % 3 != 0 {
            pixel.copy_from_slice(&[
                (i % 251) as u8,
                ((i / 7) % 256) as u8,
                ((i / 31) % 256) as u8,
                [0, 1, 127, 255][i % 4],
            ]);
        }
    }
    let start = Instant::now();
    let fast = Raster::from_rgba(w, h, &bytes).unwrap();
    let fast_import = start.elapsed();
    let start = Instant::now();
    let mut scalar = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let at = ((y * w + x) * 4) as usize;
            scalar.set(x as i32, y as i32, bytes[at..at + 4].try_into().unwrap());
        }
    }
    let scalar_import = start.elapsed();
    let start = Instant::now();
    let prepared = fast.rgba();
    let fast_materialization = start.elapsed();
    let start = Instant::now();
    let expected = pixel_reference(&scalar);
    let scalar_materialization = start.elapsed();
    assert_eq!(prepared, bytes);
    assert_eq!(prepared, expected);
    assert_eq!(fast.tiles, scalar.tiles);
    println!("2048x1536: import rows={fast_import:?}, scalar={scalar_import:?}; materialize rows={fast_materialization:?}, scalar={scalar_materialization:?}");
}

#[test]
fn effect_sources_preserve_sparse_local_pixels_offsets_and_reuse_cache() {
    let mut doc = Document::new(600, 300).unwrap();
    let mut layer = Layer::new("Offset", "paint", 271, 267);
    layer.x = -17;
    layer.y = 12;
    layer.pixels.set(0, 0, [200, 9, 3, 0]);
    layer.pixels.set(256, 256, [140, 71, 38, 200]);
    layer.effects.push(identity_effect());
    let original = pixel_reference(&layer.pixels);
    let original_tiles = layer.pixels.clone();
    let mut fill = Layer::new("Legacy fill", "fill", 9, 7);
    fill.x = 512;
    fill.y = 11;
    fill.color = [21, 45, 89, 103];
    fill.effects.push(identity_effect());
    doc.layers = vec![layer, fill];
    let masks = vec![None; 2];
    let prepared = effects::prepare(&doc, &masks).unwrap();
    assert_eq!(prepared[0].as_ref().unwrap().bytes, original);
    assert_eq!(
        prepared[0].as_ref().unwrap().get(256, 256),
        [140, 71, 38, 200]
    );
    assert_eq!(
        prepared[1].as_ref().unwrap().bytes,
        [21, 45, 89, 103].repeat(9 * 7)
    );
    let revisited = effects::prepare(&doc, &masks).unwrap();
    for i in 0..2 {
        assert!(Arc::ptr_eq(
            prepared[i].as_ref().unwrap(),
            revisited[i].as_ref().unwrap()
        ));
    }
    for (key, tile) in &doc.layers[0].pixels.tiles {
        assert!(Arc::ptr_eq(tile, &original_tiles.tiles[key]));
    }
    assert_eq!(doc.layers[0].pixels.rgba(), original);
}

#[test]
fn prepared_folder_uses_child_effects_and_offsets_before_its_own_effect() {
    let mut doc = Document::new(9, 7).unwrap();
    let mut folder = Layer::new("Folder", "group", 9, 7);
    folder.effects.push(Effect {
        weight: 1.0,
        id: id(),
        kind: "invert".into(),
        enabled: true,
        settings: json!({}),
    });
    let mut child = Layer::new("Offset child", "paint", 4, 3);
    child.parent = Some(folder.id.clone());
    child.x = 2;
    child.y = 1;
    child.opacity = 0.6;
    for y in 0..3 {
        for x in 0..4 {
            child.pixels.set(x, y, [80, 140, 210, 170]);
        }
    }
    child.effects.push(Effect {
        weight: 1.0,
        id: id(),
        kind: "hsl".into(),
        enabled: true,
        settings: json!({"hue":40}),
    });
    let mut background = Layer::new("Background", "fill", 9, 7);
    background.parent = Some(folder.id.clone());
    background.color = [27, 47, 67, 255];
    let mut expected_child = Image {
        width: 4,
        height: 3,
        bytes: pixel_reference(&child.pixels),
    };
    effects::apply(&mut expected_child, "hsl", &json!({"hue":40})).unwrap();
    let mut expected = Image {
        width: 9,
        height: 7,
        bytes: vec![0; 9 * 7 * 4],
    };
    for y in 0..7 {
        for x in 0..9 {
            let pixel = blend(
                background.color,
                expected_child.get(x - child.x, y - child.y),
                child.opacity,
                "normal",
            );
            let offset = ((y * 9 + x) * 4) as usize;
            expected.bytes[offset..offset + 4].copy_from_slice(&pixel);
        }
    }
    effects::apply(&mut expected, "invert", &json!({})).unwrap();
    doc.layers = vec![folder, child, background];
    let prepared = effects::prepare(&doc, &vec![None; 3]).unwrap();
    assert_eq!(prepared[0].as_ref().unwrap().bytes, expected.bytes);
}
