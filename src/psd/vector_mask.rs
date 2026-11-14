//! Bounded Photoshop path-record inspection for standard vector masks.

const RECORD_BYTES: usize = 26;
const MAX_RECORDS: usize = 4096;
pub(super) const MAX_BYTES: usize = 8 + RECORD_BYTES * MAX_RECORDS;
const INVERTED: u32 = 1;
const UNLINKED: u32 = 1 << 1;
const DISABLED: u32 = 1 << 2;

pub(super) struct Mask {
    pub flags: u32,
    pub initially_full: Option<bool>,
    pub closed: usize,
    pub open: usize,
    pub knots: usize,
}

impl Mask {
    pub fn is_noop(&self) -> bool {
        self.flags & !(INVERTED | UNLINKED | DISABLED) == 0
            && (self.flags & DISABLED != 0
                || self.initially_full.is_some_and(|full| full != (self.flags & INVERTED != 0)))
            && self.closed == 0
            && self.open == 0
    }

    pub(super) fn protection_reason(&self, key: &[u8]) -> String {
        format!(
            "Photoshop vector mask {} remains protected ({} closed paths, {} open paths, {} knots, initial fill {}, flags {:#x})",
            String::from_utf8_lossy(key), self.closed, self.open, self.knots,
            match self.initially_full { Some(true) => "full", Some(false) => "empty", None => "unspecified" },
            self.flags,
        )
    }
}

pub(super) fn inspect(bytes: &[u8]) -> Result<Mask, &'static str> {
    let Some((header, records)) = bytes.split_at_checked(8) else {
        return Err("missing version and flags");
    };
    let (records, tail) = records.as_chunks::<RECORD_BYTES>();
    if header[..4] != 3u32.to_be_bytes() || !tail.is_empty() || records.len() > MAX_RECORDS {
        return Err("unsupported version, path record alignment or record count");
    }
    let selector = |record: &[u8; RECORD_BYTES]| u16::from_be_bytes([record[0], record[1]]);
    let Some((rule, mut path)) = records.split_first() else {
        return Err("missing path fill rule");
    };
    if selector(rule) != 6 || rule[2..].iter().any(|&value| value != 0) {
        return Err("unsupported path fill rule");
    }
    let mut mask = Mask {
        flags: u32::from_be_bytes(header[4..].try_into().unwrap()),
        initially_full: None,
        closed: 0,
        open: 0,
        knots: 0,
    };
    if let Some((initial, remaining)) = path.split_first().filter(|(r, _)| selector(r) == 8) {
        if initial[2] != 0 || initial[3] > 1 || initial[4..].iter().any(|&value| value != 0) {
            return Err("unsupported initial path fill");
        }
        mask.initially_full = Some(initial[3] == 1);
        path = remaining;
    }
    while let Some((length, remaining)) = path.split_first() {
        let knot_selectors = match selector(length) {
            0 => {
                mask.closed += 1;
                [1, 2]
            }
            3 => {
                mask.open += 1;
                [4, 5]
            }
            _ => return Err("unexpected path record selector"),
        };
        if length[4..].iter().any(|&value| value != 0) {
            return Err("subpath has additional operation data");
        }
        let count = usize::from(u16::from_be_bytes([length[2], length[3]]));
        let Some((knots, rest)) = remaining.split_at_checked(count).filter(|_| count != 0) else {
            return Err("empty or truncated subpath");
        };
        for knot in knots {
            if !knot_selectors.contains(&selector(knot)) {
                return Err("unexpected Bezier knot selector");
            }
            // Validate all six signed 8.24 components without projecting them to pixels.
            if knot[2..].chunks_exact(4).any(|value| {
                !(-0x1000_0000..0x1000_0000)
                    .contains(&i32::from_be_bytes(value.try_into().unwrap()))
            }) {
                return Err("Bezier coordinate exceeds the Photoshop path range");
            }
        }
        mask.knots += count;
        path = rest;
    }
    Ok(mask)
}

#[cfg(test)]
pub(in crate::psd) fn full_mask_fixture() -> Vec<u8> {
    let mut data = vec![0; 8 + 2 * RECORD_BYTES];
    data[3] = 3;
    data[9] = 6;
    data[8 + RECORD_BYTES + 1] = 8;
    data[8 + RECORD_BYTES + 3] = 1;
    data
}

