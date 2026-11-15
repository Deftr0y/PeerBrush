//! Retained standard document metadata. Only understood inactive/advisory
//! definitions qualify; recognition alone never grants editable import.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalMetadata {
    pub key: String,
    pub data: Vec<u8>,
}
pub(crate) const KEYS: &[&str] = &["Patt", "FMsk", "CAI ", "OCIO", "GenI", "cinf"];

pub(super) fn encoded_bytes(entries: &[GlobalMetadata], layout: super::Layout) -> u64 {
    entries
        .iter()
        .map(|m| {
            8 + if layout.tag_large(m.key.as_bytes()) {
                8
            } else {
                4
            } + (m.data.len() as u64).div_ceil(4) * 4
        })
        .sum()
}
pub(super) fn write(
    output: &mut dyn std::io::Write,
    entry: &GlobalMetadata,
    layout: super::Layout,
) -> Result<(), String> {
    output.write_all(b"8BIM").map_err(|e| e.to_string())?;
    output
        .write_all(entry.key.as_bytes())
        .map_err(|e| e.to_string())?;
    layout.write_length_for_tag(output, entry.key.as_bytes(), entry.data.len() as u64)?;
    output.write_all(&entry.data).map_err(|e| e.to_string())?;
    output
        .write_all(&[0; 3][..(4 - entry.data.len() % 4) % 4])
        .map_err(|e| e.to_string())
}

