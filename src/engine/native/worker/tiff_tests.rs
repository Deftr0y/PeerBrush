use super::tests::{exercise_edits, Fixture};
use serde_json::json;
use std::io::Cursor;

fn number(value: u64, size: usize, little: bool) -> Vec<u8> {
    if little {
        value.to_le_bytes()[..size].to_vec()
    } else {
        value.to_be_bytes()[8 - size..].to_vec()
    }
}
// Independent TIFF/BigTIFF, including padded edge tiles and separate planes.
pub(crate) fn fixture(depth: u16, planar: bool, tiled: bool, little: bool, big: bool) -> Vec<u8> {
    fixture_geometry(depth, planar, tiled, little, big, (270, 19), 6)
}

pub(crate) fn fixture_geometry(
    depth: u16,
    planar: bool,
    tiled: bool,
    little: bool,
    big: bool,
    (width, height): (u32, u32),
    orientation: u64,
) -> Vec<u8> {
    let (cw, ch) = if tiled { (128, 16) } else { (width, 7) };
    let (columns, lines) = (width.div_ceil(cw), height.div_ceil(ch));
    let header = if big { 16 } else { 8 };
    let mut bytes = vec![0; header];
    bytes[..2].copy_from_slice(if little { b"II" } else { b"MM" });
    bytes[2..4].copy_from_slice(&number(if big { 43 } else { 42 }, 2, little));
    if big {
        bytes[4..6].copy_from_slice(&number(8, 2, little));
    }
    let mut offsets = Vec::new();
    let mut lengths = Vec::new();
    for plane in 0..if planar { 4 } else { 1 } {
        for by in 0..lines {
            for bx in 0..columns {
                let offset = bytes.len();
                offsets.push(offset as u64);
                let rows = if tiled {
                    ch
                } else {
                    (height - by * ch).min(ch)
                };
                for y in 0..rows {
                    for x in 0..cw {
                        for c in if planar { plane..plane + 1 } else { 0..4 } {
                            let (x, y) = (bx * cw + x, by * ch + y);
                            let value = if x >= width || y >= height || (c == 3 && (x + y) % 5 == 0)
                            {
                                0
                            } else {
                                ((x * 137 + y * 271 + c * 13007 + 12347) % 65536) as u16
                            };
                            if depth == 16 {
                                bytes.extend(number(u64::from(value), 2, little));
                            } else {
                                bytes.push(crate::raster::project16(value));
                            }
                        }
                    }
                }
                lengths.push((bytes.len() - offset) as u64);
            }
        }
    }
    let pointer = if big { 8 } else { 4 };
    let mut fields: Vec<(u16, u16, u64, Vec<u8>)> = Vec::new();
    let mut field = |tag, kind, values: &[u64]| {
        let size = match kind {
            3 => 2,
            4 => 4,
            16 => 8,
            _ => unreachable!(),
        };
        fields.push((
            tag,
            kind,
            values.len() as u64,
            values
                .iter()
                .flat_map(|&v| number(v, size, little))
                .collect(),
        ));
    };
    field(256, 4, &[u64::from(width)]);
    field(257, 4, &[u64::from(height)]);
    field(258, 3, &[u64::from(depth); 4]);
    field(259, 3, &[1]);
    field(262, 3, &[2]);
    field(274, 3, &[orientation]);
    field(277, 3, &[4]);
    field(284, 3, &[if planar { 2 } else { 1 }]);
    field(338, 3, &[2]);
    field(339, 3, &[1]);
    if tiled {
        field(322, 4, &[u64::from(cw)]);
        field(323, 4, &[u64::from(ch)]);
    } else {
        field(278, 4, &[u64::from(ch)]);
    }
    field(
        if tiled { 324 } else { 273 },
        if big { 16 } else { 4 },
        &offsets,
    );
    field(
        if tiled { 325 } else { 279 },
        if big { 16 } else { 4 },
        &lengths,
    );
    fields.sort_by_key(|f| f.0);
    let mut entries = Vec::new();
    for (tag, kind, count, payload) in fields.iter() {
        entries.extend(number(u64::from(*tag), 2, little));
        entries.extend(number(u64::from(*kind), 2, little));
        entries.extend(number(*count, if big { 8 } else { 4 }, little));
        if payload.len() <= pointer {
            entries.extend_from_slice(payload);
            entries.resize(entries.len() + pointer - payload.len(), 0);
        } else {
            entries.extend(number(bytes.len() as u64, pointer, little));
            bytes.extend_from_slice(payload);
        }
    }
    let directory = bytes.len() as u64;
    bytes[if big { 8 } else { 4 }..header].copy_from_slice(&number(directory, pointer, little));
    bytes.extend(number(fields.len() as u64, if big { 8 } else { 2 }, little));
    bytes.extend(entries);
    bytes.extend(number(0, pointer, little));
    bytes
}

