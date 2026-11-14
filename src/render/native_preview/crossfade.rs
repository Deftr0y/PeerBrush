//! Completed fades retain admitted display pixels/tags, never source sessions.
use super::*;
use crate::{engine::native::NativeDocument, psd::memory36::MetadataCharge};
use std::fmt::Write;
mod streamed;

pub(crate) struct Blend {
    before: DocumentKey,
    after: DocumentKey,
    document: String,
    revisions: [u64; 2],
    fraction: f32,
    signature: [u8; 32],
    charge: Arc<MetadataCharge>,
}
impl Blend {
    pub(crate) fn capture(
        before: &NativeDocument,
        after: &NativeDocument,
        fraction: f32,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        let a = before.metadata();
        let b = after.metadata();
        if !fraction.is_finite()
            || !(0. ..=1.).contains(&fraction)
            || a.id() != b.id()
            || a.id().len() > 128
            || a.revision() >= b.revision()
            || a.width() != b.width()
            || a.height() != b.height()
            || a.depth() != b.depth()
            || !before.budget().same_pool(after.budget())
            || !before.disk_budget().same_pool(after.disk_budget())
        {
            return Err("Canvas fade differs from its native document transition".into());
        }
        let charge = MetadataCharge::reserve(after.budget(), a.id().len() + 1024, check)?;
        struct Digest(ring::digest::Context);
        impl std::fmt::Write for Digest {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                self.0.update(text.as_bytes());
                Ok(())
            }
        }
        let mut digest = Digest(ring::digest::Context::new(&ring::digest::SHA256));
        digest.0.update(b"PeerBrush native display blend v1\0");
        write!(digest, "{:?}/{:?}", before.key(), after.key())
            .map_err(|_| "Canvas fade identity encoding failed")?;
        digest.0.update(&fraction.to_bits().to_be_bytes());
        let signature = digest.0.finish().as_ref().try_into().unwrap();
        check()?;
        Ok(Self {
            before: before.key(),
            after: after.key(),
            document: a.id().into(),
            revisions: [a.revision(), b.revision()],
            fraction,
            signature,
            charge,
        })
    }
    pub(crate) fn signature(&self) -> [u8; 32] {
        self.signature
    }
    pub(crate) fn matches(&self, document: &NativeDocument) -> bool {
        self.after == document.key()
            && self.charge.uses_budget(document.budget())
            && self.document == document.metadata().id()
            && self.revisions[1] == document.metadata().revision()
    }
    pub(crate) fn matches_before(&self, document: &NativeDocument) -> bool {
        self.before == document.key() && self.document == document.metadata().id()
            && self.revisions[0] == document.metadata().revision()
            && self.charge.uses_budget(document.budget())
    }
    fn validate_tags(&self, before: &Tag, after: &Tag) -> Result<()> {
        let a = before.key();
        let b = after.key();
        if a.source != self.before
            || b.source != self.after
            || a.identity.document_id != self.document
            || b.identity.document_id != self.document
            || a.identity.revision != self.revisions[0]
            || b.identity.revision != self.revisions[1]
            || a.identity.project_id != b.identity.project_id
            || a.rect != b.rect
            || a.width != b.width
            || a.height != b.height
            || a.target != b.target
            || a.mask != b.mask
            || a.selection_view.is_some()
            || b.selection_view.is_some()
            || before.0.profile != after.0.profile
            || before.0.grid != after.0.grid
            || !self.charge.uses_budget(&after.0.pool)
            || !before.0.pool.same_pool(&after.0.pool)
        {
            return Err("Canvas fade pixels no longer match the captured transition/view".into());
        }
        Ok(())
    }
    fn output_tag(&self, after: &Tag) -> Result<(Tag, Arc<Origin>)> {
        let key = after.key();
        let bytes = key
            .identity
            .project_id
            .len()
            .checked_add(key.identity.document_id.len())
            .and_then(|n| n.checked_add(key.target.as_ref().map_or(0, String::len)))
            .and_then(|n| n.checked_add(1024))
            .ok_or("Canvas fade tag size overflow")?;
        let tags = after.0.pool
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut key = key.clone();
        key.source = crate::engine::scene::presentation::display_binding();
        let prepared = Arc::new(Prepared {
            requested_rect: after.0.requested_rect,
            key,
            target: after.0.target,
            grid: after.0.grid,
            pixel_limit: after.0.pixel_limit,
            pool: after.0.pool.clone(),
            profile: after.0.profile,
            raster_damage: None,
            _tags: tags,
        });
        let origin = Origin::blended(after, Tag(prepared.clone()), self.signature)?;
        Ok((Tag(prepared), origin))
    }
    /// Consume the candidate. Refusal/cancellation cannot publish a partial blend.
    pub(crate) fn render(
        &self,
        from: &Baseline,
        mut after: Frame,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<(Frame, Arc<Origin>)> {
        check()?;
        self.validate_tags(&from.tag, &after.tag())?;
        if from.status != after.display_status {
            return Err("Canvas fade display status changed".into());
        }
        validate(&after)?;
        let (tag, origin) = self.output_tag(&after.tag())?;
        let prepared = tag.0;
        let width = after.key().width as usize;
        for stripe in &mut after.stripes {
            check()?;
            let mut output =
                ScratchBytes::new(&prepared.pool, stripe.bytes.len(), 64 * 1024 * 1024)
                    .map_err(|e| e.to_string())?;
            for y in stripe.rows[0]..stripe.rows[1] {
                check()?;
                let old = from.row(y)?;
                let at = (y - stripe.rows[0]) as usize * width * 4;
                for ((a, b), out) in old
                    .chunks_exact(4)
                    .zip(stripe.bytes[at..at + width * 4].chunks_exact(4))
                    .zip(output[at..at + width * 4].chunks_exact_mut(4))
                {
                    pixel(a, b, self.fraction, out);
                }
            }
            stripe.bytes = output.freeze();
        }
        after.prepared = prepared;
        check()?;
        if !origin.matches_frame(&after) {
            return Err("Canvas fade provenance differs from the displayed pixels".into());
        }
        Ok((after, origin))
    }
}

