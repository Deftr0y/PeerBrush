//! The interpreter and parent enforce the same exposed editing surface.
use super::*;

pub(super) fn validate_edit(command: &Value) -> Result<(), String> {
    let object = command
        .as_object()
        .ok_or("Code command must be an object")?;
    if !matches!(
        command["op"].as_str(),
        Some(
            "paint"
                | "shape"
                | "gradient"
                | "fill"
                | "adjust"
                | "move"
                | "transform"
                | "layer.update"
                | "effect.add"
                | "effect.update"
                | "effect.delete"
                | "effect.reorder"
                | "mask.add"
                | "mask.remove"
                | "mask.toggle"
                | "mask.refine"
                | "mask.from_selection"
                | "mask.from_color"
                | "mask.step.add"
                | "mask.step.update"
                | "mask.step.delete"
                | "mask.step.reorder"
                | "filter.add"
                | "filter.update"
                | "filter.delete"
                | "filter.reorder"
                | "source.update"
                | "selection"
                | "selection.refine"
                | "selection.clear"
                | "selection.reselect"
                | "selection.invert"
                | "selection.modify"
        )
    ) {
        return Err("Operation is not exposed to code; use native pixel buffers or documented editing commands".into());
    }
    if object.keys().any(|key| {
        matches!(
            key.as_str(),
            "path" | "png" | "rgba" | "preset" | "locked" | "source_revision" | "document_id"
        )
    }) {
        return Err("Code cannot access files, resolve mutable presets, change locks or override source guards".into());
    }
    Ok(())
}

/// Pixel buffers have their own exact schema. The caller owns the decode
/// scratch; no second unadmitted native buffer is allocated here.
pub(super) fn validate_returned(
    command: &Value,
    config: evaluation::Config,
    scratch: &mut [u8],
) -> Result<usize, String> {
    if command["op"] != "pixels.replace" {
        validate_edit(command)?;
        return Ok(0);
    }
    let object = command
        .as_object()
        .ok_or("Code command must be an object")?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "op" | "layer" | "rect" | "bit_depth" | "rgba"))
        || command["layer"]
            .as_str()
            .is_none_or(|id| id.is_empty() || id.len() > 1024)
    {
        return Err("Invalid code pixel command".into());
    }
    let (_, count) = replacement_shape([config.width, config.height], config.bit_depth, command)?;
    let length = count * usize::from(config.bit_depth / 8) * 4;
    let encoded = command["rgba"].as_str().ok_or("Missing native RGBA data")?;
    if encoded.len() != length.div_ceil(3) * 4 || scratch.len() < length {
        return Err("Native RGBA length does not match rectangle/depth".into());
    }
    let decoded = STANDARD
        .decode_slice(encoded, &mut scratch[..length])
        .map_err(|_| "Invalid base64 RGBA data")?;
    if decoded != length {
        return Err("Native RGBA length does not match rectangle/depth".into());
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exposed_surface_rejects_authority_overrides_and_validates_native_payloads() {
        for op in [
            "layer.create",
            "project.open",
            "pixels.replace",
            "",
            "unknown",
        ] {
            assert!(validate_edit(&json!({"op":op})).is_err());
        }
        for key in [
            "path",
            "png",
            "rgba",
            "preset",
            "locked",
            "source_revision",
            "document_id",
        ] {
            let mut command = json!({"op":"layer.update","layer":"layer","opacity":0.5});
            command[key] = Value::Null;
            assert!(validate_edit(&command).is_err(), "{key}");
        }
        for depth in [8, 16] {
            let config = evaluation::Config {
                width: 3,
                height: 2,
                bit_depth: depth,
            };
            let words = [12347u16, 23459, 34571, 61379];
            let bytes: Vec<u8> = if depth == 16 {
                words.into_iter().flat_map(u16::to_le_bytes).collect()
            } else {
                words.map(|word| (word >> 8) as u8).to_vec()
            };
            let original = json!({"op":"pixels.replace","layer":"layer","rect":[0,0,1,1],"bit_depth":depth,"rgba":STANDARD.encode(&bytes)});
            let mut scratch = [0; 16];
            assert_eq!(
                validate_returned(&original, config, &mut scratch).unwrap(),
                1
            );
            assert_eq!(&scratch[..bytes.len()], bytes);
            for failure in ["depth", "length", "base64", "guard", "layer", "rect"] {
                let mut command = original.clone();
                match failure {
                    "depth" => command["bit_depth"] = json!(32),
                    "length" => command["rgba"] = json!(STANDARD.encode([0u8; 3])),
                    "base64" => {
                        command["rgba"] =
                            json!("!".repeat(original["rgba"].as_str().unwrap().len()))
                    }
                    "guard" => command["source_revision"] = json!(1),
                    "layer" => command["layer"] = json!(""),
                    "rect" => command["rect"] = json!([0, 0, 4, 1]),
                    _ => unreachable!(),
                }
                assert!(
                    validate_returned(&command, config, &mut scratch).is_err(),
                    "{failure}"
                );
            }
            assert!(validate_returned(&original, config, &mut []).is_err());
        }
    }
}
