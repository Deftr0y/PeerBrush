//! One admitted JSON-protocol payload; row samples retain original channel bits.
use crate::{
    code::{replacement_pixel, replacement_shape, SampleOrder, MAX_PIXELS},
    edit_bounds::input::RasterInput,
    large_document::{PinnedBytes, ScratchBytes},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};

pub(super) struct Replacement {
    rect: [i32; 4],
    depth: u16,
    bytes: PinnedBytes,
}
impl Replacement {
    pub(super) fn new(
        input: &RasterInput<'_>,
        command: &serde_json::Value,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if input.is_mask()
            || input.layer_metadata().kind() != "paint"
            || input.layer_metadata().source().is_some()
        {
            return Err("Native pixel replacement requires a paint layer".into());
        }
        let metadata = input.document().metadata();
        let (rect, count) = replacement_shape(
            [metadata.width(), metadata.height()],
            metadata.depth(),
            command,
        )?;
        let encoded = command["rgba"]
            .as_str()
            .filter(|s| s.len() <= MAX_PIXELS * 8 * 4 / 3 + 4)
            .ok_or("Invalid bounded base64 RGBA data")?;
        let length = count * usize::from(input.depth() / 8) * 4;
        if encoded.len() != length.div_ceil(3) * 4 {
            return Err("Native RGBA length does not match rectangle/depth".into());
        }
        // decode_slice admits a complete last quartet before removing padding.
        let mut bytes = ScratchBytes::new(input.pool(), length + 2, MAX_PIXELS * 8 + 2)
            .map_err(|e| e.to_string())?;
        let decoded = STANDARD
            .decode_slice(encoded, &mut bytes)
            .map_err(|_| "Invalid base64 RGBA data")?;
        if decoded != length {
            return Err("Native RGBA length does not match rectangle/depth".into());
        }
        check()?;
        Ok(Self {
            rect,
            depth: input.depth(),
            bytes: bytes.freeze(),
        })
    }
    pub(super) fn pixel(&self, x: i32, y: i32) -> [u16; 4] {
        let at = ((y - self.rect[1]) as usize * (self.rect[2] - self.rect[0]) as usize
            + (x - self.rect[0]) as usize)
            * usize::from(self.depth / 8)
            * 4;
        replacement_pixel(&self.bytes, self.depth, SampleOrder::Little, at)
    }
}