pub(crate) fn compressed(compression: ::tiff::encoder::Compression) -> Vec<u8> {
    use ::tiff::{
        encoder::{colortype, TiffEncoder},
        tags::Tag,
    };
    let mut bytes = Cursor::new(Vec::new());
    {
        let mut encoder = TiffEncoder::new(&mut bytes)
            .unwrap()
            .with_compression(compression);
        let mut first = encoder.new_image::<colortype::RGBA8>(270, 9).unwrap();
        first.rows_per_strip(3).unwrap();
        first.encoder().write_tag(Tag::Orientation, 1u16).unwrap();
        let rgba: Vec<_> = (0..270 * 9 * 4).map(|i| (i * 137 + 17) as u8).collect();
        first.write_data(&rgba).unwrap();
        let mut second = encoder.new_image::<colortype::RGBA16>(270, 9).unwrap();
        second.rows_per_strip(4).unwrap();
        second.encoder().write_tag(Tag::Orientation, 6u16).unwrap();
        let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        second
            .encoder()
            .write_tag(Tag::IccProfile, profile.as_slice())
            .unwrap();
        let rgba: Vec<_> = (0..270 * 9 * 4).map(|i| (i * 137 + 12347) as u16).collect();
        second.write_data(&rgba).unwrap();
    }
    bytes.into_inner()
}

#[test]
fn classic_big_endian_planar_and_tiled_tiffs_preserve_samples_placement_and_undo() {
    let root = Fixture::new();
    let path = root.path().join("native.TIFF");
    for incoming in [8, 16] {
        for (planar, tiled, little, big) in [
            (false, false, true, false),
            (true, false, false, false),
            (false, true, false, true),
            (true, true, true, true),
        ] {
            std::fs::write(&path, fixture(incoming, planar, tiled, little, big)).unwrap();
            for depth in [8, 16] {
                for command in [
                    json!({"op":"image.import","path":path,"page":0,"parent":"B","x":-3,"y":2}),
                    json!({"op":"image.place","path":path,"page":0,"layer":"outside","new_layer":false,"rect":[-5,-2,9,7]}),
                    json!({"op":"image.paste","path":path,"page":0,"layer":"outside","origin":[-4,7]}),
                ] {
                    exercise_edits(super::canvas_tests::document(depth), &[command], true);
                }
                exercise_edits(
                    super::canvas_tests::document(depth),
                    &[
                        json!({"op":"image.patch","path":path,"layer":"outside","x":-3,"y":0,"rect":[0,0,6,5]}),
                    ],
                    incoming <= depth,
                );
            }
        }
    }
}

#[test]
fn compressed_pages_keep_explicit_selection_icc_orientation_and_native_depth() {
    use ::tiff::encoder::{Compression, DeflateLevel};
    let root = Fixture::new();
    let path = root.path().join("pages.tif");
    for compression in [
        Compression::Uncompressed,
        Compression::Lzw,
        Compression::Deflate(DeflateLevel::Balanced),
        Compression::Packbits,
    ] {
        std::fs::write(&path, compressed(compression)).unwrap();
        for depth in [8, 16] {
            for page in [0, 1] {
                for op in ["image.import", "image.place", "image.paste"] {
                    exercise_edits(
                        super::canvas_tests::document(depth),
                        &[json!({"op":op,"path":path,"page":page,"layer":"outside"})],
                        true,
                    );
                }
            }
            for fields in [
                json!({}),
                json!({"page":2}),
                json!({"page":null}),
                json!({"page":-1}),
                json!({"page":0,"frame":0}),
                json!({"page":0,"variant":0}),
            ] {
                let mut invalid = json!({"op":"image.import","path":path});
                invalid
                    .as_object_mut()
                    .unwrap()
                    .extend(fields.as_object().unwrap().clone());
                exercise_edits(
                    super::canvas_tests::document(depth),
                    &[json!({"op":"image.import","path":path,"page":0}), invalid],
                    false,
                );
            }
            exercise_edits(
                super::canvas_tests::document(depth),
                &[json!({"op":"image.patch","path":path,"layer":"outside","x":1,"y":2})],
                true,
            );
        }
    }
}

