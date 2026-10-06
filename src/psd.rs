//! A bounded PSD v1 codec. Unsupported Photoshop features are never silently rewritten.
use crate::{
    engine::{id, Document, Layer, Mask, MaskStep},
    raster::{check_size, Raster},
};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::io::Read;

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("PSD length overflow")?;
        let v = self.data.get(self.pos..end).ok_or("Truncated PSD")?;
        self.pos = end;
        Ok(v)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16, String> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    fn block(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.bytes(n)
    }
}
fn u16b(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes())
}
fn u32b(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes())
}
fn i32b(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_be_bytes())
}
fn block(out: &mut Vec<u8>, v: &[u8]) {
    u32b(out, v.len() as u32);
    out.extend_from_slice(v);
}
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}
fn tag(out: &mut Vec<u8>, key: &[u8; 4], v: &[u8]) {
    out.extend_from_slice(b"8BIM");
    out.extend_from_slice(key);
    block(out, v);
    if v.len() % 2 != 0 {
        out.push(0);
    }
}

#[derive(Serialize, Deserialize)]
struct Embedded {
    format: u32,
    standard_hash: u64,
    document: Document,
}
struct BoundedWriter<W> {
    inner: W,
    written: usize,
}
impl<W: std::io::Write> std::io::Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.written.saturating_add(bytes.len()) > 128 * 1024 * 1024 {
            return Err(std::io::Error::other(
                "Editable PSD source data exceeds the initial 128 MiB limit",
            ));
        }
        let n = self.inner.write(bytes)?;
        self.written += n;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub fn encode(doc: &Document) -> Result<Vec<u8>, String> {
    if doc.read_only {
        return Err(
            "Cannot save an unsupported PSD without explicitly creating a compatible copy".into(),
        );
    }
    validate(doc)?;
    let masks = doc
        .layers
        .iter()
        .map(|l| {
            l.mask
                .as_ref()
                .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
        })
        .collect::<Vec<_>>();
    let colors = crate::effects::prepare(doc, &masks)?;
    let mut ordered = Vec::new();
    fn order<'a>(doc: &'a Document, parent: Option<&str>, out: &mut Vec<(&'a Layer, u32)>) {
        for l in doc.layers.iter().filter(|l| l.parent.as_deref() == parent) {
            if l.kind == "group" && crate::effects::active(l) {
                // A group effect is baked as one standard raster layer; embedded sources retain its editable children.
                out.push((l, 0));
            } else if l.kind == "group" {
                out.push((l, 1));
                order(doc, Some(&l.id), out);
                out.push((l, 3));
            } else {
                out.push((l, 0));
            }
        }
    }
    order(doc, None, &mut ordered);
    let empty_layer = Layer::new("Canvas", "paint", doc.width, doc.height);
    if ordered.is_empty() {
        ordered.push((&empty_layer, 0));
    }
    // PSD stores layer records from bottom to top, including group dividers.
    ordered.reverse();
    if ordered.len() > 200 {
        return Err("Too many PSD layer records".into());
    }
    let mut records = vec![];
    let mut channels = vec![];
    for (l, section) in &ordered {
        let (w, h) = if *section == 0 {
            (l.pixels.width, l.pixels.height)
        } else {
            (0, 0)
        };
        let (x, y) = if *section == 0 { (l.x, l.y) } else { (0, 0) };
        i32b(&mut records, y);
        i32b(&mut records, x);
        i32b(&mut records, y + h as i32);
        i32b(&mut records, x + w as i32);
        let mask = l.mask.as_ref();
        let has_mask = mask.is_some() && *section != 3;
        let prepared = if has_mask {
            mask.and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
        } else {
            None
        };
        u16b(&mut records, if has_mask { 5 } else { 4 });
        for ch in [-1i16, 0, 1, 2]
            .into_iter()
            .chain(if has_mask { Some(-2) } else { None })
        {
            let mut data = vec![];
            u16b(&mut data, 0);
            let (cw, ch_height) = if ch == -2 {
                (l.pixels.width, l.pixels.height)
            } else {
                (w, h)
            };
            for yy in 0..ch_height {
                for xx in 0..cw {
                    data.push(if ch == -2 {
                        (l.mask_value_prepared(xx as i32, yy as i32, prepared.as_deref(), true)
                            * 255.0)
                            .round() as u8
                    } else {
                        let derived = doc
                            .layers
                            .iter()
                            .position(|n| n.id == l.id)
                            .and_then(|i| colors[i].as_ref());
                        let p = if let Some(image) = derived {
                            image.get(xx as i32, yy as i32)
                        } else if l.kind == "fill" {
                            l.color
                        } else {
                            l.pixels.get(xx as i32, yy as i32)
                        };
                        p[if ch == -1 { 3 } else { ch as usize }]
                    });
                }
            }
            u16b(&mut records, ch as u16);
            u32b(&mut records, data.len() as u32);
            channels.extend(data);
        }
        records.extend_from_slice(b"8BIM");
        records.extend_from_slice(match l.blend.as_str() {
            "multiply" => b"mul ",
            "screen" => b"scrn",
            "overlay" => b"over",
            "darken" => b"dark",
            "lighten" => b"lite",
            _ => b"norm",
        });
        records.push((l.opacity * 255.0).round() as u8);
        records.push(0);
        records.push(if l.visible { 0 } else { 2 });
        records.push(0);
        let mut extra = vec![];
        if has_mask {
            let mut m = vec![];
            i32b(&mut m, y);
            i32b(&mut m, x);
            i32b(&mut m, y + l.pixels.height as i32);
            i32b(&mut m, x + l.pixels.width as i32);
            m.extend_from_slice(&[
                255,
                if mask.is_some_and(|m| !m.enabled) {
                    2
                } else {
                    0
                },
                0,
                0,
            ]);
            block(&mut extra, &m);
        } else {
            u32b(&mut extra, 0);
        }
        u32b(&mut extra, 0);
        let name = if *section == 3 {
            "</Layer group>"
        } else {
            &l.name
        };
        let n = name.as_bytes().len().min(255);
        extra.push(n as u8);
        extra.extend_from_slice(&name.as_bytes()[..n]);
        while extra.len() % 4 != 0 {
            extra.push(0);
        }
        let mut unicode = vec![];
        let utf16: Vec<u16> = name.encode_utf16().collect();
        u32b(&mut unicode, utf16.len() as u32);
        for c in utf16 {
            u16b(&mut unicode, c);
        }
        tag(&mut extra, b"luni", &unicode);
        if *section != 0 {
            let mut s = vec![];
            u32b(&mut s, *section);
            tag(&mut extra, b"lsct", &s);
        }
        block(&mut records, &extra);
    }
    let mut info = vec![];
    // A negative count identifies the merged fourth channel as transparency.
    u16b(&mut info, (-(ordered.len() as i16)) as u16);
    info.extend(records);
    info.extend(channels);
    if info.len() % 2 != 0 {
        info.push(0);
    }
    let mut layer_section = vec![];
    block(&mut layer_section, &info);
    u32b(&mut layer_section, 0);
    let (w, h, rgba, _) = doc.preview(None, doc.width.max(doc.height), None, false)?;
    let mut composite = vec![];
    u16b(&mut composite, 0);
    for c in 0..4 {
        for p in rgba.chunks_exact(4) {
            // Photoshop stores the merged RGB preview over a white matte.
            // Layer channel pixels remain straight alpha.
            composite.push(if c < 3 {
                ((p[c] as u32 * p[3] as u32 + 255 * (255 - p[3] as u32) + 127) / 255) as u8
            } else {
                p[3]
            });
        }
    }
    let embedded = Embedded {
        format: if doc.layers.iter().any(|l| {
            !l.effects.is_empty()
                || l.mask
                    .as_ref()
                    .is_some_and(|m| m.steps.iter().any(|s| !s.settings.is_null()))
        }) {
            3
        } else if doc.layers.iter().any(|l| {
            l.mask
                .as_ref()
                .is_some_and(|m| m.steps.iter().any(|s| s.kind == "blur"))
        }) {
            2
        } else {
            1
        },
        standard_hash: hash(&layer_section) ^ hash(&composite),
        document: doc.clone(),
    };
    let mut z = BoundedWriter {
        inner: ZlibEncoder::new(Vec::new(), Compression::fast()),
        written: 0,
    };
    serde_json::to_writer(&mut z, &embedded).map_err(|e| e.to_string())?;
    let compressed = z.inner.finish().map_err(|e| e.to_string())?;
    let mut resources = vec![];
    resources.extend_from_slice(b"8BIM");
    u16b(&mut resources, 4000);
    resources.push(12);
    resources.extend_from_slice(b"PeerBrush.v1");
    resources.push(0);
    let mut payload = b"PBR1".to_vec();
    payload.extend(compressed);
    block(&mut resources, &payload);
    if payload.len() % 2 != 0 {
        resources.push(0);
    }
    let mut out = b"8BPS".to_vec();
    u16b(&mut out, 1);
    out.extend_from_slice(&[0; 6]);
    u16b(&mut out, 4);
    u32b(&mut out, h);
    u32b(&mut out, w);
    u16b(&mut out, 8);
    u16b(&mut out, 3);
    u32b(&mut out, 0);
    block(&mut out, &resources);
    block(&mut out, &layer_section);
    out.extend(composite);
    if out.len() > 256 * 1024 * 1024 {
        return Err("PSD output exceeds the initial 256 MiB file limit".into());
    }
    Ok(out)
}

