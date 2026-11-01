//! Actual bounded standard-channel rows from one immutable captured scene.
//! This provider grants no save, source-adoption or destination authority.
use super::{
    bulk36::Digest,
    scene_save36::{self, CapturedRows},
    standard36::{NativeRows, RowRequest, RowSource},
};
use crate::{
    engine::{native::LayerMetadata, scene::SceneSnapshot},
    large_document::{Error, ScratchBytes},
    production_export::{native_rows, Format, Output, Resampling},
    raster::backed::Region,
};

type Result<T> = std::result::Result<T, String>;
const STRIPE_PIXELS: usize = 128 * 1024;

pub(crate) struct Rows36<'a> {
    scene: &'a SceneSnapshot,
    provenance: Digest,
    // Construct only on the first current-composite request. Unsupported
    // derived work cannot prevent inspection of explicit raw source channels.
    composite: Option<native_rows::Rows<'a>>,
}
impl<'a> Rows36<'a> {
    pub(crate) fn new(scene: &'a SceneSnapshot) -> Result<Self> {
        let doc = scene.metadata();
        if doc.read_only()
            || doc.profile().is_some()
            || ![8, 16].contains(&doc.depth())
            || !(1..=16000).contains(&doc.width())
            || !(1..=16000).contains(&doc.height())
        {
            return Err("Native standard rows require a supported editable RGB scene".into());
        }
        Ok(Self {
            scene,
            provenance: scene_save36::provenance(scene),
            composite: None,
        })
    }
    fn validate(&self, request: RowRequest, output: &[u8]) -> Result<Stripe> {
        let width = i64::from(request.bounds[2]) - i64::from(request.bounds[0]);
        let height = i64::from(request.bounds[3]) - i64::from(request.bounds[1]);
        let end = request.first_row.checked_add(request.rows);
        // PSD layer alpha uses channel -1; the merged planar image uses 3.
        let channel = if request.source == RowSource::MergedWhiteMatte {
            (0..=3).contains(&request.channel)
        } else {
            [-2, -1, 0, 1, 2].contains(&request.channel)
        };
        if request.provenance != self.provenance
            || request.depth != self.scene.metadata().depth()
            || !channel
            || !(1..=16000).contains(&width)
            || !(1..=16000).contains(&height)
            || request.bounds.iter().any(|v| v.unsigned_abs() > 116000)
            || request.rows == 0
            || request.rows > 256
            || end.is_none_or(|n| i64::from(n) > height)
        {
            return Err("Native standard row request differs from its captured frame".into());
        }
        let pixels = (width as usize)
            .checked_mul(request.rows as usize)
            .filter(|n| *n <= STRIPE_PIXELS)
            .ok_or("Native standard stripe exceeds 128 KiPixel admission")?;
        let bytes = pixels
            .checked_mul(usize::from(request.depth / 8))
            .ok_or("Native standard channel stripe size overflow")?;
        if output.len() != bytes {
            return Err("Native standard channel buffer has the wrong exact length".into());
        }
        let top = i64::from(request.bounds[1]) + i64::from(request.first_row);
        Ok(Stripe {
            width: width as usize,
            pixels,
            bounds: [
                request.bounds[0],
                top.try_into().map_err(|_| "Native stripe top overflow")?,
                request.bounds[2],
                (top + i64::from(request.rows))
                    .try_into()
                    .map_err(|_| "Native stripe bottom overflow")?,
            ],
        })
    }
    fn layer(&self, index: usize) -> Result<LayerMetadata<'a>> {
        self.scene
            .metadata()
            .layer(index)
            .ok_or_else(|| "Native standard row layer is missing".into())
    }
    fn raw(
        &self,
        request: RowRequest,
        stripe: Stripe,
        layer: LayerMetadata<'a>,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        if !["paint", "fill", "group", "adjustment"].contains(&layer.kind()) {
            return Err("Native standard layer kind requires explicit source support".into());
        }
        if request.bounds != self.raw_frame(layer, request.source)? {
            return Err(
                "Native layer channel request differs from its planned source frame".into(),
            );
        }
        if request.channel == -2 {
            return Err(if layer.mask().is_some() {
                "Raw standard mask channels require the explicit prepared mask provider"
            } else {
                "Native standard layer has no mask channel"
            }
            .into());
        }
        let source = self.scene.raster(layer.pixels().slot())?;
        if !source.uses_budget(self.scene.budget())
            || !source.uses_disk_budget(self.scene.disk_budget())
            || source.depth() != request.depth
        {
            return Err("Native standard source differs from the captured global admission".into());
        }
        if matches!(request.source, RowSource::Layer(_))
            && (layer.source().is_some()
                || !["paint", "fill"].contains(&layer.kind())
                || layer.effects().any(|e| e.enabled() && e.weight() > 0.))
        {
            return Err(
                "Current standard layer requires its explicit regional source/effect provider"
                    .into(),
            );
        }
        // Legacy standard fill channels contain the procedural colour, including
        // hidden RGB; a fill's stored raster is not a substitute for that colour.
        if layer.kind() == "fill" {
            let pixel = if request.depth == 16 {
                layer.color().map(|v| u16::from(v) * 257)
            } else {
                layer.color().map(u16::from)
            };
            return write_channel(request, stripe, output, check, |_, _| Ok(pixel));
        }
        let [ox, oy] = layer.origin();
        let local = [
            stripe.bounds[0].checked_sub(ox),
            stripe.bounds[1].checked_sub(oy),
            stripe.bounds[2].checked_sub(ox),
            stripe.bounds[3].checked_sub(oy),
        ];
        let [Some(l), Some(t), Some(r), Some(b)] = local else {
            return Err("Native standard local source coordinates overflow".into());
        };
        let region = Region::new([l, t, r, b]).map_err(|e| e.to_string())?;
        let mut failed_check = None;
        let prepared = source
            .prepare_region(region, &mut || {
                check().map_err(|reason| {
                    failed_check = Some(reason);
                    Error::Cancelled
                })
            })
            .map_err(|e| failed_check.unwrap_or_else(|| e.to_string()))?;
        check()?;
        // From this point sampling is memory-only; source I/O has completed.
        write_channel(request, stripe, output, check, |x, y| {
            if request.depth == 16 {
                prepared.pixel16(x - ox, y - oy).map_err(|e| e.to_string())
            } else {
                prepared
                    .pixel(x - ox, y - oy)
                    .map(|p| p.map(u16::from))
                    .map_err(|e| e.to_string())
            }
        })
    }
    fn raw_frame(&self, layer: LayerMetadata<'_>, source: RowSource) -> Result<[i32; 4]> {
        if matches!(source, RowSource::OriginalLayer(_))
            && ["group", "adjustment"].contains(&layer.kind())
        {
            return Ok(self.canvas());
        }
        let [x, y] = layer.origin();
        let right = i64::from(x) + i64::from(layer.pixels().width());
        let bottom = i64::from(y) + i64::from(layer.pixels().height());
        Ok([
            x,
            y,
            right
                .try_into()
                .map_err(|_| "Native layer right overflow")?,
            bottom
                .try_into()
                .map_err(|_| "Native layer bottom overflow")?,
        ])
    }
    fn canvas(&self) -> [i32; 4] {
        [
            0,
            0,
            self.scene.metadata().width() as i32,
            self.scene.metadata().height() as i32,
        ]
    }
    fn composite(
        &mut self,
        request: RowRequest,
        stripe: Stripe,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        if request.bounds != self.canvas() || request.channel == -2 {
            return Err("Native composite request differs from its full captured canvas".into());
        }
        if self
            .scene
            .metadata()
            .layers()
            .any(|layer| layer.source().is_some())
        {
            return Err("Current standard composite requires the explicit regional editable-source provider".into());
        }
        if self.composite.is_none() {
            self.composite = Some(native_rows::Rows::new(self.scene)?);
        }
        let renderer = self
            .composite
            .as_ref()
            .ok_or("Missing captured composite provider")?;
        if !std::ptr::eq(renderer.scene(), self.scene) {
            return Err("Native composite provider belongs to another scene".into());
        }
        let doc = self.scene.metadata();
        let render = Output {
            target: None,
            target_name: String::new(),
            variant: String::new(),
            filename: String::new(),
            document_rect: self.canvas(),
            width: doc.width(),
            height: doc.height(),
            format: Format::Png,
            native_source_depth: doc.depth(),
            bit_depth: doc.depth(),
            scale_xy: [1., 1.],
        };
        let bytes = stripe
            .pixels
            .checked_mul(usize::from(request.depth / 2))
            .ok_or("Native RGBA composite stripe overflow")?;
        let mut rgba = ScratchBytes::new(self.scene.budget(), bytes, STRIPE_PIXELS * 8)
            .map_err(|e| e.to_string())?;
        renderer.fill(
            &render,
            request.depth,
            Resampling::Nearest,
            request.first_row,
            request.rows,
            &mut rgba,
            check,
        )?;
        check()?;
        let stride = usize::from(request.depth / 2);
        let mut at = 0usize;
        write_channel(request, stripe, output, check, |_, _| {
            let pixel = &rgba[at..at + stride];
            at += stride;
            let mut pixel = if request.depth == 16 {
                std::array::from_fn(|c| u16::from_be_bytes([pixel[c * 2], pixel[c * 2 + 1]]))
            } else {
                std::array::from_fn(|c| u16::from(pixel[c]))
            };
            if request.source == RowSource::MergedWhiteMatte {
                let maximum = if request.depth == 16 {
                    65535u64
                } else {
                    255u64
                };
                let alpha = u64::from(pixel[3]);
                for channel in &mut pixel[..3] {
                    *channel =
                        ((u64::from(*channel) * alpha + maximum * (maximum - alpha) + maximum / 2)
                            / maximum) as u16;
                }
            }
            Ok(pixel)
        })
    }
}
#[derive(Clone, Copy)]
struct Stripe {
    width: usize,
    pixels: usize,
    bounds: [i32; 4],
}
fn write_channel(
    request: RowRequest,
    stripe: Stripe,
    output: &mut [u8],
    check: &dyn Fn() -> Result<()>,
    mut sample: impl FnMut(i32, i32) -> Result<[u16; 4]>,
) -> Result<()> {
    let channel = if request.channel == -1 {
        3
    } else {
        request.channel as usize
    };
    if channel > 3 {
        return Err("Native standard colour channel is invalid".into());
    }
    let stride = usize::from(request.depth / 8);
    for row in 0..request.rows as usize {
        check()?;
        for column in 0..stripe.width {
            let value = sample(
                stripe.bounds[0] + column as i32,
                stripe.bounds[1] + row as i32,
            )?[channel];
            let at = (row * stripe.width + column) * stride;
            if request.depth == 16 {
                output[at..at + 2].copy_from_slice(&value.to_be_bytes());
            } else {
                output[at] = value
                    .try_into()
                    .map_err(|_| "Native8 standard sample exceeds its precision")?;
            }
        }
    }
    check()
}
impl CapturedRows for Rows36<'_> {
    fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
}
impl NativeRows for Rows36<'_> {
    fn provenance(&self) -> Digest {
        self.provenance
    }
    fn fill(
        &mut self,
        request: RowRequest,
        output: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        check()?;
        let stripe = self.validate(request, output)?;
        match request.source {
            RowSource::Layer(index) | RowSource::OriginalLayer(index) => {
                let layer = self.layer(index)?;
                self.raw(request, stripe, layer, output, check)
            }
            RowSource::BakedComposite | RowSource::MergedWhiteMatte => {
                self.composite(request, stripe, output, check)
            }
            RowSource::Zero if request.channel != -2 => {
                output.fill(0);
                check()
            }
            RowSource::Zero => Err("Explicit zero records do not have a mask channel".into()),
        }
    }
}

#[cfg(test)]
#[path = "native_rows36_tests.rs"]
mod tests;