fn object<'a>(
    value: &'a Value,
    class: &str,
    keys: &[&str],
) -> Option<&'a serde_json::Map<String, Value>> {
    let items = value["items"].as_object()?;
    (value["class"] == class
        && items.len() == keys.len()
        && keys.iter().all(|k| items.contains_key(*k)))
    .then_some(items)
}
fn text(value: &Value) -> Option<&str> {
    if value["type"] != "TEXT" {
        return None;
    }
    let text = value["value"].as_str()?;
    Some(text.strip_suffix('\0').unwrap_or(text))
}
fn enumeration(value: &Value, class: &str, name: &str) -> bool {
    value["type"] == "enum" && value["value"] == serde_json::json!([class, name])
}
fn version(value: &Value) -> bool {
    value["type"] == "Objc"
        && object(&value["value"], "null", &["major", "minor", "fix"]).is_some_and(|items| {
            items
                .values()
                .all(|n| n["type"] == "long" && n["value"].as_u64().is_some_and(|v| v <= 65535))
        })
}
pub(crate) fn validate_entry(key: &str, data: &[u8]) -> Result<(), String> {
    if !KEYS.contains(&key) || data.len() > 4096 {
        return Err("unsupported document metadata key or size".into());
    }
    if key == "Patt" {
        return if data.is_empty() {
            Ok(())
        } else {
            Err("pattern assets require supported native styles".into())
        };
    }
    if key == "FMsk" {
        return if data.len() == 12
            && data[..2] == [0; 2]
            && data[8..10] == [0; 2]
            && u16::from_be_bytes([data[10], data[11]]) <= 100
        {
            Ok(())
        } else {
            Err("unsupported filter-mask display settings".into())
        };
    }
    let root = if key == "CAI " {
        if data.len() < 4 || data[..4] != 3u32.to_be_bytes() {
            return Err("unsupported content-credentials version".into());
        }
        // Photoshop can append eight zero bytes to a version 3 descriptor.
        super::styles::descriptor_block_with_zero_trailer(&data[4..], 8)?
    } else {
        super::styles::descriptor_block(data)?
    };
    let supported = match key {
        "CAI " => object(&root, "null", &["enab", "generationalGuid"]).is_some_and(|items| {
            items["enab"]["type"] == "bool"
                && items["enab"]["value"] == false
                && text(&items["generationalGuid"]) == Some("")
        }),
        "OCIO" => object(
            &root,
            "documentColorManagementInfo",
            &["Knd ", "ocio_display_view"],
        )
        .is_some_and(|items| {
            text(&items["Knd "]) == Some("none")
                && items["ocio_display_view"]["type"] == "Objc"
                && object(
                    &items["ocio_display_view"]["value"],
                    "viewColorManagementInfo",
                    &["display", "view"],
                )
                .is_some_and(|view| view.values().all(|v| text(v) == Some("")))
        }),
        "GenI" => object(
            &root,
            "genTechInfo",
            &["isUsingGenTech", "externalModelList"],
        )
        .is_some_and(|items| {
            items["isUsingGenTech"]["type"] == "long"
                && items["isUsingGenTech"]["value"] == 0
                && items["externalModelList"]["type"] == "VlLs"
                && items["externalModelList"]["value"] == serde_json::json!([])
        }),
        "cinf" => object(
            &root,
            "null",
            &[
                "Vrsn",
                "psVersion",
                "description",
                "reason",
                "Engn",
                "enableCompCoreGPU",
                "enableCompCoreThreads",
                "compCoreGPUSupport",
            ],
        )
        .is_some_and(|items| {
            version(&items["Vrsn"])
                && version(&items["psVersion"])
                && text(&items["description"]) == Some("")
                && text(&items["reason"]) == Some("")
                && enumeration(&items["Engn"], "Engn", "compCore")
                && enumeration(&items["enableCompCoreGPU"], "enable", "feature")
                && enumeration(&items["enableCompCoreThreads"], "enable", "feature")
                && enumeration(&items["compCoreGPUSupport"], "reason", "featureDisabled")
        }),
        _ => false,
    };
    if supported {
        Ok(())
    } else {
        Err("active or unfamiliar document metadata settings".into())
    }
}
pub(crate) fn validate(entries: &[GlobalMetadata]) -> Result<(), String> {
    if entries.len() > KEYS.len() {
        return Err("Too many Photoshop document metadata records".into());
    }
    for (i, entry) in entries.iter().enumerate() {
        if entries[..i].iter().any(|old| old.key == entry.key) {
            return Err("Duplicate Photoshop document metadata".into());
        }
        validate_entry(&entry.key, &entry.data)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    fn id(out: &mut Vec<u8>, value: &str) {
        out.extend(
            (if value.len() == 4 {
                0
            } else {
                value.len() as u32
            })
            .to_be_bytes(),
        );
        out.extend(value.as_bytes());
    }
    fn text_bytes(value: &str) -> Vec<u8> {
        let words: Vec<_> = value.encode_utf16().collect();
        let mut out = (words.len() as u32).to_be_bytes().to_vec();
        for word in words {
            out.extend(word.to_be_bytes());
        }
        out
    }
    fn descriptor(class: &str, items: Vec<(&str, &[u8; 4], Vec<u8>)>) -> Vec<u8> {
        let mut out = text_bytes("");
        id(&mut out, class);
        out.extend((items.len() as u32).to_be_bytes());
        for (key, kind, value) in items {
            id(&mut out, key);
            out.extend(kind);
            out.extend(value);
        }
        out
    }
    fn enumeration(class: &str, name: &str) -> Vec<u8> {
        let mut out = vec![];
        id(&mut out, class);
        id(&mut out, name);
        out
    }
    fn block(key: &str, body: Vec<u8>) -> GlobalMetadata {
        GlobalMetadata {
            key: key.into(),
            data: [16u32.to_be_bytes().to_vec(), body].concat(),
        }
    }
    pub(crate) fn fixtures() -> Vec<GlobalMetadata> {
        let mut cai = block(
            "CAI ",
            descriptor(
                "null",
                vec![
                    ("enab", b"bool", vec![0]),
                    ("generationalGuid", b"TEXT", text_bytes("\0")),
                ],
            ),
        );
        cai.data.splice(..0, 3u32.to_be_bytes());
        let version = || {
            descriptor(
                "null",
                vec![
                    ("major", b"long", 1u32.to_be_bytes().to_vec()),
                    ("minor", b"long", 0u32.to_be_bytes().to_vec()),
                    ("fix", b"long", 0u32.to_be_bytes().to_vec()),
                ],
            )
        };
        vec![
            GlobalMetadata {
                key: "Patt".into(),
                data: vec![],
            },
            GlobalMetadata {
                key: "FMsk".into(),
                data: vec![0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 50],
            },
            cai,
            block(
                "OCIO",
                descriptor(
                    "documentColorManagementInfo",
                    vec![
                        ("Knd ", b"TEXT", text_bytes("none\0")),
                        (
                            "ocio_display_view",
                            b"Objc",
                            descriptor(
                                "viewColorManagementInfo",
                                vec![
                                    ("display", b"TEXT", text_bytes("\0")),
                                    ("view", b"TEXT", text_bytes("\0")),
                                ],
                            ),
                        ),
                    ],
                ),
            ),
            block(
                "GenI",
                descriptor(
                    "genTechInfo",
                    vec![
                        ("isUsingGenTech", b"long", 0u32.to_be_bytes().to_vec()),
                        ("externalModelList", b"VlLs", 0u32.to_be_bytes().to_vec()),
                    ],
                ),
            ),
            block(
                "cinf",
                descriptor(
                    "null",
                    vec![
                        ("Vrsn", b"Objc", version()),
                        ("psVersion", b"Objc", version()),
                        ("description", b"TEXT", text_bytes("\0")),
                        ("reason", b"TEXT", text_bytes("\0")),
                        ("Engn", b"enum", enumeration("Engn", "compCore")),
                        (
                            "enableCompCoreGPU",
                            b"enum",
                            enumeration("enable", "feature"),
                        ),
                        (
                            "enableCompCoreThreads",
                            b"enum",
                            enumeration("enable", "feature"),
                        ),
                        (
                            "compCoreGPUSupport",
                            b"enum",
                            enumeration("reason", "featureDisabled"),
                        ),
                    ],
                ),
            ),
        ]
    }

    pub(crate) fn fixtures_with_credentials_trailer() -> Vec<GlobalMetadata> {
        let mut metadata = fixtures();
        metadata[2].data.extend([0; 8]);
        metadata
    }

    #[test]
    fn disabled_credentials_accept_exactly_eight_additional_zero_bytes() {
        let canonical = &fixtures()[2].data;
        let padded = &fixtures_with_credentials_trailer()[2].data;
        assert!(validate_entry("CAI ", padded).is_ok());
        assert!(super::super::styles::descriptor_block(&padded[4..]).is_err());
        for length in 4..=12 {
            let mut candidate = canonical.clone();
            candidate.extend(std::iter::repeat_n(0, length));
            assert_eq!(validate_entry("CAI ", &candidate).is_ok(), length == 8);
        }
        for at in padded.len() - 8..padded.len() {
            let mut changed = padded.clone();
            changed[at] = 1;
            assert!(validate_entry("CAI ", &changed).is_err());
        }
        let mut active = padded.clone();
        let enabled = active.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
        active[enabled] = 1;
        assert!(validate_entry("CAI ", &active).is_err());
    }

    #[test]
    fn inactive_metadata_is_bounded_and_unknown_or_active_variants_are_refused() {
        let fixtures = fixtures();
        validate(&fixtures).unwrap();
        let mut duplicate = fixtures.clone();
        duplicate.push(fixtures[0].clone());
        assert!(validate(&duplicate).is_err());
        assert!(validate_entry("Patt", &[1]).is_err());
        assert!(validate_entry("fake", &[]).is_err());
        assert!(validate_entry("OCIO", &vec![0; 4097]).is_err());
        for entry in fixtures.iter().filter(|v| !v.data.is_empty()) {
            for end in 0..entry.data.len() {
                assert!(
                    validate_entry(&entry.key, &entry.data[..end]).is_err(),
                    "{}:{end}",
                    entry.key
                );
            }
            let mut changed = entry.data.clone();
            changed.extend([1, 2, 3, 4]);
            assert!(validate_entry(&entry.key, &changed).is_err());
        }
        let mut cai = fixtures[2].clone();
        let at = cai.data.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
        cai.data[at] = 1;
        assert!(validate_entry(&cai.key, &cai.data).is_err());
        let mut ocio = fixtures[3].clone();
        let at = ocio
            .data
            .windows(8)
            .position(|v| v == [0, b'n', 0, b'o', 0, b'n', 0, b'e'])
            .unwrap();
        ocio.data[at + 1] = b'd';
        assert!(validate_entry(&ocio.key, &ocio.data).is_err());
        let mut generator = fixtures[4].clone();
        let at = generator
            .data
            .windows(4)
            .position(|v| v == b"long")
            .unwrap()
            + 4;
        generator.data[at + 3] = 1;
        assert!(validate_entry(&generator.key, &generator.data).is_err());
    }

    #[test]
    fn document_metadata_survives_native_psd_psb_standard_channels_edit_and_undo() {
        use crate::{
            engine::{Document, Engine},
            psd::{self, Cursor, Layout},
        };
        for depth in [8, 16] {
            let metadata = fixtures_with_credentials_trailer();
            let mut engine = Engine::new();
            engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(2, 1, depth).unwrap();
            engine.resident_state_mut().expect("resident fixture").document.layers[0]
                .pixels
                .set16(0, 0, [12347, 23459, 34571, 65535]);
            engine.resident_state_mut().expect("resident fixture").document.layers[0]
                .pixels
                .set16(1, 0, [57001, 39003, 13007, 65535]);
            engine.resident_state_mut().expect("resident fixture").document.psd_global_metadata = metadata.clone();
            let before = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
            let id = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
            engine
                .edit(
                    "human",
                    &[serde_json::json!({"op":"layer.update","layer":id,"name":"Edited"})],
                    None,
                    None,
                    "Rename",
                )
                .unwrap();
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.psd_global_metadata, metadata);
            for layout in [Layout::Psd, Layout::Psb] {
                let saved = psd::encode_layout(&engine.resident_state().expect("resident fixture").document, layout).unwrap();
                let mut file = Cursor::new(&saved);
                file.bytes(26).unwrap();
                file.block().unwrap();
                let resource_start = file.pos;
                file.block().unwrap();
                let mut ordinary = saved[..resource_start].to_vec();
                ordinary.extend([0; 4]);
                ordinary.extend(&saved[file.pos..]);
                for bytes in [&saved, &ordinary] {
                    for loaded in [
                        psd::decode(bytes).unwrap(),
                        psd::decode_reader(&mut std::io::Cursor::new(bytes), &Default::default())
                            .unwrap(),
                    ] {
                        assert!(!loaded.read_only, "{:?}", loaded.warnings);
                        assert_eq!(loaded.psd_global_metadata, metadata);
                        assert_eq!(
                            loaded.layers[0].pixels.rgba16(),
                            engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16()
                        );
                        assert_eq!(
                            loaded.export_png().unwrap(),
                            engine.resident_state().expect("resident fixture").document.export_png().unwrap()
                        );
                    }
                }
                // Private definitions must not override standard metadata, even
                // if an external tool changes only a valid display setting.
                let mut changed = saved.clone();
                let at = changed.windows(8).position(|w| w == b"8BIMFMsk").unwrap();
                let payload = at + if layout == Layout::Psb { 16 } else { 12 };
                changed[payload + 11] = 49;
                let changed =
                    psd::adjustments::tests::replace_sources(&changed, layout, &engine.resident_state().expect("resident fixture").document, 37);
                assert!(psd::decode(&changed).unwrap().read_only);
                assert!(
                    psd::decode_reader(&mut std::io::Cursor::new(&changed), &Default::default())
                        .unwrap()
                        .read_only
                );
                let last = ordinary.len() - 1;
                ordinary[last] ^= 1;
                assert!(psd::decode(&ordinary).unwrap().read_only);
            }
            assert_eq!(
                serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap()["layers"],
                before["layers"]
            );
        }
    }
}