fn decode_channel(data: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let count = (w as usize)
        .checked_mul(h as usize)
        .ok_or("Channel overflow")?;
    if count > 32 * 1024 * 1024 {
        return Err("PSD channel exceeds allocation budget".into());
    }
    let mut r = Cursor::new(data);
    let kind = r.u16()?;
    match kind {
        0 => Ok(r.bytes(count)?.to_vec()),
        1 => {
            let mut lengths = vec![];
            for _ in 0..h {
                lengths.push(r.u16()? as usize);
            }
            let mut out = Vec::with_capacity(count);
            for n in lengths {
                let row = r.bytes(n)?;
                let start = out.len();
                let mut i = 0;
                while i < row.len() {
                    let n = row[i] as i8;
                    i += 1;
                    match n {
                        0..=127 => {
                            let len = n as usize + 1;
                            if i + len > row.len() {
                                return Err("Invalid RLE packet".into());
                            }
                            out.extend_from_slice(&row[i..i + len]);
                            i += len;
                        }
                        -127..=-1 => {
                            let value = *row.get(i).ok_or("Invalid RLE repeat")?;
                            i += 1;
                            out.resize(out.len() + (-n as i16 + 1) as usize, value);
                        }
                        _ => {}
                    }
                    if out.len() - start > w as usize {
                        return Err("RLE row overflow".into());
                    }
                }
                if out.len() - start != w as usize {
                    return Err("Invalid RLE row width".into());
                }
            }
            Ok(out)
        }
        2 | 3 => {
            let mut out = vec![];
            ZlibDecoder::new(&data[2..])
                .take(count as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|e| e.to_string())?;
            if out.len() != count {
                return Err("Invalid ZIP channel size".into());
            }
            if kind == 3 && w > 0 {
                for row in out.chunks_exact_mut(w as usize) {
                    for i in 1..row.len() {
                        row[i] = row[i].wrapping_add(row[i - 1]);
                    }
                }
            }
            Ok(out)
        }
        _ => Err("Unknown PSD compression".into()),
    }
}
struct Record {
    layer: Layer,
    channels: Vec<(i16, usize)>,
    bounds: [i32; 4],
    mask_bounds: Option<[i32; 4]>,
    mask_default: u8,
    mask_enabled: bool,
    section: u32,
}

