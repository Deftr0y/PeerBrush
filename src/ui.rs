use crate::icons::{self, Icon};
mod animation;
mod brush;
mod color;
mod effects;
mod layers;
mod liquify;
mod selection;
use crate::{controls, thumbnails};
use crate::{
    engine::Document,
    raster::Pixel,
    server::{self, Connection, Shared},
};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Stroke, TextureHandle, Vec2};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::Duration,
};

const ACCENT: Color32 = Color32::from_rgb(233, 84, 32);
const AI_BLUE: Color32 = Color32::from_rgb(90, 170, 255);
const MUTED: Color32 = Color32::from_rgb(188, 183, 189);
fn key_modifiers(input: &egui::InputState, key: egui::Key) -> egui::Modifiers {
    input
        .events
        .iter()
        .find_map(|event| match event {
            egui::Event::Key {
                key: k,
                physical_key,
                pressed: true,
                modifiers,
                ..
            } if *k == key || *physical_key == Some(key) => Some(*modifiers),
            _ => None,
        })
        .unwrap_or(input.modifiers)
}
fn command_key(input: &egui::InputState, key: egui::Key) -> bool {
    let mods = key_modifiers(input, key);
    (mods.command || mods.ctrl) && input.key_pressed(key)
}
fn paint_tool(painter: &egui::Painter, rect: Rect, tool: Tool, _color: Color32) {
    use crate::icons::{self, Icon};
    let icon = match tool {
        Tool::None => Icon::Cursor,
        Tool::Move => Icon::Move,
        Tool::Rotate => Icon::Rotate,
        Tool::Scale => Icon::Scale,
        Tool::Brush => Icon::Brush,
        Tool::Smudge => Icon::Smudge,
        Tool::Liquify => Icon::Liquify,
        Tool::Eraser => Icon::Eraser,
        Tool::Fill => Icon::Fill,
        Tool::Gradient => Icon::Gradient,
        Tool::Rectangle => Icon::Rectangle,
        Tool::Ellipse => Icon::Ellipse,
        Tool::Selection => Icon::Selection,
        Tool::Picker => Icon::Picker,
        Tool::Pan => Icon::Pan,
        Tool::SmartMask => Icon::Wand,
    };
    icons::paint(painter, rect, icon, true);
}
fn eye_button(ui: &mut egui::Ui, visible: bool) -> egui::Response {
    crate::icons::button(
        ui,
        if visible { Icon::Eye } else { Icon::EyeOff },
        "Show / hide layer",
    )
}
#[derive(PartialEq, Clone, Copy)]
enum Tool {
    None,
    Move,
    Rotate,
    Scale,
    Brush,
    Smudge,
    Liquify,
    Eraser,
    Fill,
    Rectangle,
    Ellipse,
    Selection,
    Picker,
    Pan,
    Gradient,
    SmartMask,
}
impl Tool {
    fn label(self) -> &'static str {
        match self {
            Self::None => "Select · Q",
            Self::Move => "Move · W",
            Self::Rotate => "Rotate · E",
            Self::Scale => "Scale · R",
            Self::Brush => "Brush · B",
            Self::Smudge => "Wet blend · U",
            Self::Liquify => "Liquify · L",
            Self::Eraser => "Eraser",
            Self::Fill => "Fill",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Selection => "Select",
            Self::Picker => "Pick color",
            Self::Pan => "Pan",
            Self::Gradient => "Gradient",
            Self::SmartMask => "Smart mask · K",
        }
    }
}
struct Preview {
    live: String,
    revision: u64,
    doc_id: String,
    w: u32,
    h: u32,
    bytes: Vec<u8>,
    dirty: Option<[u32; 4]>,
    selection: Option<[i32; 4]>,
    selection_polygon: Option<Vec<[f32; 2]>>,
    selection_coverage: Option<crate::selection::Coverage>,
    target: String,
    mask: bool,
}
struct CanvasSelection {
    document: String,
    revision: u64,
    bounds: Option<[i32; 4]>,
    polygon: Option<Vec<[f32; 2]>>,
    coverage: Option<crate::selection::Coverage>,
}
struct ConnectOutcome {
    summary: String,
    registrations: usize,
    retry: bool,
}
type ConnectAction = Arc<dyn Fn(&Path, &Path) -> Result<ConnectOutcome, String> + Send + Sync>;
struct RenameEdit {
    document: String,
    layer: String,
    original: String,
    text: String,
    revision: u64,
    focus: bool,
}
struct MergeReply {
    document: String,
    result: Result<Value, String>,
}
struct LayerDrag {
    gesture: String,
    id: String,
    revision: u64,
    offset: f32,
    target: usize,
    ids: Vec<String>,
    commands: Vec<Value>,
}
struct EyeSweep {
    gesture: String,
    previous: Pos2,
    revision: u64,
    visible: bool,
    ids: HashSet<String>,
}
pub struct PeerBrush {
    shared: Shared,
    connection: Connection,
    selected: String,
    selection_layers: HashSet<String>,
    selection_anchor: String,
    eye_sweep: Option<EyeSweep>,
    mask: bool,
    isolate: bool,
    tool: Tool,
    selection_kind: String,
    selection_mode: String,
    selection_feather: f32,
    selection_tolerance: f32,
    selection_contiguous: bool,
    selection_merged: bool,
    selection_width: f32,
    selection_path: Vec<[f32; 2]>,
    selection_gesture_mode: Option<String>,
    project_settings: Option<(u32, u32, u16)>,
    color: Pixel,
    mask_value: u8,
    radius: f32,
    size_drag: Option<(Pos2, f32)>,
    live_gesture: Option<String>,
    brush: crate::brush::Settings,
    liquify: liquify::Settings,
    liquify_effect: Option<String>,
    stroke_pressures: Vec<f32>,
    stroke_has_pressure: bool,
    current_pressure: Option<f32>,
    background: Pixel,
    mask_background: u8,
    show_brush: bool,
    brush_preview: Option<(String, TextureHandle)>,
    color_editor: Option<color::Editor>,
    layer_drag: Option<LayerDrag>,
    layer_rects: HashMap<String, Rect>,
    eye_rects: HashMap<String, Rect>,
    effect_add_rect: Option<Rect>,
    clipboard: crate::clipboard::Worker,
    zoom: f32,
    pan: Vec2,
    frame_pending: bool,
    logo: TextureHandle,
    wordmark: TextureHandle,
    texture: Option<TextureHandle>,
    canvas_pixels: Option<(u32, u32, Vec<u8>)>,
    canvas_selection: Option<CanvasSelection>,
    animation: animation::Animation,
    preview_rx: mpsc::Receiver<Preview>,
    preview_tx: mpsc::Sender<Preview>,
    pending: bool,
    preview_cache: Arc<std::sync::Mutex<crate::preview::Cache>>,
    last_preview: Option<(String, u64, String, bool)>,
    thumbs: HashMap<String, (u64, TextureHandle)>,
    thumb_worker: thumbnails::Worker,
    thumb_pending: HashMap<String, u64>,
    thumb_document: String,
    parameter_gesture: Option<(egui::Id, String)>,
    blend_hover: Option<(String, String)>,
    transient: Vec<Value>,
    mask_tolerance: f32,
    mask_contiguous: bool,
    smart_mask_mode: String,
    points: Vec<[f32; 2]>,
    drag_start: Option<[f32; 2]>,
    view_rect: Option<Rect>,
    message: String,
    job_rx: mpsc::Receiver<Result<String, String>>,
    job_tx: mpsc::Sender<Result<String, String>>,
    busy: bool,
    show_new: bool,
    new_width: u32,
    new_bit_depth: u16,
    new_height: u32,
    show_connection: bool,
    connection_codex: bool,
    connect_action: ConnectAction,
    connect_rx: mpsc::Receiver<Result<ConnectOutcome, String>>,
    connect_tx: mpsc::Sender<Result<ConnectOutcome, String>>,
    connecting: bool,
    connect_ready: bool,
    connect_retry: bool,
    connect_feedback: String,
    connect_button_rect: Option<Rect>,
    connection_settings_rect: Option<Rect>,
    merge_rx: mpsc::Receiver<MergeReply>,
    merge_tx: mpsc::Sender<MergeReply>,
    rename_edit: Option<RenameEdit>,
    name_click: Option<(String, Pos2, f64)>,
    layer_clipboard: bool,
    opacity_rect: Option<Rect>,
    blend_rect: Option<Rect>,
    color_tab_rect: Option<Rect>,
    mask_tab_rect: Option<Rect>,
    effect_area_rect: Option<Rect>,
    new_folder_rect: Option<Rect>,
    collapsed: HashSet<String>,
    mask_step: Option<String>,
    activity_text: String,
    activity_changed: std::time::Instant,
    gizmo_handle: u8,
    drag_revision: Option<u64>,
    gizmo_bounds: Option<(String, u64, [i32; 4])>,
}
impl PeerBrush {
    pub fn new(cc: &eframe::CreationContext<'_>, shared: Shared, connection: Connection) -> Self {
        if let Some(state) = &cc.wgpu_render_state {
            let _ = crate::gpu::install(state);
        }
        Self::init(&cc.egui_ctx, shared, connection)
    }
    fn init(ctx: &egui::Context, shared: Shared, connection: Connection) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        for (name, bytes) in [
            (
                "Ubuntu Sans",
                include_bytes!("../assets/fonts/UbuntuSans-Regular.ttf").as_slice(),
            ),
            (
                "Ubuntu Sans Medium",
                include_bytes!("../assets/fonts/UbuntuSans-Medium.ttf").as_slice(),
            ),
            (
                "Ubuntu Sans Semibold",
                include_bytes!("../assets/fonts/UbuntuSans-SemiBold.ttf").as_slice(),
            ),
        ] {
            fonts
                .font_data
                .insert(name.into(), egui::FontData::from_static(bytes).into());
        }
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "Ubuntu Sans".into());
        fonts.families.insert(
            egui::FontFamily::Name("medium".into()),
            vec!["Ubuntu Sans Medium".into()],
        );
        fonts.families.insert(
            egui::FontFamily::Name("semibold".into()),
            vec!["Ubuntu Sans Semibold".into()],
        );
        ctx.set_fonts(fonts);
        let image = image::load_from_memory(include_bytes!("../assets/peerbrush-logo.png"))
            .unwrap()
            .to_rgba8();
        let logo = ctx.load_texture(
            "PeerBrush mark",
            egui::ColorImage::from_rgba_unmultiplied(
                [image.width() as usize, image.height() as usize],
                image.as_raw(),
            ),
            egui::TextureOptions::LINEAR,
        );
        let img = image::load_from_memory(include_bytes!("../assets/peerbrush-wordmark.png"))
            .unwrap()
            .to_rgba8();
        let mut b = [img.width(), img.height(), 0, 0];
        for (x, y, p) in img.enumerate_pixels() {
            if p[3] > 180 {
                b = [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)];
            }
        }
        let img = image::imageops::crop_imm(&img, b[0], b[1], b[2] - b[0], b[3] - b[1]).to_image();
        let img = image::imageops::resize(
            &img,
            1024,
            (img.height() as f32 * 1024.0 / img.width() as f32).round() as u32,
            image::imageops::FilterType::Lanczos3,
        );
        let wordmark = ctx.load_texture(
            "PeerBrush wordmark",
            egui::ColorImage::from_rgba_unmultiplied(
                [img.width() as usize, img.height() as usize],
                img.as_raw(),
            ),
            egui::TextureOptions::LINEAR,
        );
        let mut v = egui::Visuals::dark();
        v.panel_fill = Color32::from_rgb(41, 39, 43);
        v.window_fill = Color32::from_rgb(51, 49, 54);
        v.extreme_bg_color = Color32::from_rgb(36, 34, 38);
        v.faint_bg_color = Color32::from_rgb(51, 49, 54);
        v.override_text_color = Some(Color32::from_rgb(245, 242, 240));
        v.selection.bg_fill = Color32::from_rgb(74, 48, 71);
        v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
        v.widgets.active.bg_fill = Color32::from_rgb(74, 48, 71);
        v.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
        v.widgets.active.fg_stroke = Stroke::new(1.5_f32, Color32::from_rgb(245, 242, 240));
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(61, 57, 64));
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, MUTED);
        v.widgets.hovered.bg_fill = Color32::from_rgb(75, 70, 78);
        v.widgets.inactive.bg_fill = Color32::from_rgb(64, 61, 67);
        v.widgets.inactive.bg_stroke = Stroke::new(0.7_f32, Color32::from_rgb(81, 76, 84));
        v.widgets.inactive.corner_radius = egui::CornerRadius::same(7);
        v.widgets.hovered.corner_radius = egui::CornerRadius::same(7);
        v.widgets.active.corner_radius = egui::CornerRadius::same(7);
        v.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(65, 61, 67));
        v.window_corner_radius = egui::CornerRadius::same(12);
        v.menu_corner_radius = egui::CornerRadius::same(9);
        ctx.set_visuals(v);
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.icon_width = 16.0;
        style.spacing.slider_width = 120.0;
        style.visuals.slider_trailing_fill = true;
        style.spacing.button_padding = Vec2::new(10.0, 6.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(20.0, egui::FontFamily::Name("semibold".into())),
        );
        ctx.set_style(style);
        let selected = shared
            .lock()
            .unwrap()
            .doc
            .layers
            .first()
            .map(|l| l.id.clone())
            .unwrap_or_default();
        let (preview_tx, preview_rx) = mpsc::channel();
        let (job_tx, job_rx) = mpsc::channel();
        let (connect_tx, connect_rx) = mpsc::channel();
        let (merge_tx, merge_rx) = mpsc::channel();
        Self {
            shared,
            connection,
            selection_layers: [selected.clone()].into_iter().collect(),
            selection_anchor: selected.clone(),
            eye_sweep: None,
            selected,
            mask: false,
            isolate: false,
            tool: Tool::Brush,
            selection_kind: "rectangle".into(),
            selection_mode: "replace".into(),
            selection_feather: 0.,
            selection_tolerance: 24.,
            selection_contiguous: true,
            selection_merged: true,
            selection_width: 12.,
            selection_path: vec![],
            selection_gesture_mode: None,
            project_settings: None,
            color: [233, 84, 32, 255],
            mask_value: 0,
            radius: 18.0,
            size_drag: None,
            live_gesture: None,
            brush: crate::brush::Settings::default(),
            liquify: liquify::Settings::default(),
            liquify_effect: None,
            stroke_pressures: vec![],
            stroke_has_pressure: false,
            current_pressure: None,
            background: [245, 242, 240, 255],
            mask_background: 255,
            show_brush: false,
            brush_preview: None,
            color_editor: None,
            layer_drag: None,
            layer_rects: HashMap::new(),
            eye_rects: HashMap::new(),
            effect_add_rect: None,
            clipboard: crate::clipboard::Worker::new(ctx.clone()),
            zoom: 1.0,
            pan: Vec2::ZERO,
            frame_pending: false,
            logo,
            wordmark,
            texture: None,
            canvas_pixels: None,
            canvas_selection: None,
            animation: animation::Animation::default(),
            preview_rx,
            preview_tx,
            pending: false,
            preview_cache: Arc::new(std::sync::Mutex::new(crate::preview::Cache::default())),
            last_preview: None,
            thumbs: HashMap::new(),
            thumb_worker: thumbnails::Worker::new(ctx.clone()),
            thumb_pending: HashMap::new(),
            thumb_document: String::new(),
            parameter_gesture: None,
            blend_hover: None,
            transient: vec![],
            mask_tolerance: 12.0,
            mask_contiguous: true,
            smart_mask_mode: "replace".into(),
            points: vec![],
            drag_start: None,
            view_rect: None,
            message: "Ready".into(),
            job_rx,
            job_tx,
            busy: false,
            show_new: false,
            new_width: 1024,
            new_bit_depth: 8,
            new_height: 768,
            show_connection: false,
            connection_codex: true,
            connect_action: Arc::new(|executable, state_dir| {
                crate::discovery::connect(executable, state_dir).map(|report| ConnectOutcome {
                    summary: report.summary(),
                    registrations: report.registered_count,
                    retry: report.needs_retry(),
                })
            }),
            connect_rx,
            connect_tx,
            connecting: false,
            connect_ready: false,
            connect_retry: false,
            connect_feedback: String::new(),
            connect_button_rect: None,
            connection_settings_rect: None,
            merge_rx,
            merge_tx,
            rename_edit: None,
            name_click: None,
            layer_clipboard: false,
            opacity_rect: None,
            blend_rect: None,
            color_tab_rect: None,
            mask_tab_rect: None,
            effect_area_rect: None,
            new_folder_rect: None,
            collapsed: HashSet::new(),
            mask_step: None,
            activity_text: String::new(),
            activity_changed: std::time::Instant::now(),
            gizmo_handle: 0,
            drag_revision: None,
            gizmo_bounds: None,
        }
    }
    fn edit(&mut self, commands: Vec<Value>, label: &str) {
        let result = self
            .shared
            .lock()
            .unwrap()
            .edit("human", &commands, None, None, label);
        match result {
            Ok(result) => {
                if let Some(id) = result["created"]
                    .as_array()
                    .and_then(|ids| ids.first())
                    .and_then(Value::as_str)
                {
                    self.selected = id.into();
                    self.selection_layers = [id.to_string()].into_iter().collect();
                    self.selection_anchor = id.into();
                    self.mask = false;
                    self.mask_step = None;
                }
                self.message = label.into();
                self.last_preview = None;
            }
            Err(e) => self.message = e,
        }
    }
    fn selected_layer_ids(&self, doc: &Document) -> Vec<String> {
        doc.layers
            .iter()
            .filter(|l| self.selection_layers.contains(&l.id))
            .map(|l| l.id.clone())
            .collect()
    }
    fn copy_request(
        &self,
        doc: &Document,
        cut: bool,
        merged: bool,
    ) -> Option<crate::clipboard::Request> {
        if self.layer_clipboard && (cut || !merged) {
            let ids = self.selected_layer_ids(doc);
            Some(if cut {
                crate::clipboard::Request::CutLayers {
                    shared: self.shared.clone(),
                    doc: doc.clone(),
                    ids,
                }
            } else {
                crate::clipboard::Request::CopyLayers {
                    doc: doc.clone(),
                    ids,
                }
            })
        } else if cut {
            None
        } else {
            Some(crate::clipboard::Request::Copy {
                doc: doc.clone(),
                target: self.selected.clone(),
                mask: self.mask,
                merged,
            })
        }
    }
    fn create_folder(&mut self, doc: &Document) {
        let ids = self.selected_layer_ids(doc);
        self.edit(
            vec![json!({"op":"group.create_selected","layers":ids,"name":"Folder"})],
            "Group selected layers",
        );
        self.layer_clipboard = true;
        self.collapsed.remove(&self.selected);
    }
    fn selection_points(&self, doc: &Document, rect: Rect, scale: f32) -> Vec<Pos2> {
        let preview = self
            .canvas_selection
            .as_ref()
            .filter(|p| p.document == doc.id && p.revision == doc.revision);
        let (bounds, polygon) = preview
            .map_or((doc.selection, doc.selection_polygon.as_ref()), |p| {
                (p.bounds, p.polygon.as_ref())
            });
        let Some(bounds) = bounds else {
            return vec![];
        };
        let corners;
        let points = if let Some(polygon) = polygon {
            polygon.as_slice()
        } else {
            corners = [
                [bounds[0] as f32, bounds[1] as f32],
                [bounds[2] as f32, bounds[1] as f32],
                [bounds[2] as f32, bounds[3] as f32],
                [bounds[0] as f32, bounds[3] as f32],
            ];
            &corners
        };
        points
            .iter()
            .map(|p| rect.min + Vec2::new(p[0] * scale, p[1] * scale))
            .collect()
    }
    fn duplicate_layers(&mut self, doc: &Document) {
        let ids = self.selected_layer_ids(doc);
        let result = {
            let mut e = self.shared.lock().unwrap();
            e.edit(
                "human",
                &[json!({"op":"layer.duplicate","layers":ids})],
                Some(doc.revision),
                None,
                "Duplicate layers",
            )
            .map(|result| {
                let created = result["created"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<HashSet<_>>();
                crate::tree::roots(&e.doc, &created)
            })
        };
        match result {
            Ok(ids) => {
                if let Some(id) = ids.first() {
                    self.select_content(id);
                }
                self.selection_layers = ids.into_iter().collect();
                self.layer_clipboard = true;
                self.message = "Duplicated layers".into();
                self.last_preview = None;
            }
            Err(e) => self.message = e,
        }
    }
    fn layer_cmd(&mut self, op: &str, extra: Value, label: &str) {
        if !self.selection_layers.contains(&self.selected) {
            self.selection_layers = [self.selected.clone()].into_iter().collect();
        }
        let mut c = extra;
        c["op"] = json!(op);
        c["layer"] = json!(self.selected);
        let commands = if [
            "layer.update",
            "layer.delete",
            "effect.add",
            "mask.add",
            "mask.remove",
            "mask.toggle",
            "fill",
            "paint.fill",
        ]
        .contains(&op)
        {
            let ids = if op == "layer.delete" {
                crate::tree::roots(&self.shared.lock().unwrap().doc, &self.selection_layers)
            } else {
                self.selection_layers.iter().cloned().collect()
            };
            ids.iter()
                .map(|id| {
                    let mut cmd = c.clone();
                    cmd["layer"] = json!(id);
                    cmd
                })
                .collect()
        } else {
            vec![c]
        };
        self.edit(commands, label);
    }
    fn layer_parameter(&mut self, extra: Value, label: &str, response: &egui::Response) {
        let down = response.is_pointer_button_down_on() || response.dragged();
        if down
            && self
                .parameter_gesture
                .as_ref()
                .is_none_or(|(id, _)| *id != response.id)
        {
            self.parameter_gesture = Some((response.id, crate::engine::id()));
        }
        let gesture = if down {
            self.parameter_gesture.as_ref().map(|(_, key)| key.as_str())
        } else {
            None
        };
        let mut command = extra;
        command["op"] = json!(if command.get("effect").is_some() {
            "effect.update"
        } else if command.get("step").is_some() {
            "mask.step.update"
        } else {
            "layer.update"
        });
        command["layer"] = json!(self.selected);
        let commands = if command["op"] == "layer.update" {
            self.selection_layers
                .iter()
                .map(|id| {
                    let mut c = command.clone();
                    c["layer"] = json!(id);
                    c
                })
                .collect::<Vec<_>>()
        } else {
            vec![command]
        };
        let result = self
            .shared
            .lock()
            .unwrap()
            .edit_with_gesture("human", &commands, None, None, label, gesture);
        self.message = result.map(|_| label.into()).unwrap_or_else(|e| e);
        self.last_preview = None;
    }
    fn job<F: FnOnce() -> Result<String, String> + Send + 'static>(&mut self, f: F) {
        if self.busy {
            return;
        }
        self.busy = true;
        let tx = self.job_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
    }
    fn connect_ai(&mut self, ctx: &egui::Context) {
        if self.connecting {
            return;
        }
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                self.receive_connection(Err(format!("Could not locate PeerBrush: {error}")));
                return;
            }
        };
        let state_dir: PathBuf = self.connection.state_dir.clone();
        let action = self.connect_action.clone();
        let tx = self.connect_tx.clone();
        let ctx = ctx.clone();
        self.connecting = true;
        self.connect_feedback = "Preparing MCP discovery and installed AI clients…".into();
        self.message = self.connect_feedback.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("peerbrush-ai-connect".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    action(&executable, &state_dir)
                }))
                .unwrap_or_else(|_| Err("MCP setup could not complete. Try again.".into()));
                let _ = tx.send(result);
                ctx.request_repaint();
            })
        {
            self.receive_connection(Err(format!("Could not start MCP setup: {error}")));
        }
    }
    fn receive_connection(&mut self, result: Result<ConnectOutcome, String>) {
        self.connecting = false;
        match result {
            Ok(report) => {
                self.connect_ready = report.registrations > 0;
                self.connect_retry = report.retry;
                self.connect_feedback = report.summary;
            }
            Err(error) => {
                self.connect_ready = false;
                self.connect_retry = true;
                self.connect_feedback = error;
            }
        }
        self.message = self
            .connect_feedback
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        // Client presence belongs to the live MCP handshake, never to a setup result.
    }
    fn merge_layers(&mut self, ctx: &egui::Context, doc: &Document) {
        if self.busy {
            return;
        }
        let mut ids = doc
            .layers
            .iter()
            .filter(|l| self.selection_layers.contains(&l.id))
            .map(|l| l.id.clone())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            ids.push(self.selected.clone());
        }
        let document = doc.id.clone();
        let revision = doc.revision;
        let shared = self.shared.clone();
        let tx = self.merge_tx.clone();
        let ctx = ctx.clone();
        self.busy = true;
        self.message = "Merging layers…".into();
        if let Err(error) = std::thread::Builder::new()
            .name("peerbrush-merge".into())
            .spawn(move || {
                let result = crate::merge::edit(&shared, "human", &ids, Some(revision), None, None);
                let _ = tx.send(MergeReply { document, result });
                ctx.request_repaint();
            })
        {
            self.busy = false;
            self.message = format!("Could not start merging layers: {error}");
        }
    }
    fn open(&mut self) {
        let dirty = {
            let e = self.shared.lock().unwrap();
            e.doc.revision != e.saved_revision
        };
        if dirty {
            self.message = "Save your work before opening another document".into();
            return;
        }
        let s = self.shared.clone();
        self.job(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Photoshop document", &["psd"])
                .pick_file()
            else {
                return Ok("Ready".into());
            };
            server::open(&s, &path)?;
            Ok("PSD opened".into())
        });
    }
    fn save(&mut self, save_as: bool) {
        let s = self.shared.clone();
        let current = if save_as {
            None
        } else {
            s.lock().unwrap().path.clone()
        };
        self.job(move || {
            let path = current.or_else(|| {
                rfd::FileDialog::new()
                    .add_filter("Photoshop document", &["psd"])
                    .set_file_name("Untitled.psd")
                    .save_file()
            });
            let Some(path) = path else {
                return Ok("Ready".into());
            };
            server::save(&s, &path)?;
            Ok("PSD saved".into())
        });
    }
    fn export(&mut self) {
        let s = self.shared.clone();
        self.job(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("PNG", &["png"])
                .set_file_name("PeerBrush.png")
                .save_file()
            else {
                return Ok("Ready".into());
            };
            server::export(&s, &path)?;
            Ok("PNG exported".into())
        });
    }
    fn import(&mut self) {
        let shared = self.shared.clone();
        self.job(move || {
            let Some(paths) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg"])
                .pick_files()
            else {
                return Ok("Ready".into());
            };
            let commands: Vec<Value> = paths
                .iter()
                .map(|path| json!({"op":"image.import","path":path}))
                .collect();
            shared
                .lock()
                .unwrap()
                .edit("human", &commands, None, None, "Import images")?;
            Ok(format!("Imported {} image layers", paths.len()))
        });
    }
    fn drop_files(&mut self, paths: Vec<std::path::PathBuf>) {
        if self.busy {
            self.message = "Finish the current file operation before dropping files".into();
            return;
        }
        let psds = paths
            .iter()
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("psd"))
            })
            .count();
        if psds > 0 && paths.len() != 1 {
            self.message =
                "Drop one PSD to open it, or several PNG/JPEG images to add layers".into();
            return;
        }
        let shared = self.shared.clone();
        if psds == 1 {
            let dirty = {
                let e = shared.lock().unwrap();
                e.doc.revision != e.saved_revision
            };
            if dirty {
                self.message = "Save your work before dropping another PSD".into();
                return;
            }
            self.job(move || {
                server::open(&shared, &paths[0])?;
                Ok("PSD opened".into())
            });
        } else if !paths.is_empty() {
            self.job(move || {
                let commands: Vec<Value> = paths
                    .iter()
                    .map(|p| json!({"op":"image.import","path":p}))
                    .collect();
                shared
                    .lock()
                    .unwrap()
                    .edit("human", &commands, None, None, "Drop images")?;
                Ok(format!("Added {} image layers", paths.len()))
            });
        }
    }
    fn liquify_command(&self, points: &[[f32; 2]]) -> Value {
        let mut command = liquify::command(&self.selected, points, self.radius, &self.liquify);
        command["mask"] = json!(self.mask);
        if let Some(effect) = &self.liquify_effect {
            command["effect"] = json!(effect);
        }
        command
    }
    fn activate_liquify(&mut self, effect: Option<String>) {
        self.tool = Tool::Liquify;
        self.mask = false;
        self.liquify_effect = effect;
        self.message =
            "Liquify: paint on the canvas · Choose Push, Expand, Pinch or Restore above".into();
    }
    fn preview_variant(&self) -> String {
        let target = if self.isolate {
            self.selected.as_str()
        } else {
            ""
        };
        let stroke = if [Tool::Brush, Tool::Eraser, Tool::Smudge].contains(&self.tool)
            && !self.points.is_empty()
        {
            let color = if self.mask {
                [
                    self.mask_value,
                    self.mask_value,
                    self.mask_value,
                    self.color[3],
                ]
            } else {
                self.color
            };
            Some(self.stroke_command(self.points.clone(), color))
        } else {
            None
        };
        let stroke = if self.tool == Tool::Liquify && !self.points.is_empty() {
            Some(self.liquify_command(&self.points))
        } else {
            stroke
        };
        format!(
            "{target};{:?};{};{}",
            self.blend_hover,
            stroke.map(|v| v.to_string()).unwrap_or_default(),
            serde_json::to_string(&self.transient).unwrap()
        )
    }
    fn live_key(&self) -> String {
        if let Some(sweep) = &self.eye_sweep {
            return format!("visibility:{}:{}", sweep.gesture, sweep.revision);
        }
        if let Some(drag) = &self.layer_drag {
            return format!("reorder:{}:{}", drag.gesture, drag.revision);
        }
        if !self.points.is_empty() || self.gizmo_handle != 0 {
            if let Some(id) = &self.live_gesture {
                return format!("{id}:{}:{}:{}", self.selected, self.mask, self.isolate);
            }
        }
        String::new()
    }
    fn set_canvas(&mut self, ctx: &egui::Context, w: u32, h: u32, bytes: Vec<u8>) {
        let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &bytes);
        if let Some(t) = &mut self.texture {
            t.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture = Some(ctx.load_texture("canvas", image, egui::TextureOptions::LINEAR));
        }
        self.canvas_pixels = Some((w, h, bytes));
    }
    fn set_canvas_dirty(
        &mut self,
        ctx: &egui::Context,
        w: u32,
        h: u32,
        bytes: Vec<u8>,
        dirty: Option<[u32; 4]>,
    ) {
        if let Some([left, top, right, bottom]) = dirty.filter(|b| b[0] < b[2] && b[1] < b[3]) {
            if self
                .canvas_pixels
                .as_ref()
                .is_some_and(|(cw, ch, _)| (*cw, *ch) == (w, h))
            {
                if let Some(texture) = &mut self.texture {
                    let mut cropped =
                        Vec::with_capacity(((right - left) * (bottom - top) * 4) as usize);
                    for y in top..bottom {
                        cropped.extend_from_slice(
                            &bytes[((y * w + left) * 4) as usize..((y * w + right) * 4) as usize],
                        );
                    }
                    texture.set_partial(
                        [left as usize, top as usize],
                        egui::ColorImage::from_rgba_unmultiplied(
                            [(right - left) as usize, (bottom - top) as usize],
                            &cropped,
                        ),
                        egui::TextureOptions::LINEAR,
                    );
                    self.canvas_pixels = Some((w, h, bytes));
                    return;
                }
            }
        }
        self.set_canvas(ctx, w, h, bytes);
    }
    fn request_preview(&mut self, ctx: &egui::Context, _doc: &Document) {
        let current = self.shared.lock().unwrap().doc.clone();
        let doc = &current;
        let time = ctx.input(|i| i.time);
        let edge = self
            .view_rect
            .map_or(1536, |r| {
                (r.width().max(r.height()) * ctx.pixels_per_point()).ceil() as u32
            })
            .clamp(512, 1536);
        let variant = format!(
            "{};{};{edge}",
            self.preview_variant(),
            self.animation.render_key(time)
        );
        let mut live = self.live_key();
        if live.is_empty() {
            let ai = self.animation.live_key();
            if !ai.is_empty() {
                live = format!("{ai}:{}:{}:{}", self.selected, self.mask, self.isolate);
            }
        }
        while let Ok(p) = self.preview_rx.try_recv() {
            self.pending = false;
            if p.doc_id == doc.id
                && p.revision == doc.revision
                && (p.target == variant || (!live.is_empty() && p.live == live))
                && p.mask == (self.mask && self.isolate)
            {
                if !p.bytes.is_empty() {
                    self.canvas_selection = Some(CanvasSelection {
                        document: p.doc_id.clone(),
                        revision: p.revision,
                        bounds: p.selection,
                        polygon: p.selection_polygon.clone(),
                        coverage: p.selection_coverage.clone(),
                    });
                    if !self
                        .animation
                        .fade_preview(self.canvas_pixels.as_ref(), &p, time)
                    {
                        self.set_canvas_dirty(ctx, p.w, p.h, p.bytes, p.dirty);
                    }
                }
                if p.revision != doc.revision {
                    self.last_preview = None;
                }
            } else {
                self.last_preview = None;
            }
        }
        if let Some((w, h, bytes)) = self.animation.fade_pixels(time) {
            self.set_canvas(ctx, w, h, bytes);
        }
        if self.animation.fading() || self.animation.progress(time) < 1.0 {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        let key = (
            doc.id.clone(),
            doc.revision,
            variant.clone(),
            self.mask && self.isolate,
        );
        if !self.pending && self.last_preview.as_ref() != Some(&key) {
            self.pending = true;
            self.last_preview = Some(key);
            let mut doc = self.animation.document(doc, time, true);
            let mut commands = self.transient.clone();
            if let Some((layer, blend)) = &self.blend_hover {
                commands.push(json!({"op":"layer.update","layer":layer,"blend":blend}));
            }
            if [Tool::Brush, Tool::Eraser, Tool::Smudge].contains(&self.tool)
                && !self.points.is_empty()
            {
                let color = if self.mask {
                    [
                        self.mask_value,
                        self.mask_value,
                        self.mask_value,
                        self.color[3],
                    ]
                } else {
                    self.color
                };
                commands.push(self.stroke_command(self.points.clone(), color));
            }
            if self.tool == Tool::Liquify && !self.points.is_empty() {
                commands.push(self.liquify_command(&self.points));
            }
            let dirty = if !live.is_empty()
                && commands.len() == 1
                && matches!(commands[0]["op"].as_str(), Some("paint" | "smudge"))
            {
                self.shared
                    .lock()
                    .unwrap()
                    .scopes(&commands[0])
                    .first()
                    .and_then(|scope| scope.rect)
            } else {
                None
            };
            let cache = self.preview_cache.clone();
            let tx = self.preview_tx.clone();
            let ctx = ctx.clone();
            let layer = if self.isolate {
                Some(self.selected.clone())
            } else {
                None
            };
            let mask = self.mask && self.isolate;
            std::thread::spawn(move || {
                let id = doc.id.clone();
                let revision = doc.revision;
                let output = (|| {
                    if !commands.is_empty() {
                        doc = cache
                            .lock()
                            .map_err(|_| "Preview worker cache unavailable")?
                            .edit(doc, &commands, &live)?;
                    }
                    let rendered = cache
                        .lock()
                        .map_err(|_| "Preview worker cache unavailable")?
                        .render(&doc, &live, dirty, edge, layer.as_deref(), mask)?;
                    Ok::<_, String>((
                        rendered.width,
                        rendered.height,
                        rendered.bytes,
                        rendered.dirty,
                        doc.selection,
                        doc.selection_polygon.clone(),
                        doc.selection_coverage.clone(),
                    ))
                })();
                let (w, h, bytes, dirty, selection, selection_polygon, selection_coverage) =
                    output.unwrap_or((0, 0, vec![], None, None, None, None));
                let _ = tx.send(Preview {
                    live,
                    doc_id: id,
                    revision,
                    target: variant,
                    mask,
                    w,
                    h,
                    bytes,
                    dirty,
                    selection,
                    selection_polygon,
                    selection_coverage,
                });
                ctx.request_repaint();
            });
        }
    }
    fn thumbnail(
        &mut self,
        _ui: &egui::Ui,
        doc: &Document,
        id: &str,
        mask: bool,
    ) -> Option<TextureHandle> {
        self.thumbnail_for(doc, id, mask, None)
    }
    fn thumbnail_for(
        &mut self,
        doc: &Document,
        id: &str,
        mask: bool,
        step: Option<usize>,
    ) -> Option<TextureHandle> {
        let suffix = step
            .and_then(|i| {
                doc.layers
                    .iter()
                    .find(|l| l.id == id)?
                    .mask
                    .as_ref()?
                    .steps
                    .get(i)
            })
            .map(|s| s.id.as_str())
            .unwrap_or("");
        let key = format!("{id}:{mask}:{suffix}");
        let ready = self.thumbs.get(&key).is_some_and(|t| t.0 == doc.revision);
        if !ready && self.thumb_pending.get(&key) != Some(&doc.revision) {
            let current = self.shared.lock().unwrap().doc.clone();
            // Controls may receive an interpolated presentation document. Thumbnails describe
            // committed source and must never populate its effect caches using intermediate values.
            let source = if current.id == doc.id && current.revision == doc.revision {
                current
            } else {
                doc.clone()
            };
            if self.thumb_worker.request(thumbnails::Request {
                key: key.clone(),
                document: source,
                layer: id.into(),
                mask,
                step,
            }) {
                self.thumb_pending.insert(key.clone(), doc.revision);
            }
        }
        self.thumbs.get(&key).map(|(_, texture)| texture.clone())
    }
    fn layer_menu(&mut self, ui: &mut egui::Ui, doc: &Document, l: &crate::engine::Layer) {
        if ui
            .add_enabled(
                !self.busy && !doc.read_only,
                egui::Button::new("Merge layers").shortcut_text(if cfg!(target_os = "macos") {
                    "⌘ E"
                } else {
                    "Ctrl+E"
                }),
            )
            .on_hover_text(
                "Merge selected layers · A single layer merges down · A folder merges its contents",
            )
            .clicked()
        {
            if !self.selection_layers.contains(&l.id) {
                self.select_content(&l.id);
            }
            self.merge_layers(ui.ctx(), doc);
            ui.close_menu();
        }
        ui.separator();
        for (title, op, extra) in [
            ("Rename", "rename", json!({})),
            ("Duplicate", "layer.duplicate", json!({})),
            (
                if l.locked { "Unlock" } else { "Lock" },
                "layer.update",
                json!({"locked":!l.locked}),
            ),
            ("Add mask", "mask.add", json!({"value":255})),
        ] {
            if ui
                .add_enabled(
                    !(op == "layer.duplicate" && l.kind == "group")
                        && !(op == "mask.add" && l.mask.is_some()),
                    egui::Button::new(title),
                )
                .clicked()
            {
                self.selected = l.id.clone();
                if op == "rename" {
                    self.begin_rename(doc, &l.id);
                } else {
                    self.layer_cmd(op, extra, title);
                }
                ui.close_menu();
            }
        }
        if l.mask.is_some() && ui.button("Remove mask").clicked() {
            self.selected = l.id.clone();
            self.layer_cmd("mask.remove", json!({}), "Remove mask");
            self.mask = false;
            ui.close_menu();
        }
        ui.separator();
        if l.parent.is_some() && ui.button("Move out of group").clicked() {
            self.selected = l.id.clone();
            self.layer_cmd("layer.parent", json!({"parent":null}), "Move out of group");
            ui.close_menu();
        }
        ui.menu_button("Move into group", |ui| {
            for group in doc
                .layers
                .iter()
                .filter(|g| g.kind == "group" && g.id != l.id)
            {
                if ui.button(&group.name).clicked() {
                    self.selected = l.id.clone();
                    self.layer_cmd(
                        "layer.parent",
                        json!({"parent":group.id}),
                        "Move into group",
                    );
                    ui.close_menu();
                }
            }
        });
        let doc = self.shared.lock().unwrap().doc.clone();
        if ui
            .button(if l.clip_to.is_some() {
                "Release clipping"
            } else {
                "Clip to layer below"
            })
            .clicked()
        {
            let base = if l.clip_to.is_some() {
                None
            } else {
                doc.layers
                    .iter()
                    .skip_while(|b| b.id != l.id)
                    .skip(1)
                    .find(|b| b.parent == l.parent)
                    .map(|b| b.clip_to.clone().unwrap_or_else(|| b.id.clone()))
            };
            self.selected = l.id.clone();
            self.layer_cmd("layer.clip", json!({"base":base}), "Layer clipping");
            ui.close_menu();
        }
        ui.separator();
        if ui
            .button(RichText::new("Delete layer").color(Color32::from_rgb(255, 154, 144)))
            .clicked()
        {
            self.selected = l.id.clone();
            self.layer_cmd("layer.delete", json!({}), "Delete layer");
            ui.close_menu();
        }
    }
    fn layer_footer(&mut self, ui: &mut egui::Ui, l: &crate::engine::Layer) {
        let mut blend = l.blend.clone();
        self.blend_hover = None;
        let combo = egui::ComboBox::from_id_salt("blend")
            .selected_text(
                crate::raster::BLENDS
                    .iter()
                    .find(|(mode, _)| *mode == blend)
                    .map(|(_, label)| *label)
                    .unwrap_or("Normal"),
            )
            .width(ui.available_width() - 10.0)
            .show_ui(ui, |ui| {
                for &(name, label) in crate::raster::BLENDS {
                    let response = ui.selectable_value(&mut blend, name.into(), label);
                    if response.hovered() {
                        self.blend_hover = Some((l.id.clone(), name.into()));
                    }
                    if response.clicked() {
                        self.blend_hover = None;
                        self.layer_cmd("layer.update", json!({"blend":blend}), "Layer blend");
                    }
                }
            });
        self.blend_rect = Some(combo.response.rect);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Opacity").size(12.0).color(MUTED));
            let mut percent = l.opacity * 100.0;
            let width = (ui.available_width() - 3.0).max(60.0);
            let response = controls::range(
                ui,
                ("opacity", &l.id),
                &mut percent,
                0.0..=100.0,
                width,
                "%",
                0,
                false,
            );
            self.opacity_rect = Some(response.rect);
            if response.changed() {
                self.layer_parameter(json!({"opacity":percent/100.0}), "Layer opacity", &response);
            }
        });
    }
    fn channel_tabs(&mut self, ui: &mut egui::Ui, has_mask: bool) {
        ui.spacing_mut().item_spacing.x = 0.0;
        let color = ui.add(
            egui::Button::new("Color")
                .selected(!self.mask)
                .corner_radius(egui::CornerRadius {
                    nw: 6,
                    sw: 6,
                    ne: 0,
                    se: 0,
                })
                .min_size(Vec2::new(64.0, 28.0)),
        );
        self.color_tab_rect = Some(color.rect);
        if color.clicked() {
            self.mask = false;
            self.last_preview = None;
        }
        let mask = ui.add_enabled(
            has_mask,
            egui::Button::new("Mask")
                .selected(self.mask)
                .corner_radius(egui::CornerRadius {
                    nw: 0,
                    sw: 0,
                    ne: 6,
                    se: 6,
                })
                .min_size(Vec2::new(64.0, 28.0)),
        );
        self.mask_tab_rect = Some(mask.rect);
        if mask.clicked() {
            self.mask = true;
            self.last_preview = None;
        }
        mask.on_hover_text(if has_mask {
            "Edit mask"
        } else {
            "Add a mask first"
        });
    }

    fn layers(&mut self, ui: &mut egui::Ui, doc: &Document) {
        ui.add_space(10.0);
        ui.horizontal(|ui|{ui.label(RichText::new("Layers").size(15.0).strong());ui.label(RichText::new(doc.layers.len().to_string()).size(11.0).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                let folder = icons::button(ui, Icon::Folder, "New folder · contains selected layers");
                self.new_folder_rect = Some(folder.rect);
                if folder.clicked() { self.create_folder(doc); }
                ui.menu_button("◐",|ui| {
                    for (kind,label) in [("color_balance","Color balance"),("hsl","Hue / saturation"),("levels","Levels"),("curves","Curves")] {
                        if ui.button(label).clicked() {
                            let ids:Vec<_>=doc.layers.iter().filter(|l|self.selection_layers.contains(&l.id)).map(|l|l.id.clone()).collect();
                            self.edit(vec![json!({"op":"adjustment.add","layers":ids,"kind":kind,"name":label})],"New adjustment");
                            let id=self.shared.lock().unwrap().doc.layers.iter().find(|l|l.kind=="adjustment" && l.name==label).map(|l|l.id.clone());if let Some(id)=id{self.select_content(&id);}
                            ui.close_menu();
                        }
                    }
                }).response.on_hover_text("Add clipped adjustment · multiple layers become a shared color group");
                if icons::button(ui, Icon::AddLayer, "New paint layer").clicked(){self.edit(vec![json!({"op":"layer.add","kind":"paint","name":format!("Paint {}",doc.layers.len()+1)})],"New paint layer");}
            });
        });
        ui.add_space(12.0);
        let selected = doc.layers.iter().find(|l| l.id == self.selected).cloned();
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        self.layer_list(ui, doc);
        ui.add_space(10.0);
        self.effect_add_rect = None;
        self.opacity_rect = None;
        self.blend_rect = None;
        self.color_tab_rect = None;
        self.mask_tab_rect = None;
        self.effect_area_rect = None;
        if let Some(l) = selected {
            let remaining = ui.available_rect_before_wrap();
            let stack_rect = Rect::from_min_max(
                remaining.min,
                egui::pos2(
                    remaining.right(),
                    (remaining.bottom() - 78.0).max(remaining.top() + 54.0),
                ),
            );
            self.effect_area_rect = Some(stack_rect);
            let mut stack_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("selected effects")
                    .max_rect(stack_rect),
            );
            stack_ui.set_clip_rect(stack_rect);
            egui::Frame::new()
                .fill(Color32::TRANSPARENT)
                .inner_margin(4)
                .show(&mut stack_ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        self.channel_tabs(ui, l.mask.is_some());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .selectable_label(self.isolate, "Solo")
                                .on_hover_text("Isolate content or mask")
                                .clicked()
                            {
                                self.isolate = !self.isolate;
                                self.last_preview = None;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);
                    if l.kind=="fill"&&!self.mask {
                        ui.horizontal(|ui| {controls::label(ui,"Fill color");
                            let response=ui.add_sized([28.0,24.0],egui::Button::new("").fill(Color32::from_rgba_unmultiplied(l.color[0],l.color[1],l.color[2],l.color[3])));
                            if response.clicked(){self.open_color(color::Target::Layer(l.id.clone()),l.color);}
                        });
                    }
                    if !self.mask {
                        self.color_stack(ui,&l);
                        if l.mask.is_none() {ui.horizontal(|ui| {
                            for (name,value) in [("Add Mask",255)] {
                                if ui.button(name).clicked(){self.layer_cmd("mask.add",json!({"value":value}),"Add mask");self.mask=true;}
                            }
                        });}
                    } else if let Some(m) = &l.mask {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("MASK STACK").size(10.0).color(MUTED)).on_hover_text("Bottom runs first · Top runs last");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let mut enabled = m.enabled;
                                    if ui.checkbox(&mut enabled, "Enabled").changed() {
                                        self.layer_cmd(
                                            "mask.toggle",
                                            json!({"enabled":enabled}),
                                            "Toggle mask",
                                        );
                                    }
                                },
                            );
                        });
                        let add = ui.menu_button("+ Add effect", |ui| {
                            for kind in ["paint", "fill", "invert", "levels", "blur", "curves", "gaussian", "adjust"] {
                                if ui.button(if kind=="blur"{"Feather"}else{effects::effect_name(kind)}).clicked() {
                                    self.layer_cmd(
                                        "mask.step.add",
                                        json!({"kind":kind}),
                                        "Add mask step",
                                    );
                                    ui.close_menu();
                                }
                            }
                        });
                        self.effect_add_rect = Some(add.response.rect);
                        egui::ScrollArea::vertical()
                            .id_salt("mask effects")
                            .max_height((ui.available_height() - 8.0).max(24.0))
                            .show(ui, |ui| {
                                for (index, step) in m.steps.iter().enumerate().rev() {
                                    let preview=self.thumbnail_for(doc,&l.id,true,Some(index));
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x=5.0;
                                        let mut enabled = step.enabled;
                                        if ui.checkbox(&mut enabled, "").changed() {
                                            self.layer_cmd(
                                                "mask.step.update",
                                                json!({"step":step.id,"enabled":enabled}),
                                                "Toggle mask step",
                                            );
                                        }
                                        if let Some(preview)=preview {
                                            if ui.add(egui::ImageButton::new((preview.id(),Vec2::splat(23.0))).frame(false)).on_hover_text("Result through this effect · click to target its paint step").clicked() {
                                                self.mask=true;
                                                self.mask_step=if step.kind=="paint"{Some(step.id.clone())}else{None};
                                            }
                                        } else {ui.allocate_space(Vec2::splat(23.0));}
                                        let name=if step.kind=="blur" {"Feather".into()} else {format!("{}{}",step.kind[..1].to_uppercase(),&step.kind[1..])};
                                        if ui
                                            .selectable_label(
                                                self.mask_step.as_deref() == Some(&step.id),
                                                RichText::new(name).size(12.0),
                                            )
                                            .clicked()
                                        {
                                            self.mask = true;
                                            self.mask_step = if step.kind == "paint" {
                                                Some(step.id.clone())
                                            } else {
                                                None
                                            };
                                            self.last_preview = None;
                                        }
                                        if ["fill","levels","blur"].contains(&step.kind.as_str()) {
                                            let mut value=step.value;
                                            let range=match step.kind.as_str(){"fill"=>0.0..=255.0,"blur"=>0.0..=64.0,_=>0.1..=5.0};
                                            let response=controls::range(ui,&step.id,&mut value,range,80.0,if step.kind=="blur"{" px"}else{""},if step.kind=="levels"{1}else{0},false);
                                            if response.changed() {self.layer_parameter(json!({"step":step.id,"value":value}),"Mask parameter",&response);}
                                        }
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if icons::small_button(
                                                    ui,
                                                    Icon::Trash,
                                                    "Remove mask effect",
                                                )
                                                .on_hover_text("Remove step")
                                                .clicked()
                                                {
                                                    self.layer_cmd(
                                                        "mask.step.delete",
                                                        json!({"step":step.id}),
                                                        "Remove mask step",
                                                    );
                                                    self.mask_step = None;
                                                }
                                                if index + 1 < m.steps.len()
                                                    && icons::small_button(
                                                        ui,
                                                        Icon::Up,
                                                        "Move mask effect up",
                                                    )
                                                    .on_hover_text("Move step up")
                                                    .clicked()
                                                {
                                                    self.layer_cmd(
                                                        "mask.step.reorder",
                                                        json!({"step":step.id,"index":index+1}),
                                                        "Reorder mask step",
                                                    );
                                                }
                                                if index > 0 && icons::small_button(ui, Icon::Down, "Move mask effect down").clicked() {
                                                    self.layer_cmd("mask.step.reorder", json!({"step":step.id,"index":index-1}), "Reorder mask step");
                                                }
                                            },
                                        );
                                    });
                                    if !step.settings.is_null() {
                                        ui.push_id((&step.id,"mask settings"),|ui| {
                                            let mut values=step.settings.clone();
                                            if let Some(response)=effects::settings(ui,&step.kind,&mut values) {
                                                self.layer_parameter(json!({"step":step.id,"settings":values}),"Mask effect parameter",&response);
                                            }
                                        });
                                    }
                                }
                            });
                    } else {
                        ui.label(
                            RichText::new("Reveal. Hide. Refine.")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.horizontal(|ui| {
                            if ui.button("Add Mask").clicked() {
                                self.layer_cmd("mask.add", json!({"value":255}), "Add mask");
                                self.mask = true;
                            }
                        });
                    }
                });
            ui.advance_cursor_after_rect(stack_rect);
            ui.add_space(5.0);
            ui.separator();
            self.layer_footer(ui, &l);
        }
    }
    fn gizmo(
        &mut self,
        ui: &mut egui::Ui,
        doc: &Document,
        rect: Rect,
        scale: f32,
        available: Rect,
    ) {
        let targets = doc
            .layers
            .iter()
            .filter(|l| self.selection_layers.contains(&l.id) && l.kind != "group")
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return;
        }
        let key = targets
            .iter()
            .map(|l| l.id.as_str())
            .collect::<Vec<_>>()
            .join(";");
        if self
            .gizmo_bounds
            .as_ref()
            .is_none_or(|(ids, rev, _)| *ids != key || *rev != doc.revision)
        {
            let b = targets
                .iter()
                .map(|l| {
                    let p = l.pixels.content_bounds().unwrap_or([
                        0,
                        0,
                        l.pixels.width as i32,
                        l.pixels.height as i32,
                    ]);
                    [p[0] + l.x, p[1] + l.y, p[2] + l.x, p[3] + l.y]
                })
                .reduce(|a, b| {
                    [
                        a[0].min(b[0]),
                        a[1].min(b[1]),
                        a[2].max(b[2]),
                        a[3].max(b[3]),
                    ]
                })
                .unwrap();
            self.gizmo_bounds = Some((key, doc.revision, b));
        }
        let b = self.gizmo_bounds.as_ref().unwrap().2;
        let b = doc.selection.unwrap_or(b);
        let pivot = [(b[0] + b[2]) as f32 / 2.0, (b[1] + b[3]) as f32 / 2.0];
        let screen = |p: [f32; 2]| rect.min + Vec2::new(p[0] * scale, p[1] * scale);
        let center = screen(pivot);
        let xend = center + Vec2::new(65.0, 0.0);
        let yend = center - Vec2::new(0.0, 65.0);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let hit = pointer
            .filter(|p| available.contains(*p))
            .map(|p| {
                if self.tool == Tool::Rotate {
                    if ((p - center).length() - 58.0).abs() < 10.0 {
                        3
                    } else {
                        0
                    }
                } else if (p - center).length() < 12.0 {
                    3
                } else if self.tool == Tool::Scale {
                    if (p - xend).length() < 12.0 {
                        1
                    } else if (p - yend).length() < 12.0 {
                        2
                    } else {
                        0
                    }
                } else if p.y > center.y - 9.0
                    && p.y < center.y + 9.0
                    && p.x > center.x + 12.0
                    && p.x < xend.x + 10.0
                {
                    1
                } else if p.x > center.x - 9.0
                    && p.x < center.x + 9.0
                    && p.y > yend.y - 10.0
                    && p.y < center.y - 12.0
                {
                    2
                } else {
                    0
                }
            })
            .unwrap_or(0);
        if hit != 0 {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if hit != 0
            && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
            && targets.iter().all(|l| !l.locked)
        {
            if let Some(pos) = pointer {
                self.gizmo_handle = hit;
                self.live_gesture = Some(crate::engine::id());
                self.drag_start =
                    Some([(pos.x - rect.left()) / scale, (pos.y - rect.top()) / scale]);
                self.drag_revision = Some(doc.revision);
            }
        }
        let mut dx = 0.0;
        let mut dy = 0.0;
        let mut angle = 0.0f32;
        let mut sx = 1.0f32;
        let mut sy = 1.0f32;
        if let (Some(a), Some(pos)) = (self.drag_start, pointer) {
            if self.gizmo_handle != 0 {
                let p = [(pos.x - rect.left()) / scale, (pos.y - rect.top()) / scale];
                if self.tool == Tool::Move {
                    dx = if self.gizmo_handle == 2 {
                        0.0
                    } else {
                        p[0] - a[0]
                    };
                    dy = if self.gizmo_handle == 1 {
                        0.0
                    } else {
                        p[1] - a[1]
                    };
                } else if self.tool == Tool::Rotate {
                    angle = ((p[1] - pivot[1]).atan2(p[0] - pivot[0])
                        - (a[1] - pivot[1]).atan2(a[0] - pivot[0]))
                    .to_degrees();
                    if ui.input(|i| i.modifiers.shift) {
                        angle = (angle / 15.0).round() * 15.0;
                    }
                } else {
                    let amount = (((pos.x - screen(a).x) - (pos.y - screen(a).y)) * 0.012)
                        .exp()
                        .clamp(0.05, 20.0);
                    if self.gizmo_handle != 2 {
                        sx = amount;
                    }
                    if self.gizmo_handle != 1 {
                        sy = amount;
                    }
                    if ui.input(|i| i.modifiers.shift) {
                        sx = amount;
                        sy = amount;
                    }
                }
            }
        }
        if self.gizmo_handle != 0 {
            for target in &targets {
                self.transient.push(if self.tool==Tool::Move {
                    json!({"op":"move","layer":target.id,"dx":dx.round(),"dy":dy.round(),"mask":self.mask,"step":if targets.len()==1 {self.mask_step.clone()}else{None},"selection_only":doc.selection.is_some()})
                } else {json!({"op":"transform","layer":target.id,"angle":angle,"scale_x":sx,"scale_y":sy,"pivot":pivot,"mask":self.mask,"step":if targets.len()==1 {self.mask_step.clone()}else{None},"selection_only":doc.selection.is_some()})});
            }
        }
        let painter = ui.painter().with_clip_rect(available);
        let (sin, cos) = angle.to_radians().sin_cos();
        let corners = [
            [b[0] as f32, b[1] as f32],
            [b[2] as f32, b[1] as f32],
            [b[2] as f32, b[3] as f32],
            [b[0] as f32, b[3] as f32],
        ];
        let transformed: Vec<Pos2> = corners
            .iter()
            .map(|p| {
                let x = (p[0] - pivot[0]) * sx;
                let y = (p[1] - pivot[1]) * sy;
                screen([
                    pivot[0] + x * cos - y * sin + dx,
                    pivot[1] + x * sin + y * cos + dy,
                ])
            })
            .collect();
        for i in 0..4 {
            painter.line_segment(
                [transformed[i], transformed[(i + 1) % 4]],
                Stroke::new(1.0_f32, ACCENT),
            );
        }
        if self.tool == Tool::Rotate {
            painter.circle_stroke(
                center,
                58.0,
                Stroke::new(
                    2.5_f32,
                    if hit != 0 {
                        ACCENT
                    } else {
                        Color32::from_rgb(235, 198, 109)
                    },
                ),
            );
        } else {
            let red = Color32::from_rgb(240, 110, 105);
            let green = Color32::from_rgb(119, 214, 141);
            if self.tool == Tool::Move {
                painter.arrow(center, xend - center, Stroke::new(2.5_f32, red));
                painter.arrow(center, yend - center, Stroke::new(2.5_f32, green));
            } else {
                painter.line_segment([center, xend], Stroke::new(2.0_f32, red));
                painter.line_segment([center, yend], Stroke::new(2.0_f32, green));
                painter.rect_filled(Rect::from_center_size(xend, Vec2::splat(11.0)), 1.0, red);
                painter.rect_filled(Rect::from_center_size(yend, Vec2::splat(11.0)), 1.0, green);
            }
            painter.rect_filled(
                Rect::from_center_size(center, Vec2::splat(13.0)),
                2.0,
                if hit == 3 { ACCENT } else { Color32::WHITE },
            );
        }
        if self.gizmo_handle != 0
            && ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary))
        {
            let commands = std::mem::take(&mut self.transient);
            let result = self.shared.lock().unwrap().edit(
                "human",
                &commands,
                self.drag_revision,
                None,
                self.tool.label(),
            );
            self.message = result
                .map(|_| "Transform applied · Undo to try again".into())
                .unwrap_or_else(|e| e);
            self.last_preview = None;
            self.drag_start = None;
            self.gizmo_handle = 0;
            self.drag_revision = None;
        }
    }
    fn pick_color(&mut self, doc: &Document, point: [f32; 2]) {
        let (x, y) = (point[0].floor() as i32, point[1].floor() as i32);
        if self.mask {
            if let Some(l) = doc.layers.iter().find(|l| l.id == self.selected) {
                self.mask_value = (l.mask_value_raw(x - l.x, y - l.y) * 255.0).round() as u8;
            }
        } else if let Ok((_, _, bytes, _)) = doc.preview(Some([x, y, x + 1, y + 1]), 1, None, false)
        {
            self.color.copy_from_slice(&bytes[..4]);
        }
    }
    fn canvas(&mut self, ui: &mut egui::Ui, doc: &Document) {
        self.transient.clear();
        if let Some(drag) = &self.layer_drag {
            self.transient.extend(drag.commands.clone());
        }
        if let Some(sweep) = &self.eye_sweep {
            self.transient.extend(
                sweep
                    .ids
                    .iter()
                    .map(|id| json!({"op":"layer.update","layer":id,"visible":sweep.visible})),
            );
        }
        let available = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(available, egui::Sense::click_and_drag());
        if response.hovered()
            && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
        {
            self.layer_clipboard = false;
        }
        let fit = ((available.width() - 60.0) / doc.width as f32)
            .min((available.height() - 60.0) / doc.height as f32)
            .max(0.01);
        if self.frame_pending {
            let target = doc.selection.filter(|r| r[2] > r[0] && r[3] > r[1]);
            if let Some(r) = target {
                let width = (r[2] - r[0]) as f32;
                let height = (r[3] - r[1]) as f32;
                // Ten percent breathing room, fitted in document coordinates.
                let scale =
                    (available.width() / (width * 1.12)).min(available.height() / (height * 1.12));
                self.zoom = (scale / fit).clamp(0.01, 8192.0);
                let center = Vec2::new(
                    (r[0] as f32 + r[2] as f32) * 0.5,
                    (r[1] as f32 + r[3] as f32) * 0.5,
                );
                self.pan = (Vec2::new(doc.width as f32, doc.height as f32) * 0.5 - center)
                    * fit
                    * self.zoom;
            } else {
                self.zoom = 1.0;
                self.pan = Vec2::ZERO;
            }
            self.frame_pending = false;
        }
        let scale = fit * self.zoom;
        let size = Vec2::new(doc.width as f32 * scale, doc.height as f32 * scale);
        let rect = Rect::from_center_size(available.center() + self.pan, size);
        self.view_rect = Some(rect);
        let painter = ui.painter().with_clip_rect(available);
        let visible = rect.intersect(available);
        let cell = 16.0;
        let cols = (visible.width() / cell).ceil() as i32;
        let rows = (visible.height() / cell).ceil() as i32;
        for y in 0..rows {
            for x in 0..cols {
                let min = visible.min + Vec2::new(x as f32 * cell, y as f32 * cell);
                let c = if (x + y) % 2 == 0 {
                    Color32::from_rgb(66, 63, 68)
                } else {
                    Color32::from_rgb(52, 50, 55)
                };
                painter.rect_filled(
                    Rect::from_min_size(min, Vec2::splat(cell)).intersect(visible),
                    0,
                    c,
                );
            }
        }
        if let Some(texture) = &self.texture {
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0_f32, Color32::from_rgb(90, 84, 93)),
            egui::StrokeKind::Outside,
        );
        let to_screen = |p: [f32; 2]| rect.min + Vec2::new(p[0] * scale, p[1] * scale);
        let to_doc = |p: Pos2| [(p.x - rect.min.x) / scale, (p.y - rect.min.y) / scale];
        let size_mode = [Tool::Brush, Tool::Eraser, Tool::Smudge, Tool::Liquify]
            .contains(&self.tool)
            && ui.input(|i| i.key_down(egui::Key::S) && !i.modifiers.command);
        if size_mode && (response.hovered() || self.size_drag.is_some()) {
            if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
                let (anchor, start) = *self.size_drag.get_or_insert((pointer, self.radius));
                self.radius = (start * ((pointer.x - anchor.x) * 0.012).exp()).clamp(0.5, 512.0);
                self.points.clear();
                self.stroke_pressures.clear();
                self.stroke_has_pressure = false;
                self.current_pressure = None;
                self.drag_start = None;
                if self.tool == Tool::Liquify {
                    liquify::cursor(&painter, pointer, self.radius, scale);
                } else {
                    self.tip_outline(&painter, pointer, scale);
                }
                painter.text(
                    pointer + Vec2::new(16.0, 20.0),
                    egui::Align2::LEFT_TOP,
                    format!("{} px", (self.radius * 2.0).round()),
                    egui::FontId::proportional(13.0),
                    Color32::WHITE,
                );
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            }
            return;
        } else if self.size_drag.take().is_some() {
            self.points.clear();
            self.stroke_pressures.clear();
            self.stroke_has_pressure = false;
            self.current_pressure = None;
            self.drag_start = None;
            return;
        }
        let coverage = self
            .canvas_selection
            .as_ref()
            .filter(|p| p.document == doc.id && p.revision == doc.revision)
            .and_then(|p| p.coverage.as_ref())
            .or(doc
                .selection_coverage
                .as_ref()
                .filter(|m| doc.selection == Some(m.bounds)));
        if let Some(coverage) = coverage {
            for contour in &coverage.contours {
                let points = contour.iter().map(|p| to_screen(*p)).collect::<Vec<_>>();
                selection::polygon(ui, &painter, &points);
            }
        } else {
            selection::polygon(ui, &painter, &self.selection_points(doc, rect, scale));
        }
        self.ai_canvas(ui, &painter, doc, rect, scale);
        for lease in &self.shared.lock().unwrap().leases {
            for scope in &lease.scopes {
                if lease.owner == "human" {
                    if let Some(r) = scope.rect {
                        let r = Rect::from_min_max(
                            to_screen([r[0] as f32, r[1] as f32]),
                            to_screen([r[2] as f32, r[3] as f32]),
                        );
                        painter.rect_stroke(
                            r,
                            3.0,
                            Stroke::new(2.0_f32, ACCENT),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let old = self.zoom;
                self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.01, 8192.0);
                if let Some(anchor) = ui.input(|i| i.pointer.hover_pos()) {
                    self.pan += (anchor - rect.center()) * (1.0 - self.zoom / old);
                }
            }
            if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                if rect.contains(p)
                    && ([Tool::Brush, Tool::Eraser, Tool::Smudge].contains(&self.tool))
                    && !(self.tool == Tool::Brush && ui.input(|i| i.modifiers.alt))
                {
                    self.tip_outline(&painter, p, scale);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                }
            }
        }
        if ui.input(|i| i.modifiers.alt) && response.dragged_by(egui::PointerButton::Secondary) {
            let delta = response.drag_delta();
            let old = self.zoom;
            self.zoom = (self.zoom * ((delta.x - delta.y) * 0.008).exp()).clamp(0.01, 8192.0);
            if let Some(anchor) = response.interact_pointer_pos() {
                self.pan += (anchor - rect.center()) * (1.0 - self.zoom / old);
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
            return;
        }
        if response.dragged_by(egui::PointerButton::Middle)
            || ((self.tool == Tool::Pan || ui.input(|i| i.key_down(egui::Key::Space)))
                && response.dragged())
        {
            self.pan += response.drag_delta();
            return;
        }
        if self.tool == Tool::Brush && ui.input(|i| i.modifiers.alt) {
            if !self.points.is_empty() {
                let color = if self.mask {
                    [
                        self.mask_value,
                        self.mask_value,
                        self.mask_value,
                        self.color[3],
                    ]
                } else {
                    self.color
                };
                self.paint_command(self.points.clone(), color, "Brush stroke");
                self.points.clear();
                self.stroke_pressures.clear();
                self.stroke_has_pressure = false;
                self.current_pressure = None;
                self.live_gesture = None;
            }
            if response.hovered() {
                if let Some(pointer) = ui
                    .input(|i| i.pointer.hover_pos())
                    .filter(|p| rect.contains(*p))
                {
                    // White over black remains legible over both light and dark pixels.
                    for width in [3.0_f32, 1.0] {
                        let color = if width == 3.0 {
                            Color32::BLACK
                        } else {
                            Color32::WHITE
                        };
                        for delta in [Vec2::new(7.0, 0.0), Vec2::new(0.0, 7.0)] {
                            painter.line_segment(
                                [pointer - delta, pointer + delta],
                                Stroke::new(width, color),
                            );
                        }
                    }
                    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                    if ui.input(|i| i.pointer.primary_down()) || response.clicked() {
                        let current = self.shared.lock().unwrap().doc.clone();
                        self.pick_color(&current, to_doc(pointer));
                    }
                }
            }
            self.drag_start = None;
            return;
        }
        if self.tool == Tool::Selection {
            self.selection_canvas(ui, doc, &response, &painter, rect, scale);
            return;
        }
        if [Tool::Move, Tool::Rotate, Tool::Scale].contains(&self.tool) {
            self.gizmo(ui, doc, rect, scale, available);
            return;
        }
        if self.tool == Tool::Liquify && response.hovered() {
            if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
                liquify::cursor(&painter, pointer, self.radius, scale);
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            }
        }
        if let Some(force) = Self::pointer_pressure(ui) {
            self.current_pressure = Some(force);
        }
        if ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Touch {
                        phase: egui::TouchPhase::End | egui::TouchPhase::Cancel,
                        ..
                    }
                )
            })
        }) {
            self.current_pressure = None;
        }
        if response.drag_started() {
            if let Some(pos) = ui
                .input(|i| i.pointer.press_origin())
                .or(response.interact_pointer_pos())
            {
                if rect.contains(pos) {
                    let point = to_doc(pos);
                    self.drag_start = Some(point);
                    self.points = vec![point];
                    self.stroke_pressures = vec![self.current_pressure.unwrap_or(1.)];
                    self.stroke_has_pressure = self.current_pressure.is_some();
                    self.live_gesture = Some(crate::engine::id());
                }
            }
        }
        if response.dragged() && self.drag_start.is_some() {
            if let Some(pos) = response.interact_pointer_pos() {
                let p = to_doc(pos);
                if self
                    .points
                    .last()
                    .map(|v| (v[0] - p[0]).abs() + (v[1] - p[1]).abs() > 0.5)
                    .unwrap_or(true)
                {
                    self.points.push(p);
                    self.stroke_pressures
                        .push(self.current_pressure.unwrap_or(1.));
                    self.stroke_has_pressure |= self.current_pressure.is_some();
                }
            }
        }
        if !self.points.is_empty() {
            if ![Tool::Brush, Tool::Eraser, Tool::Smudge, Tool::Liquify].contains(&self.tool) {
                if let (Some(a), Some(b)) = (self.drag_start, self.points.last()) {
                    let r = Rect::from_two_pos(to_screen(a), to_screen(*b));
                    if self.tool == Tool::Selection {
                        selection::outline(ui, &painter, r);
                    } else {
                        painter.rect_stroke(
                            r,
                            0.0,
                            Stroke::new(1.0_f32, ACCENT),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }
        }
        if [Tool::Rectangle, Tool::Ellipse, Tool::Gradient].contains(&self.tool) {
            if let (Some(a), Some(b)) = (self.drag_start, self.points.last()) {
                let area = [
                    a[0].min(b[0]) as i32,
                    a[1].min(b[1]) as i32,
                    a[0].max(b[0]) as i32,
                    a[1].max(b[1]) as i32,
                ];
                self.transient.push(json!({"op":if self.tool==Tool::Gradient{"gradient"}else{"shape"},"layer":self.selected,"kind":if self.tool==Tool::Ellipse{"ellipse"}else{"rectangle"},"rect":area,"color":if self.mask{[self.mask_value,self.mask_value,self.mask_value,self.color[3]]}else{self.color},"mask":self.mask,"step":self.mask_step}));
            }
        }
        if response.drag_stopped() {
            self.transient.clear();
            if let (Some(a), Some(b)) = (self.drag_start, self.points.last().copied()) {
                let area = [
                    a[0].min(b[0]) as i32,
                    a[1].min(b[1]) as i32,
                    a[0].max(b[0]) as i32,
                    a[1].max(b[1]) as i32,
                ];
                match self.tool{
            Tool::Brush|Tool::Eraser|Tool::Smudge=>{let c=if self.mask{[self.mask_value,self.mask_value,self.mask_value,self.color[3]]}else{self.color};self.paint_command(self.points.clone(),c,"Brush stroke");},
            Tool::Liquify=>{let command=self.liquify_command(&self.points);self.edit(vec![command],"Liquify stroke");},
            Tool::Move=>self.layer_cmd("move",json!({"dx":(b[0]-a[0]) as i32,"dy":(b[1]-a[1]) as i32}),"Move layer"),
            Tool::Rectangle|Tool::Ellipse=>self.layer_cmd("shape",json!({"kind":if self.tool==Tool::Ellipse{"ellipse"}else{"rectangle"},"rect":area,"color":if self.mask{[self.mask_value,self.mask_value,self.mask_value,self.color[3]]}else{self.color},"mask":self.mask,"step":self.mask_step}),"Draw shape"),
            Tool::Selection=>self.edit(vec![json!({"op":"selection","rect":area})],"Select region"),
            Tool::Gradient=>self.layer_cmd("gradient",json!({"rect":area,"color":if self.mask{[self.mask_value,self.mask_value,self.mask_value,self.color[3]]}else{self.color},"mask":self.mask,"step":self.mask_step}),"Draw gradient"),_=>{}
        }
            }
            self.points.clear();
            self.stroke_pressures.clear();
            self.stroke_has_pressure = false;
            self.current_pressure = None;
            self.drag_start = None;
        }
        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                if rect.contains(pos) {
                    let p = to_doc(pos);
                    match self.tool {
                        Tool::Brush | Tool::Eraser | Tool::Smudge => {
                            let c = if self.mask {
                                [
                                    self.mask_value,
                                    self.mask_value,
                                    self.mask_value,
                                    self.color[3],
                                ]
                            } else {
                                self.color
                            };
                            self.stroke_pressures=vec![self.current_pressure.unwrap_or(1.)];self.stroke_has_pressure=self.current_pressure.is_some();
                            self.paint_command(vec![p],c,"Brush dot");
                        }
                        Tool::Liquify => {self.edit(vec![self.liquify_command(&[p])],"Liquify dab");}
                        Tool::Fill => {
                            self.layer_cmd("paint.fill", json!({"color":if self.mask{[self.mask_value,self.mask_value,self.mask_value,self.color[3]]}else{self.color},"mask":self.mask,"step":self.mask_step}), "Fill layer")
                        }
                        Tool::SmartMask => {
                            self.layer_cmd("mask.from_color",json!({"point":[p[0] as i32,p[1] as i32],"tolerance":self.mask_tolerance/100.0,"contiguous":self.mask_contiguous,"mode":self.smart_mask_mode}),"Smart mask");
                            self.mask=true;self.mask_step=None;
                        }
                        Tool::Picker => self.pick_color(doc, p),
                        _ => {}
                    }
                }
            }
        }
    }
}
impl PeerBrush {
    fn draw(&mut self, ctx: &egui::Context) {
        let panel = self.shared.lock().unwrap().capture_panel.take();
        if let Some(panel) = panel {
            self.show_brush = panel == "brush";
            if panel == "color" {
                self.open_color(color::Target::Foreground, self.color);
            } else {
                self.color_editor = None;
            }
        }
        {
            let mut e = self.shared.lock().unwrap();
            if e.focus_requested {
                e.focus_requested = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Screenshot {
                user_data, image, ..
            } = event
            {
                if let Some(path) = user_data
                    .data
                    .as_ref()
                    .and_then(|d| d.downcast_ref::<std::path::PathBuf>())
                {
                    let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                    if let Ok(png) =
                        crate::raster::png(image.size[0] as u32, image.size[1] as u32, &bytes)
                    {
                        let _ = server::atomic_write(path, &png);
                    }
                }
            }
        }
        if !self.pending {
            if let Some(path) = self.shared.lock().unwrap().capture_ui.take() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(path)));
            }
        }
        while let Ok(result) = self.connect_rx.try_recv() {
            self.receive_connection(result);
        }
        while let Ok(reply) = self.merge_rx.try_recv() {
            self.busy = false;
            match reply.result {
                Ok(result) => {
                    let created = result["created"]
                        .as_array()
                        .and_then(|ids| ids.first())
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if let Some(id) = created {
                        let still_present = {
                            let e = self.shared.lock().unwrap();
                            e.doc.id == reply.document && e.doc.layers.iter().any(|l| l.id == id)
                        };
                        if still_present {
                            self.select_content(&id);
                        }
                    }
                    self.message = "Merged layers".into();
                    self.last_preview = None;
                }
                Err(error) => {
                    self.message = error;
                }
            }
        }
        if self.connecting {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        while let Ok(reply) = self.clipboard.replies.try_recv() {
            if !reply.selected_layers.is_empty() {
                self.select_content(&reply.selected_layers[0]);
                self.selection_layers = reply.selected_layers.into_iter().collect();
                self.layer_clipboard = true;
            } else if let Some(id) = reply.selected {
                self.select_content(&id);
                self.layer_clipboard = false;
            }
            self.message = reply.result.unwrap_or_else(|e| e);
            self.last_preview = None;
        }
        while let Ok(result) = self.job_rx.try_recv() {
            self.busy = false;
            if result
                .as_ref()
                .is_ok_and(|msg| msg.starts_with("Imported ") || msg.starts_with("Added "))
            {
                if let Some(l) = self.shared.lock().unwrap().doc.layers.first() {
                    self.selected = l.id.clone();
                    self.selection_layers = [l.id.clone()].into_iter().collect();
                    self.selection_anchor = l.id.clone();
                    self.mask = false;
                    self.mask_step = None;
                }
            }
            self.message = result.unwrap_or_else(|e| e);
            self.last_preview = None;
        }
        let (doc, ai_change) = {
            let mut e = self.shared.lock().unwrap();
            e.expire();
            (e.doc.clone(), e.ai_change.clone())
        };
        if self.rename_edit.as_ref().is_some_and(|edit| {
            edit.document != doc.id || !doc.layers.iter().any(|l| l.id == edit.layer)
        }) {
            self.rename_edit = None;
        }
        let time = ctx.input(|i| i.time);
        if self.animation.observe(&doc, ai_change, time) {
            // Reveal the destination so an AI hierarchy move remains visible.
            for scope in self.animation.scopes(time) {
                let mut parent = scope.target.and_then(|id| {
                    doc.layers
                        .iter()
                        .find(|l| l.id == id)
                        .and_then(|l| l.parent.clone())
                });
                for _ in 0..16 {
                    let Some(id) = parent else { break };
                    self.collapsed.remove(&id);
                    parent = doc
                        .layers
                        .iter()
                        .find(|l| l.id == id)
                        .and_then(|l| l.parent.clone());
                }
            }
            ctx.request_repaint();
        }
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.animation.cancel();
        }
        if !ctx.input(|i| i.pointer.any_down()) {
            self.parameter_gesture = None;
        }
        self.thumb_worker.document(&doc);
        if self.thumb_document != doc.id {
            self.thumbs.clear();
            self.thumb_pending.clear();
            self.thumb_document = doc.id.clone();
        }
        while let Ok(reply) = self.thumb_worker.replies.try_recv() {
            if self.thumb_pending.get(&reply.key) == Some(&reply.revision) {
                self.thumb_pending.remove(&reply.key);
            }
            if reply.document == doc.id && reply.revision == doc.revision {
                if let Some((w, h, bytes)) = reply.pixels {
                    let image =
                        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &bytes);
                    if let Some((revision, texture)) = self.thumbs.get_mut(&reply.key) {
                        texture.set(image, egui::TextureOptions::LINEAR);
                        *revision = doc.revision;
                    } else {
                        self.thumbs.insert(
                            reply.key.clone(),
                            (
                                doc.revision,
                                ctx.load_texture(reply.key, image, egui::TextureOptions::LINEAR),
                            ),
                        );
                    }
                }
            }
        }
        self.selection_layers
            .retain(|id| doc.layers.iter().any(|l| l.id == *id));
        if self.selection_layers.is_empty() && doc.layers.iter().any(|l| l.id == self.selected) {
            self.selection_layers.insert(self.selected.clone());
        }
        if !doc.layers.iter().any(|l| l.id == self.selected) {
            self.selected = doc.layers.first().map(|l| l.id.clone()).unwrap_or_default();
            self.mask = false;
            self.selection_layers = [self.selected.clone()].into_iter().collect();
            self.selection_anchor = self.selected.clone();
            self.thumbs.clear();
        }
        let dropped = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect::<Vec<_>>()
        });
        if !dropped.is_empty() {
            self.drop_files(dropped);
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("file-drop"),
            ));
            let bounds = ctx.screen_rect().shrink(8.0);
            painter.rect_stroke(
                bounds,
                8.0,
                Stroke::new(3.0_f32, ACCENT),
                egui::StrokeKind::Inside,
            );
            painter.text(
                bounds.center(),
                egui::Align2::CENTER_CENTER,
                "Drop PSD to open · Drop images to add layers",
                egui::FontId::proportional(23.0),
                Color32::WHITE,
            );
        }
        let typing = self.rename_edit.is_some()
            || ctx
                .memory(|m| m.focused())
                .is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some());
        if !typing {
            let (copy, cut, paste, merged) = ctx.input(|i| {
                (
                    i.events.iter().any(|e| matches!(e, egui::Event::Copy))
                        || (command_key(i, egui::Key::C)),
                    i.events.iter().any(|e| matches!(e, egui::Event::Cut))
                        || (command_key(i, egui::Key::X)),
                    i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))
                        || (command_key(i, egui::Key::V)),
                    i.modifiers.shift,
                )
            });
            if copy || cut {
                if let Some(request) = self.copy_request(&doc, cut, merged) {
                    self.message = self
                        .clipboard
                        .request(request)
                        .map(|_| {
                            if cut {
                                "Cutting layers…".into()
                            } else if self.layer_clipboard && !merged {
                                "Copying layers…".into()
                            } else {
                                "Copying selection…".into()
                            }
                        })
                        .unwrap_or_else(|e| e);
                }
            }
            if paste {
                self.message = self
                    .clipboard
                    .request(crate::clipboard::Request::Paste {
                        shared: self.shared.clone(),
                        target: self.selected.clone(),
                        revision: doc.revision,
                    })
                    .map(|_| "Pasting image…".into())
                    .unwrap_or_else(|e| e);
            }
            ctx.input(|i| {
                if i.modifiers.alt && !i.modifiers.command && (i.key_pressed(egui::Key::B) || i.key_pressed(egui::Key::Backspace)) {
                    let color=if self.mask {[self.mask_value,self.mask_value,self.mask_value,255]}else{self.color};
                    self.layer_cmd("paint.fill",json!({"color":color,"mask":self.mask,"step":self.mask_step}),"Fill foreground");
                }
                if command_key(i,egui::Key::Z) {
                    let r = if key_modifiers(i,egui::Key::Z).shift {
                        self.shared.lock().unwrap().redo("human")
                    } else {
                        self.shared.lock().unwrap().undo("human")
                    };
                    if let Err(e) = r {
                        self.message = e;
                    }
                    self.last_preview = None;
                }
                if command_key(i,egui::Key::S) {
                    self.save(key_modifiers(i,egui::Key::S).shift);
                }
                if command_key(i,egui::Key::O) {
                    self.open();
                }
                if command_key(i,egui::Key::E) {
                    self.merge_layers(ctx, &doc);
                }
                if command_key(i,egui::Key::D) {
                    self.edit(vec![json!({"op":if key_modifiers(i,egui::Key::D).shift{"selection.reselect"}else{"selection.clear"}})],if key_modifiers(i,egui::Key::D).shift{"Reselect"}else{"Deselect"});
                }
                if command_key(i,egui::Key::A) {self.edit(vec![json!({"op":"selection","kind":"rectangle","rect":[0,0,doc.width,doc.height]})],"Select all");}
                if command_key(i,egui::Key::J) && self.layer_clipboard {self.duplicate_layers(&doc);}
                for (key, tool) in [
                    (egui::Key::B, Tool::Brush),
                    (egui::Key::U, Tool::Smudge),
                    (egui::Key::L, Tool::Liquify),
                    (egui::Key::D, Tool::Eraser),
                    (egui::Key::Q, Tool::None),
                    (egui::Key::W, Tool::Move),
                    (egui::Key::E, Tool::Rotate),
                    (egui::Key::R, Tool::Scale),
                    (egui::Key::G, Tool::Fill),
                    (egui::Key::I, Tool::Picker),
                    (egui::Key::H, Tool::Pan),
                    (egui::Key::M, Tool::Selection),
                    (egui::Key::K, Tool::SmartMask),
                ] {
                    if !key_modifiers(i,key).command && !key_modifiers(i,key).ctrl && !key_modifiers(i,key).alt && (i.key_pressed(key)
                        || i.events.iter().any(|e| matches!(e, egui::Event::Key { physical_key: Some(k), pressed: true, .. } if *k == key))) {
                        self.tool = tool;
                        self.drag_start = None;
                        self.points.clear();self.stroke_pressures.clear();self.stroke_has_pressure=false;self.current_pressure=None;
                        self.gizmo_handle = 0;
                    }
                }
                if !i.modifiers.command && i.key_pressed(egui::Key::F) {
                    self.frame_pending = true;
                }
                if i.key_pressed(egui::Key::Escape) {
                    self.points.clear();self.stroke_pressures.clear();self.stroke_has_pressure=false;self.current_pressure=None;
                    self.drag_start = None;
                    self.gizmo_handle = 0;
                }
                if i.key_pressed(egui::Key::X) && !i.modifiers.command {
                    self.swap_colors();
                }
                if !i.modifiers.command && [Tool::Brush, Tool::Eraser, Tool::Smudge, Tool::Liquify].contains(&self.tool) {
                    let smaller = i.key_pressed(egui::Key::OpenBracket)
                        || i.events
                            .iter()
                            .any(|e| matches!(e,egui::Event::Text(t) if t=="{"));
                    let larger = i.key_pressed(egui::Key::CloseBracket)
                        || i.events
                            .iter()
                            .any(|e| matches!(e,egui::Event::Text(t) if t=="}"));
                    if smaller || larger {
                        self.radius =
                            (self.radius * if larger { 1.1 } else { 1.0 / 1.1 }).clamp(0.5, 512.0);
                        self.message = format!("Brush size {} px", (self.radius * 2.0).round());
                    }
                }
                if i.key_pressed(egui::Key::Escape) {
                    self.layer_drag = None;
                    self.eye_sweep = None;
                }
            });
        }
        let (connected, activity) = {
            let e = self.shared.lock().unwrap();
            (!e.mcp_clients.is_empty(), e.activity.clone())
        };
        let activity = activity.split_whitespace().collect::<Vec<_>>().join(" ");
        let activity = if activity.is_empty() {
            if connected {
                "Connected. Ready to create together".into()
            } else {
                "You and your AI. Same canvas.".into()
            }
        } else {
            activity
        };
        if activity != self.activity_text {
            self.activity_text = activity;
            self.activity_changed = std::time::Instant::now();
        }
        egui::TopBottomPanel::top("header")
            .exact_height(54.0)
            .show(ctx, |ui| {
                let bounds = ui.max_rect();
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(5.0);
                    ui.add(egui::Image::new((self.logo.id(), Vec2::splat(42.0))));
                    ui.add_space(9.0);
                    let aspect=self.wordmark.size_vec2().x/self.wordmark.size_vec2().y;
                    ui.add(egui::Image::new((self.wordmark.id(),Vec2::new(136.0,136.0/aspect))))
                        .on_hover_text("PeerBrush · You and your AI. Same canvas.");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let settings = icons::small_button(ui, Icon::Adjust, "AI connection settings");
                        self.connection_settings_rect = Some(settings.rect);
                        if settings.clicked() { self.show_connection = true; }
                        let title = if self.connecting {
                            "Connecting…"
                        } else if self.connect_retry {
                            "Retry AI"
                        } else if self.connect_ready {
                            "MCP ready"
                        } else {
                            "Connect AI"
                        };
                        let button = ui.add_enabled(!self.connecting, egui::Button::new(title).min_size(Vec2::new(110.0, 28.0)).fill(Color32::from_rgb(74, 48, 71)));
                        self.connect_button_rect = Some(button.rect);
                        if button.clicked() { self.connect_ai(ui.ctx()); }
                        if self.connecting {
                            let spinner = Rect::from_center_size(button.rect.left_center() + Vec2::new(11.0, 0.0), Vec2::splat(10.0));
                            egui::Spinner::new().size(10.0).color(ACCENT).paint_at(ui, spinner);
                        }
                        button.on_hover_text(if self.connect_feedback.is_empty() {
                            "Advertise PeerBrush and set up installed AI clients. An agent turns the MCP indicator green when it connects."
                        } else { &self.connect_feedback });
                        let color = if connected {
                            Color32::from_rgb(166, 215, 157)
                        } else {
                            Color32::from_rgb(255, 154, 144)
                        };
                        ui.label(
                            RichText::new(if connected {
                                "MCP connected"
                            } else {
                                "MCP · waiting for AI"
                            })
                            .size(11.0)
                            .color(color),
                        ).on_hover_text("The server is listening. Connect AI sets up discovery and installed clients; this indicator turns green after an actual agent handshake.");
                        let (_, r) = ui.allocate_space(Vec2::new(6.0, 6.0));
                        ui.painter().circle_filled(r.center(), 2.5, color);
                    });
                });
                let width = (bounds.width() - 610.0).clamp(200.0, 540.0);
                let center = Rect::from_center_size(bounds.center(), Vec2::new(width, 32.0));
                ui.painter()
                    .rect_filled(center, 9.0, Color32::from_rgb(36, 34, 38));
                let progress = (self.activity_changed.elapsed().as_secs_f32() / 0.3).min(1.0);
                let alpha = 0.75 + progress * 0.25;
                let pulse = ui.input(|i| i.time) as f32;
                ui.painter().circle_filled(
                    center.left_center() + Vec2::new(13.0, 0.0),
                    if connected {
                        3.0 + 0.7 * (pulse * 3.0).sin()
                    } else {
                        2.5
                    },
                    if connected {
                        AI_BLUE
                    } else {
                        MUTED.gamma_multiply(0.5)
                    },
                );
                if connected {
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                if progress < 1.0 {
                    ctx.request_repaint();
                }
                let label_rect = Rect::from_min_max(
                    center.min + Vec2::new(28.0, (1.0 - progress) * 3.0),
                    center.max - Vec2::new(10.0, 0.0),
                );
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(label_rect).layout(
                        egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                    ),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&self.activity_text)
                                    .size(12.0)
                                    .color(if self.animation.active(ui.input(|i| i.time)) { AI_BLUE.gamma_multiply(alpha) } else { MUTED.gamma_multiply(alpha) }),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&self.activity_text);
                    },
                );
            });
        egui::TopBottomPanel::top("context")
            .exact_height(48.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.menu_button("File", |ui| {
                        if ui.button("New canvas").clicked() {
                            self.show_new = true;
                            ui.close_menu();
                        }
                        if ui.button("Open PSD").clicked() {
                            self.open();
                            ui.close_menu();
                        }
                        if ui.button("Save PSD").clicked() {
                            self.save(false);
                            ui.close_menu();
                        }
                        if ui.button("Save As...").clicked() {
                            self.save(true);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Import images...").clicked() {
                            self.import();
                            ui.close_menu();
                        }
                        if ui.button("Export PNG...").clicked() {
                            self.export();
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Select",|ui| {
                        for (label,op) in [("All · Ctrl+A","all"),("Deselect · Ctrl+D","selection.clear"),("Reselect · Ctrl+Shift+D","selection.reselect"),("Inverse","selection.invert")] {
                            if ui.button(label).clicked(){self.edit(vec![if op=="all"{json!({"op":"selection","kind":"rectangle","rect":[0,0,doc.width,doc.height]})}else{json!({"op":op})}],label);ui.close_menu();}
                        }
                        for (label,mode) in [("Expand by 1 px","expand"),("Contract by 1 px","contract")] {if ui.button(label).clicked(){self.edit(vec![json!({"op":"selection.modify","mode":mode,"radius":1})],label);ui.close_menu();}}
                    });
                    if ui
                        .add_enabled_ui(!self.busy, |ui| {
                            icons::button(ui, Icon::Save, "Save PSD · Ctrl/Cmd+S")
                        })
                        .inner
                        .on_hover_text("Save PSD · Ctrl/Cmd+S")
                        .clicked()
                    {
                        self.save(false);
                    }
                    ui.separator();
                    let (_, icon) = ui.allocate_space(Vec2::splat(24.0));
                    paint_tool(ui.painter(), icon, self.tool, ACCENT);
                    ui.label(RichText::new(self.tool.label()).size(12.0));
                    ui.add_space(6.0);
                    if self.mask {
                        ui.label(RichText::new("MASK").size(10.0).color(ACCENT));
                    }
                    if [Tool::Brush, Tool::Eraser, Tool::Smudge].contains(&self.tool) {
                        controls::label(ui, "Size");
                        let mut size = self.radius * 2.0;
                        controls::range(
                            ui,
                            "brush size",
                            &mut size,
                            1.0..=1024.0,
                            130.0,
                            " px",
                            0,
                            true,
                        );
                        self.radius = size / 2.0;
                        if icons::button(ui, Icon::Adjust, "Brush settings").clicked() {
                            self.show_brush = !self.show_brush;
                        }
                    }
                    if self.tool == Tool::Liquify {
                        liquify::toolbar(ui, &mut self.radius, &mut self.liquify);
                    }
                    if self.tool == Tool::SmartMask {
                        controls::label(ui, "Tolerance");
                        controls::range(
                            ui,
                            "mask tolerance",
                            &mut self.mask_tolerance,
                            0.0..=100.0,
                            110.0,
                            "%",
                            0,
                            false,
                        );
                        ui.checkbox(&mut self.mask_contiguous, "Contiguous");
                        egui::ComboBox::from_id_salt("mask mode")
                            .selected_text(&self.smart_mask_mode)
                            .show_ui(ui, |ui| {
                                for name in ["replace", "add", "subtract"] {
                                    ui.selectable_value(
                                        &mut self.smart_mask_mode,
                                        name.into(),
                                        name,
                                    );
                                }
                            });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(ui, Icon::Frame, "Frame selection or canvas · F").clicked()
                        {
                            self.frame_pending = true;
                        }
                        ui.label(
                            RichText::new(format!(
                                "{}%",
                                self.view_rect
                                    .map(|r| (r.width() / doc.width as f32 * 100.0).round() as u32)
                                    .unwrap_or(100)
                            ))
                            .size(11.0)
                            .color(MUTED),
                        );
                        if self.busy {
                            ui.spinner();
                        }
                    });
                });
            });
        if self.tool == Tool::Selection {
            egui::TopBottomPanel::top("selection context")
                .exact_height(40.)
                .show(ctx, |ui| self.selection_toolbar(ui));
        }
        egui::TopBottomPanel::bottom("status")
            .exact_height(28.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add(egui::Label::new(RichText::new(format!("{} × {} · {} bit", doc.width, doc.height, doc.bit_depth)).color(MUTED)).sense(egui::Sense::click())).on_hover_text("Right-click for canvas dimensions and channel depth").context_menu(|ui| {
                        if ui.button("Project dimensions and depth…").clicked(){self.project_settings=Some((doc.width,doc.height,doc.bit_depth));ui.close_menu();}
                    });
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "{}{}",
                            doc.name,
                            if self.shared.lock().unwrap().saved_revision != doc.revision {
                                " *"
                            } else {
                                ""
                            }
                        ))
                        .size(11.0)
                        .color(MUTED),
                    );
                    ui.separator();
                    ui.label(RichText::new(&self.message).size(11.0));
                    if doc.read_only {
                        ui.label(RichText::new("READ ONLY").size(10.0).color(ACCENT))
                            .on_hover_text(doc.warnings.join("\n"));
                        if ui.button(format!("Edit {}-bit copy",doc.bit_depth)).on_hover_text("Create a flattened editable copy at the original color precision. Unsupported Photoshop layers are flattened; the source file stays untouched.").clicked() {
                            let shared=self.shared.clone();let revision=doc.revision;
                            self.job(move||{server::compatible_copy(&shared,"human",Some(revision))?;Ok("Editing a flattened copy at original precision · save under a new name".into())});
                        }
                    }
                    let leases = self.shared.lock().unwrap().leases.clone();
                    if !leases.is_empty() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button(RichText::new("Take back control").color(ACCENT))
                                .clicked()
                            {
                                {
                                    let mut e = self.shared.lock().unwrap();
                                    e.leases.clear();
                                    e.activity.clear();
                                }
                                self.message = "AI reservations released".into();
                            }
                            ui.label(RichText::new(&leases[0].description).color(
                                if leases[0].owner == "human" {
                                    ACCENT
                                } else {
                                    AI_BLUE
                                },
                            ));
                        });
                    } else {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new("You and your AI. Same canvas.").color(MUTED));
                        });
                    }
                });
            });
        egui::SidePanel::left("tools")
            .exact_width(60.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.spacing_mut().item_spacing.y = 4.0;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height((ui.available_height() - 70.0).max(60.0))
                    .show(ui, |ui| {
                        for (index, tool) in [
                            Tool::None,
                            Tool::Move,
                            Tool::Rotate,
                            Tool::Scale,
                            Tool::Brush,
                            Tool::Smudge,
                            Tool::Liquify,
                            Tool::Eraser,
                            Tool::Fill,
                            Tool::Gradient,
                            Tool::Rectangle,
                            Tool::Ellipse,
                            Tool::Selection,
                            Tool::Picker,
                            Tool::Pan,
                            Tool::SmartMask,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            if index == 4 || index == 10 {
                                ui.add_space(4.0);
                                ui.separator();
                                ui.add_space(4.0);
                            }
                            let r = ui
                                .add_sized(
                                    [42.0, 39.0],
                                    egui::Button::new("")
                                        .selected(self.tool == tool)
                                        .frame(false),
                                )
                                .on_hover_text(tool.label());
                            let active = ui.ctx().animate_bool_with_time(
                                r.id.with("selected"),
                                self.tool == tool,
                                0.1,
                            );
                            if active > 0.0 {
                                ui.painter().rect_filled(
                                    r.rect,
                                    6,
                                    Color32::from_rgb(74, 48, 71).gamma_multiply(active),
                                );
                                ui.painter().rect_filled(
                                    Rect::from_min_size(
                                        r.rect.left_top(),
                                        Vec2::new(2.0, r.rect.height()),
                                    ),
                                    1,
                                    ACCENT,
                                );
                            }
                            let ai_tool = self.animation.tool(ui.input(|i| i.time));
                            let ai_active = matches!(
                                (ai_tool, tool),
                                (Some("brush"), Tool::Brush)
                                    | (Some("smudge"), Tool::Smudge)
                                    | (Some("liquify"), Tool::Liquify)
                                    | (Some("eraser"), Tool::Eraser)
                                    | (Some("move"), Tool::Move)
                                    | (Some("rotate"), Tool::Rotate)
                                    | (Some("scale"), Tool::Scale)
                                    | (Some("mask"), Tool::SmartMask)
                                    | (Some("fill"), Tool::Fill)
                                    | (Some("selection"), Tool::Selection)
                            );
                            if ai_active {
                                let pulse =
                                    0.09 + 0.025 * (ui.input(|i| i.time) as f32 * 4.0).sin();
                                ui.painter()
                                    .rect_filled(r.rect, 6, AI_BLUE.gamma_multiply(pulse));
                                ui.painter().line_segment(
                                    [r.rect.right_top(), r.rect.right_bottom()],
                                    Stroke::new(2.0_f32, AI_BLUE),
                                );
                            }
                            paint_tool(
                                ui.painter(),
                                r.rect,
                                tool,
                                if self.tool == tool { ACCENT } else { MUTED },
                            );
                            if r.clicked() {
                                self.tool = tool;
                                self.drag_start = None;
                                self.points.clear();
                                self.stroke_pressures.clear();
                                self.stroke_has_pressure = false;
                                self.current_pressure = None;
                                self.gizmo_handle = 0;
                            }
                        }
                    });
                ui.add_space((ui.available_height() - 64.0).max(0.0));
                self.palette(ui);
            });
        egui::SidePanel::right("layer_panel")
            .default_width(320.0)
            .min_width(300.0)
            .max_width(470.0)
            .show(ctx, |ui| {
                let display = self.animation.document(&doc, ui.input(|i| i.time), false);
                if self.ai_layer(&self.selected, ui.input(|i| i.time)) {
                    ui.style_mut().visuals.selection.stroke.color = AI_BLUE;
                }
                self.layers(ui, &display);
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(32, 30, 34))
                    .inner_margin(8),
            )
            .show(ctx, |ui| {
                self.canvas(ui, &doc);
            });

        self.request_preview(ctx, &doc);
        if self.liquify_effect.as_ref().is_some_and(|id| {
            !doc.layers
                .iter()
                .find(|l| l.id == self.selected)
                .is_some_and(|l| {
                    l.effects
                        .iter()
                        .any(|e| e.id == *id && e.kind == "liquify" && e.enabled)
                })
        }) {
            self.liquify_effect = None;
        }
        if let Some((mut w, mut h, mut depth)) = self.project_settings {
            let mut open = true;
            let mut apply = false;
            egui::Window::new("Project dimensions and depth")
                .open(&mut open)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Width");
                        ui.add(egui::DragValue::new(&mut w).range(1..=8192).suffix(" px"));
                        ui.label("Height");
                        ui.add(egui::DragValue::new(&mut h).range(1..=8192).suffix(" px"));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Channels");
                        ui.selectable_value(&mut depth, 8, "8 bit");
                        ui.selectable_value(&mut depth, 16, "16 bit");
                    });
                    ui.label("Canvas size preserves layer pixels and positions.");
                    if depth == 8 && doc.bit_depth == 16 {
                        ui.colored_label(
                            ACCENT,
                            "Converting to 8 bit quantizes the channels. Undo restores 16 bit.",
                        );
                    }
                    apply = ui.button("Apply dimensions and depth").clicked();
                });
            self.project_settings = if open { Some((w, h, depth)) } else { None };
            if apply {
                self.edit(
                    vec![json!({"op":"document.settings","width":w,"height":h,"bit_depth":depth})],
                    "Project dimensions and depth",
                );
                self.project_settings = None;
                self.frame_pending = true;
            }
        }
        if self.show_brush {
            self.brush_settings(ctx);
        }
        self.color_window(ctx);
        if self.show_new {
            let mut open = true;
            egui::Window::new("New canvas")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Width");
                        controls::numeric(ui, &mut self.new_width, 16..=8192);
                        ui.label("Height");
                        controls::numeric(ui, &mut self.new_height, 16..=8192);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        ui.selectable_value(&mut self.new_bit_depth, 8, "8 bit");
                        ui.selectable_value(&mut self.new_bit_depth, 16, "16 bit");
                    });
                    if {
                        let e = self.shared.lock().unwrap();
                        e.doc.revision != e.saved_revision
                    } {
                        ui.label("Creating a canvas replaces unsaved work.");
                    }
                    if ui.button("Create canvas").clicked() {
                        let result = Document::new_depth(
                            self.new_width,
                            self.new_height,
                            self.new_bit_depth,
                        )
                        .and_then(|d| self.shared.lock().unwrap().replace(d, None));
                        match result {
                            Ok(_) => {
                                self.texture = None;
                                self.canvas_pixels = None;
                                self.animation.cancel();
                                self.show_new = false;
                                self.pan = Vec2::ZERO;
                                self.zoom = 1.0;
                                self.last_preview = None;
                            }
                            Err(e) => self.message = e,
                        }
                    }
                });
            if !open {
                self.show_new = false;
            }
        }
        if self.show_connection {
            let mut open = true;
            egui::Window::new("AI connection settings").open(&mut open).default_width(550.0).show(ctx, |ui| {
                ui.label(RichText::new("MCP is ready").color(Color32::from_rgb(166, 215, 157)).strong());
                ui.label("Connect AI handles discovery and installed clients. Manual configuration is available here for other clients.");
                if !self.connect_feedback.is_empty() { ui.label(&self.connect_feedback); }
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.connection_codex, true, "Codex");
                    ui.selectable_value(&mut self.connection_codex, false, "Other clients");
                });
                let exe = std::env::current_exe().unwrap_or_default().to_string_lossy().into_owned();
                let state = self.connection.state_dir.to_string_lossy().into_owned();
                let config = if self.connection_codex {
                    ui.label("Add to ~/.codex/config.toml, then restart Codex.");
                    format!("[mcp_servers.peerbrush]\ncommand = {}\nargs = {}\n", serde_json::to_string(&exe).unwrap(), serde_json::to_string(&["mcp", "--state-dir", &state]).unwrap())
                } else {
                    ui.label("Add to your client's MCP configuration.");
                    serde_json::to_string_pretty(&json!({"mcpServers":{"peerbrush":{"command":exe,"args":["mcp","--state-dir",state]}}})).unwrap()
                };
                let mut display = config.clone();
                ui.add(egui::TextEdit::multiline(&mut display).font(egui::TextStyle::Monospace).desired_rows(if self.connection_codex { 4 } else { 8 }).desired_width(f32::INFINITY));
                if ui.button("Copy configuration").clicked() { ui.ctx().copy_text(config); }
                ui.label(RichText::new(format!("Local endpoint · 127.0.0.1:{}", self.connection.port)).size(11.0).color(MUTED));
                if self.connection.state_dir.join("recovery.psd").exists() && ui.button("Recover last autosave").clicked() {
                    let s = self.shared.clone();
                    let path = self.connection.state_dir.join("recovery.psd");
                    self.job(move || {
                        server::open(&s, &path)?;
                        { let mut e = s.lock().unwrap(); e.path = None; e.file_version = None; e.saved_revision = u64::MAX; }
                        Ok("Recovered autosave — save to your chosen PSD".into())
                    });
                }
            });
            self.show_connection = open;
        }
        if self.pending || self.busy || !self.points.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
}

