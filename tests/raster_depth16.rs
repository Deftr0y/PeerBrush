use peerbrush::raster::{self, Raster, TILE};
use std::sync::Arc;

#[test]
fn sixteen_bit_sparse_storage_roundtrips_lower_bits_hidden_rgb_and_cow() {
    let mut r = Raster::new_depth(513, 259, 16);
    r.set16(512, 258, [12345, 23456, 34567, 1]);
    r.set16(256, 1, [91, 123, 257, 0]);
    assert!(r.tiles.is_empty());
    assert_eq!(r.samples16.len(), 2);
    assert_eq!(r.get16(512, 258), [12345, 23456, 34567, 1]);
    let before = r.clone();
    let words = r.rgba16();
    let imported = Raster::from_rgba16(513, 259, &words).unwrap();
    assert_eq!(imported.rgba16(), words);
    assert_eq!(imported.get16(256, 1), [91, 123, 257, 0]);
    for (key, tile) in &r.samples16 {
        assert!(Arc::ptr_eq(tile, &before.samples16[key]));
    }
    r.set16(512, 258, [12346, 23457, 34568, 2]);
    assert_eq!(before.get16(512, 258), [12345, 23456, 34567, 1]);
    assert_eq!(r.get16(512, 258), [12346, 23457, 34568, 2]);
    assert_eq!(r.content_bounds(), Some([512, 258, 513, 259]));
    assert_eq!(r.bytes(), 2 * (TILE * TILE * 8) as usize);
    let json = serde_json::to_vec(&r).unwrap();
    let loaded: Raster = serde_json::from_slice(&json).unwrap();
    assert_eq!(loaded.rgba16(), r.rgba16());
    assert!(loaded.tiles.is_empty());
    loaded.validate_layout().unwrap();
}

#[test]
fn sixteen_bit_import_clips_padding_and_projection_never_becomes_authoritative() {
    let mut r = Raster::from_rgba16(1, 1, &[12345, 23456, 34567, 45678]).unwrap();
    let original = r.rgba16();
    let projected = r.rgba();
    assert_eq!(projected, [48, 91, 135, 178]);
    assert_eq!(r.rgba16(), original);
    Arc::make_mut(r.samples16.get_mut(&(0, 0)).unwrap())[4..8].copy_from_slice(&[65535; 4]);
    r.samples16.insert(
        (u32::MAX, u32::MAX),
        Arc::new(vec![65535; (TILE * TILE * 4) as usize]),
    );
    assert_eq!(r.rgba16(), original);
    assert_eq!(r.rgba(), projected);
    assert_eq!(r.tile_bounds(), Some([0, 0, 1, 1]));
    assert!(r.validate_layout().is_err());
    let mut eight = Raster::from_rgba(1, 1, &[7, 11, 19, 29]).unwrap();
    eight.promote16();
    assert_eq!(eight.get16(0, 0), [7 * 257, 11 * 257, 19 * 257, 29 * 257]);
    assert!(eight.tiles.is_empty());
    assert_eq!(eight.rgba(), [7, 11, 19, 29]);
    eight.set(0, 0, [43, 47, 53, 59]);
    assert_eq!(eight.get16(0, 0), [43 * 257, 47 * 257, 53 * 257, 59 * 257]);
}

#[test]
fn high_depth_sampling_and_blending_keep_sub_eight_bit_changes() {
    let r = Raster::from_rgba16(
        2,
        1,
        &[10001, 20001, 30001, 65535, 10003, 20003, 30003, 65535],
    )
    .unwrap();
    assert_eq!(r.sample16(0.5, 0.), [10002, 20002, 30002, 65535]);
    let mixed = raster::blend16(
        [10001, 20001, 30001, 65535],
        [10003, 20003, 30003, 65535],
        0.5,
        "normal",
    );
    assert_eq!(mixed, [10002, 20002, 30002, 65535]);
    let faint = raster::blend16([0; 4], [12345, 23456, 34567, 1], 1., "normal");
    assert_eq!(faint, [12345, 23456, 34567, 1]);
    let encoded = raster::png16(2, 1, &r.rgba16()).unwrap();
    let decoded = image::load_from_memory(&encoded).unwrap().to_rgba16();
    assert_eq!(decoded.as_raw(), &r.rgba16());
}

#[test]
fn legacy_dot_paint_and_erase_preserve_untouched_native_channels() {
    let mut r = Raster::from_rgba16(1, 1, &[12345, 23456, 34567, 45678]).unwrap();
    r.paint_dot(0.5, 0.5, 2., [0, 0, 0, 1], false, true, None);
    let erased = r.get16(0, 0);
    assert_eq!(&erased[..3], &[12345, 23456, 34567]);
    assert_eq!(erased[3], (45678f64 * (1. - 1. / 255.)).round() as u16);
    assert_ne!(erased[3] % 257, 0);
    r.paint_dot16(
        0.5,
        0.5,
        2.,
        [10001, 20001, 30001, 65535],
        false,
        false,
        None,
    );
    assert_eq!(r.get16(0, 0), [10001, 20001, 30001, 65535]);
}
