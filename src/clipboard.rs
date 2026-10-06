//! Selection pixels and OS clipboard transport. Editing stays in the shared engine.
use crate::{
    engine::{Document, Layer},
    raster::{check_size, png, Raster},
    server::Shared,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{borrow::Cow, sync::mpsc};

#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    pub origin: Option<[i32; 2]>,
}
impl Image {
    pub fn copy(doc: &Document, target: &str, mask: bool, merged: bool) -> Result<Self, String> {
        let area = doc
            .selection
            .unwrap_or([0, 0, doc.width as i32, doc.height as i32]);
        if area[2] <= area[0]
            || area[3] <= area[1]
            || area[2] <= 0
            || area[3] <= 0
            || area[0] >= doc.width as i32
            || area[1] >= doc.height as i32
        {
            return Err("Select an area inside the canvas to copy".into());
        }
        let (width, height, mut bytes, area) =
            doc.preview(Some(area), 8192, (!merged).then_some(target), mask)?;
        if !merged && !mask {
            let layer = doc
                .layers
                .iter()
                .find(|l| l.id == target)
                .ok_or("Layer no longer exists")?;
            let prepared = layer
                .mask
                .as_ref()
                .and_then(|m| m.prepare(layer.pixels.width, layer.pixels.height));
            for y in 0..height {
                for x in 0..width {
                    let alpha = &mut bytes[((y * width + x) * 4 + 3) as usize];
                    *alpha = (*alpha as f32
                        * layer.mask_value_prepared(
                            area[0] + x as i32 - layer.x,
                            area[1] + y as i32 - layer.y,
                            prepared.as_deref(),
                            false,
                        )
                        * layer.opacity)
                        .round() as u8;
                }
            }
        }
        Ok(Self {
            width,
            height,
            bytes,
            origin: Some([area[0], area[1]]),
        })
    }
    pub fn command(&self, context: &str) -> Result<Value, String> {
        check_size(self.width, self.height)?;
        Ok(
            json!({"op":"image.paste","layer":context,"png":STANDARD.encode(png(self.width, self.height, &self.bytes)?),"origin":self.origin}),
        )
    }
}

/// Called inside an Engine transaction so grouping, pixels and selection share one undo step.
pub fn paste(doc: &mut Document, command: &Value) -> Result<(), String> {
    let context = command["layer"]
        .as_str()
        .ok_or("Choose a layer or folder before pasting")?;
    let index = doc
        .layers
        .iter()
        .position(|l| l.id == context)
        .ok_or("Paste destination no longer exists")?;
    let selected = &doc.layers[index];
    let mut parent = Some(selected.id.as_str());
    for _ in 0..=16 {
        let Some(pid) = parent else { break };
        let l = doc
            .layers
            .iter()
            .find(|l| l.id == pid)
            .ok_or("Invalid paste destination")?;
        if l.locked {
            return Err("Unlock the destination before pasting".into());
        }
        parent = l.parent.as_deref();
    }
    let wrap = selected.kind != "group" && selected.parent.is_none();
    if doc.layers.len() + if wrap { 2 } else { 1 } > 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let encoded = command["png"]
        .as_str()
        .ok_or("Paste needs base64 PNG pixels")?;
    if encoded.len() > 180 * 1024 * 1024 {
        return Err("Clipboard image is too large".into());
    }
    let encoded = STANDARD.decode(encoded).map_err(|e| e.to_string())?;
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(encoded), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
    check_size(image.width(), image.height())?;
    let mut pasted = Layer::new("Pasted image", "paint", image.width(), image.height());
    pasted.pixels = Raster::from_rgba(image.width(), image.height(), image.as_raw())?;
    let origin = if command["origin"].is_null() {
        [
            (doc.width as i32 - image.width() as i32) / 2,
            (doc.height as i32 - image.height() as i32) / 2,
        ]
    } else {
        let a = command["origin"]
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or("Invalid paste origin")?;
        let coordinate = |v: &Value| {
            v.as_i64()
                .filter(|n| (-100000..=100000).contains(n))
                .map(|n| n as i32)
                .ok_or("Invalid paste origin")
        };
        [coordinate(&a[0])?, coordinate(&a[1])?]
    };
    pasted.x = origin[0];
    pasted.y = origin[1];
    if wrap {
        let mut folder = Layer::new(
            &format!("{} group", selected.name),
            "group",
            doc.width,
            doc.height,
        );
        // Retain the source interaction with the backdrop when introducing a group.
        folder.blend = selected.blend.clone();
        folder.parent = selected.parent.clone();
        doc.layers[index].blend = "normal".into();
        pasted.parent = Some(folder.id.clone());
        doc.layers[index].parent = Some(folder.id.clone());
        doc.layers.insert(index, folder);
        doc.layers.insert(index + 1, pasted);
    } else {
        pasted.parent = if selected.kind == "group" {
            Some(selected.id.clone())
        } else {
            selected.parent.clone()
        };
        let at = index + usize::from(selected.kind == "group");
        doc.layers.insert(at, pasted);
    }
    doc.selection = None;
    Ok(())
}

