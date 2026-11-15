use super::*;
use crate::{
    engine::scene::{Content36, Source36},
    fonts::{prepared::FontOwner, FontError, TextFont},
    large_document::ResidentBudget,
    raster::backed::Region,
    source::{
        prepared::{NativeRegion, PreparedSource},
        TextLayout,
    },
};
use ab_glyph::Font;
use std::sync::{mpsc, Mutex, OnceLock, Weak};

const SIZE: [u32; 2] = [240, 38];
type Reply = Mutex<Option<Result<NativeRegion, String>>>;

struct Request {
    font: TextFont,
    reply: Weak<Reply>,
    ctx: egui::Context,
}

fn render(
    font: TextFont,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<NativeRegion, String> {
    // One independent presentation job can hold an admitted font and shaping scratch.
    let pool = ResidentBudget::new(320 * 1024 * 1024).map_err(|error| error.to_string())?;
    let font_owner = Arc::new(FontOwner::load(&font, &pool, check)?);
    let sample = [
        "Aa Bb 123",
        "Аа Бб",
        "Αα Ββ",
        "אבגד",
        "مرحبا",
        "你好世界",
        "あいうえお",
        "한글",
        "नमस्ते",
    ]
    .into_iter()
    .find(|text| text.chars().all(|ch| font_owner.font().glyph_id(ch).0 != 0))
    .ok_or("This face has no glyphs for the available samples")?;
    let _definition = pool
        .reserve_bytes(4096, 4096)
        .map_err(|error| error.to_string())?;
    let source = Source36 {
        width: SIZE[0],
        height: SIZE[1],
        matrix: [1., 0., 0., 1., 0., 4.],
        content: Content36::Text {
            text: sample.into(),
            font,
            layout: TextLayout::Shaped,
            size: 27.,
            line_height: 1.,
            letter_spacing: 0.,
            align: "center".into(),
            color: [65535; 4],
        }
        .into(),
    };
    let prepared = PreparedSource::new(&source, Some(font_owner), &pool, check)?;
    let area =
        Region::new([0, 0, SIZE[0] as i32, SIZE[1] as i32]).map_err(|error| error.to_string())?;
    let pixels = prepared.render_region(SIZE, area, 8, &pool, check)?;
    if pixels.bytes().chunks_exact(4).all(|pixel| pixel[3] == 0) {
        return Err("The selected face produced an empty sample".into());
    }
    Ok(pixels)
}

fn worker() -> Option<&'static mpsc::SyncSender<Request>> {
    static WORKER: OnceLock<Option<mpsc::SyncSender<Request>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (send, receive) = mpsc::sync_channel::<Request>(1);
            std::thread::Builder::new()
                .name("PeerBrush font samples".into())
                .spawn(move || {
                    while let Ok(request) = receive.recv() {
                        let mut check = || {
                            if request.reply.strong_count() == 0 {
                                return Err("Font sample cancelled".into());
                            }
                            Ok(())
                        };
                        if check().is_err() {
                            continue;
                        }
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            render(request.font, &mut check)
                        }))
                        .unwrap_or_else(|_| Err("This face could not render a sample".into()));
                        if let Some(reply) = request.reply.upgrade() {
                            *reply.lock().unwrap() = Some(result);
                            request.ctx.request_repaint();
                        }
                    }
                })
                .ok()?;
            Some(send)
        })
        .as_ref()
}

#[derive(Default)]
pub(super) struct Preview {
    owner: Option<egui::Id>,
    key: Option<(TextFont, u64)>,
    ready_at: f64,
    pending: Option<Arc<Reply>>,
    rendered: Option<Result<TextureHandle, String>>,
}

impl Preview {
    pub(super) fn close(&mut self, owner: egui::Id) {
        if self.owner == Some(owner) {
            self.pending = None;
            if self.rendered.as_ref().is_some_and(Result::is_err) {
                self.rendered = None;
            }
        }
    }

    pub(super) fn area(ui: &mut egui::Ui) -> egui::Response {
        ui.allocate_response(
            Vec2::new(SIZE[0] as f32, SIZE[1] as f32),
            egui::Sense::hover(),
        )
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        owner: egui::Id,
        area: egui::Response,
        font: &TextFont,
        generation: u64,
        browse: bool,
    ) {
        let now = ui.input(|input| input.time);
        if self.owner != Some(owner)
            || self
                .key
                .as_ref()
                .is_none_or(|(face, epoch)| face != font || *epoch != generation)
        {
            *self = Self {
                owner: Some(owner),
                key: Some((font.clone(), generation)),
                ready_at: now + if browse { 0.18 } else { 0. },
                ..Self::default()
            };
        }
        if self.rendered.is_none() && self.pending.is_none() {
            if now < self.ready_at {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(self.ready_at - now));
            } else if let Some(worker) = worker() {
                let reply = Arc::new(Mutex::new(None));
                let request = Request {
                    font: font.clone(),
                    reply: Arc::downgrade(&reply),
                    ctx: ui.ctx().clone(),
                };
                match worker.try_send(request) {
                    Ok(()) => self.pending = Some(reply),
                    Err(mpsc::TrySendError::Full(_)) => {
                        ui.ctx().request_repaint_after(Duration::from_millis(50))
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        self.rendered = Some(Err("Font sample worker is unavailable".into()))
                    }
                }
            } else {
                self.rendered = Some(Err("Font sample worker is unavailable".into()));
            }
        }
        if let Some(result) = self
            .pending
            .as_ref()
            .and_then(|reply| reply.try_lock().ok()?.take())
        {
            self.pending = None;
            if result.as_ref().is_err_and(|error| {
                *error == FontError::Busy.to_string() || *error == FontError::Loading.to_string()
            }) {
                self.ready_at = now + 0.05;
                ui.ctx().request_repaint_after(Duration::from_millis(50));
            } else {
                self.rendered = Some(result.map(|pixels| {
                    ui.ctx().load_texture(
                        "font sample",
                        egui::ColorImage::from_rgba_unmultiplied(
                            SIZE.map(|n| n as usize),
                            pixels.bytes(),
                        ),
                        egui::TextureOptions::LINEAR,
                    )
                }));
            }
        }
        area.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Image,
                ui.is_enabled(),
                format!("Sample of {}", font.label()),
            )
        });
        match &self.rendered {
            Some(Ok(texture)) => {
                ui.painter().image(
                    texture.id(),
                    area.rect,
                    Rect::from_min_max(Pos2::ZERO, egui::pos2(1., 1.)),
                    ui.visuals().text_color(),
                );
            }
            state => {
                let text = if state.is_some() {
                    "Font sample unavailable"
                } else {
                    "Rendering font sample…"
                };
                ui.painter().text(
                    area.rect.center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(12.),
                    ui.visuals().weak_text_color(),
                );
            }
        }
        if let Some(Err(error)) = &self.rendered {
            area.on_hover_text(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_render_distinct_glyphs_and_observe_cancellation() {
        let regular = render("regular".into(), &mut || Ok(())).unwrap();
        let semibold = render("semibold".into(), &mut || Ok(())).unwrap();
        assert_eq!(regular.bytes().len(), (SIZE[0] * SIZE[1] * 4) as usize);
        assert_ne!(regular.bytes(), semibold.bytes());
        let stopped = render("regular".into(), &mut || Err("sample cancelled".into()));
        assert_eq!(stopped.err().as_deref(), Some("sample cancelled"));
    }
}