#[cfg(test)]
pub(in crate::psd) fn inverted_empty_mask_fixture() -> Vec<u8> {
    let mut data = full_mask_fixture();
    data[7] = (INVERTED | UNLINKED) as u8;
    data[8 + RECORD_BYTES + 3] = 0;
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_mask_inspection_checks_subpath_lengths_selectors_and_native_coordinates() {
        let mut path = full_mask_fixture();
        assert!(inspect(&path).unwrap().is_noop());
        let first = path.len();
        path.resize(first + 3 * RECORD_BYTES, 0);
        path[first + 3] = 2;
        path[first + RECORD_BYTES + 1] = 1;
        path[first + 2 * RECORD_BYTES + 1] = 2;
        let coordinate = first + RECORD_BYTES + 2;
        path[coordinate..coordinate + 4].copy_from_slice(&(-0x1000_0000i32).to_be_bytes());
        let active = inspect(&path).unwrap();
        assert_eq!((active.closed, active.open, active.knots), (1, 0, 2));
        assert!(!active.is_noop());
        path[7] = (INVERTED | DISABLED) as u8;
        assert!(!inspect(&path).unwrap().is_noop());
        assert!(inspect(&path[..path.len() - RECORD_BYTES]).is_err());
        path[first + 2 * RECORD_BYTES + 1] = 5;
        assert!(inspect(&path).is_err());
        path[first + 2 * RECORD_BYTES + 1] = 2;
        path[coordinate..coordinate + 4].copy_from_slice(&0x1000_0000i32.to_be_bytes());
        assert!(inspect(&path).is_err());
        path[coordinate..coordinate + 4].fill(0);
        path[first + 4] = 1;
        assert!(inspect(&path).is_err());
    }

    #[test]
    fn pathless_vector_flags_preserve_only_no_effect_combinations() {
        for (flags, full, accepted) in [
            (0, true, true),
            (UNLINKED, true, true),
            (UNLINKED, false, false),
            (DISABLED, false, true),
            (UNLINKED | DISABLED, true, true),
            (INVERTED, true, false),
            (INVERTED, false, true),
            (INVERTED | DISABLED, true, true),
            (DISABLED | 8, false, false),
        ] {
            let mut payload = full_mask_fixture();
            payload[4..8].copy_from_slice(&flags.to_be_bytes());
            payload[8 + RECORD_BYTES + 3] = u8::from(full);
            assert_eq!(inspect(&payload).unwrap().is_noop(), accepted);
        }
        for (flags, accepted) in [
            (0, false),
            (DISABLED, true),
            (INVERTED | UNLINKED | DISABLED, true),
            (DISABLED | 8, false),
        ] {
            let mut payload = full_mask_fixture();
            payload.truncate(8 + RECORD_BYTES);
            payload[4..8].copy_from_slice(&flags.to_be_bytes());
            assert_eq!(inspect(&payload).unwrap().is_noop(), accepted);
        }
    }

    #[test]
    fn disabled_pathless_vector_mask_without_initial_fill_round_trips() {
        use crate::psd::*;
        let mut payload = full_mask_fixture();
        payload.truncate(8 + RECORD_BYTES);
        payload[4..8].copy_from_slice(&DISABLED.to_be_bytes());
        let mut doc = Document::new_depth(2, 1, 16).unwrap();
        doc.layers[0].pixels.set16(0, 0, [12347, 25127, 42013, 65535]);
        doc.layers[0].psd_metadata.push(LayerMetadata {
            key: "vmsk".into(),
            data: payload,
        });
        for layout in [Layout::Psd, Layout::Psb] {
            let bytes = encode_layout(&doc, layout).unwrap();
            let mut reader = Cursor::new(&bytes);
            reader.bytes(26).unwrap();
            reader.block().unwrap();
            let resources = reader.pos;
            reader.block().unwrap();
            let mut standard = bytes[..resources].to_vec();
            u32b(&mut standard, 0);
            standard.extend_from_slice(&bytes[reader.pos..]);
            for reopened in [
                decode(&bytes).unwrap(),
                decode_reader(&mut std::io::Cursor::new(&standard), &Default::default()).unwrap(),
            ] {
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].psd_metadata, doc.layers[0].psd_metadata);
                assert_eq!(reopened.layers[0].pixels.get16(0, 0), doc.layers[0].pixels.get16(0, 0));
            }
            let old = adjustments::tests::replace_sources(&bytes, layout, &doc, 139);
            assert!(decode(&old).unwrap().read_only);
        }
    }

    #[test]
    fn inverted_empty_vector_mask_and_global_application_round_trip_without_baking() {
        use crate::psd::*;
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0]
                .pixels
                .set16(0, 0, [12347, 25127, 42013, 65535]);
            doc.layers[0].psd_metadata = vec![
                LayerMetadata {
                    key: "tsly".into(),
                    data: vec![1, 0, 0, 0],
                },
                LayerMetadata {
                    key: "vmgm".into(),
                    data: vec![1, 0, 0, 0],
                },
                LayerMetadata {
                    key: "vmsk".into(),
                    data: inverted_empty_mask_fixture(),
                },
            ];
            assert!(validate_metadata(&doc.layers[0].psd_metadata[..2]).is_err());
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let index = structural::index(
                    &mut std::io::Cursor::new(&bytes),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                for entry in &doc.layers[0].psd_metadata {
                    let tag = index.layers[0]
                        .tags
                        .iter()
                        .find(|tag| tag.key.as_slice() == entry.key.as_bytes())
                        .unwrap();
                    let start = tag.data.offset as usize;
                    assert_eq!(&bytes[start..start + tag.data.bytes as usize], &entry.data);
                }
                let old = adjustments::tests::replace_sources(&bytes, layout, &doc, 50);
                assert!(decode(&old).unwrap().read_only);

                let mut cursor = Cursor::new(&bytes);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let resource_length = cursor.pos;
                cursor.block().unwrap();
                let removed = cursor.pos - resource_length - 4;
                let mut conventional = bytes[..resource_length].to_vec();
                u32b(&mut conventional, 0);
                conventional.extend_from_slice(&bytes[cursor.pos..]);
                for reopened in [
                    decode(&bytes).unwrap(),
                    decode(&conventional).unwrap(),
                    decode_reader(&mut std::io::Cursor::new(&conventional), &Default::default())
                        .unwrap(),
                ] {
                    assert!(!reopened.read_only, "{:?}", reopened.warnings);
                    assert_eq!(reopened.layers[0].psd_metadata, doc.layers[0].psd_metadata);
                    assert_eq!(
                        reopened.layers[0].pixels.get16(0, 0),
                        doc.layers[0].pixels.get16(0, 0)
                    );
                }
                for (key, value) in [(b"tsly", 0), (b"vmgm", 2)] {
                    let tag = index.layers[0]
                        .tags
                        .iter()
                        .find(|tag| &tag.key == key)
                        .unwrap();
                    let at = tag.data.offset as usize;
                    let mut changed = conventional.clone();
                    changed[at - removed] = value;
                    assert!(decode(&changed).unwrap().read_only);
                    let mut changed = bytes.clone();
                    changed[at] = value;
                    let rebound = adjustments::tests::replace_sources(&changed, layout, &doc, 51);
                    assert!(decode(&rebound).unwrap().read_only);
                }
                let tag = index.layers[0]
                    .tags
                    .iter()
                    .find(|tag| tag.key == *b"vmsk")
                    .unwrap();
                let at = tag.data.offset as usize - removed;
                let mut changed = conventional.clone();
                changed[at + 8 + RECORD_BYTES + 3] = 1;
                assert!(decode(&changed).unwrap().read_only);
            }
        }
    }

    #[test]
    fn only_the_inert_vector_mask_is_preserved_in_standard_and_supplemental_data() {
        use crate::psd::*;
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0]
                .pixels
                .set16(0, 0, [12347, 25127, 42013, 65535]);
            let metadata = LayerMetadata {
                key: "vmsk".into(),
                data: full_mask_fixture(),
            };
            doc.layers[0].psd_metadata.push(metadata.clone());
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let index = structural::index(
                    &mut std::io::Cursor::new(&bytes),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                let tag = index.layers[0]
                    .tags
                    .iter()
                    .find(|tag| tag.key == *b"vmsk")
                    .unwrap();
                let start = tag.data.offset as usize;
                assert_eq!(
                    &bytes[start..start + tag.data.bytes as usize],
                    metadata.data
                );
                assert!(!decode(&bytes).unwrap().read_only);
                let mut file = Cursor::new(&bytes);
                file.bytes(26).unwrap();
                file.block().unwrap();
                let resource_length = file.pos;
                file.block().unwrap();
                let mut standard = bytes[..resource_length].to_vec();
                u32b(&mut standard, 0);
                standard.extend_from_slice(&bytes[file.pos..]);
                let outside =
                    decode_reader(&mut std::io::Cursor::new(&standard), &Default::default())
                        .unwrap();
                assert!(!outside.read_only, "{:?}", outside.warnings);
                assert_eq!(outside.layers[0].psd_metadata, [metadata.clone()]);
                assert_eq!(
                    outside.layers[0].pixels.get16(0, 0),
                    doc.layers[0].pixels.get16(0, 0)
                );

                let mut changed = bytes.clone();
                changed[start + 8 + RECORD_BYTES + 3] = 0;
                let changed = adjustments::tests::replace_sources(&changed, layout, &doc, 42);
                assert!(decode(&changed).unwrap().read_only);
                let old = adjustments::tests::replace_sources(&bytes, layout, &doc, 41);
                assert!(decode(&old).unwrap().read_only);
                let start = start - (file.pos - resource_length - 4);
                standard[start + 8 + RECORD_BYTES + 3] = 0;
                let protected = decode(&standard).unwrap();
                assert!(protected.read_only);
                assert!(protected.source_structure[0].has_mask);
                assert!(protected
                    .warnings
                    .iter()
                    .any(|reason| reason.contains("vector mask")));
            }
        }
    }
}