pub fn decode(data: &[u8]) -> Result<Document, String> {
    if data.len() > 256 * 1024 * 1024 {
        return Err("Initial PSD file limit is 256 MiB".into());
    }
    let mut r = Cursor::new(data);
    if r.bytes(4)? != b"8BPS" || r.u16()? != 1 {
        return Err("Only PSD v1 is supported; PSB is not supported yet".into());
    }
    r.bytes(6)?;
    let channel_count = r.u16()? as usize;
    let h = r.u32()?;
    let w = r.u32()?;
    check_size(w, h)?;
    let depth = r.u16()?;
    let mode = r.u16()?;
    if depth != 8 || mode != 3 || !(3..=4).contains(&channel_count) {
        return Err("Initial version opens 8-bit RGB PSDs with 3 or 4 channels".into());
    }
    r.block()?;
    let resources = r.block()?;
    let mut rr = Cursor::new(resources);
    let mut embedded = None;
    let mut warnings = vec![];
    while rr.pos < resources.len() {
        if rr.bytes(4)? != b"8BIM" {
            return Err("Invalid image resource".into());
        }
        let key = rr.u16()?;
        let n = rr.u8()? as usize;
        rr.bytes(n)?;
        if (n + 1) % 2 != 0 {
            rr.bytes(1)?;
        }
        let payload = rr.block()?;
        if key == 4000 && payload.starts_with(b"PBR1") {
            embedded = Some(payload[4..].to_vec());
        }
        if key == 1039 {
            warnings.push("Embedded color profiles are not managed by the initial editor".into());
        }
        if payload.len() % 2 != 0 {
            rr.bytes(1)?;
        }
    }
    let layer_section = r.block()?;
    let composite_data = &data[r.pos..];
    let mut ls = Cursor::new(layer_section);
    let mut records = vec![];
    let mut merged_transparency = false;
    if !layer_section.is_empty() {
        let info = ls.block()?;
        if info.len() >= 2 {
            let mut lr = Cursor::new(info);
            let signed_count = lr.i16()?;
            merged_transparency = signed_count < 0;
            let count = signed_count.unsigned_abs() as usize;
            if count > 200 {
                return Err("Too many PSD layers for this version".into());
            }
            for _ in 0..count {
                let bounds = [lr.i32()?, lr.i32()?, lr.i32()?, lr.i32()?];
                if bounds.iter().any(|v| v.unsigned_abs() > 100000) {
                    return Err("PSD layer coordinates exceed initial limits".into());
                }
                let lh = bounds[2]
                    .checked_sub(bounds[0])
                    .ok_or("Invalid layer bounds")?;
                let lw = bounds[3]
                    .checked_sub(bounds[1])
                    .ok_or("Invalid layer bounds")?;
                if lw < 0
                    || lh < 0
                    || lw > 8192
                    || lh > 8192
                    || lw as i64 * lh as i64 > 32 * 1024 * 1024
                {
                    return Err("PSD layer bounds exceed limits".into());
                }
                let nc = lr.u16()?;
                if nc > 8 {
                    return Err("Unsupported PSD layer channels".into());
                }
                let mut channels = vec![];
                for _ in 0..nc {
                    channels.push((lr.i16()?, lr.u32()? as usize));
                }
                if lr.bytes(4)? != b"8BIM" {
                    return Err("Invalid blend signature".into());
                }
                let blendkey = lr.bytes(4)?;
                let blend = match blendkey {
                    b"norm" => "normal",
                    b"mul " => "multiply",
                    b"scrn" => "screen",
                    b"over" => "overlay",
                    b"dark" => "darken",
                    b"lite" => "lighten",
                    _ => {
                        warnings.push("Unsupported PSD blending mode".into());
                        "normal"
                    }
                };
                let opacity = lr.u8()? as f32 / 255.0;
                if lr.u8()? != 0 {
                    warnings.push("Clipping layers are not editable in the initial version".into());
                }
                let flags = lr.u8()?;
                lr.u8()?;
                let extra = lr.block()?;
                let mut ex = Cursor::new(extra);
                let maskdata = ex.block()?;
                let mut mask_bounds = None;
                let mut mask_default = 255;
                let mut mask_enabled = true;
                if maskdata.len() >= 18 {
                    let mut m = Cursor::new(maskdata);
                    mask_bounds = Some([m.i32()?, m.i32()?, m.i32()?, m.i32()?]);
                    if mask_bounds
                        .unwrap()
                        .iter()
                        .any(|v| v.unsigned_abs() > 100000)
                    {
                        return Err("PSD mask coordinates exceed initial limits".into());
                    }
                    mask_default = m.u8()?;
                    let mask_flags = m.u8()?;
                    mask_enabled = mask_flags & 2 == 0;
                    if mask_flags & !2 != 0 {
                        warnings.push("Advanced PSD mask flags are not supported yet".into());
                    }
                }
                ex.block()?;
                let np = ex.u8()? as usize;
                let mut name = String::from_utf8_lossy(ex.bytes(np)?).to_string();
                let padded = (np + 1 + 3) & !3;
                ex.bytes(padded - np - 1)?;
                let mut section = 0;
                while ex.pos + 12 <= extra.len() {
                    let signature = ex.bytes(4)?;
                    if signature != b"8BIM" && signature != b"8B64" {
                        break;
                    }
                    let key = ex.bytes(4)?;
                    let payload = ex.block()?;
                    if payload.len() % 2 != 0 {
                        ex.bytes(1)?;
                    }
                    match key {
                        b"luni" => {
                            let mut n = Cursor::new(payload);
                            let count = n.u32()? as usize;
                            if count > 10000 {
                                return Err("PSD name is too long".into());
                            }
                            let mut chars = vec![];
                            for _ in 0..count {
                                chars.push(n.u16()?);
                            }
                            name = String::from_utf16_lossy(&chars);
                        }
                        b"lsct" | b"lsdk" => {
                            section = Cursor::new(payload).u32()?;
                        }
                        b"lyid" | b"lspf" | b"clbl" | b"infx" | b"knko" | b"iOpa" | b"sn2P" => {}
                        _ => warnings.push(format!(
                            "Unsupported Photoshop feature: {}",
                            String::from_utf8_lossy(key)
                        )),
                    }
                }
                let mut layer = Layer::new(
                    &name,
                    if section == 1 || section == 2 {
                        "group"
                    } else {
                        "paint"
                    },
                    lw as u32,
                    lh as u32,
                );
                layer.x = bounds[1];
                layer.y = bounds[0];
                layer.visible = flags & 2 == 0;
                layer.opacity = opacity;
                layer.blend = blend.into();
                records.push(Record {
                    layer,
                    channels,
                    bounds,
                    mask_bounds,
                    mask_default,
                    mask_enabled,
                    section,
                });
            }
            let mut total = 0usize;
            for rec in &mut records {
                let lw = rec.layer.pixels.width;
                let lh = rec.layer.pixels.height;
                let n = lw as usize * lh as usize;
                total = total.checked_add(n * 4).ok_or("PSD allocation overflow")?;
                if total > 512 * 1024 * 1024 {
                    return Err("PSD layers exceed initial memory budget".into());
                }
                let mut rgba = vec![0; n * 4];
                for p in rgba.chunks_exact_mut(4) {
                    p[3] = 255;
                }
                for &(ch, size) in &rec.channels {
                    let bytes = lr.bytes(size)?;
                    if ch == -2 {
                        if let Some(b) = rec.mask_bounds {
                            let mw = b[3].checked_sub(b[1]).ok_or("Invalid mask")?;
                            let mh = b[2].checked_sub(b[0]).ok_or("Invalid mask")?;
                            if mw < 0 || mh < 0 {
                                return Err("Invalid mask bounds".into());
                            }
                            let values = decode_channel(bytes, mw as u32, mh as u32)?;
                            let (pw, ph) = if rec.layer.kind == "group" {
                                (w, h)
                            } else {
                                (lw, lh)
                            };
                            let mut pixels = Raster::new(pw, ph);
                            for yy in 0..mh {
                                for xx in 0..mw {
                                    let v = values[(yy * mw + xx) as usize];
                                    pixels.set(
                                        xx + b[1] - rec.bounds[1],
                                        yy + b[0] - rec.bounds[0],
                                        [v, v, v, 255],
                                    );
                                }
                            }
                            rec.layer.mask = Some(Mask {
                                cache_key: crate::engine::id(),
                                enabled: rec.mask_enabled,
                                steps: vec![
                                    MaskStep {
                                        id: id(),
                                        kind: "fill".into(),
                                        enabled: true,
                                        value: rec.mask_default as f32,
                                        pixels: Raster::new(pw, ph),
                                        settings: serde_json::Value::Null,
                                    },
                                    MaskStep {
                                        id: id(),
                                        kind: "paint".into(),
                                        enabled: true,
                                        value: 0.0,
                                        pixels,
                                        settings: serde_json::Value::Null,
                                    },
                                ],
                            });
                        }
                    } else if (-1..=2).contains(&ch) {
                        let values = decode_channel(bytes, lw, lh)?;
                        let c = if ch == -1 { 3 } else { ch as usize };
                        for (p, value) in rgba.chunks_exact_mut(4).zip(values) {
                            p[c] = value;
                        }
                    }
                }
                if lw > 0 && lh > 0 {
                    rec.layer.pixels = Raster::from_rgba(lw, lh, &rgba)?;
                } else if rec.layer.kind == "group" {
                    rec.layer.pixels = Raster::new(w, h);
                }
            }
        }
    }
    let merged = decode_composite(composite_data, w, h, channel_count, merged_transparency)?;
    if let Some(bytes) = embedded {
        let mut json = vec![];
        let result = ZlibDecoder::new(&bytes[..])
            .take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut json);
        if result.is_ok() && json.len() <= 128 * 1024 * 1024 {
            if let Ok(e) = serde_json::from_slice::<Embedded>(&json) {
                if (1..=3).contains(&e.format)
                    && e.standard_hash == hash(layer_section) ^ hash(composite_data)
                    && validate(&e.document).is_ok()
                {
                    return Ok(e.document);
                }
            }
        }
        warnings.push(
            "PeerBrush definitions were changed or removed externally; using saved PSD pixels"
                .into(),
        );
    }
    warnings.sort();
    warnings.dedup();
    let mut doc = Document::new(w, h)?;
    doc.layers.clear();
    let mut parents = Vec::<String>::new();
    for mut rec in records.into_iter().rev() {
        if rec.section == 3 {
            parents.pop();
            continue;
        }
        rec.layer.parent = parents.last().cloned();
        if rec.layer.kind == "group" {
            parents.push(rec.layer.id.clone());
        }
        doc.layers.push(rec.layer);
    }
    if doc.layers.is_empty() || !warnings.is_empty() {
        let mut l = Layer::new("Saved PSD composite", "paint", w, h);
        l.pixels = merged;
        doc.layers = vec![l];
        doc.read_only = !warnings.is_empty();
    }
    doc.warnings = warnings;
    Ok(doc)
}
fn decode_composite(
    data: &[u8],
    w: u32,
    h: u32,
    count: usize,
    transparency: bool,
) -> Result<Raster, String> {
    let mut r = Cursor::new(data);
    let kind = r.u16()?;
    let n = w as usize * h as usize;
    let mut channels = vec![];
    if kind == 0 {
        for _ in 0..count {
            channels.push(r.bytes(n)?.to_vec());
        }
    } else if kind == 1 {
        let mut sizes = vec![];
        for _ in 0..count {
            let mut s = vec![];
            for _ in 0..h {
                s.push(r.u16()?);
            }
            sizes.push(s);
        }
        for s in sizes {
            let mut b = vec![];
            u16b(&mut b, 1);
            for v in &s {
                u16b(&mut b, *v);
            }
            let total: usize = s.iter().map(|v| *v as usize).sum();
            b.extend_from_slice(r.bytes(total)?);
            channels.push(decode_channel(&b, w, h)?);
        }
    } else if kind == 2 || kind == 3 {
        let mut decoded = vec![];
        ZlibDecoder::new(&data[2..])
            .take((n * count + 1) as u64)
            .read_to_end(&mut decoded)
            .map_err(|e| e.to_string())?;
        if decoded.len() != n * count {
            return Err("Wrong composite size".into());
        }
        for c in 0..count {
            let mut v = decoded[c * n..(c + 1) * n].to_vec();
            if kind == 3 {
                for row in v.chunks_exact_mut(w as usize) {
                    for i in 1..row.len() {
                        row[i] = row[i].wrapping_add(row[i - 1]);
                    }
                }
            }
            channels.push(v);
        }
    } else {
        return Err("Unsupported composite compression".into());
    }
    let mut rgba = vec![0; n * 4];
    for i in 0..n {
        let alpha = if transparency && count >= 4 {
            channels[3][i]
        } else {
            255
        };
        for c in 0..3 {
            rgba[i * 4 + c] = if alpha == 0 {
                0
            } else {
                (((channels[c][i] as i32 + alpha as i32 - 255) * 255 + alpha as i32 / 2)
                    / alpha as i32)
                    .clamp(0, 255) as u8
            };
        }
        rgba[i * 4 + 3] = alpha;
    }
    Raster::from_rgba(w, h, &rgba)
}
pub fn validate(doc: &Document) -> Result<(), String> {
    check_size(doc.width, doc.height)?;
    if doc.layers.len() > 100 {
        return Err("Layer limit".into());
    }
    let mut ids = std::collections::HashSet::new();
    let mut total = 0usize;
    for l in &doc.layers {
        if !ids.insert(l.id.clone()) {
            return Err("Duplicate layer ID".into());
        }
        check_size(l.pixels.width, l.pixels.height)?;
        if l.x.unsigned_abs() > 100000
            || l.y.unsigned_abs() > 100000
            || !["paint", "fill", "group"].contains(&l.kind.as_str())
        {
            return Err("Invalid layer coordinates or kind".into());
        }
        if !l.opacity.is_finite() {
            return Err("Invalid opacity".into());
        }
        total += l.pixels.bytes();
        let mut p = l.parent.as_deref();
        let mut depth = 0;
        while let Some(parent) = p {
            let n = doc
                .layers
                .iter()
                .find(|x| x.id == parent && x.kind == "group")
                .ok_or("Invalid group parent")?;
            p = n.parent.as_deref();
            depth += 1;
            if depth > 16 {
                return Err("Cyclic groups".into());
            }
        }
        if l.effects.len() > 32 {
            return Err("Effect stack limit".into());
        }
        for e in &l.effects {
            crate::effects::validate(&e.kind, &e.settings)?;
        }
        if let Some(m) = &l.mask {
            if m.steps.len() > 32 {
                return Err("Too many mask steps".into());
            }
            for s in &m.steps {
                total += s.pixels.bytes();
                let valid = match s.kind.as_str() {
                    "blur" => (0.0..=64.0).contains(&s.value),
                    "levels" => (0.1..=5.0).contains(&s.value),
                    "fill" | "paint" | "invert" => (0.0..=255.0).contains(&s.value),
                    "curves" | "adjust" | "gaussian" => {
                        crate::effects::validate(
                            if s.kind == "gaussian" {
                                "blur"
                            } else {
                                &s.kind
                            },
                            &s.settings,
                        )?;
                        true
                    }
                    _ => false,
                };
                if !s.value.is_finite() || !valid {
                    return Err("Invalid mask step".into());
                }
            }
        }
        if total > 512 * 1024 * 1024 {
            return Err("Document memory limit".into());
        }
    }
    crate::mask::validate_budget(&doc.layers)?;
    crate::effects::validate_budget(doc)?;
    Ok(())
}
