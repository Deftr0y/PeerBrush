//! Bounded stock-egui uploads. Pixel charges follow the actual ColorImage owner,
//! including queued/cloned deltas after presenter/context/texture teardown.
use crate::{
    large_document::{Reservation, ResidentBudget},
    render::native_preview::{Frame, Tag},
};
use eframe::egui;
use std::sync::Arc;
#[path = "native_texture/retirement.rs"]
mod retirement;
#[path = "native_texture/tiles.rs"]
mod tiles;
pub(crate) use retirement::Retirement;
const MAX_PIXELS: usize = 128 * 1024;
type Result<T> = std::result::Result<T, String>;

struct FrameRows<'a>(&'a Frame);
impl tiles::Rows for FrameRows<'_> {
    fn row(&self, y: u32) -> Result<&[u8]> {
        let stripes = self.0.stripes();
        let index = stripes.partition_point(|stripe| stripe.rows()[1] <= y);
        let stripe = stripes
            .get(index)
            .ok_or("Native presentation row is outside its complete frame")?;
        if y < stripe.rows()[0] {
            return Err("Native presentation frame has a row gap".into());
        }
        let stride = self.0.key().width as usize * 4;
        let at = (y - stripe.rows()[0]) as usize * stride;
        stripe
            .bytes()
            .get(at..at + stride)
            .ok_or_else(|| "Native presentation frame row is incomplete".into())
    }
}
pub(crate) struct Prepared {
    tiles: tiles::Tiles,
    tag: Tag,
    status: crate::color_profile::prepared::DisplayStatus,
}
impl Prepared {
    /// Run off UI. Frame remains charged throughout conversion; no source IO,
    /// layer metadata, profile executor or session is retained in this output.
    pub(crate) fn from_frame(
        frame: Frame,
        retirement: &Retirement,
        max_texture_side: usize,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        if frame
            .stripes()
            .iter()
            .any(|s| !s.bytes().uses_budget(retirement.pool()))
        {
            return Err("Native upload frame uses another admission pool".into());
        }
        let mut next = 0;
        for stripe in frame.stripes() {
            if stripe.rows()[0] != next
                || stripe.rows()[1] <= next
                || stripe.bytes().len()
                    != (stripe.rows()[1] - next) as usize * frame.key().width as usize * 4
            {
                return Err("Native display stripe coverage is inconsistent".into());
            }
            next = stripe.rows()[1];
        }
        if next != frame.key().height {
            return Err("Native display frame is incomplete".into());
        }
        let tiles = tiles::Tiles::prepare(
            frame.key().width,
            frame.key().height,
            max_texture_side,
            &FrameRows(&frame),
            retirement,
            check,
        )?;
        let tag = frame.tag();
        let status = frame.display_status();
        drop(frame);
        Ok(Self { tiles, tag, status })
    }
    pub(crate) fn tag(&self) -> &Tag {
        &self.tag
    }
    pub(crate) fn profile_warning(&self) -> Option<&'static str> {
        self.status.warning()
    }
}
pub(crate) struct Canvas {
    textures: tiles::Textures,
    tag: Tag,
    status: crate::color_profile::prepared::DisplayStatus,
}
impl Canvas {
    #[cfg(test)]
    pub(crate) fn texture_ids(&self) -> impl Iterator<Item = egui::TextureId> + '_ {
        self.textures.ids()
    }
    pub(crate) fn key(&self) -> &crate::render::native_preview::Key {
        self.tag.key()
    }
    pub(crate) fn profile_warning(&self) -> Option<&'static str> {
        self.status.warning()
    }
    pub(crate) fn paint_with(
        &self,
        painter: &egui::Painter,
        project: impl Fn([f32; 2]) -> egui::Pos2,
        tint: egui::Color32,
    ) {
        let rect = self.key().rect;
        self.textures.paint_with(
            painter,
            |uv| {
                project([
                    rect[0] as f32 + (rect[2] - rect[0]) as f32 * uv[0],
                    rect[1] as f32 + (rect[3] - rect[1]) as f32 * uv[1],
                ])
            },
            tint,
        )
    }
}
#[derive(Default)]
pub(crate) struct Presenter {
    current: Option<Canvas>,
}
impl Presenter {
    /// The caller rechecks actual current source/epoch before supplying this tag.
    /// A refusal leaves the preceding complete canvas untouched.
    pub(crate) fn present(
        &mut self,
        prepared: Prepared,
        current: &Tag,
        ctx: &egui::Context,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        check()?;
        if prepared.tag.key() != current.key() {
            return Err("Native display upload belongs to a stale request".into());
        }
        let textures = prepared.tiles.publish(ctx, check)?;
        self.current = Some(Canvas {
            textures,
            tag: prepared.tag,
            status: prepared.status,
        });
        Ok(())
    }
    pub(crate) fn current(&self) -> Option<&Canvas> {
        self.current.as_ref()
    }
    pub(crate) fn clear(&mut self) {
        self.current = None;
    }
}
#[cfg(test)]
#[path = "native_texture/tests.rs"]
mod tests;