pub(crate) struct Baseline {
    stripes: Vec<Stripe>,
    outline: Arc<crate::engine::native::SelectionOutline>,
    tag: Tag,
    status: DisplayStatus,
    _index: Reservation,
}
impl Baseline {
    pub(crate) fn capture(
        frame: &Frame,
        document: &NativeDocument,
        project: &str,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        Self::try_capture(frame, document, project, check)?
            .ok_or_else(|| "Canvas fade baseline admission refused".into())
    }
    pub(crate) fn try_capture(
        frame: &Frame,
        document: &NativeDocument,
        project: &str,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Option<Self>> {
        Self::try_capture_with_headroom(frame, document, project, 0, check)
    }
    pub(crate) fn try_capture_with_headroom(
        frame: &Frame,
        document: &NativeDocument,
        project: &str,
        upload_bytes: usize,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Option<Self>> {
        check()?;
        let key = frame.key();
        let doc = document.metadata();
        if key.source != document.key()
            || key.identity.project_id != project
            || key.identity.document_id != doc.id()
            || key.identity.revision != doc.revision()
            || key.rect[0] < 0 || key.rect[1] < 0
            || key.rect[2] > doc.width() as i32 || key.rect[3] > doc.height() as i32
            || key.rect[0] >= key.rect[2] || key.rect[1] >= key.rect[3]
            || key.selection_view.is_some()
            || !document.budget().same_pool(frame.budget())
        {
            return Err("Canvas fade baseline is not the observed committed frame".into());
        }
        validate(frame)?;
        let outline = frame
            .outline
            .as_ref()
            .ok_or("Canvas fade outline is unavailable")?
            .clone();
        let bytes = frame
            .stripes
            .len()
            .checked_mul(std::mem::size_of::<Stripe>())
            .and_then(|n| n.checked_add(512))
            .ok_or("Canvas fade index size overflow")?;
        // Optional cache pins prevent upload conversion from retiring stripes.
        // Decline that retention when the remaining upload cannot fit. Actual
        // uploads still obtain their own reservations and may lose a race.
        if bytes.checked_add(upload_bytes).is_none_or(|needed| {
            needed > frame.budget().limit().saturating_sub(frame.budget().used())
        }) {
            return Ok(None);
        }
        let index = match frame.budget().reserve_bytes(bytes, 4 * 1024 * 1024) {
            Ok(index) => index,
            Err(_) => return Ok(None),
        };
        let mut stripes = Vec::new();
        if stripes.try_reserve_exact(frame.stripes.len()).is_err() {
            return Ok(None);
        }
        for stripe in &frame.stripes {
            check()?;
            stripes.push(Stripe {
                rows: stripe.rows,
                bytes: stripe.bytes.clone(),
            });
        }
        check()?;
        Ok(Some(Self {
            stripes,
            outline,
            tag: frame.tag(),
            status: frame.display_status,
            _index: index,
        }))
    }
    pub(crate) fn tag(&self) -> &Tag {
        &self.tag
    }
    pub(super) fn status(&self) -> DisplayStatus {
        self.status
    }
    /// Rebase only complete admitted stripes. A cut stripe is rendered again.
    pub(super) fn pan_stripe(&self, rect: [i32; 4], y: u32) -> Option<([u32; 2], &PinnedBytes)> {
        let old_top = self.tag.key().rect[1];
        let first = (rect[1] + y as i32 - old_top).max(0) as u32;
        let index = self.stripes.partition_point(|stripe| stripe.rows[0] < first);
        let stripe = self.stripes.get(index)?;
        let start = old_top + stripe.rows[0] as i32;
        let end = old_top + stripe.rows[1] as i32;
        if end > rect[3] {
            return None;
        }
        Some(([(start - rect[1]) as u32, (end - rect[1]) as u32], &stripe.bytes))
    }
    /// Copy the shared columns into a private full-width output band.
    pub(super) fn copy_pan_rows(
        &self,
        rect: [i32; 4],
        rows: Range<u32>,
        output: &mut [u8],
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<[i32; 2]> {
        let old = self.tag.key().rect;
        let left = old[0].max(rect[0]);
        let right = old[2].min(rect[2]);
        let row = rect[1] + rows.start as i32 - old[1];
        let stride = (rect[2] - rect[0]) as usize * 4;
        if row < 0 || row as usize + rows.len() > self.tag.key().height as usize
            || left >= right || output.len() != stride * rows.len()
        {
            return Err("Preview overlap lies outside its admitted rows".into());
        }
        let source = (left - old[0]) as usize * 4..(right - old[0]) as usize * 4;
        let target = (left - rect[0]) as usize * 4..(right - rect[0]) as usize * 4;
        for (offset, out) in output.chunks_exact_mut(stride).enumerate() {
            check()?;
            out[target.clone()].copy_from_slice(&self.row(row as u32 + offset as u32)?[source.clone()]);
        }
        Ok([left, right])
    }
    /// Share immutable pixel pins and selection metadata, admitting the new index.
    pub(crate) fn frame(&self, check: &dyn Fn() -> Result<()>) -> Result<Frame> {
        check()?;
        let bytes = self
            .stripes
            .len()
            .checked_mul(std::mem::size_of::<Stripe>())
            .and_then(|n| n.checked_add(512))
            .ok_or("Canvas fade index size overflow")?;
        let index = self
            .tag
            .0
            .pool
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut stripes = Vec::new();
        stripes
            .try_reserve_exact(self.stripes.len())
            .map_err(|_| "Canvas fade index allocation refused")?;
        for stripe in &self.stripes {
            check()?;
            stripes.push(Stripe {
                rows: stripe.rows,
                bytes: stripe.bytes.clone(),
            });
        }
        check()?;
        Ok(Frame {
            outline: Some(self.outline.clone()),
            stripes,
            prepared: self.tag.0.clone(),
            display_status: self.status,
            _index: index,
        })
    }
    fn row(&self, y: u32) -> Result<&[u8]> {
        let index = self.stripes.partition_point(|stripe| stripe.rows[1] <= y);
        let stripe = self
            .stripes
            .get(index)
            .ok_or("Canvas fade baseline row is absent")?;
        if y < stripe.rows[0] {
            return Err("Canvas fade baseline has a row gap".into());
        }
        let stride = self.tag.key().width as usize * 4;
        let at = (y - stripe.rows[0]) as usize * stride;
        stripe
            .bytes
            .get(at..at + stride)
            .ok_or_else(|| "Canvas fade baseline row is incomplete".into())
    }
}

fn validate(frame: &Frame) -> Result<()> {
    if frame.key().width == 0 || frame.key().height == 0 {
        return Err("Canvas fade requires nonempty display dimensions".into());
    }
    let mut next = 0;
    for stripe in frame.stripes() {
        if stripe.rows[0] != next
            || stripe.rows[1] <= next
            || stripe.rows[1] > frame.key().height
            || stripe.bytes.len()
                != (stripe.rows[1] - next) as usize * frame.key().width as usize * 4
            || !stripe.bytes.uses_budget(frame.budget())
        {
            return Err("Canvas fade requires complete admitted frame rows".into());
        }
        next = stripe.rows[1];
    }
    if next != frame.key().height || next == 0 {
        return Err("Canvas fade requires a complete nonempty frame".into());
    }
    Ok(())
}

fn pixel(a: &[u8], b: &[u8], fraction: f32, out: &mut [u8]) {
    if fraction == 0. {
        out.copy_from_slice(a);
        return;
    }
    if fraction == 1. {
        out.copy_from_slice(b);
        return;
    }
    let t = f64::from(fraction);
    let alpha = f64::from(a[3]) * (1. - t) + f64::from(b[3]) * t;
    for channel in 0..3 {
        out[channel] = if alpha > 0. {
            ((f64::from(a[channel]) * f64::from(a[3]) * (1. - t)
                + f64::from(b[channel]) * f64::from(b[3]) * t)
                / alpha)
                .round() as u8
        } else {
            0
        };
    }
    out[3] = alpha.round() as u8;
}

#[cfg(test)]
mod tests;
