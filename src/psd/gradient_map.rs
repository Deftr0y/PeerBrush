//! Bounded solid RGB Gradient Map records for PSD and PSB.
use crate::effects::gradient_map::{MAX_NAME, MAX_STOPS, Method, Settings, Stop};
use serde_json::Value;
use super::{Cursor, u16b, u32b};

pub(super) const MAX_RECORD: usize = 56 + 2 * (MAX_NAME + 1) + MAX_STOPS * 30;

pub(super) fn descriptor(settings: &Settings<'_>) -> Vec<u8> {
    let size = 56 + 2 * (settings.name.encode_utf16().count() + 1)
        + settings.colors.len() * 20 + settings.alpha.len() * 10;
    let mut bytes = Vec::with_capacity(size.next_multiple_of(4));
    u16b(&mut bytes, 3);
    bytes.extend([u8::from(settings.reverse), 0]);
    bytes.extend(match settings.method {
        Method::Classic => b"Gcls",
        Method::Linear => b"Lnr ",
        Method::Perceptual => b"Perc",
    });
    u32b(&mut bytes, settings.name.encode_utf16().count() as u32 + 1);
    for unit in settings.name.encode_utf16().chain([0]) {
        u16b(&mut bytes, unit);
    }
    u16b(&mut bytes, settings.colors.len() as u16);
    for stop in &settings.colors {
        u32b(&mut bytes, stop.position);
        u32b(&mut bytes, stop.midpoint);
        u16b(&mut bytes, 0);
        for channel in stop.value {
            u16b(&mut bytes, channel);
        }
        bytes.extend([0; 4]);
    }
    u16b(&mut bytes, settings.alpha.len() as u16);
    for stop in &settings.alpha {
        u32b(&mut bytes, stop.position);
        u32b(&mut bytes, stop.midpoint);
        u16b(&mut bytes, u16::from(stop.value));
    }
    u16b(&mut bytes, 2);
    u16b(&mut bytes, settings.smoothness);
    bytes.extend(settings.solid_fields);
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    bytes
}

fn stops<T>(
    cursor: &mut Cursor<'_>, minimum: usize,
    mut read: impl FnMut(&mut Cursor<'_>) -> Result<T, String>,
) -> Result<Vec<Stop<T>>, String> {
    let count = usize::from(cursor.u16()?);
    if !(minimum..=MAX_STOPS).contains(&count) {
        return Err("Gradient Map has an unsupported stop count".into());
    }
    let mut stops = Vec::with_capacity(count);
    for _ in 0..count {
        let position = cursor.u32()?;
        let midpoint = cursor.u32()?;
        if position > 4096 || midpoint > 100 || stops.last().is_some_and(|stop: &Stop<T>| stop.position >= position) {
            return Err("Gradient Map stops have invalid coordinates or ordering".into());
        }
        stops.push(Stop { position, midpoint, value: read(cursor)? });
    }
    Ok(stops)
}

pub(super) fn settings(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_RECORD {
        return Err("Gradient Map record exceeds its supported size".into());
    }
    let mut cursor = Cursor::new(bytes);
    let version = cursor.u16()?;
    let reverse = cursor.u8()?;
    let dither = cursor.u8()?;
    if reverse > 1 || dither > 1 {
        return Err("Invalid Gradient Map flags".into());
    }
    if dither == 1 {
        return Err("Dithered Photoshop Gradient Maps remain protected".into());
    }
    let method = match version {
        1 => Method::Classic,
        3 => match cursor.bytes(4)? {
            b"Gcls" => Method::Classic,
            b"Lnr " => Method::Linear,
            b"Perc" => Method::Perceptual,
            _ => return Err("Unsupported Gradient Map interpolation method".into()),
        },
        _ => return Err("Unsupported Gradient Map version".into()),
    };
    let units = usize::try_from(cursor.u32()?).map_err(|_| "Invalid Gradient Map name length")?;
    if units > MAX_NAME + 1 {
        return Err("Gradient Map name exceeds its supported length".into());
    }
    let name: Vec<_> = cursor.bytes(units * 2)?.chunks_exact(2)
        .map(|unit| u16::from_be_bytes([unit[0], unit[1]])).collect();
    let name = name.strip_suffix(&[0]).unwrap_or(&name);
    if name.len() > MAX_NAME || name.contains(&0) {
        return Err("Invalid Gradient Map name terminator".into());
    }
    let name = String::from_utf16(name).map_err(|_| "Invalid Gradient Map Unicode name")?;
    let colors = stops(&mut cursor, 2, |cursor| {
        if cursor.u16()? != 0 {
            return Err("Non-RGB Gradient Map color stops remain protected".into());
        }
        let rgb = [cursor.u16()?, cursor.u16()?, cursor.u16()?];
        if cursor.bytes(4)? != [0; 4] {
            return Err("Invalid Gradient Map color stop padding".into());
        }
        Ok(rgb)
    })?;
    let alpha = stops(&mut cursor, 0, |cursor| {
        u8::try_from(cursor.u16()?).map_err(|_| "Invalid Gradient Map opacity".into())
    })?;
    if cursor.u16()? != 2 {
        return Err("Unsupported Gradient Map expansion".into());
    }
    let smoothness = cursor.u16()?;
    let solid_fields: [u8; 36] = cursor.bytes(36)?.try_into().unwrap();
    if solid_fields[..4] != [0, 32, 0, 0] || smoothness > 4096 {
        return Err("Noise or unsupported Gradient Map records remain protected".into());
    }
    let padding = &bytes[cursor.pos..];
    if padding.len() > 3 || padding.iter().any(|&value| value != 0) {
        return Err("Invalid Gradient Map padding".into());
    }
    Ok(Settings { name: &name, method, reverse: reverse == 1, smoothness, colors, alpha, solid_fields }.value())
}
