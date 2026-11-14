//! The interpreter receives metadata and raw pixel replies, never edit authority.
use super::*;
use serde::{ser::SerializeSeq, Deserialize, Serialize, Serializer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::code) struct Config {
    pub(in crate::code) width: u32,
    pub(in crate::code) height: u32,
    pub(in crate::code) bit_depth: u16,
}
impl Config {
    pub(in crate::code) fn from_view(doc: DocumentView<'_>) -> Self {
        Self {
            width: doc.width(),
            height: doc.height(),
            bit_depth: doc.depth(),
        }
    }
    pub(in crate::code) fn validate(&self) -> Result<(), String> {
        if !(1..=16000).contains(&self.width)
            || !(1..=16000).contains(&self.height)
            || ![8, 16].contains(&self.bit_depth)
        {
            return Err("Code metadata needs supported dimensions and channel depth".into());
        }
        Ok(())
    }
}
pub(in crate::code) trait Pixels: Send {
    fn pixel(&self, layer: &str, x: INT, y: INT) -> Result<[u16; 4], String>;
    fn patch(&self, layer: &str, rect: [i32; 4]) -> Result<Vec<u16>, String>;
}
pub(in crate::code) struct Input {
    pub(in crate::code) config: Config,
    pub(in crate::code) metadata: Dynamic,
    pub(in crate::code) pixels: Box<dyn Pixels>,
    pub(in crate::code) progress: Box<dyn Fn(u64) -> Option<Dynamic> + Send + Sync>,
}
pub(in crate::code) struct ResidentPixels {
    doc: Document,
    scopes: Vec<Scope>,
}
impl ResidentPixels {
    pub(in crate::code) fn new(doc: Document, scopes: Vec<Scope>) -> Self {
        Self { doc, scopes }
    }
}
impl Pixels for ResidentPixels {
    fn pixel(&self, layer: &str, x: INT, y: INT) -> Result<[u16; 4], String> {
        if x < 0 || y < 0 || x >= INT::from(self.doc.width) || y >= INT::from(self.doc.height) {
            return Err("Pixel coordinates must be inside the document".into());
        }
        let layer = self
            .doc
            .layers
            .iter()
            .find(|l| l.id == layer)
            .ok_or("Unknown pixel source")?;
        if layer.kind != "paint" || layer.source.is_some() {
            return Err("Native pixel access requires a paint layer".into());
        }
        let (x, y) = (x - INT::from(layer.x), y - INT::from(layer.y));
        if x < 0
            || y < 0
            || x >= INT::from(layer.pixels.width)
            || y >= INT::from(layer.pixels.height)
        {
            return Ok([0; 4]);
        }
        let x = i32::try_from(x).map_err(|_| "Pixel source coordinate is unsupported")?;
        let y = i32::try_from(y).map_err(|_| "Pixel source coordinate is unsupported")?;
        Ok(if self.doc.bit_depth == 16 {
            layer.pixels.get16(x, y)
        } else {
            layer.pixels.get(x, y).map(u16::from)
        })
    }
    fn patch(&self, layer: &str, rect: [i32; 4]) -> Result<Vec<u16>, String> {
        if rect[0] < 0
            || rect[1] < 0
            || rect[0] >= rect[2]
            || rect[1] >= rect[3]
            || rect[2] > self.doc.width as i32
            || rect[3] > self.doc.height as i32
        {
            return Err("Pixel rectangle must be inside the document".into());
        }
        let count = (rect[2] - rect[0]) as usize * (rect[3] - rect[1]) as usize;
        if count > MAX_PIXELS {
            return Err("Code exceeds 262144 patch pixels".into());
        }
        contained(
            (&self.doc).into(),
            &self.scopes,
            &[Scope {
                target: Some(layer.into()),
                rect: Some(rect),
            }],
        )?;
        let mut words = Vec::new();
        words
            .try_reserve_exact(count * 4)
            .map_err(|_| "Code pixel buffer allocation refused")?;
        for y in rect[1]..rect[3] {
            for x in rect[0]..rect[2] {
                words.extend_from_slice(&self.pixel(layer, INT::from(x), INT::from(y))?);
            }
        }
        Ok(words)
    }
}

#[derive(Serialize)]
struct Header<'a> {
    width: u32,
    height: u32,
    bit_depth: u16,
    channel_max: u16,
    layers: Layers<'a>,
}
struct Layers<'a>(DocumentView<'a>);
impl Serialize for Layers<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Layer<'a> {
            id: &'a str,
            name: &'a str,
            kind: &'a str,
            parent: Option<&'a str>,
            x: i32,
            y: i32,
            locked: bool,
        }
        let mut output = serializer.serialize_seq(None)?;
        for layer in self.0.layers() {
            let [x, y] = layer.origin();
            output.serialize_element(&Layer {
                id: layer.id(),
                name: layer.name(),
                kind: layer.kind(),
                parent: layer.parent(),
                x,
                y,
                locked: layer.locked(),
            })?;
        }
        output.end()
    }
}
pub(in crate::code) fn metadata(doc: DocumentView<'_>) -> Result<Dynamic, String> {
    rhai::serde::to_dynamic(header(doc)).map_err(|e| e.to_string())
}
pub(in crate::code) fn write_metadata(
    doc: DocumentView<'_>,
    writer: impl std::io::Write,
) -> Result<(), String> {
    serde_json::to_writer(writer, &header(doc)).map_err(|e| e.to_string())
}
fn header(doc: DocumentView<'_>) -> Header<'_> {
    Header {
        width: doc.width(),
        height: doc.height(),
        bit_depth: doc.depth(),
        channel_max: if doc.depth() == 16 { 65535 } else { 255 },
        layers: Layers(doc),
    }
}