impl eframe::App for PeerBrush {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.draw(ctx);
    }
    fn on_exit(&mut self) {
        let doc = {
            let e = self.shared.lock().unwrap();
            if e.doc.revision == e.saved_revision {
                return;
            }
            e.doc.clone()
        };
        if let Ok(bytes) = crate::psd::encode(&doc) {
            let _ = server::atomic_write(&self.connection.state_dir.join("recovery.psd"), &bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use std::sync::{Arc, Mutex};
    fn frame(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
    ) {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360.0, 900.0))),
            events,
            modifiers,
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.draw(ctx));
    }
    fn fixture() -> (PeerBrush, egui::Context) {
        let ctx = egui::Context::default();
        let app = PeerBrush::init(
            &ctx,
            Arc::new(Mutex::new(Engine::new())),
            Connection {
                port: 0,
                token: String::new(),
                state_dir: std::env::temp_dir(),
                instance_lock: None,
            },
        );
        (app, ctx)
    }
    fn key(app: &mut PeerBrush, ctx: &egui::Context, key: egui::Key) {
        frame(
            app,
            ctx,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            Default::default(),
        );
        frame(
            app,
            ctx,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Default::default(),
            }],
            Default::default(),
        );
    }
    fn button(
        pos: Pos2,
        button: egui::PointerButton,
        pressed: bool,
        modifiers: egui::Modifiers,
    ) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        }
    }
    #[test]
    fn f_frames_selection_with_margin_and_resets_full_canvas() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        let baseline = app.view_rect.unwrap();
        app.zoom = 0.4;
        app.pan = Vec2::new(180.0, -120.0);
        app.shared.lock().unwrap().doc.selection = Some([100, 80, 300, 180]);
        key(&mut app, &ctx, egui::Key::F);
        let framed = app.view_rect.unwrap();
        let doc = app.shared.lock().unwrap().doc.clone();
        let scale = framed.width() / doc.width as f32;
        let center = framed.min + Vec2::new(200.0, 130.0) * scale;
        assert!((center - baseline.center()).length() < 1.0);
        assert!(app.zoom > 2.0);
        assert!(200.0 * scale < baseline.width() + 60.0);
        app.shared.lock().unwrap().doc.selection = None;
        key(&mut app, &ctx, egui::Key::F);
        assert_eq!(app.zoom, 1.0);
        assert_eq!(app.pan, Vec2::ZERO);
        assert!((app.view_rect.unwrap().center() - baseline.center()).length() < 1.0);
        assert!(app.shared.lock().unwrap().undo.is_empty());
    }
    #[test]
    fn tool_keys_survive_layer_and_button_focus_but_leave_text_fields_alone() {
        let (mut app, ctx) = fixture();
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let id = app.selected.clone();
        let row = app.layer_rects[&id];
        click(&mut app, &ctx, row.center());
        key(&mut app, &ctx, egui::Key::W);
        assert!(app.tool == Tool::Move);
        let eye = app.eye_rects[&id].center();
        click(&mut app, &ctx, eye);
        // Custom icon buttons don't take keyboard focus. Exercise a normal focused
        // control too: the previous wants_keyboard_input guard blocked tool keys here.
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.button("Focused control").request_focus();
            });
        });
        assert!(ctx.wants_keyboard_input());
        for (k, t) in [
            (egui::Key::W, Tool::Move),
            (egui::Key::E, Tool::Rotate),
            (egui::Key::R, Tool::Scale),
            (egui::Key::Q, Tool::None),
        ] {
            key(&mut app, &ctx, k);
            assert!(app.tool == t);
        }
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::W),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            Default::default(),
        );
        assert!(app.tool == Tool::Move);
        let text_id = egui::Id::new("test text edit");
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut text = String::new();
                ui.add(egui::TextEdit::singleline(&mut text).id(text_id))
                    .request_focus();
            });
        });
        assert!(egui::TextEdit::load_state(&ctx, text_id).is_some());
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::R,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            Default::default(),
        );
        assert!(app.tool == Tool::Move);
    }
    #[test]
    fn long_color_and_mask_stacks_keep_add_effect_visible_in_a_small_window() {
        let (mut app, ctx) = fixture();
        let id = app.selected.clone();
        let mut commands = vec![json!({"op":"mask.add","layer":id})];
        for _ in 0..12 {
            commands.push(json!({"op":"effect.add","layer":id,"kind":"levels"}));
            commands.push(json!({"op":"mask.step.add","layer":id,"kind":"levels"}));
        }
        app.shared
            .lock()
            .unwrap()
            .edit("human", &commands, None, None, "stack")
            .unwrap();
        for mask in [false, true] {
            app.mask = mask;
            for _ in 0..3 {
                let raw = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 600.0))),
                    ..Default::default()
                };
                let _ = ctx.run(raw, |ctx| app.draw(ctx));
            }
            let add = app.effect_add_rect.unwrap();
            assert!(
                add.top() > 0.0 && add.bottom() < 580.0,
                "Add effect clipped at {add:?}"
            );
        }
    }
    #[test]
    fn maya_shortcuts_and_move_drag_produce_one_undoable_edit() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        for (k, t) in [
            (egui::Key::W, Tool::Move),
            (egui::Key::E, Tool::Rotate),
            (egui::Key::R, Tool::Scale),
            (egui::Key::Q, Tool::None),
            (egui::Key::D, Tool::Eraser),
        ] {
            key(&mut app, &ctx, k);
            assert!(app.tool == t);
        }
        key(&mut app, &ctx, egui::Key::W);
        let a = app.view_rect.unwrap().center();
        let b = a + Vec2::new(30.0, 20.0);
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(a),
                button(a, egui::PointerButton::Primary, true, Default::default()),
            ],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(b)],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![button(
                b,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
            Default::default(),
        );
        let mut engine = app.shared.lock().unwrap();
        assert!(engine.doc.layers[0].x > 0);
        assert!(engine.doc.layers[0].y > 0);
        assert_eq!(engine.undo.len(), 1);
        engine.undo("human").unwrap();
        assert_eq!((engine.doc.layers[0].x, engine.doc.layers[0].y), (0, 0));
    }
    #[test]
    fn middle_drag_pans_and_alt_right_drag_zooms_without_editing() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        key(&mut app, &ctx, egui::Key::Q);
        let a = app.view_rect.unwrap().center();
        let b = a + Vec2::new(42.0, 20.0);
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(a),
                button(a, egui::PointerButton::Middle, true, Default::default()),
            ],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(b)],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![button(
                b,
                egui::PointerButton::Middle,
                false,
                Default::default(),
            )],
            Default::default(),
        );
        assert!(app.pan.length() > 10.0);
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        frame(
            &mut app,
            &ctx,
            vec![button(a, egui::PointerButton::Secondary, true, alt)],
            alt,
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(a + Vec2::new(50.0, 0.0))],
            alt,
        );
        frame(
            &mut app,
            &ctx,
            vec![button(
                a + Vec2::new(50.0, 0.0),
                egui::PointerButton::Secondary,
                false,
                alt,
            )],
            alt,
        );
        assert!(app.zoom > 1.0);
        assert_eq!(app.shared.lock().unwrap().doc.revision, 0);
    }

    #[test]
    fn native_file_drop_imports_and_selects_an_undoable_layer() {
        let (mut app, ctx) = fixture();
        let path =
            std::env::temp_dir().join(format!("peerbrush-drop-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&path, crate::raster::png(2, 2, &[190; 16]).unwrap()).unwrap();
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360.0, 900.0))),
            dropped_files: vec![egui::DroppedFile {
                path: Some(path.clone()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.draw(ctx));
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(10));
            frame(&mut app, &ctx, vec![], Default::default());
            if !app.busy {
                break;
            }
        }
        assert!(!app.busy);
        let mut engine = app.shared.lock().unwrap();
        assert_eq!(engine.doc.layers.len(), 2);
        assert_eq!(app.selected, engine.doc.layers[0].id);
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers.len(), 1);
        std::fs::remove_file(path).unwrap();
    }
    fn click(app: &mut PeerBrush, ctx: &egui::Context, pos: Pos2) {
        frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(pos),
                button(pos, egui::PointerButton::Primary, true, Default::default()),
            ],
            Default::default(),
        );
        frame(
            app,
            ctx,
            vec![button(
                pos,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
            Default::default(),
        );
    }
    #[test]
    fn whole_row_selection_and_eye_buttons_are_independent_before_thumbnails_load() {
        let (mut app, ctx) = fixture();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.add","kind":"paint"})],
                None,
                None,
                "layer",
            )
            .unwrap();
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let ids = app
            .shared
            .lock()
            .unwrap()
            .doc
            .layers
            .iter()
            .map(|l| l.id.clone())
            .collect::<Vec<_>>();
        let row = app.layer_rects[&ids[1]];
        click(
            &mut app,
            &ctx,
            egui::pos2(row.right() - 6.0, row.top() + 2.0),
        );
        assert_eq!(app.selected, ids[1]);
        let eye = app.eye_rects[&ids[0]].center();
        click(&mut app, &ctx, eye);
        assert!(!app.shared.lock().unwrap().doc.layers[0].visible);
        assert_eq!(app.selected, ids[0]);
        click(&mut app, &ctx, eye);
        assert!(app.shared.lock().unwrap().doc.layers[0].visible);
    }
    #[test]
    fn dragging_a_layer_previews_order_then_commits_one_undoable_edit() {
        let (mut app, ctx) = fixture();
        {
            let mut e = app.shared.lock().unwrap();
            e.edit(
                "human",
                &[
                    json!({"op":"layer.add","kind":"paint"}),
                    json!({"op":"layer.add","kind":"paint"}),
                ],
                None,
                None,
                "layers",
            )
            .unwrap();
            e.undo.clear();
        }
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let doc = app.shared.lock().unwrap().doc.clone();
        let id = doc.layers[0].id.clone();
        let a = app.layer_rects[&id].left_top() + Vec2::new(20.0, 2.0);
        let b = app.layer_rects[&doc.layers[2].id].center();
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(a),
                button(a, egui::PointerButton::Primary, true, Default::default()),
            ],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(b)],
            Default::default(),
        );
        frame(&mut app, &ctx, vec![], Default::default());
        assert_eq!(app.layer_drag.as_ref().unwrap().target, 2);
        assert_eq!(app.shared.lock().unwrap().doc.revision, doc.revision);
        assert!(app
            .transient
            .iter()
            .any(|c| c["op"] == "layer.reorder" && c["index"] == 2));
        frame(
            &mut app,
            &ctx,
            vec![button(
                b,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
            Default::default(),
        );
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.doc.layers[2].id, id);
        assert_eq!(e.undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers[0].id, id);
    }
    #[test]
    fn x_swaps_foreground_background_and_independent_mask_colors() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        let (fore, back) = (app.color, app.background);
        key(&mut app, &ctx, egui::Key::X);
        assert_eq!((app.color, app.background), (back, fore));
        app.mask = true;
        app.mask_value = 32;
        app.mask_background = 210;
        key(&mut app, &ctx, egui::Key::X);
        assert_eq!((app.mask_value, app.mask_background), (210, 32));
        assert_eq!((app.color, app.background), (back, fore));
    }
    #[test]
    fn gizmo_drag_contains_actual_pixel_preview_without_committing_history() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        key(&mut app, &ctx, egui::Key::W);
        let a = app.view_rect.unwrap().center();
        let b = a + Vec2::new(25.0, 10.0);
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(a),
                button(a, egui::PointerButton::Primary, true, Default::default()),
            ],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(b)],
            Default::default(),
        );
        assert!(app
            .transient
            .iter()
            .any(|c| c["op"] == "move" && c["dx"].as_f64().unwrap() > 0.0));
        assert_eq!(app.shared.lock().unwrap().doc.revision, 0);
        key(&mut app, &ctx, egui::Key::Escape);
        assert_eq!(app.shared.lock().unwrap().doc.revision, 0);
    }
    #[test]
    fn multiple_rows_move_together_and_shared_edits_are_one_undo_step() {
        let (mut app, ctx) = fixture();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[
                    json!({"op":"layer.add","kind":"paint"}),
                    json!({"op":"layer.add","kind":"paint"}),
                ],
                None,
                None,
                "layers",
            )
            .unwrap();
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let ids = app
            .shared
            .lock()
            .unwrap()
            .doc
            .layers
            .iter()
            .map(|l| l.id.clone())
            .collect::<Vec<_>>();
        let a = app.layer_rects[&ids[0]].center();
        let b = app.layer_rects[&ids[1]].center();
        click(&mut app, &ctx, a);
        let cmd = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(b),
                button(b, egui::PointerButton::Primary, true, cmd),
            ],
            cmd,
        );
        frame(
            &mut app,
            &ctx,
            vec![button(b, egui::PointerButton::Primary, false, cmd)],
            cmd,
        );
        assert_eq!(app.selection_layers.len(), 2);
        assert!(
            app.rename_edit.is_none(),
            "Rapid Ctrl-click on another row must not start rename"
        );
        app.shared.lock().unwrap().undo.clear();
        app.layer_cmd("layer.update", json!({"opacity":0.7}), "Opacity");
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        for id in [&ids[0], &ids[1]] {
            assert_eq!(
                app.shared
                    .lock()
                    .unwrap()
                    .doc
                    .layers
                    .iter()
                    .find(|l| &l.id == id)
                    .unwrap()
                    .opacity,
                0.7
            );
        }
        key(&mut app, &ctx, egui::Key::W);
        let a = app.view_rect.unwrap().center();
        let b = a + Vec2::new(25.0, 15.0);
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(a),
                button(a, egui::PointerButton::Primary, true, Default::default()),
            ],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(b)],
            Default::default(),
        );
        assert_eq!(
            app.transient.iter().filter(|c| c["op"] == "move").count(),
            2
        );
        frame(
            &mut app,
            &ctx,
            vec![button(
                b,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
            Default::default(),
        );
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.undo.len(), 2);
        let p = e
            .doc
            .layers
            .iter()
            .filter(|l| app.selection_layers.contains(&l.id))
            .map(|l| (l.x, l.y))
            .collect::<Vec<_>>();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0], p[1]);
        assert!(p[0].0 > 0);
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers[0].x, 0);
    }
    #[test]
    fn dragging_into_a_folder_and_back_out_previews_and_commits_parent_changes() {
        let (mut app, ctx) = fixture();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[
                    json!({"op":"layer.add","kind":"group"}),
                    json!({"op":"layer.add","kind":"paint"}),
                ],
                None,
                None,
                "layers",
            )
            .unwrap();
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let doc = app.shared.lock().unwrap().doc.clone();
        let id = doc.layers[0].id.clone();
        let folder = doc.layers[1].id.clone();
        app.shared.lock().unwrap().undo.clear();
        let a = app.layer_rects[&id].left_top() + Vec2::new(20.0, 2.0);
        let b = app.layer_rects[&folder].center();
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(a),
                button(a, egui::PointerButton::Primary, true, Default::default()),
            ],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(b)],
            Default::default(),
        );
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app
            .transient
            .iter()
            .any(|c| c["op"] == "layer.parent" && c["parent"] == folder));
        assert!(app
            .shared
            .lock()
            .unwrap()
            .doc
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .parent
            .is_none());
        frame(
            &mut app,
            &ctx,
            vec![button(
                b,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
            Default::default(),
        );
        assert_eq!(
            app.shared
                .lock()
                .unwrap()
                .doc
                .layers
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .parent
                .as_deref(),
            Some(folder.as_str())
        );
        for _ in 0..12 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let a = app.layer_rects[&id].left_top() + Vec2::new(20.0, 2.0);
        let b = app.layer_rects[&id].left_top() + Vec2::new(12.0, 21.0);
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(a),
                button(a, egui::PointerButton::Primary, true, Default::default()),
            ],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(b)],
            Default::default(),
        );
        frame(&mut app, &ctx, vec![], Default::default());
        frame(
            &mut app,
            &ctx,
            vec![button(
                b,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
            Default::default(),
        );
        let mut e = app.shared.lock().unwrap();
        assert!(e
            .doc
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .parent
            .is_none());
        assert_eq!(e.undo.len(), 2);
        e.undo("human").unwrap();
        assert_eq!(
            e.doc
                .layers
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .parent
                .as_deref(),
            Some(folder.as_str())
        );
    }
    #[test]
    fn sweeping_eyes_previews_visibility_then_commits_one_undo_step() {
        let (mut app, ctx) = fixture();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[
                    json!({"op":"layer.add","kind":"paint"}),
                    json!({"op":"layer.add","kind":"paint"}),
                ],
                None,
                None,
                "layers",
            )
            .unwrap();
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let ids = app
            .shared
            .lock()
            .unwrap()
            .doc
            .layers
            .iter()
            .map(|l| l.id.clone())
            .collect::<Vec<_>>();
        app.shared.lock().unwrap().undo.clear();
        let a = app.eye_rects[&ids[0]].center();
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(a),
                button(a, egui::PointerButton::Primary, true, Default::default()),
            ],
            Default::default(),
        );
        // A fast sweep can skip an entire row between input events.
        let p = app.eye_rects[&ids[2]].center();
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(p)],
            Default::default(),
        );
        assert!(app
            .shared
            .lock()
            .unwrap()
            .doc
            .layers
            .iter()
            .all(|l| l.visible));
        assert_eq!(
            app.transient
                .iter()
                .filter(|c| c["op"] == "layer.update" && c["visible"] == false)
                .count(),
            3
        );
        let b = app.eye_rects[&ids[2]].center();
        frame(
            &mut app,
            &ctx,
            vec![button(
                b,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
            Default::default(),
        );
        let mut e = app.shared.lock().unwrap();
        assert!(e.doc.layers.iter().all(|l| !l.visible));
        assert_eq!(e.undo.len(), 1);
        e.undo("human").unwrap();
        assert!(e.doc.layers.iter().all(|l| l.visible));
    }
    #[test]
    fn alt_b_fills_a_selection_and_mask_and_remains_undoable() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        app.shared.lock().unwrap().doc.selection = Some([3, 4, 8, 9]);
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::B,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: alt,
            }],
            alt,
        );
        {
            let mut e = app.shared.lock().unwrap();
            assert_eq!(e.doc.layers[0].pixels.get(4, 5), app.color);
            assert_eq!(e.doc.layers[0].pixels.get(1, 1), [0; 4]);
            assert_eq!(e.undo.len(), 1);
            e.undo("human").unwrap();
        }
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"mask.add","layer":app.selected,"value":255})],
                None,
                None,
                "mask",
            )
            .unwrap();
        app.mask = true;
        app.mask_value = 22;
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::B,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: alt,
            }],
            alt,
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::B,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: alt,
            }],
            alt,
        );
        let e = app.shared.lock().unwrap();
        assert!((e.doc.layers[0].mask_value(4, 5) * 255.0 - 22.0).abs() < 1.0);
        assert_eq!(e.doc.layers[0].pixels.get(4, 5), [0; 4]);
    }
    #[test]
    fn brackets_and_s_pointer_motion_resize_without_painting() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        let radius = app.radius;
        key(&mut app, &ctx, egui::Key::CloseBracket);
        assert!(app.radius > radius);
        key(&mut app, &ctx, egui::Key::OpenBracket);
        assert!((app.radius - radius).abs() < 0.01);
        let a = app.view_rect.unwrap().center();
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(a),
                egui::Event::Key {
                    key: egui::Key::S,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                },
            ],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(a + Vec2::new(30.0, 0.0))],
            Default::default(),
        );
        assert!(app.radius > radius);
        assert_eq!(app.shared.lock().unwrap().doc.revision, 0);
        assert!(app.points.is_empty());
    }
    #[test]
    fn alt_temporarily_samples_color_without_painting_or_switching_tools() {
        let (mut app, ctx) = fixture();
        {
            let mut e = app.shared.lock().unwrap();
            e.doc.layers[0].kind = "fill".into();
            e.doc.layers[0].color = [24, 120, 222, 255];
        }
        frame(&mut app, &ctx, vec![], Default::default());
        let point = app.view_rect.unwrap().center();
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(point),
                button(point, egui::PointerButton::Primary, true, alt),
            ],
            alt,
        );
        frame(
            &mut app,
            &ctx,
            vec![button(point, egui::PointerButton::Primary, false, alt)],
            alt,
        );
        assert_eq!(app.color, [24, 120, 222, 255]);
        assert!(app.tool == Tool::Brush);
        assert!(app.points.is_empty());
        assert_eq!(app.shared.lock().unwrap().doc.revision, 0);
        assert!(app.shared.lock().unwrap().undo.is_empty());
        // Releasing Alt restores brush input, including mask painting.
        frame(&mut app, &ctx, vec![], Default::default());
        app.shared.lock().unwrap().doc.layers[0].kind = "paint".into();
        app.color = [233, 84, 32, 255];
        click(&mut app, &ctx, point);
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        assert_eq!(app.shared.lock().unwrap().doc.revision, 1);
    }
    #[test]
    fn ai_hierarchy_moves_reveal_folders_and_animate_without_extra_edits() {
        let (mut app, ctx) = fixture();
        let folder = app
            .shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.add","kind":"group","name":"Destination"})],
                None,
                None,
                "Group",
            )
            .unwrap()["created"][0]
            .as_str()
            .unwrap()
            .to_string();
        let layer = app
            .shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.add","kind":"paint","name":"Move me"})],
                None,
                None,
                "Layer",
            )
            .unwrap()["created"][0]
            .as_str()
            .unwrap()
            .to_string();
        app.select_content(&layer);
        app.collapsed.insert(folder.clone());
        let draw_at = |app: &mut PeerBrush, time| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360.0, 900.0))),
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            );
        };
        draw_at(&mut app, 1.0);
        let before = app.layer_rects[&layer];
        app.shared
            .lock()
            .unwrap()
            .edit(
                "agent",
                &[json!({"op":"layer.parent","layer":layer,"parent":folder})],
                None,
                None,
                "Organize",
            )
            .unwrap();
        let revision = app.shared.lock().unwrap().doc.revision;
        let history = app.shared.lock().unwrap().undo.len();
        draw_at(&mut app, 1.01);
        assert!(!app.collapsed.contains(&folder));
        assert!(app.ai_layer(&layer, ctx.input(|i| i.time)));
        draw_at(&mut app, 1.19);
        let middle = app.layer_rects[&layer];
        draw_at(&mut app, 1.60);
        let after = app.layer_rects[&layer];
        assert!(
            middle.top() > before.top() && middle.top() < after.top(),
            "Layer should visibly travel between rows: {before:?}, {middle:?}, {after:?}"
        );
        assert_eq!(app.shared.lock().unwrap().doc.revision, revision);
        assert_eq!(app.shared.lock().unwrap().undo.len(), history);
        assert_eq!(
            app.shared
                .lock()
                .unwrap()
                .doc
                .layers
                .iter()
                .find(|l| l.id == layer)
                .unwrap()
                .parent
                .as_deref(),
            Some(folder.as_str())
        );
    }
    #[test]
    fn ai_opacity_ui_animation_does_not_write_intermediate_values_into_history() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        let layer = app.selected.clone();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "agent",
                &[json!({"op":"layer.update","layer":layer,"opacity":0.17})],
                None,
                None,
                "Opacity",
            )
            .unwrap();
        let revision = app.shared.lock().unwrap().doc.revision;
        for _ in 0..12 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let e = app.shared.lock().unwrap();
        assert_eq!(e.doc.revision, revision);
        assert_eq!(e.doc.layers[0].opacity, 0.17);
        assert_eq!(e.undo.len(), 1);
    }
    #[test]
    fn moving_pointer_accepts_last_completed_preview_from_the_same_live_gesture() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        app.pending = false;
        app.live_gesture = Some("stroke".into());
        app.points = vec![[10.0, 10.0], [20.0, 10.0]];
        let doc = app.shared.lock().unwrap().doc.clone();
        let live = app.live_key();
        let variant = app.preview_variant();
        app.points.push([30.0, 10.0]);
        app.preview_tx
            .send(Preview {
                live,
                doc_id: doc.id.clone(),
                revision: doc.revision,
                selection: doc.selection,
                selection_polygon: doc.selection_polygon.clone(),
                selection_coverage: doc.selection_coverage.clone(),
                target: variant,
                mask: false,
                w: 1,
                h: 1,
                dirty: None,
                bytes: vec![255, 80, 20, 255],
            })
            .unwrap();
        app.request_preview(&ctx, &doc);
        assert!(app.texture.is_some());
    }
    #[test]
    fn connect_ai_click_starts_async_setup_without_a_modal_or_fake_client_presence() {
        let (mut app, ctx) = fixture();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        app.connect_action = Arc::new(move |exe, state| {
            started_tx
                .send((exe.to_path_buf(), state.to_path_buf()))
                .unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            Ok(ConnectOutcome {
                summary: "Ready in Codex. Reload its MCP tools.".into(),
                registrations: 1,
                retry: false,
            })
        });
        frame(&mut app, &ctx, vec![], Default::default());
        let button = app.connect_button_rect.unwrap().center();
        click(&mut app, &ctx, button);
        let (executable, state) = started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(executable, std::env::current_exe().unwrap());
        assert_eq!(state, app.connection.state_dir);
        assert!(app.connecting);
        assert!(!app.show_connection);
        assert!(!app.busy);
        // The worker is intentionally blocked: manual canvas shortcuts still run.
        key(&mut app, &ctx, egui::Key::Q);
        assert!(app.tool == Tool::None);
        app.connect_ai(&ctx);
        assert!(
            started_rx.try_recv().is_err(),
            "Repeated clicks must not start duplicate setup jobs"
        );
        assert!(app.shared.lock().unwrap().mcp_clients.is_empty());
        release_tx.send(()).unwrap();
        let result = app.connect_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        app.connect_tx.send(result).unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(!app.connecting);
        assert!(app.connect_ready);
        assert!(!app.connect_retry);
        assert!(!app.show_connection);
        assert_eq!(app.message, "Ready in Codex. Reload its MCP tools.");
        assert!(app.shared.lock().unwrap().mcp_clients.is_empty());
    }
    #[test]
    fn secondary_connection_settings_opens_manual_configuration_without_starting_setup() {
        let (mut app, ctx) = fixture();
        app.connect_action = Arc::new(|_, _| panic!("Settings must not register any client"));
        frame(&mut app, &ctx, vec![], Default::default());
        let settings = app.connection_settings_rect.unwrap().center();
        click(&mut app, &ctx, settings);
        assert!(app.show_connection);
        assert!(!app.connecting);
        assert!(app.connect_feedback.is_empty());
    }
    #[test]
    fn failed_or_partial_setup_exposes_retry_and_never_claims_an_agent_connected() {
        let (mut app, ctx) = fixture();
        app.connect_tx
            .send(Err(
                "Claude configuration is locked. Close it and retry.".into()
            ))
            .unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app.connect_retry);
        assert!(!app.connect_ready);
        assert!(app.message.contains("configuration is locked"));
        app.connect_tx
            .send(Ok(ConnectOutcome {
                summary: "Codex ready; another client requires approval.".into(),
                registrations: 1,
                retry: true,
            }))
            .unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app.connect_ready && app.connect_retry);
        assert!(app.shared.lock().unwrap().mcp_clients.is_empty());
        assert!(!app.show_connection);
    }
    #[test]
    fn command_e_merges_selected_layers_selects_result_and_keeps_one_undo_entry() {
        let (mut app, ctx) = fixture();
        {
            let mut e = app.shared.lock().unwrap();
            e.doc = Document::new(32, 32).unwrap();
            e.edit(
                "human",
                &[json!({"op":"layer.add","kind":"paint","name":"Top"})],
                None,
                None,
                "Add",
            )
            .unwrap();
            e.doc.layers[0].pixels.set(4, 4, [233, 84, 32, 255]);
            e.doc.layers[1].pixels.set(8, 8, [24, 120, 222, 255]);
            e.undo.clear();
        }
        let ids = app
            .shared
            .lock()
            .unwrap()
            .doc
            .layers
            .iter()
            .map(|l| l.id.clone())
            .collect::<Vec<_>>();
        app.selected = ids[0].clone();
        app.selection_layers = ids.iter().cloned().collect();
        frame(&mut app, &ctx, vec![], Default::default());
        let command = egui::Modifiers {
            command: true,
            ctrl: cfg!(not(target_os = "macos")),
            mac_cmd: cfg!(target_os = "macos"),
            ..Default::default()
        };
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::E,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: command,
            }],
            command,
        );
        let reply = app.merge_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(reply.result.is_ok());
        app.merge_tx.send(reply).unwrap();
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::E,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: command,
            }],
            command,
        );
        assert!(!app.busy);
        assert_eq!(app.selection_layers.len(), 1);
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.doc.layers.len(), 1);
        assert_eq!(app.selected, e.doc.layers[0].id);
        assert!(!ids.contains(&app.selected));
        assert_eq!(e.undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(
            e.doc
                .layers
                .iter()
                .map(|l| l.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
    }
    #[test]
    fn mask_stack_displays_last_operation_on_top_without_changing_execution_order() {
        let (mut app, ctx) = fixture();
        {
            let mut e = app.shared.lock().unwrap();
            e.doc = Document::new(32, 32).unwrap();
            e.doc.layers[0].kind = "fill".into();
            let layer = e.doc.layers[0].id.clone();
            e.edit(
                "human",
                &[
                    json!({"op":"mask.add","layer":layer,"value":128}),
                    json!({"op":"mask.step.add","layer":layer,"kind":"invert"}),
                    json!({"op":"mask.step.add","layer":layer,"kind":"levels","value":2.0}),
                ],
                None,
                None,
                "Mask stack",
            )
            .unwrap();
            app.selected = layer.clone();
            app.selection_layers = [layer].into_iter().collect();
        }
        app.mask = true;
        let source = app.shared.lock().unwrap().doc.clone();
        let expected = source.preview(None, 32, None, false).unwrap().2;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360.0, 1200.0))),
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        );
        let mut labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text)
                    if ["Fill", "Paint", "Invert", "Levels"]
                        .contains(&text.galley.job.text.as_str()) =>
                {
                    Some((text.pos.y, text.galley.job.text.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        labels.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(
            labels.into_iter().map(|(_, name)| name).collect::<Vec<_>>(),
            ["Levels", "Invert", "Paint", "Fill"]
        );
        let e = app.shared.lock().unwrap();
        assert_eq!(
            e.doc.layers[0]
                .mask
                .as_ref()
                .unwrap()
                .steps
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>(),
            source.layers[0]
                .mask
                .as_ref()
                .unwrap()
                .steps
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(e.doc.preview(None, 32, None, false).unwrap().2, expected);
        // Invert then gamma 2 gives 63; reversing execution would give 191.
        assert_eq!(expected[3], 63);
        assert_eq!(e.undo.len(), 1);
    }
    fn small_fixture() -> (PeerBrush, egui::Context) {
        let (mut app, ctx) = fixture();
        let doc = Document::new(32, 32).unwrap();
        let id = doc.layers[0].id.clone();
        app.shared.lock().unwrap().doc = doc;
        app.select_content(&id);
        (app, ctx)
    }
    fn modified_key(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) {
        for pressed in [true, false] {
            frame(
                app,
                ctx,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                }],
                modifiers,
            );
        }
    }
    #[test]
    fn layer_clipboard_intent_tracks_rows_and_canvas_without_touching_os_clipboard() {
        let (mut app, ctx) = small_fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        let id = app.selected.clone();
        let row = app.layer_rects[&id];
        click(&mut app, &ctx, row.right_center() - Vec2::new(30.0, 0.0));
        let doc = app.shared.lock().unwrap().doc.clone();
        assert!(app.layer_clipboard);
        assert!(
            matches!(app.copy_request(&doc, false, false), Some(crate::clipboard::Request::CopyLayers { ids, .. }) if ids == vec![id.clone()])
        );
        assert!(
            matches!(app.copy_request(&doc, true, false), Some(crate::clipboard::Request::CutLayers { doc: copied, ids, .. }) if copied.id == doc.id && copied.revision == doc.revision && ids == vec![id.clone()])
        );
        assert!(matches!(
            app.copy_request(&doc, false, true),
            Some(crate::clipboard::Request::Copy { merged: true, .. })
        ));
        app.tool = Tool::None;
        let canvas = app.view_rect.unwrap().center();
        click(&mut app, &ctx, canvas);
        assert!(!app.layer_clipboard);
        assert!(matches!(
            app.copy_request(&doc, false, false),
            Some(crate::clipboard::Request::Copy { merged: false, .. })
        ));
        assert!(app.copy_request(&doc, true, false).is_none());
        assert!(app.shared.lock().unwrap().undo.is_empty());
    }
    #[test]
    fn command_j_duplicates_all_selected_roots_in_one_undo_and_selects_copies() {
        let (mut app, ctx) = small_fixture();
        let original = {
            let mut e = app.shared.lock().unwrap();
            e.edit(
                "human",
                &[json!({"op":"layer.add","kind":"paint","name":"Second"})],
                None,
                None,
                "Setup",
            )
            .unwrap();
            e.undo.clear();
            e.doc
                .layers
                .iter()
                .map(|l| l.id.clone())
                .collect::<Vec<_>>()
        };
        app.selected = original[0].clone();
        app.selection_layers = original.iter().cloned().collect();
        app.layer_clipboard = true;
        modified_key(
            &mut app,
            &ctx,
            egui::Key::J,
            egui::Modifiers {
                command: true,
                ctrl: true,
                ..Default::default()
            },
        );
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.doc.layers.len(), 4);
        assert_eq!(app.selection_layers.len(), 2);
        assert!(app.selection_layers.iter().all(|id| !original.contains(id)));
        assert!(app.selection_layers.contains(&app.selected));
        assert_eq!(e.undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(
            e.doc
                .layers
                .iter()
                .map(|l| l.id.clone())
                .collect::<Vec<_>>(),
            original
        );
    }
    #[test]
    fn inline_rename_double_click_uses_text_focus_and_commits_one_edit() {
        let (mut app, ctx) = small_fixture();
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let layer = app.selected.clone();
        let row = app.layer_rects[&layer];
        let name = row.right_center() - Vec2::new(30.0, 0.0);
        click(&mut app, &ctx, name);
        click(&mut app, &ctx, name);
        assert!(
            app.rename_edit.is_some(),
            "Double-click must open the inline editor"
        );
        frame(&mut app, &ctx, vec![], Default::default());
        let edit = app.rename_edit.as_ref().unwrap();
        let text_id = egui::Id::new(("layer rename", &edit.document, &edit.layer));
        assert_eq!(ctx.memory(|m| m.focused()), Some(text_id));
        assert!(egui::TextEdit::load_state(&ctx, text_id).is_some());
        let tool = app.tool;
        let colors = (app.color, app.background);
        for key in [egui::Key::W, egui::Key::E, egui::Key::R, egui::Key::X] {
            self::key(&mut app, &ctx, key);
        }
        assert!(app.tool == tool);
        assert_eq!((app.color, app.background), colors);
        for key in [egui::Key::D, egui::Key::C, egui::Key::X, egui::Key::V] {
            modified_key(
                &mut app,
                &ctx,
                key,
                egui::Modifiers {
                    command: true,
                    ctrl: true,
                    ..Default::default()
                },
            );
        }
        assert!(app.shared.lock().unwrap().undo.is_empty());
        app.rename_edit.as_mut().unwrap().text = "Renamed artwork".into();
        key(&mut app, &ctx, egui::Key::Enter);
        assert!(app.rename_edit.is_none());
        let e = app.shared.lock().unwrap();
        assert_eq!(e.doc.layers[0].name, "Renamed artwork");
        assert_eq!(e.undo.len(), 1);
    }
    #[test]
    fn inline_folder_rename_escape_cancels_and_focus_loss_commits_once() {
        let (mut app, ctx) = small_fixture();
        let folder = {
            let mut e = app.shared.lock().unwrap();
            let result = e
                .edit(
                    "human",
                    &[json!({"op":"group.create_selected","layers":[],"name":"Folder"})],
                    None,
                    None,
                    "Setup",
                )
                .unwrap();
            e.undo.clear();
            result["created"][0].as_str().unwrap().to_string()
        };
        let doc = app.shared.lock().unwrap().doc.clone();
        app.begin_rename(&doc, &folder);
        frame(&mut app, &ctx, vec![], Default::default());
        app.rename_edit.as_mut().unwrap().text = "Discard me".into();
        key(&mut app, &ctx, egui::Key::Escape);
        assert!(app.rename_edit.is_none());
        assert!(app.shared.lock().unwrap().undo.is_empty());
        app.begin_rename(&doc, &folder);
        frame(&mut app, &ctx, vec![], Default::default());
        app.rename_edit.as_mut().unwrap().text = "Assets".into();
        let canvas = app.view_rect.unwrap().center();
        app.tool = Tool::None;
        click(&mut app, &ctx, canvas);
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app.rename_edit.is_none());
        let e = app.shared.lock().unwrap();
        assert_eq!(
            e.doc.layers.iter().find(|l| l.id == folder).unwrap().name,
            "Assets"
        );
        assert_eq!(e.undo.len(), 1);
    }
    #[test]
    fn footer_stays_below_effects_with_connected_color_mask_tabs() {
        let (mut app, ctx) = small_fixture();
        let id = app.selected.clone();
        let mut commands = vec![json!({"op":"mask.add","layer":id})];
        for _ in 0..12 {
            commands.push(json!({"op":"effect.add","layer":id,"kind":"levels"}));
            commands.push(json!({"op":"mask.step.add","layer":id,"kind":"levels"}));
        }
        app.shared
            .lock()
            .unwrap()
            .edit("human", &commands, None, None, "Stack")
            .unwrap();
        for mask in [false, true] {
            app.mask = mask;
            for _ in 0..3 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            Pos2::ZERO,
                            Vec2::new(1100.0, 600.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                );
            }
            let color = app.color_tab_rect.unwrap();
            let mask = app.mask_tab_rect.unwrap();
            assert!((color.center().y - mask.center().y).abs() < 1.0);
            assert!((0.0..=20.0).contains(&(mask.left() - color.right())));
            let effect = app.effect_area_rect.unwrap();
            let blend = app.blend_rect.unwrap();
            let opacity = app.opacity_rect.unwrap();
            assert!(blend.top() >= effect.bottom());
            assert!(opacity.top() >= blend.bottom());
            assert!(
                opacity.bottom() < 580.0,
                "Footer must remain visible: {opacity:?}"
            );
            assert!(effect.contains_rect(app.effect_add_rect.unwrap()));
        }
    }
    #[test]
    fn alt_backspace_floods_only_selection_with_foreground_and_one_undo() {
        let (mut app, ctx) = small_fixture();
        app.color = [240, 34, 72, 255];
        app.shared.lock().unwrap().doc.selection = Some([4, 5, 10, 11]);
        modified_key(
            &mut app,
            &ctx,
            egui::Key::Backspace,
            egui::Modifiers {
                alt: true,
                ..Default::default()
            },
        );
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.doc.layers[0].pixels.get(4, 5), app.color);
        assert_eq!(e.doc.layers[0].pixels.get(9, 10), app.color);
        assert_eq!(e.doc.layers[0].pixels.get(3, 5), [0; 4]);
        assert_eq!(e.doc.layers[0].pixels.get(10, 10), [0; 4]);
        assert_eq!(e.doc.layers[0].kind, "paint");
        assert_eq!(e.undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers[0].pixels.get(4, 5), [0; 4]);
    }
    #[test]
    fn new_folder_button_contains_selected_layers_and_selects_folder() {
        let (mut app, ctx) = small_fixture();
        let ids = {
            let mut e = app.shared.lock().unwrap();
            e.edit(
                "human",
                &[json!({"op":"layer.add","kind":"paint","name":"Second"})],
                None,
                None,
                "Setup",
            )
            .unwrap();
            e.undo.clear();
            e.doc
                .layers
                .iter()
                .map(|l| l.id.clone())
                .collect::<Vec<_>>()
        };
        app.selection_layers = ids.iter().cloned().collect();
        frame(&mut app, &ctx, vec![], Default::default());
        let folder = app.new_folder_rect.unwrap().center();
        click(&mut app, &ctx, folder);
        let e = app.shared.lock().unwrap();
        assert_eq!(e.doc.layers.len(), 3);
        assert_eq!(
            e.doc
                .layers
                .iter()
                .find(|l| l.id == app.selected)
                .unwrap()
                .kind,
            "group"
        );
        assert_eq!(
            app.selection_layers,
            [app.selected.clone()].into_iter().collect()
        );
        assert!(ids.iter().all(|id| e
            .doc
            .layers
            .iter()
            .find(|l| l.id == *id)
            .unwrap()
            .parent
            .as_deref()
            == Some(&app.selected)));
        assert_eq!(e.undo.len(), 1);
    }
    #[test]
    fn selection_ants_follow_the_rendered_transform_polygon_and_reject_stale_geometry() {
        let (mut app, _) = small_fixture();
        let mut doc = app.shared.lock().unwrap().doc.clone();
        doc.selection = Some([4, 4, 12, 12]);
        doc.selection_polygon = Some(vec![[8.0, 4.0], [12.0, 8.0], [8.0, 12.0], [4.0, 8.0]]);
        let rect = Rect::from_min_size(egui::pos2(20.0, 30.0), Vec2::splat(64.0));
        let preview_polygon = vec![[12.0, 5.0], [16.0, 9.0], [12.0, 13.0], [8.0, 9.0]];
        app.canvas_selection = Some(CanvasSelection {
            document: doc.id.clone(),
            revision: doc.revision,
            bounds: Some([8, 5, 16, 13]),
            polygon: Some(preview_polygon.clone()),
            coverage: None,
        });
        let points = app.selection_points(&doc, rect, 2.0);
        assert_eq!(
            points,
            preview_polygon
                .iter()
                .map(|p| rect.min + Vec2::new(p[0] * 2.0, p[1] * 2.0))
                .collect::<Vec<_>>()
        );
        doc.revision += 1;
        let points = app.selection_points(&doc, rect, 2.0);
        assert_eq!(
            points,
            doc.selection_polygon
                .as_ref()
                .unwrap()
                .iter()
                .map(|p| rect.min + Vec2::new(p[0] * 2.0, p[1] * 2.0))
                .collect::<Vec<_>>()
        );
        assert!(
            points[0].y != points[1].y,
            "Rotated edges cannot become the axis-aligned bounding rectangle"
        );
    }
    #[test]
    fn multiple_layer_gizmo_rotates_selected_pixels_around_selection_center() {
        let (mut app, ctx) = small_fixture();
        let ids = {
            let mut e = app.shared.lock().unwrap();
            e.edit(
                "human",
                &[json!({"op":"layer.add","kind":"paint"})],
                None,
                None,
                "Setup",
            )
            .unwrap();
            e.doc.selection = Some([4, 5, 12, 13]);
            e.undo.clear();
            e.doc
                .layers
                .iter()
                .map(|l| l.id.clone())
                .collect::<Vec<_>>()
        };
        app.selection_layers = ids.iter().cloned().collect();
        app.tool = Tool::Rotate;
        frame(&mut app, &ctx, vec![], Default::default());
        let rect = app.view_rect.unwrap();
        let scale = rect.width() / 32.0;
        let center = rect.min + Vec2::new(8.0 * scale, 9.0 * scale);
        let start = center + Vec2::new(58.0, 0.0);
        let end = center + Vec2::new(0.0, 58.0);
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(start),
                button(
                    start,
                    egui::PointerButton::Primary,
                    true,
                    Default::default(),
                ),
            ],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(end)],
            Default::default(),
        );
        let transforms = app
            .transient
            .iter()
            .filter(|c| c["op"] == "transform")
            .collect::<Vec<_>>();
        assert_eq!(
            transforms.len(),
            2,
            "The selection-centered rotation ring must be interactive"
        );
        for command in transforms {
            assert_eq!(command["pivot"], json!([8.0, 9.0]));
            assert_eq!(command["selection_only"], true);
            assert!((command["angle"].as_f64().unwrap() - 90.0).abs() < 0.01);
        }
        assert!(
            app.shared.lock().unwrap().undo.is_empty(),
            "Live rotation preview must not commit history"
        );
    }
    #[test]
    fn liquify_and_wet_blend_shortcuts_preserve_text_editing() {
        let (mut app, ctx) = small_fixture();
        key(&mut app, &ctx, egui::Key::L);
        assert!(app.tool == Tool::Liquify);
        key(&mut app, &ctx, egui::Key::U);
        assert!(app.tool == Tool::Smudge);
        let text_id = egui::Id::new("Liquify shortcut text guard");
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut text = String::new();
                ui.add(egui::TextEdit::singleline(&mut text).id(text_id))
                    .request_focus();
            });
        });
        key(&mut app, &ctx, egui::Key::L);
        assert!(app.tool == Tool::Smudge);
        assert!(app.shared.lock().unwrap().undo.is_empty());
    }
    #[test]
    fn liquify_and_wet_blend_show_actual_live_pixels_then_commit_the_identical_single_edit() {
        for (shortcut, tool) in [(egui::Key::L, Tool::Liquify), (egui::Key::U, Tool::Smudge)] {
            let (mut app, ctx) = small_fixture();
            let mut source = vec![];
            for _ in 0..32 {
                for x in 0..32 {
                    source.extend_from_slice(if x < 16 {
                        &[220, 20, 30, 255]
                    } else {
                        &[20, 60, 200, 255]
                    });
                }
            }
            {
                let mut engine = app.shared.lock().unwrap();
                engine.doc.layers[0].pixels =
                    crate::raster::Raster::from_rgba(32, 32, &source).unwrap();
            }
            key(&mut app, &ctx, shortcut);
            assert!(app.tool == tool);
            app.radius = 8.0;
            frame(&mut app, &ctx, vec![], Default::default());
            let rect = app.view_rect.unwrap();
            let scale = rect.width() / 32.0;
            let start = rect.min + Vec2::new(10.0 * scale, 16.0 * scale);
            let end = rect.min + Vec2::new(22.0 * scale, 16.0 * scale);
            frame(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(start),
                    button(
                        start,
                        egui::PointerButton::Primary,
                        true,
                        Default::default(),
                    ),
                ],
                Default::default(),
            );
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(end)],
                Default::default(),
            );
            assert!(app.points.len() >= 2);
            let command = if tool == Tool::Liquify {
                app.liquify_command(&app.points)
            } else {
                app.stroke_command(app.points.clone(), app.color)
            };
            let document = app.shared.lock().unwrap().doc.clone();
            let preview = Engine::preview_edits(document.clone(), &[command]).unwrap();
            let expected = preview.preview(None, 32, None, false).unwrap().2;
            assert_ne!(
                expected, source,
                "The gesture must change visible pixels while held"
            );
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while app
                .canvas_pixels
                .as_ref()
                .is_none_or(|(_, _, pixels)| *pixels != expected)
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(5));
                frame(&mut app, &ctx, vec![], Default::default());
            }
            assert_eq!(
                app.canvas_pixels.as_ref().unwrap().2,
                expected,
                "Rendered live preview must show the engine's actual edited pixels"
            );
            assert_eq!(app.shared.lock().unwrap().doc.revision, document.revision);
            assert!(app.shared.lock().unwrap().undo.is_empty());
            frame(
                &mut app,
                &ctx,
                vec![button(
                    end,
                    egui::PointerButton::Primary,
                    false,
                    Default::default(),
                )],
                Default::default(),
            );
            let mut engine = app.shared.lock().unwrap();
            assert_eq!(
                engine.doc.preview(None, 32, None, false).unwrap().2,
                expected
            );
            assert_eq!(engine.undo.len(), 1);
            if tool == Tool::Liquify {
                assert_eq!(engine.doc.layers[0].pixels.rgba(), source);
                assert_eq!(engine.doc.layers[0].effects[0].kind, "liquify");
            } else {
                assert_ne!(engine.doc.layers[0].pixels.rgba(), source);
            }
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.layers[0].pixels.rgba(), source);
        }
    }
    #[test]
    fn liquify_on_mask_does_not_silently_change_layer_color() {
        let (mut app, ctx) = small_fixture();
        let id = app.selected.clone();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"mask.add","layer":id})],
                None,
                None,
                "Setup",
            )
            .unwrap();
        app.shared.lock().unwrap().undo.clear();
        key(&mut app, &ctx, egui::Key::L);
        app.mask = true;
        let source = app.shared.lock().unwrap().doc.clone();
        frame(&mut app, &ctx, vec![], Default::default());
        let start = app.view_rect.unwrap().center();
        let end = start + Vec2::new(25.0, 0.0);
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(start),
                button(
                    start,
                    egui::PointerButton::Primary,
                    true,
                    Default::default(),
                ),
            ],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(end)],
            Default::default(),
        );
        frame(
            &mut app,
            &ctx,
            vec![button(
                end,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
            Default::default(),
        );
        let engine = app.shared.lock().unwrap();
        assert!(engine.doc.layers[0].effects.is_empty());
        assert_eq!(
            engine.doc.layers[0].pixels.rgba(),
            source.layers[0].pixels.rgba()
        );
        assert!(engine.undo.is_empty());
        assert!(
            app.message.contains("Color"),
            "Explain the unsupported mask channel compactly"
        );
    }
    #[test]
    fn command_d_deselects_in_layer_focus_and_shift_d_reselects() {
        let (mut app, ctx) = small_fixture();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"selection","kind":"ellipse","rect":[2,2,10,10]})],
                None,
                None,
                "Select",
            )
            .unwrap();
        app.layer_clipboard = true;
        let count = app.shared.lock().unwrap().doc.layers.len();
        modified_key(
            &mut app,
            &ctx,
            egui::Key::D,
            egui::Modifiers {
                command: true,
                ctrl: true,
                ..Default::default()
            },
        );
        assert!(app.shared.lock().unwrap().doc.selection.is_none());
        assert_eq!(app.shared.lock().unwrap().doc.layers.len(), count);
        modified_key(
            &mut app,
            &ctx,
            egui::Key::D,
            egui::Modifiers {
                command: true,
                ctrl: true,
                shift: true,
                ..Default::default()
            },
        );
        assert_eq!(
            app.shared.lock().unwrap().doc.selection,
            Some([2, 2, 10, 10])
        );
    }
    #[test]
    fn completed_gestures_reject_old_revision_previews_even_when_given_old_ui_snapshot() {
        for op in [
            "paint",
            "move",
            "transform",
            "layer.reorder",
            "layer.update",
        ] {
            let (mut app, ctx) = small_fixture();
            let source = app.shared.lock().unwrap().doc.clone();
            let layer = app.selected.clone();
            let command = match op {
                "paint" => {
                    json!({"op":op,"layer":layer,"points":[[4,4],[10,10]],"radius":2,"color":[255,0,0,255]})
                }
                "move" => json!({"op":op,"layer":layer,"dx":2,"dy":1}),
                "transform" => json!({"op":op,"layer":layer,"angle":15,"scale_x":1,"scale_y":1}),
                "layer.reorder" => json!({"op":op,"layer":layer,"index":0}),
                _ => json!({"op":op,"layer":layer,"visible":false}),
            };
            app.shared
                .lock()
                .unwrap()
                .edit("human", &[command], None, None, "Commit gesture")
                .unwrap();
            app.set_canvas(&ctx, 1, 1, vec![111, 22, 33, 255]);
            app.pending = true;
            app.preview_tx
                .send(Preview {
                    live: String::new(),
                    doc_id: source.id.clone(),
                    revision: source.revision,
                    w: 1,
                    h: 1,
                    bytes: vec![0, 0, 0, 255],
                    dirty: None,
                    selection: None,
                    selection_polygon: None,
                    selection_coverage: None,
                    target: format!("{};;1536", app.preview_variant()),
                    mask: false,
                })
                .unwrap();
            app.request_preview(&ctx, &source);
            assert_eq!(
                app.canvas_pixels.as_ref().unwrap().2,
                vec![111, 22, 33, 255],
                "{op} flashed an old image"
            );
            assert_eq!(app.last_preview.as_ref().unwrap().1, source.revision + 1);
        }
    }
    #[test]
    fn fast_native_control_chord_uses_event_modifiers_after_control_is_released() {
        let (mut app, ctx) = small_fixture();
        app.shared.lock().unwrap().doc.selection = Some([2, 2, 8, 8]);
        app.tool = Tool::Selection;
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::D,
                physical_key: Some(egui::Key::D),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            }],
            Default::default(),
        );
        assert!(app.shared.lock().unwrap().doc.selection.is_none());
        assert!(app.tool == Tool::Selection);
    }
}