pub enum Request {
    Copy {
        doc: Document,
        target: String,
        mask: bool,
        merged: bool,
    },
    Paste {
        shared: Shared,
        target: String,
        revision: u64,
    },
}
pub struct Reply {
    pub result: Result<String, String>,
    pub selected: Option<String>,
}
pub struct Worker {
    tx: mpsc::SyncSender<Request>,
    pub replies: mpsc::Receiver<Reply>,
}
impl Default for Worker {
    fn default() -> Self {
        Self::new(eframe::egui::Context::default())
    }
}
impl Worker {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let (tx, rx) = mpsc::sync_channel(1);
        let (reply_tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            // Retain ownership for Linux clipboard providers while the application is open.
            let mut clipboard = arboard::Clipboard::new().ok();
            let mut copied: Option<Image> = None;
            while let Ok(request) = rx.recv() {
                let mut selected = None;
                let result = (|| {
                    if clipboard.is_none() {
                        clipboard = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
                    }
                    let clipboard = clipboard.as_mut().unwrap();
                    match request {
                        Request::Copy {
                            doc,
                            target,
                            mask,
                            merged,
                        } => {
                            let image = Image::copy(&doc, &target, mask, merged)?;
                            clipboard
                                .set_image(arboard::ImageData {
                                    width: image.width as usize,
                                    height: image.height as usize,
                                    bytes: Cow::Borrowed(&image.bytes),
                                })
                                .map_err(|e| e.to_string())?;
                            copied = Some(image);
                            Ok("Selection copied".into())
                        }
                        Request::Paste {
                            shared,
                            target,
                            revision,
                        } => {
                            let pixels = clipboard.get_image().map_err(|_| {
                                "Copy an image or a selection before pasting".to_string()
                            })?;
                            let width = u32::try_from(pixels.width)
                                .map_err(|_| "Clipboard image is too large")?;
                            let height = u32::try_from(pixels.height)
                                .map_err(|_| "Clipboard image is too large")?;
                            check_size(width, height)?;
                            let origin = copied
                                .as_ref()
                                .filter(|old| {
                                    old.width == width
                                        && old.height == height
                                        && old.bytes.as_slice() == pixels.bytes.as_ref()
                                })
                                .and_then(|old| old.origin);
                            let command = Image {
                                width,
                                height,
                                bytes: pixels.bytes.into_owned(),
                                origin,
                            }
                            .command(&target)?;
                            let mut engine = shared.lock().unwrap();
                            let result = engine.edit(
                                "human",
                                &[command],
                                Some(revision),
                                None,
                                "Paste image",
                            )?;
                            selected = result["created"]
                                .as_array()
                                .and_then(|ids| {
                                    ids.iter().filter_map(Value::as_str).find(|id| {
                                        engine
                                            .doc
                                            .layers
                                            .iter()
                                            .any(|l| l.id == *id && l.kind == "paint")
                                    })
                                })
                                .map(String::from);
                            Ok("Pasted as a new layer".into())
                        }
                    }
                })();
                if reply_tx.send(Reply { result, selected }).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self { tx, replies }
    }
    pub fn request(&self, request: Request) -> Result<(), String> {
        self.tx
            .try_send(request)
            .map_err(|_| "Finish the current clipboard operation first".into())
    }
}