#[test]
fn truncated_tiff_batches_and_late_leases_do_not_adopt_output() {
    super::qoi_tests::failed_streams("tif", fixture(8, false, false, true, false));
}

pub(crate) fn bilevel(white_is_zero: bool) -> Vec<u8> {
    use ::tiff::{encoder::TiffEncoder, tags::Tag};
    let mut bytes = Cursor::new(Vec::new());
    {
        let mut encoder = TiffEncoder::new(&mut bytes).unwrap();
        let mut directory = encoder.image_directory().unwrap();
        let pixels = [0xa6, 0x80, 0x59, 0x00, 0xe3, 0x80];
        let offset = directory.write_data(pixels.as_slice()).unwrap();
        for (tag, value) in [
            (Tag::ImageWidth, 9),
            (Tag::ImageLength, 3),
            (Tag::RowsPerStrip, 3),
            (Tag::StripOffsets, offset as u32),
            (Tag::StripByteCounts, 6),
        ] {
            directory.write_tag(tag, value).unwrap();
        }
        for (tag, value) in [
            (Tag::BitsPerSample, 1u16),
            (Tag::SamplesPerPixel, 1),
            (Tag::Compression, 1),
            (
                Tag::PhotometricInterpretation,
                if white_is_zero { 0 } else { 1 },
            ),
        ] {
            directory.write_tag(tag, value).unwrap();
        }
        directory.finish().unwrap();
    }
    bytes.into_inner()
}

#[test]
fn grayscale_bilevel_and_raw_cmyk_patches_match_existing_channel_semantics() {
    use ::tiff::encoder::{colortype, TiffEncoder};
    let root = Fixture::new();
    let path = root.path().join("samples.tif");
    let mut cases = vec![(8, false, bilevel(false)), (8, false, bilevel(true))];
    macro_rules! encoded {
        ($kind:ty,$depth:expr,$cmyk:expr,$pixels:expr) => {{
            let mut bytes = Cursor::new(Vec::new());
            TiffEncoder::new(&mut bytes)
                .unwrap()
                .write_image::<$kind>(2, 1, $pixels)
                .unwrap();
            cases.push(($depth, $cmyk, bytes.into_inner()));
        }};
    }
    encoded!(colortype::Gray8, 8, false, &[17, 193]);
    encoded!(colortype::Gray16, 16, false, &[12347, 49153]);
    encoded!(colortype::RGB8, 8, false, &[17, 29, 41, 53, 67, 89]);
    encoded!(
        colortype::RGB16,
        16,
        false,
        &[12347, 23459, 34571, 54321, 32109, 10987]
    );
    encoded!(
        colortype::CMYK8,
        8,
        true,
        &[17, 29, 41, 53, 67, 89, 103, 127]
    );
    encoded!(
        colortype::CMYK16,
        16,
        true,
        &[12347, 23459, 34571, 49153, 54321, 32109, 10987, 17777]
    );
    for (incoming, cmyk, bytes) in cases {
        std::fs::write(&path, bytes).unwrap();
        for depth in [8, 16] {
            for op in ["image.import", "image.place", "image.paste"] {
                exercise_edits(
                    super::canvas_tests::document(depth),
                    &[json!({"op":op,"path":path,"layer":"outside"})],
                    !cmyk,
                );
            }
            exercise_edits(
                super::canvas_tests::document(depth),
                &[json!({"op":"image.patch","path":path,"layer":"outside","x":1,"y":2})],
                incoming <= depth,
            );
        }
    }
}
