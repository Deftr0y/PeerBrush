use crate::icons::{self, Icon};
mod animation;
mod brush;
mod color;
mod effects;
mod filters;
mod geometry;
mod history;
mod layers;
mod lifecycle;
mod liquify;
mod recovery;
mod refinement;
mod retouch;
mod selection;
mod source;
mod tabs;
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
        Tool::Clone => Icon::Clone,
        Tool::Heal => Icon::Heal,
        Tool::Liquify => Icon::Liquify,
        Tool::Eraser => Icon::Eraser,
        Tool::Fill => Icon::Fill,
        Tool::Gradient => Icon::Gradient,
        Tool::Rectangle => Icon::Rectangle,
        Tool::Ellipse => Icon::Ellipse,
        Tool::Selection => Icon::Selection,
        Tool::Picker => Icon::Picker,
        Tool::Pan => Icon::Pan,
        Tool::MagicWand => Icon::Wand,
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
    Clone,
    Heal,
    Liquify,
    Eraser,
    Fill,
    Rectangle,
    Ellipse,
    Selection,
    Picker,
    Pan,
    Gradient,
    MagicWand,
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
            Self::Clone => "Clone · C",
            Self::Heal => "Heal · J",
            Self::Liquify => "Liquify · L",
            Self::Eraser => "Eraser",
            Self::Fill => "Fill",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Selection => "Select",
            Self::Picker => "Pick color",
            Self::Pan => "Pan",
            Self::Gradient => "Gradient",
            Self::MagicWand => "Magic Wand · K",
        }
    }
}
struct Preview {
    coarse: bool,
    error: Option<String>,
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
    project: String,
    document: String,
    result: Result<Value, String>,
}
struct ImportFile {
    path: PathBuf,
    encoded: crate::image_import::Encoded,
    index: usize,
}
struct PendingImport {
    project: String,
    document: String,
    revision: u64,
    files: Vec<ImportFile>,
}
struct ImportReply {
    project: String,
    document: String,
    revision: u64,
    result: Result<Vec<ImportFile>, String>,
}
struct JobReply {
    project: String,
    result: Result<String, String>,
}
#[derive(Clone, PartialEq)]
struct TabTransfer {
    source: String,
    destination: String,
    source_document: String,
    destination_document: String,
    source_revision: u64,
    destination_revision: u64,
    layers: Vec<String>,
    target: String,
    move_layers: bool,
}
impl TabTransfer {
    fn request(&self) -> crate::workspace::Transfer<'_> {
        crate::workspace::Transfer {
            source: &self.source,
            destination: &self.destination,
            source_document: &self.source_document,
            destination_document: &self.destination_document,
            source_revision: self.source_revision,
            destination_revision: self.destination_revision,
            layers: &self.layers,
            target: &self.target,
            move_layers: self.move_layers,
            actor: "human",
            source_task: None,
            destination_task: None,
        }
    }
}
enum TransferOutput {
    Preview(Result<(Document, u32, u32, Vec<u8>), String>),
    Commit(Result<Value, String>),
}
struct TransferReply {
    spec: TabTransfer,
    output: TransferOutput,
}
struct ProjectView {
    selected: String,
    selection_layers: HashSet<String>,
    selection_anchor: String,
    mask: bool,
    mask_step: Option<String>,
    effect_selected: Option<(String, bool, String)>,
    isolate: bool,
    collapsed: HashSet<String>,
    zoom: f32,
    pan: Vec2,
    message: String,
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
struct ParameterEdit {
    control: egui::Id,
    gesture: String,
    document: String,
    revision: u64,
    commands: Vec<Value>,
    label: String,
    typing: Option<egui::Context>,
    dirty: bool,
}
pub struct PeerBrush {
    shared: Shared,
    workspace_root: Shared,
    workspace: crate::workspace::Registry,
    project_id: String,
    project_views: HashMap<String, ProjectView>,
    project_rects: HashMap<String, Rect>,
    project_labels: HashMap<String, tabs::Label>,
    tab_active: String,
    project_picker_rect: Option<Rect>,
    doc_snapshot: Document,
    jobs: HashSet<String>,
    lifecycle: Option<lifecycle::Review>,
    recovery: recovery::Browser,
    lifecycle_frame: bool,
    exit_approved: bool,
    connected: bool,
    tab_transfer: Option<TabTransfer>,
    tab_preview_requested: Option<TabTransfer>,
    tab_preview_pending: bool,
    tab_transfer_committing: bool,
    tab_preview: Option<(Document, TextureHandle)>,
    transfer_tx: mpsc::Sender<TransferReply>,
    transfer_rx: mpsc::Receiver<TransferReply>,
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
    geometry: Option<geometry::Editor>,
    source_editor: Option<source::Editor>,
    filter_editor: Option<filters::Editor>,
    task_undo_review: Option<Value>,
    proposal_review: Option<String>,
    proposal_original: bool,
    proposal_error: Option<String>,
    proposal_rendered: Option<String>,
    refinement: Option<refinement::Editor>,
    retouch_source: Option<retouch::Anchor>,
    retouch_aligned: bool,
    retouch_merged: bool,
    retouch_revision: Option<u64>,
    retouch_pick: bool,
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
    brush_search: String,
    brush_category: String,
    brush_preset: Option<String>,
    brush_name: String,
    brush_save_category: String,
    brush_thumbnails: HashMap<String, (crate::brush::Settings, TextureHandle)>,
    transform_rects: [Option<Rect>; 4],
    color_editor: Option<color::Editor>,
    layer_drag: Option<LayerDrag>,
    layer_rects: HashMap<String, Rect>,
    eye_rects: HashMap<String, Rect>,
    layer_context: Option<(String, egui::menu::MenuRootManager)>,
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
    parameter_gesture: Option<ParameterEdit>,
    blend_hover: Option<(String, String)>,
    transient: Vec<Value>,
    points: Vec<[f32; 2]>,
    drag_start: Option<[f32; 2]>,
    view_rect: Option<Rect>,
    message: String,
    last_status: String,
    import_tx: mpsc::Sender<ImportReply>,
    import_rx: mpsc::Receiver<ImportReply>,
    pending_import: Option<PendingImport>,
    job_rx: mpsc::Receiver<JobReply>,
    job_tx: mpsc::Sender<JobReply>,
    busy: bool,
    load_control: Option<crate::loading::Control>,
    load_preview: Option<TextureHandle>,
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
    effect_selected: Option<(String, bool, String)>,
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
        // Ember is the workspace palette even when the OS reports light mode.
        ctx.set_theme(egui::Theme::Dark);
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
        let (import_tx, import_rx) = mpsc::channel();
        let (connect_tx, connect_rx) = mpsc::channel();
        let (merge_tx, merge_rx) = mpsc::channel();
        let (transfer_tx, transfer_rx) = mpsc::channel();
        let workspace = crate::workspace::attach(&shared);
        let (project_id, doc_snapshot) = {
            let e = shared.lock().unwrap();
            (e.project_id.clone(), e.doc.clone())
        };
        Self {
            recovery: recovery::Browser::new(&connection.state_dir),
            workspace_root: shared.clone(),
            workspace,
            project_id,
            project_views: HashMap::new(),
            project_rects: HashMap::new(),
            project_labels: HashMap::new(),
            tab_active: String::new(),
            project_picker_rect: None,
            doc_snapshot,
            jobs: HashSet::new(),
            lifecycle: None,
            lifecycle_frame: false,
            exit_approved: false,
            connected: false,
            tab_transfer: None,
            tab_preview_requested: None,
            tab_preview_pending: false,
            tab_transfer_committing: false,
            tab_preview: None,
            transfer_tx,
            transfer_rx,
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
            geometry: None,
            source_editor: None,
            filter_editor: None,
            task_undo_review: None,
            proposal_review: None,
            proposal_original: false,
            proposal_error: None,
            proposal_rendered: None,
            refinement: None,
            retouch_source: None,
            retouch_aligned: true,
            retouch_merged: false,
            retouch_revision: None,
            retouch_pick: false,
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
            brush_search: String::new(),
            brush_category: "All".into(),
            brush_preset: None,
            brush_name: "My brush".into(),
            brush_save_category: "Paint".into(),
            brush_thumbnails: HashMap::new(),
            transform_rects: [None; 4],
            color_editor: None,
            layer_drag: None,
            layer_rects: HashMap::new(),
            eye_rects: HashMap::new(),
            layer_context: None,
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
            points: vec![],
            drag_start: None,
            view_rect: None,
            message: "Ready".into(),
            last_status: "Ready".into(),
            import_tx,
            import_rx,
            pending_import: None,
            job_rx,
            job_tx,
            busy: false,
            load_control: None,
            load_preview: None,
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
            effect_selected: None,
            activity_text: String::new(),
            activity_changed: std::time::Instant::now(),
            gizmo_handle: 0,
            drag_revision: None,
            gizmo_bounds: None,
        }
    }
    fn edit(&mut self, commands: Vec<Value>, label: &str) {
        self.close_proposal();
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
    fn select_tool(&mut self, tool: Tool) {
        self.tool = tool;
        if tool == Tool::MagicWand {
            self.selection_kind = "wand".into();
        }
        self.selection_path.clear();
        self.selection_gesture_mode = None;
        self.drag_start = None;
        self.drag_revision = None;
        self.gizmo_bounds = None;
        self.gizmo_handle = 0;
        self.points.clear();
        self.stroke_pressures.clear();
        self.stroke_has_pressure = false;
        self.current_pressure = None;
        self.transient.clear();
        self.size_drag = None;
        self.live_gesture = None;
        self.animation.cancel();
        self.last_preview = None;
    }
    fn transform_controls(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 3.0;
        // Standard button minimums exceed the footer's inner height.
        let height = ui.available_height().clamp(14.0, 20.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (index, (tool, key)) in [
                (Tool::None, "Q"),
                (Tool::Move, "W"),
                (Tool::Rotate, "E"),
                (Tool::Scale, "R"),
            ]
            .into_iter()
            .enumerate()
            .rev()
            {
                let (_, response) =
                    ui.allocate_exact_size(Vec2::new(34.0, height), egui::Sense::click());
                let response = response.on_hover_text(tool.label());
                self.transform_rects[index] = Some(response.rect);
                let active = self.tool == tool;
                if active || response.hovered() {
                    ui.painter().rect_filled(
                        response.rect,
                        2,
                        if active {
                            Color32::from_rgb(74, 48, 71)
                        } else {
                            Color32::from_white_alpha(8)
                        },
                    );
                }
                let ai = matches!(
                    (self.animation.tool(ui.input(|i| i.time)), tool),
                    (Some("move"), Tool::Move)
                        | (Some("rotate"), Tool::Rotate)
                        | (Some("scale"), Tool::Scale)
                );
                if ai {
                    ui.painter()
                        .rect_filled(response.rect, 2, AI_BLUE.gamma_multiply(0.12));
                    ui.painter().line_segment(
                        [response.rect.left_top(), response.rect.right_top()],
                        Stroke::new(1.5, AI_BLUE),
                    );
                }
                if active {
                    ui.painter().line_segment(
                        [response.rect.left_bottom(), response.rect.right_bottom()],
                        Stroke::new(1.5, ACCENT),
                    );
                }
                paint_tool(
                    ui.painter(),
                    Rect::from_center_size(
                        response.rect.left_center() + Vec2::new(10., 0.),
                        Vec2::splat(height.min(17.)),
                    ),
                    tool,
                    MUTED,
                );
                ui.painter().text(
                    response.rect.right_center() - Vec2::new(3., 0.),
                    egui::Align2::RIGHT_CENTER,
                    key,
                    egui::FontId::proportional(10.),
                    if ai {
                        AI_BLUE
                    } else if active {
                        ACCENT
                    } else {
                        MUTED
                    },
                );
                if response.clicked() {
                    self.select_tool(tool);
                }
            }
        });
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
        // An active pixel selection takes precedence over the last layer-row click.
        if self.layer_clipboard && doc.selection.is_none() && (cut || !merged) {
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
            doc.selection.map(|_| crate::clipboard::Request::Cut {
                shared: self.shared.clone(),
                doc: doc.clone(),
                target: self.selected.clone(),
                mask: self.mask,
                step: self.mask_step.clone(),
            })
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
        if self.busy {
            return;
        }
        self.close_proposal();
        self.finish_parameter(true);
        let ids = self.selected_layer_ids(doc);
        let result = {
            let mut engine = self.shared.lock().unwrap();
            if engine.doc.id != doc.id {
                Err("The document changed before grouping layers".into())
            } else if engine.doc.revision != doc.revision {
                Err("The project revision changed before grouping layers".into())
            } else {
                engine.edit(
                    "human",
                    &[json!({"op":"group.create_selected","layers":ids,"name":"Folder"})],
                    Some(doc.revision),
                    None,
                    "Group selected layers",
                )
            }
        };
        match result {
            Ok(result) => {
                if let Some(id) = result["created"]
                    .as_array()
                    .and_then(|ids| ids.first())
                    .and_then(Value::as_str)
                {
                    self.select_content(id);
                    self.collapsed.remove(id);
                }
                self.layer_clipboard = true;
                self.last_preview = None;
                self.message = "Grouped selected layers".into();
            }
            Err(error) => self.message = error,
        }
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
    fn duplicate_selection(&mut self, doc: &Document) {
        self.close_proposal();
        let result = self.shared.lock().unwrap().edit(
            "human",
            &[json!({"op":"layer.duplicate_selection","layer":self.selected,"document_id":doc.id,"source_revision":doc.revision})],
            Some(doc.revision), None, "Duplicate layer selection",
        );
        match result {
            Ok(result) => {
                if let Some(id) = result["created_roots"]
                    .as_array()
                    .and_then(|ids| ids.first())
                    .and_then(Value::as_str)
                {
                    self.select_content(id);
                }
                self.layer_clipboard = true;
                self.message = "Duplicated layer selection".into();
                self.last_preview = None;
            }
            Err(error) => self.message = error,
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
    fn finish_parameter(&mut self, cancel: bool) {
        let Some(edit) = self.parameter_gesture.take() else {
            return;
        };
        self.last_preview = None;
        if let Some(ctx) = &edit.typing {
            controls::cancel_number_edit(ctx, edit.control);
        }
        if cancel || !edit.dirty {
            return;
        }
        let mut engine = self.shared.lock().unwrap();
        if engine.doc.id != edit.document || engine.doc.revision != edit.revision {
            self.message = "Image changed during parameter edit · preview cancelled".into();
            return;
        }
        self.message = engine
            .edit(
                "human",
                &edit.commands,
                Some(edit.revision),
                None,
                &edit.label,
            )
            .map(|_| edit.label)
            .unwrap_or_else(|error| error);
    }
    fn layer_parameter(&mut self, extra: Value, label: &str, response: &egui::Response) {
        let down = response.is_pointer_button_down_on() || response.dragged();
        let mut command = extra;
        command["op"] = json!(if command.get("filter").is_some() {
            "filter.update"
        } else if command.get("effect").is_some() {
            "effect.update"
        } else if command.get("step").is_some() {
            "mask.step.update"
        } else {
            "layer.update"
        });
        if command["op"] != "filter.update" {
            command["layer"] = json!(self.selected);
        }
        let mut commands = if command["op"] == "layer.update" {
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
        if response.double_clicked() && controls::number_editing(&response.ctx, response.id) {
            self.finish_parameter(false);
            let doc = self.shared.lock().unwrap().doc.clone();
            self.parameter_gesture = Some(ParameterEdit {
                control: response.id,
                gesture: crate::engine::id(),
                document: doc.id,
                revision: doc.revision,
                commands,
                label: label.into(),
                typing: Some(response.ctx.clone()),
                dirty: false,
            });
            self.animation.cancel();
            return;
        }
        if self
            .parameter_gesture
            .as_ref()
            .is_some_and(|edit| edit.control == response.id && edit.typing.is_some())
        {
            if response.changed() {
                let edit = self.parameter_gesture.as_mut().unwrap();
                edit.commands = commands;
                edit.dirty = true;
                self.finish_parameter(false);
            }
            return;
        }
        if down {
            if self
                .parameter_gesture
                .as_ref()
                .is_some_and(|edit| edit.control != response.id)
            {
                self.finish_parameter(false);
            }
            let doc = self.shared.lock().unwrap().doc.clone();
            let (document, revision) = self
                .parameter_gesture
                .as_ref()
                .map(|edit| (edit.document.clone(), edit.revision))
                .unwrap_or((doc.id.clone(), doc.revision));
            for command in &mut commands {
                command["document_id"] = json!(document);
                command["source_revision"] = json!(revision);
            }
            match crate::engine::Engine::preview_edits(doc, &commands) {
                Ok(_) => {
                    let gesture = self
                        .parameter_gesture
                        .as_ref()
                        .map(|edit| edit.gesture.clone())
                        .unwrap_or_else(crate::engine::id);
                    self.parameter_gesture = Some(ParameterEdit {
                        control: response.id,
                        gesture,
                        document,
                        revision,
                        commands,
                        label: label.into(),
                        typing: None,
                        dirty: true,
                    });
                    self.animation.cancel();
                }
                Err(error) => {
                    self.parameter_gesture = None;
                    self.message = error;
                }
            }
        } else {
            self.finish_parameter(true);
            self.message = self
                .shared
                .lock()
                .unwrap()
                .edit("human", &commands, None, None, label)
                .map(|_| label.into())
                .unwrap_or_else(|error| error);
        }
        self.last_preview = None;
    }
    fn job<F: FnOnce() -> Result<String, String> + Send + 'static>(&mut self, f: F) {
        if self.busy {
            return;
        }
        self.busy = true;
        let project = self.project_id.clone();
        self.jobs.insert(project.clone());
        let tx = self.job_tx.clone();
        std::thread::spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
                    Err("File operation could not complete; current work preserved".into())
                });
            let _ = tx.send(JobReply { project, result });
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
        self.close_proposal();
        self.finish_parameter(true);
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
        let project = self.project_id.clone();
        let revision = doc.revision;
        let shared = self.shared.clone();
        let tx = self.merge_tx.clone();
        let ctx = ctx.clone();
        self.busy = true;
        self.jobs.insert(project.clone());
        self.message = "Merging layers…".into();
        if let Err(error) = std::thread::Builder::new()
            .name("peerbrush-merge".into())
            .spawn(move || {
                let result = crate::merge::edit_guarded(
                    &shared,
                    "human",
                    &ids,
                    Some(revision),
                    None,
                    None,
                    Some(&document),
                );
                let _ = tx.send(MergeReply {
                    project,
                    document,
                    result,
                });
                ctx.request_repaint();
            })
        {
            self.busy = false;
            self.jobs.remove(&self.project_id);
            self.message = format!("Could not start merging layers: {error}");
        }
    }
    fn open(&mut self) {
        if self.busy {
            return;
        }
        self.open_job(None, false);
    }
    fn open_job(&mut self, path: Option<PathBuf>, recover: bool) {
        if self.busy {
            return;
        }
        let s = Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
        let workspace = self.workspace.clone();
        let initiating = self.project_id.clone();
        let control = crate::loading::Control::default();
        {
            let e = s.lock().unwrap();
            control.bind(e.doc.id.clone(), e.doc.revision);
        }
        self.load_control = Some(control.clone());
        self.load_preview = None;
        self.job(move || {
            let path = path.or_else(|| {
                rfd::FileDialog::new()
                    .add_filter("Photoshop document", &["psd"])
                    .pick_file()
            });
            let Some(path) = path else {
                return Ok("Open canceled · current projects preserved".into());
            };
            server::open_progress(&s, &path, &control)?;
            if recover {
                let mut e = s.lock().unwrap();
                e.path = None;
                e.file_version = None;
                e.saved_revision = u64::MAX;
            }
            let engine = s.lock().unwrap().clone();
            crate::workspace::register_in(&workspace, engine, Some(&initiating))?;
            Ok(if recover {
                "Recovered autosave — save to your chosen PSD"
            } else {
                "PSD opened"
            }
            .into())
        });
    }
    fn save(&mut self, save_as: bool) {
        let s = self.shared.clone();
        let document = self.doc_snapshot.id.clone();
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
                return Ok("Save canceled · project remains open".into());
            };
            server::save_source(&s, &path, &document)?;
            Ok("PSD saved".into())
        });
    }
    fn export(&mut self) {
        let s = self.shared.clone();
        let document = self.doc_snapshot.id.clone();
        self.job(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("PNG", &["png"])
                .set_file_name("PeerBrush.png")
                .save_file()
            else {
                return Ok("Ready".into());
            };
            server::export_source(&s, &path, &document)?;
            Ok("PNG exported".into())
        });
    }
    fn import(&mut self) {
        self.begin_import(None);
    }
    fn begin_import(&mut self, paths: Option<Vec<PathBuf>>) {
        if self.busy || self.pending_import.is_some() {
            self.message = "Finish or cancel the current import first".into();
            return;
        }
        self.close_proposal();
        self.finish_parameter(true);
        let project = self.project_id.clone();
        let (document, revision) = {
            let e = self.shared.lock().unwrap();
            (e.doc.id.clone(), e.doc.revision)
        };
        self.busy = true;
        self.jobs.insert(project.clone());
        let tx = self.import_tx.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let paths = paths.or_else(|| {
                    rfd::FileDialog::new()
                        .add_filter("Images", crate::image_import::EXTENSIONS)
                        .pick_files()
                });
                let Some(paths) = paths else {
                    return Ok(Vec::new());
                };
                if paths.len() > 16 {
                    return Err("Import at most 16 images at once".into());
                }
                let mut bytes = 0usize;
                paths
                    .into_iter()
                    .map(|path| {
                        let encoded = crate::image_import::Encoded::file(&path)?;
                        bytes += encoded.encoded_len();
                        if bytes > 128 * 1024 * 1024 {
                            return Err("Encoded image batch exceeds 128 MiB".into());
                        }
                        Ok(ImportFile {
                            path,
                            encoded,
                            index: 0,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()
            }))
            .unwrap_or_else(|_| {
                Err("Image import could not complete; current work preserved".into())
            });
            let _ = tx.send(ImportReply {
                project,
                document,
                revision,
                result,
            });
        });
    }
    fn commit_import(&mut self) {
        if self.busy {
            return;
        }
        let Some(pending) = self.pending_import.take() else {
            return;
        };
        if pending.project != self.project_id {
            self.pending_import = Some(pending);
            return;
        }
        let shared = self.shared.clone();
        self.job(move || {
            crate::workspace::guard(&shared.lock().unwrap(),&pending.document,pending.revision)?;
            let mut budget = crate::image_import::BUDGET; let mut commands = Vec::new(); let mut pixels = Vec::new();
            for file in pending.files {
                let mut command = json!({"op":"image.import","path":file.path,"document_id":pending.document,"source_revision":pending.revision});
                if let Some(choice) = file.encoded.info.choice {command[choice]=json!(file.index);}
                let image = file.encoded.prepare(&command,budget)?;
                budget = budget.checked_sub(image.pixels.stored_bytes()).ok_or("Image batch exceeds 256 MiB")?;
                commands.push(command); pixels.push(image);
            }
            let count = commands.len(); let mut engine = shared.lock().unwrap();
            if engine.doc.id != pending.document || engine.doc.revision != pending.revision { return Err("The source project changed; import the images again".into()); }
            engine.import_images(&commands,pixels,pending.revision)?;
            Ok(format!("Imported {count} image layers"))
        });
    }
    fn import_dialog(&mut self, ctx: &egui::Context) {
        if self
            .pending_import
            .as_ref()
            .is_some_and(|p| crate::workspace::get_in(&self.workspace, &p.project).is_err())
        {
            self.pending_import = None;
            self.message = "Import source project was closed; current work preserved".into();
        }
        let Some(pending) = &mut self.pending_import else {
            return;
        };
        if pending.project != self.project_id {
            return;
        }
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("Choose image content")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Choose a frame, page or icon variant for each image.");
                for file in &mut pending.files {
                    ui.horizontal(|ui| {
                        ui.label(file.path.file_name().unwrap_or_default().to_string_lossy());
                        if let Some(choice) = file.encoded.info.choice {
                            egui::ComboBox::from_id_salt((&file.path, choice))
                                .selected_text(format!(
                                    "{choice} {} of {}",
                                    file.index + 1,
                                    file.encoded.info.count
                                ))
                                .show_ui(ui, |ui| {
                                    for index in 0..file.encoded.info.count {
                                        ui.selectable_value(
                                            &mut file.index,
                                            index,
                                            format!("{choice} {}", index + 1),
                                        );
                                    }
                                });
                        } else {
                            ui.weak(&file.encoded.info.format);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    apply = ui
                        .add_enabled(!self.busy, egui::Button::new("Import"))
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if cancel || !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.pending_import = None;
            self.message = "Import canceled".into();
        } else if apply {
            self.commit_import();
        }
    }
    fn drop_files(&mut self, paths: Vec<PathBuf>) {
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
            self.message = "Drop one PSD to open it, or image files to add layers".into();
        } else if psds == 1 {
            self.open_job(Some(paths[0].clone()), false);
        } else if !paths.is_empty() {
            self.begin_import(Some(paths));
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
        if let Some(id) = &self.proposal_review {
            return format!("proposal:{id}:{}", self.proposal_original);
        }
        let target = if self.isolate {
            self.selected.as_str()
        } else {
            ""
        };
        let stroke = if [
            Tool::Brush,
            Tool::Eraser,
            Tool::Smudge,
            Tool::Clone,
            Tool::Heal,
        ]
        .contains(&self.tool)
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
            "{target};{:?};{};{};{}",
            self.blend_hover,
            stroke.map(|v| v.to_string()).unwrap_or_default(),
            serde_json::to_string(&(
                &self.transient,
                self.parameter_gesture.as_ref().map(|edit| &edit.commands)
            ))
            .unwrap(),
            self.refinement
                .as_ref()
                .map(|r| r.preview.as_str())
                .unwrap_or("")
        )
    }
    fn live_key(&self) -> String {
        if let Some(edit) = &self.parameter_gesture {
            return format!(
                "parameter:{}:{}:{}",
                edit.gesture, edit.document, edit.revision
            );
        }
        if let Some(id) = &self.proposal_review {
            return format!("proposal:{id}:{}", self.proposal_original);
        }
        if let Some(editor) = &self.refinement {
            return format!(
                "refinement:{}:{}:{}",
                editor.gesture, editor.document, editor.revision
            );
        }
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
        if self
            .filter_editor
            .as_ref()
            .is_some_and(|e| e.document != current.id || e.revision != current.revision)
        {
            self.cancel_filters();
            self.message = "Filter preview cancelled · project changed".into();
        }
        if self
            .source_editor
            .as_ref()
            .is_some_and(|e| e.document != current.id || e.revision != current.revision)
        {
            self.cancel_source();
        }
        if self
            .geometry
            .as_ref()
            .is_some_and(|g| g.document != current.id || g.revision != current.revision)
        {
            self.cancel_geometry();
        }
        if self
            .refinement
            .as_ref()
            .is_some_and(|r| r.document != current.id || r.revision != current.revision)
            || (self.refinement.is_some()
                && (!self.points.is_empty()
                    || self.gizmo_handle != 0
                    || self.layer_drag.is_some()
                    || self.eye_sweep.is_some()
                    || self.blend_hover.is_some()
                    || self
                        .transient
                        .iter()
                        .any(|c| c["op"] != "selection.refine" && c["op"] != "mask.refine")))
        {
            self.cancel_refinement();
        }
        let doc = &current;
        let proposal_doc = self.review_document(&current);
        let preview_mask = self.mask && self.isolate && self.proposal_review.is_none();
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
            if p.doc_id != doc.id {
                continue;
            }
            if !p.coarse {
                self.pending = false;
            }
            if p.doc_id == doc.id
                && p.revision == doc.revision
                && (p.target == variant || (!live.is_empty() && p.live == live))
                && p.mask == preview_mask
                && ((self.source_editor.is_none() && self.filter_editor.is_none())
                    || p.live == live)
            {
                if let Some(editor) = &mut self.geometry {
                    editor.error = p.error.clone();
                }
                if let Some(editor) = &mut self.source_editor {
                    editor.error = p.error.clone();
                }
                if let Some(editor) = &mut self.filter_editor {
                    editor.error = p.error.clone();
                }
                if self.proposal_review.is_some() {
                    self.proposal_error = p.error.clone();
                    if p.error.is_none() && !p.bytes.is_empty() {
                        self.proposal_rendered = Some(p.live.clone());
                    }
                }
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
        let key = (doc.id.clone(), doc.revision, variant.clone(), preview_mask);
        if !self.pending && self.last_preview.as_ref() != Some(&key) {
            self.pending = true;
            self.last_preview = Some(key);
            let mut doc = proposal_doc.unwrap_or_else(|| self.animation.document(doc, time, true));
            doc.revision = current.revision;
            let mut commands = self.transient.clone();
            if let Some(edit) = &self.parameter_gesture {
                commands.extend(edit.commands.clone());
            }
            if let Some((layer, blend)) = &self.blend_hover {
                commands.push(json!({"op":"layer.update","layer":layer,"blend":blend}));
            }
            if [
                Tool::Brush,
                Tool::Eraser,
                Tool::Smudge,
                Tool::Clone,
                Tool::Heal,
            ]
            .contains(&self.tool)
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
            if self.proposal_review.is_some() {
                commands.clear();
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
            let layer = if self.isolate && self.proposal_review.is_none() {
                Some(self.selected.clone())
            } else {
                None
            };
            let mask = self.mask && self.isolate && self.proposal_review.is_none();
            let refinement = self
                .refinement
                .as_ref()
                .map(|r| (r.target.clone(), r.preview.clone()));
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
                    // A coarse shared-engine frame reaches the canvas before detailed sampling.
                    // Derived only: no source changes, history, or 16-bit conversion.
                    if dirty.is_none()
                        && refinement.is_none()
                        && edge > 512
                        && doc.width.max(doc.height) > 1024
                    {
                        if let Ok((w, h, bytes, _)) = doc.preview(None, 256, layer.as_deref(), mask)
                        {
                            let _ = tx.send(Preview {
                                coarse: true,
                                error: None,
                                live: live.clone(),
                                doc_id: id.clone(),
                                revision,
                                target: variant.clone(),
                                mask,
                                w,
                                h,
                                bytes,
                                dirty: None,
                                selection: doc.selection,
                                selection_polygon: doc.selection_polygon.clone(),
                                selection_coverage: doc.selection_coverage.clone(),
                            });
                            ctx.request_repaint();
                        }
                    }
                    let mut rendered = cache
                        .lock()
                        .map_err(|_| "Preview worker cache unavailable")?
                        .render(&doc, &live, dirty, edge, layer.as_deref(), mask)?;
                    if let Some((target, mode)) = &refinement {
                        if target.is_none() {
                            crate::selection::display::apply(
                                &doc,
                                rendered.width,
                                rendered.height,
                                &mut rendered.bytes,
                                mode,
                            );
                            rendered.dirty = None;
                        } else if mode == "mask" {
                            let (w, h, bytes, _) =
                                doc.preview(None, edge, target.as_deref(), true)?;
                            rendered.width = w;
                            rendered.height = h;
                            rendered.bytes = bytes;
                            rendered.dirty = None;
                        }
                    }
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
                let error = output.as_ref().err().cloned();
                let (w, h, bytes, dirty, selection, selection_polygon, selection_coverage) =
                    output.unwrap_or((0, 0, vec![], None, None, None, None));
                let _ = tx.send(Preview {
                    coarse: false,
                    error,
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
                egui::Button::new("Group layers").shortcut_text(if cfg!(target_os = "macos") {
                    "⌘ G"
                } else {
                    "Ctrl+G"
                }),
            )
            .clicked()
        {
            if !self.selection_layers.contains(&l.id) {
                self.select_content(&l.id);
            }
            self.create_folder(doc);
            ui.close_menu();
        }
        if ui
            .add_enabled(
                !self.busy && !doc.read_only,
                egui::Button::new("Merge layers").shortcut_text(if cfg!(target_os = "macos") {
                    "⌘ ⇧ G"
                } else {
                    "Ctrl+Shift+G"
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
        if l.source.is_some() && ui.button("Edit text/vector properties…").clicked() {
            self.select_content(&l.id);
            self.open_source(doc, "edit");
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
                crate::raster::layer_blends(&l.kind)
                    .find(|(mode, _)| *mode == blend)
                    .map(|(_, label)| label)
                    .unwrap_or("Normal"),
            )
            .width(ui.available_width() - 10.0)
            .show_ui(ui, |ui| {
                for (name, label) in crate::raster::layer_blends(&l.kind) {
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
            if response.changed() || response.double_clicked() {
                self.layer_parameter(json!({"opacity":percent/100.0}), "Layer opacity", &response);
            }
        });
    }
    fn add_selected_masks(&mut self, doc: &Document) {
        let mut ids = self.selected_layer_ids(doc);
        if !ids.contains(&self.selected) {
            ids.push(self.selected.clone());
        }
        let commands: Vec<_> = doc
            .layers
            .iter()
            .filter(|layer| ids.contains(&layer.id) && layer.mask.is_none())
            .map(|layer| json!({"op":"mask.add","layer":layer.id,"value":255}))
            .collect();
        let result = {
            let mut engine = self.shared.lock().unwrap();
            if engine.doc.id != doc.id {
                Err("The document changed before adding masks".into())
            } else {
                engine.edit("human", &commands, Some(doc.revision), None, "Add masks")
            }
        };
        match result {
            Ok(_) => {
                self.mask = true;
                self.mask_step = None;
                self.last_preview = None;
                self.message = "Added masks".into();
            }
            Err(error) => self.message = error,
        }
    }
    fn channel_tabs(&mut self, ui: &mut egui::Ui, doc: &Document, has_mask: bool) {
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
            self.finish_parameter(true);
            self.mask = false;
            self.last_preview = None;
        }
        let mask = ui.add_enabled(
            has_mask || !doc.read_only,
            egui::Button::new(if has_mask { "Mask" } else { "Add mask" })
                .selected(self.mask && has_mask)
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
            self.finish_parameter(true);
            if has_mask {
                self.mask = true;
                self.last_preview = None;
            } else {
                self.add_selected_masks(doc);
            }
        }
        mask.on_hover_text(if has_mask {
            "Edit mask"
        } else {
            "Create white masks for selected layers that do not have one"
        });
    }

    fn layers(&mut self, ui: &mut egui::Ui, doc: &Document) {
        ui.add_space(10.0);
        ui.horizontal(|ui|{ui.label(RichText::new("Layers").size(15.0).strong());ui.label(RichText::new(doc.layers.len().to_string()).size(11.0).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                let folder = ui.add_enabled_ui(!self.busy && !doc.read_only,|ui|icons::button(ui, Icon::Folder, "Group selected layers · Ctrl/Cmd+G")).inner;
                self.new_folder_rect = Some(folder.rect);
                if folder.clicked() { self.create_folder(doc); }
                ui.menu_button("◐",|ui| {
                    for (kind,label) in [("color_balance","Color balance"),("hsl","Hue / saturation"),("levels","Levels"),("curves","Curves"),("posterize","Posterize")] {
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
            if l.mask.is_none() {
                self.mask = false;
                self.mask_step = None;
            }
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
                        self.channel_tabs(ui, doc, l.mask.is_some());
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
                            self.effect_picker(ui,true,l.kind=="adjustment");
                        });
                        self.effect_add_rect = Some(add.response.rect);
                        egui::ScrollArea::vertical().id_salt("mask effects").max_height((ui.available_height()-8.0).max(24.0)).show(ui,|ui| {
                            ui.spacing_mut().item_spacing.y=3.0;
                            for (index,step) in m.steps.iter().enumerate().rev() {
                                ui.push_id(&step.id,|ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x=4.0;
                                        let mut enabled=step.enabled;
                                        if ui.checkbox(&mut enabled,"").on_hover_text("Enable mask effect").changed() {self.layer_cmd("mask.step.update",json!({"step":step.id,"enabled":enabled}),"Toggle mask step");}
                                        let (rect,_) = ui.allocate_exact_size(Vec2::splat(18.0),egui::Sense::hover());
                                        icons::paint(ui.painter(),rect,effects::effect_icon(&step.kind),step.enabled);
                                        let name=if step.kind=="blur" {"Feather"} else {effects::effect_name(&step.kind)};
                                        if Self::effect_title(ui,name,self.effect_is_selected(&l.id,true,&step.id)).clicked() {
                                            self.select_effect(&l.id,true,&step.id);
                                            self.mask_step=if step.kind=="paint" {Some(step.id.clone())} else {None};
                                            self.last_preview=None;
                                        }
                                        self.effect_weight(ui,&step.id,step.weight,true);
                                    });
                                    if self.effect_is_selected(&l.id,true,&step.id) {
                                        ui.horizontal(|ui| {
                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                                                if icons::small_button(ui,Icon::Trash,"Remove mask effect").clicked() {self.layer_cmd("mask.step.delete",json!({"step":step.id}),"Remove mask step");self.mask_step=None;self.effect_selected=None;}
                                                if index>0 && icons::small_button(ui,Icon::Down,"Move mask effect down").clicked() {self.layer_cmd("mask.step.reorder",json!({"step":step.id,"index":index-1}),"Reorder mask step");}
                                                if index+1<m.steps.len() && icons::small_button(ui,Icon::Up,"Move mask effect up").clicked() {self.layer_cmd("mask.step.reorder",json!({"step":step.id,"index":index+1}),"Reorder mask step");}
                                            });
                                        });
                                        if let Some(preview)=self.thumbnail_for(doc,&l.id,true,Some(index)) { ui.add(egui::Image::new((preview.id(),Vec2::splat(40.0)))).on_hover_text("Result through this effect"); }
                                        if ["fill","levels","blur"].contains(&step.kind.as_str()) {
                                            let mut value=step.value;
                                            let range=match step.kind.as_str(){"fill"=>0.0..=255.0,"blur"=>0.0..=64.0,_=>0.1..=5.0};
                                            ui.horizontal(|ui| {
                                                controls::label(ui,if step.kind=="fill"{"Value"}else if step.kind=="blur"{"Radius"}else{"Gamma"});
                                                let response=controls::range(ui,(&step.id,"value"),&mut value,range,180.0,if step.kind=="blur"{" px"}else{""},if step.kind=="levels"{1}else{0},false);
                                                if response.changed() || response.double_clicked(){self.layer_parameter(json!({"step":step.id,"value":value}),"Mask parameter",&response);}
                                            });
                                        }
                                        if ["curves","gaussian","adjust"].contains(&step.kind.as_str()) {
                                            let mut values=step.settings.clone();
                                            if let Some(response)=effects::settings(ui,&step.kind,&mut values) {self.layer_parameter(json!({"step":step.id,"settings":values}),"Mask parameter",&response);}
                                        }
                                    }
                                });
                            }
                        });
                    } else {
                        ui.label(
                            RichText::new("Reveal. Hide. Refine.")
                                .size(12.0)
                                .color(MUTED),
                        );
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
        let roots = crate::tree::roots(doc, &self.selection_layers.iter().cloned().collect());
        let targets = doc
            .layers
            .iter()
            .filter(|l| roots.contains(&l.id))
            .collect::<Vec<_>>();
        let tree = crate::transform::tree_ids(doc, &roots);
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
            let b = crate::transform::tree_bounds(doc, &roots).unwrap_or([
                0,
                0,
                doc.width as i32,
                doc.height as i32,
            ]);
            self.gizmo_bounds = Some((key, doc.revision, b));
        }
        let b = self.gizmo_bounds.as_ref().unwrap().2;
        let selection_only = doc.selection.is_some();
        let b = if selection_only {
            doc.selection.unwrap()
        } else {
            b
        };
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
            && doc
                .layers
                .iter()
                .filter(|l| tree.contains(&l.id))
                .all(|l| !l.locked)
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
                    json!({"op":"move","layer":target.id,"dx":dx.round(),"dy":dy.round(),"mask":self.mask,"step":if targets.len()==1 {self.mask_step.clone()}else{None},"selection_only":selection_only})
                } else {json!({"op":"transform","layer":target.id,"angle":angle,"scale_x":sx,"scale_y":sy,"pivot":pivot,"mask":self.mask,"step":if targets.len()==1 {self.mask_step.clone()}else{None},"selection_only":selection_only})});
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
        if [Tool::Clone, Tool::Heal].contains(&self.tool)
            && !self.points.is_empty()
            && self.retouch_revision != Some(doc.revision)
        {
            self.points.clear();
            self.drag_start = None;
            self.live_gesture = None;
            self.message = "Image changed during retouch · stroke cancelled".into();
        }
        self.transient.clear();
        if let Some(editor) = &self.geometry {
            self.transient.push(editor.command());
        }
        if let Some(editor) = &self.source_editor {
            self.transient.push(editor.command());
        }
        if let Some(editor) = &self.filter_editor {
            self.transient.extend(editor.preview_commands(doc));
        }
        if let Some(editor) = &self.refinement {
            self.transient.push(editor.command());
        }
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
        let [canvas_width, canvas_height] = self
            .geometry
            .as_ref()
            .map(|g| g.size())
            .unwrap_or([doc.width, doc.height]);
        let fit = ((available.width() - 60.0) / canvas_width as f32)
            .min((available.height() - 60.0) / canvas_height as f32)
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
        let size = Vec2::new(canvas_width as f32 * scale, canvas_height as f32 * scale);
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
        if let Some(texture) = self.texture.as_ref().filter(|_| {
            self.proposal_review.is_none()
                || self.proposal_rendered.as_deref() == Some(self.live_key().as_str())
        }) {
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
        if self.lifecycle.is_some() || self.lifecycle_frame {
            return;
        }
        if self.proposal_review.is_some() {
            painter.text(
                rect.left_top() + Vec2::new(8., 8.),
                egui::Align2::LEFT_TOP,
                if self.proposal_original {
                    "Original · AI proposal review"
                } else {
                    "AI proposal · preview only"
                },
                egui::FontId::proportional(12.),
                AI_BLUE,
            );
            if response.clicked() || response.drag_started() {
                self.close_proposal();
                self.message = "Proposal preview closed · current work preserved".into();
                ui.ctx().request_repaint();
            }
            return;
        }
        if self.geometry.is_some() || self.source_editor.is_some() || self.filter_editor.is_some() {
            return;
        }
        let to_screen = |p: [f32; 2]| rect.min + Vec2::new(p[0] * scale, p[1] * scale);
        let to_doc = |p: Pos2| [(p.x - rect.min.x) / scale, (p.y - rect.min.y) / scale];
        let size_mode = [
            Tool::Brush,
            Tool::Eraser,
            Tool::Smudge,
            Tool::Clone,
            Tool::Heal,
            Tool::Liquify,
        ]
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
                    && ([
                        Tool::Brush,
                        Tool::Eraser,
                        Tool::Smudge,
                        Tool::Clone,
                        Tool::Heal,
                    ]
                    .contains(&self.tool))
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
        if [Tool::Clone, Tool::Heal].contains(&self.tool) {
            if let Some(source) = &self.retouch_source {
                if source.project == doc.id {
                    let point = if self.retouch_aligned {
                        if let (Some(destination), Some(pointer)) =
                            (source.destination, response.hover_pos())
                        {
                            let p = to_doc(pointer);
                            [
                                source.point[0] + p[0] - destination[0],
                                source.point[1] + p[1] - destination[1],
                            ]
                        } else {
                            source.point
                        }
                    } else {
                        source.point
                    };
                    let p = to_screen(point);
                    for (width, color) in [(3_f32, Color32::BLACK), (1_f32, Color32::WHITE)] {
                        painter.circle_stroke(p, 5., Stroke::new(width, color));
                        painter.line_segment(
                            [p - Vec2::new(8., 0.), p + Vec2::new(8., 0.)],
                            Stroke::new(width, color),
                        );
                        painter.line_segment(
                            [p - Vec2::new(0., 8.), p + Vec2::new(0., 8.)],
                            Stroke::new(width, color),
                        );
                    }
                }
            }
            if ui.input(|i| i.modifiers.alt) || self.retouch_pick {
                let pick = if self.retouch_pick {
                    response.clicked()
                } else {
                    ui.input(|i| i.pointer.primary_pressed())
                };
                if response.hovered() && pick {
                    if let Some(p) = response.hover_pos().filter(|p| rect.contains(*p)) {
                        self.retouch_source = Some(retouch::Anchor {
                            project: doc.id.clone(),
                            layer: self.selected.clone(),
                            mask: self.mask,
                            point: to_doc(p),
                            destination: None,
                        });
                        self.message = "Clone/heal source set · paint to retouch".into();
                        self.retouch_pick = false;
                    }
                }
                self.points.clear();
                self.drag_start = None;
                return;
            }
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
        if matches!(self.tool, Tool::Selection | Tool::MagicWand) {
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
                    if !self.begin_retouch(doc, point) {
                        return;
                    }
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
            if ![
                Tool::Brush,
                Tool::Eraser,
                Tool::Smudge,
                Tool::Clone,
                Tool::Heal,
                Tool::Liquify,
            ]
            .contains(&self.tool)
            {
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
            Tool::Brush|Tool::Eraser|Tool::Smudge|Tool::Clone|Tool::Heal=>{let c=if self.mask{[self.mask_value,self.mask_value,self.mask_value,self.color[3]]}else{self.color};self.paint_command(self.points.clone(),c,"Brush stroke");},
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
                    if !self.begin_retouch(doc, p) {
                        return;
                    }
                    match self.tool {
                        Tool::Brush | Tool::Eraser | Tool::Smudge | Tool::Clone | Tool::Heal => {
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
                        Tool::Picker => self.pick_color(doc, p),
                        _ => {}
                    }
                }
            }
        }
    }
}
impl PeerBrush {
    fn switch_project(&mut self, id: &str) {
        if id == self.project_id {
            return;
        }
        let Some((_, _, shared)) = crate::workspace::entries_in(&self.workspace)
            .into_iter()
            .find(|(project, _, _)| project == id)
        else {
            return;
        };
        self.project_views.insert(
            self.project_id.clone(),
            ProjectView {
                selected: self.selected.clone(),
                selection_layers: self.selection_layers.clone(),
                selection_anchor: self.selection_anchor.clone(),
                mask: self.mask,
                mask_step: self.mask_step.clone(),
                effect_selected: self.effect_selected.clone(),
                isolate: self.isolate,
                collapsed: self.collapsed.clone(),
                zoom: self.zoom,
                pan: self.pan,
                message: self.message.clone(),
            },
        );
        self.select_tool(self.tool);
        self.finish_parameter(true);
        self.rename_edit = None;
        self.eye_sweep = None;
        self.layer_drag = None;
        self.layer_context = None;
        self.geometry = None;
        self.source_editor = None;
        self.filter_editor = None;
        self.refinement = None;
        self.retouch_source = None;
        self.task_undo_review = None;
        self.proposal_review = None;
        self.proposal_rendered = None;
        self.selection_path.clear();
        self.selection_gesture_mode = None;
        self.project_settings = None;
        self.shared = shared;
        self.project_id = id.into();
        self.busy = self.jobs.contains(id);
        if let Some(view) = self.project_views.remove(id) {
            self.selected = view.selected;
            self.selection_layers = view.selection_layers;
            self.selection_anchor = view.selection_anchor;
            self.mask = view.mask;
            self.mask_step = view.mask_step;
            self.effect_selected = view.effect_selected;
            self.isolate = view.isolate;
            self.collapsed = view.collapsed;
            self.zoom = view.zoom;
            self.pan = view.pan;
            self.message = view.message;
        } else {
            self.selected = String::new();
            self.selection_layers.clear();
            self.selection_anchor = String::new();
            self.mask = false;
            self.mask_step = None;
            self.effect_selected = None;
            self.isolate = false;
            self.collapsed.clear();
            self.zoom = 1.;
            self.pan = Vec2::ZERO;
            self.frame_pending = true;
            self.message = "Ready".into();
        }
        self.texture = None;
        self.canvas_pixels = None;
        self.canvas_selection = None;
        self.last_preview = None;
        self.pending = false;
        self.preview_cache = Arc::new(std::sync::Mutex::new(crate::preview::Cache::default()));
        self.thumbs.clear();
        self.thumb_pending.clear();
        self.thumb_document.clear();
        self.last_status.clear();
    }
    fn draw_header(&mut self, ctx: &egui::Context) {
        let (connected, activity) = {
            if let Ok(e) = self.workspace_root.try_lock() {
                self.connected = !e.mcp_clients.is_empty();
            }
            let activity = self
                .shared
                .try_lock()
                .ok()
                .map(|e| e.activity.clone())
                .unwrap_or_else(|| self.activity_text.clone());
            (self.connected, activity)
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
                if self.lifecycle.is_some() || self.lifecycle_frame {
                    ui.disable();
                }
                let bounds = ui.max_rect();
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(5.0);
                    ui.add(egui::Image::new((self.logo.id(), Vec2::splat(42.0))));
                    ui.add_space(9.0);
                    let aspect=self.wordmark.size_vec2().x/self.wordmark.size_vec2().y;
                    ui.add(egui::Image::new((self.wordmark.id(),Vec2::new(136.0,136.0/aspect))))
                        .on_hover_text("PeerBrush · You and your AI. Same canvas.");
                    ui.add_space(12.0);
                    if self.shared.try_lock().is_ok() {self.task_history_menu(ui);}
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
    }
    fn project_tabs(&mut self, ctx: &egui::Context) {
        let (select, close, hovered) = self.draw_project_tabs(ctx);
        while let Ok(reply) = self.transfer_rx.try_recv() {
            match reply.output {
                TransferOutput::Preview(result) => {
                    self.tab_preview_pending = false;
                    if self.tab_transfer.as_ref() == Some(&reply.spec)
                        && !self.tab_transfer_committing
                    {
                        match result {
                            Ok((doc, w, h, bytes)) => {
                                self.tab_preview = Some((
                                    doc,
                                    ctx.load_texture(
                                        "Cross-project preview",
                                        egui::ColorImage::from_rgba_unmultiplied(
                                            [w as usize, h as usize],
                                            &bytes,
                                        ),
                                        egui::TextureOptions::LINEAR,
                                    ),
                                ));
                            }
                            Err(error) => self.message = error,
                        }
                    }
                }
                TransferOutput::Commit(result) => {
                    self.jobs.remove(&reply.spec.source);
                    self.busy = self.jobs.contains(&self.project_id);
                    self.tab_transfer_committing = false;
                    self.tab_transfer = None;
                    self.tab_preview = None;
                    self.tab_preview_requested = None;
                    match result {
                        Ok(result) => {
                            if crate::workspace::active_id_in(&self.workspace) == reply.spec.source
                            {
                                let _ = crate::workspace::select_in(
                                    &self.workspace,
                                    &reply.spec.destination,
                                );
                                self.switch_project(&reply.spec.destination);
                                if let Some(ids) = result["created_roots"].as_array() {
                                    let ids: Vec<_> = ids
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .map(String::from)
                                        .collect();
                                    if let Some(first) = ids.first() {
                                        self.select_content(first);
                                        self.selection_layers = ids.into_iter().collect();
                                    }
                                }
                            }
                            self.message = if reply.spec.move_layers {
                                "Moved layers between projects"
                            } else {
                                "Copied layers to project"
                            }
                            .into();
                        }
                        Err(error) => self.message = error,
                    }
                    self.last_preview = None;
                }
            }
        }
        if !self.tab_transfer_committing {
            if let Some(spec) = hovered {
                if ctx.input(|i| i.pointer.button_released(egui::PointerButton::Primary)) {
                    self.layer_drag = None;
                    self.tab_transfer = Some(spec.clone());
                    self.tab_transfer_committing = true;
                    self.jobs.insert(spec.source.clone());
                    self.busy = true;
                    let workspace = self.workspace.clone();
                    let tx = self.transfer_tx.clone();
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        let result = crate::workspace::transfer_in(&workspace, &spec.request());
                        let _ = tx.send(TransferReply {
                            spec,
                            output: TransferOutput::Commit(result),
                        });
                        ctx.request_repaint();
                    });
                } else {
                    if self.tab_transfer.as_ref() != Some(&spec) {
                        self.tab_preview = None;
                    }
                    self.tab_transfer = Some(spec.clone());
                    if !self.tab_preview_pending
                        && self.tab_preview_requested.as_ref() != Some(&spec)
                    {
                        self.tab_preview_requested = Some(spec.clone());
                        self.tab_preview_pending = true;
                        let workspace = self.workspace.clone();
                        let tx = self.transfer_tx.clone();
                        let ctx = ctx.clone();
                        std::thread::spawn(move || {
                            let result =
                                crate::workspace::transfer_preview_in(&workspace, &spec.request())
                                    .and_then(|doc| {
                                        let (w, h, bytes, _) =
                                            doc.preview(None, 1400, None, false)?;
                                        Ok((doc, w, h, bytes))
                                    });
                            let _ = tx.send(TransferReply {
                                spec,
                                output: TransferOutput::Preview(result),
                            });
                            ctx.request_repaint();
                        });
                    }
                }
            } else if self.tab_transfer.take().is_some() {
                self.tab_preview = None;
                self.tab_preview_requested = None;
                self.last_preview = None;
            }
        }
        if let Some(id) = select {
            if crate::workspace::select_in(&self.workspace, &id).is_ok() {
                self.switch_project(&id);
            }
        }
        if let Some((id, _shared)) = close {
            self.request_close(lifecycle::Intent::Close(id));
        }
    }
    fn draw(&mut self, ctx: &egui::Context) {
        self.lifecycle_frame = false;
        let panel = self
            .workspace_root
            .try_lock()
            .ok()
            .and_then(|mut e| e.capture_panel.take());
        if let Some(panel) = panel {
            self.show_brush = panel == "brush";
            if panel == "color" {
                self.open_color(color::Target::Foreground, self.color);
            } else {
                self.color_editor = None;
            }
            if panel == "filters" {
                let doc = self.shared.lock().unwrap().doc.clone();
                self.open_filters(&doc);
            }
        }
        {
            if let Ok(mut e) = self.workspace_root.try_lock() {
                if e.focus_requested {
                    e.focus_requested = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
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
            if let Some(path) = self
                .workspace_root
                .try_lock()
                .ok()
                .and_then(|mut e| e.capture_ui.take())
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(path)));
            }
        }
        while let Ok(result) = self.connect_rx.try_recv() {
            self.receive_connection(result);
        }
        while let Ok(reply) = self.merge_rx.try_recv() {
            self.jobs.remove(&reply.project);
            self.busy = self.jobs.contains(&self.project_id);
            if reply.project != self.project_id {
                if let Some(view) = self.project_views.get_mut(&reply.project) {
                    view.message = reply
                        .result
                        .map(|_| "Merged layers".into())
                        .unwrap_or_else(|e| e);
                }
                continue;
            }
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
            if self
                .shared
                .try_lock()
                .ok()
                .is_none_or(|e| e.doc.id != reply.document)
            {
                continue;
            }
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
        let status = self
            .shared
            .try_lock()
            .ok()
            .map(|e| e.status.clone())
            .unwrap_or_else(|| self.last_status.clone());
        if status != self.last_status {
            self.message = status.clone();
            self.last_status = status;
        }
        self.reconcile_reservation_message();
        while let Ok(reply) = self.import_rx.try_recv() {
            self.jobs.remove(&reply.project);
            self.busy = self.jobs.contains(&self.project_id);
            match reply.result {
                Ok(files) if !files.is_empty() => {
                    let needs_choice = files.iter().any(|f| f.encoded.info.choice.is_some());
                    self.pending_import = Some(PendingImport {
                        project: reply.project,
                        document: reply.document,
                        revision: reply.revision,
                        files,
                    });
                    if !needs_choice
                        && self
                            .pending_import
                            .as_ref()
                            .is_some_and(|p| p.project == self.project_id)
                    {
                        self.commit_import();
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    if reply.project == self.project_id {
                        self.message = error;
                    } else if let Some(view) = self.project_views.get_mut(&reply.project) {
                        view.message = error;
                    }
                }
            }
        }
        while let Ok(reply) = self.job_rx.try_recv() {
            self.jobs.remove(&reply.project);
            self.busy = self.jobs.contains(&self.project_id);
            self.load_control = None;
            self.load_preview = None;
            let result = reply.result;
            if reply.project != self.project_id {
                if let Some(view) = self.project_views.get_mut(&reply.project) {
                    view.message = result.unwrap_or_else(|e| e);
                }
                continue;
            }
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
        self.lifecycle_dialog(ctx);
        if self.exit_approved {
            return;
        }
        let had_review = self.lifecycle.is_some();
        self.draw_header(ctx);
        self.project_tabs(ctx);
        if !had_review && self.lifecycle.is_some() {
            self.lifecycle_dialog(ctx);
            if self.exit_approved {
                return;
            }
        }
        if self.shared.try_lock().is_err() {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Working on this project… Switch tabs to continue elsewhere.");
            });
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if self.lifecycle.is_some() || self.lifecycle_frame {
            // The review owns keyboard/canvas input for the entire frame.
        } else if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.finish_parameter(true);
        } else if !ctx.input(|i| i.pointer.any_down())
            && !self.parameter_gesture.as_ref().is_some_and(|edit| {
                edit.typing
                    .as_ref()
                    .is_some_and(|ctx| controls::number_editing(ctx, edit.control))
            })
        {
            self.finish_parameter(false);
        }
        let (doc, ai_change) = {
            let mut e = self.shared.lock().unwrap();
            e.expire();
            (e.doc.clone(), e.ai_change.clone())
        };
        self.doc_snapshot = doc.clone();
        if self.lifecycle.is_none() && !self.lifecycle_frame {
            self.import_dialog(ctx);
        }
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
        if self
            .parameter_gesture
            .as_ref()
            .is_some_and(|edit| edit.document != doc.id || edit.revision != doc.revision)
        {
            self.finish_parameter(true);
            self.message = "Image changed during parameter edit · preview cancelled".into();
        }
        self.thumb_worker.document(&doc);
        if self.thumb_document != doc.id {
            self.retouch_source = None;
            self.retouch_revision = None;
            self.retouch_pick = false;
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
        if !dropped.is_empty() && self.lifecycle.is_none() && !self.lifecycle_frame {
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
        let typing = self.lifecycle.is_some()
            || self.filter_editor.is_some()
            || self.lifecycle_frame
            || self.rename_edit.is_some()
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
                                if doc.selection.is_some() {
                                    "Cutting selected pixels…".into()
                                } else {
                                    "Cutting layers…".into()
                                }
                            } else if self.layer_clipboard && doc.selection.is_none() && !merged {
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
                        document: doc.id.clone(),
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
                if command_key(i,egui::Key::G) {
                    if key_modifiers(i,egui::Key::G).shift {
                        self.merge_layers(ctx, &doc);
                    } else {
                        self.create_folder(&doc);
                    }
                }
                if i.key_pressed(egui::Key::Delete)
                    && self.selection_path.is_empty()
                    && !key_modifiers(i,egui::Key::Delete).any()
                {
                    if doc.selection.is_some() {
                        let roots = crate::tree::roots(&doc, &self.selection_layers);
                        let commands = roots.iter().map(|id| json!({"op":"paint.clear_selection","layer":id,"mask":self.mask,"step":self.mask_step,"document_id":doc.id,"source_revision":doc.revision})).collect();
                        self.edit(commands,"Clear selected pixels");
                    } else {
                        self.layer_cmd("layer.delete",json!({}),"Delete selected layers");
                    }
                }
                if command_key(i,egui::Key::D) {
                    if key_modifiers(i,egui::Key::D).shift {
                        self.edit(vec![json!({"op":"selection.reselect"})],"Reselect");
                    } else {
                        self.duplicate_selection(&doc);
                    }
                }
                if command_key(i,egui::Key::A) {self.edit(vec![json!({"op":"selection","kind":"rectangle","rect":[0,0,doc.width,doc.height]})],"Select all");}
                if command_key(i,egui::Key::I) && key_modifiers(i,egui::Key::I).shift && !key_modifiers(i,egui::Key::I).alt {
                    self.edit(vec![json!({"op":"selection.invert"})],"Invert selection");
                }
                if command_key(i,egui::Key::J) && self.layer_clipboard {self.duplicate_layers(&doc);}
                for (key, tool) in [
                    (egui::Key::B, Tool::Brush),
                    (egui::Key::U, Tool::Smudge),
                    (egui::Key::C, Tool::Clone),
                    (egui::Key::J, Tool::Heal),
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
                    (egui::Key::K, Tool::MagicWand),
                ] {
                    if !key_modifiers(i,key).command && !key_modifiers(i,key).ctrl && !key_modifiers(i,key).alt && (i.key_pressed(key)
                        || i.events.iter().any(|e| matches!(e, egui::Event::Key { physical_key: Some(k), pressed: true, .. } if *k == key))) {
                        self.select_tool(tool);
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
                if !i.modifiers.command && [Tool::Brush, Tool::Eraser, Tool::Smudge, Tool::Clone, Tool::Heal, Tool::Liquify].contains(&self.tool) {
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
        egui::TopBottomPanel::top("context")
            .exact_height(48.0)
            .show(ctx, |ui| {
                if self.lifecycle.is_some() || self.lifecycle_frame {
                    ui.disable();
                }
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
                        if ui.button("Recover autosaved work…").clicked() {
                            self.recovery = recovery::Browser::new(&self.connection.state_dir);
                            self.recovery.open = true;
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
                    ui.menu_button("Image",|ui| {
                        if ui.add_enabled(!doc.read_only&&!self.busy,egui::Button::new("Whole-image filters…")).clicked(){self.open_filters(&doc);ui.close_menu();}
                        if !doc.filters.is_empty(){ui.menu_button("Filter stack",|ui|self.document_filter_stack(ui,&doc));}
                        for (label,mode) in [("Crop…","crop"),("Canvas size…","canvas.resize"),("Image size…","image.resize")] {
                            if ui.add_enabled(!doc.read_only && !self.busy,egui::Button::new(label)).clicked() {self.open_geometry(&doc,mode);ui.close_menu();}
                        }
                    });
                    ui.menu_button("Layer",|ui| {
                        if ui.add_enabled(!self.busy&&!doc.read_only,egui::Button::new("Group layers").shortcut_text(if cfg!(target_os="macos") {"⌘ G"} else {"Ctrl+G"})).clicked(){self.create_folder(&doc);ui.close_menu();}
                        if ui.add_enabled(!self.busy&&!doc.read_only,egui::Button::new("Merge layers").shortcut_text(if cfg!(target_os="macos") {"⌘ ⇧ G"} else {"Ctrl+Shift+G"})).clicked(){self.merge_layers(ctx,&doc);ui.close_menu();}
                        ui.separator();
                        for (label,kind) in [("New text…","text"),("New vector shape…","shape"),("Edit text/vector…","edit")] {
                            let enabled=!doc.read_only&&!self.busy && (kind!="edit"||doc.layers.iter().any(|l|l.id==self.selected&&l.source.is_some()));
                            if ui.add_enabled(enabled,egui::Button::new(label)).clicked(){self.open_source(&doc,kind);ui.close_menu();}
                        }
                        if ui.add_enabled(!doc.read_only&&doc.layers.iter().any(|l|l.id==self.selected&&l.source.is_some()),egui::Button::new("Rasterize text/vector")).on_hover_text("Keep the current pixels and remove editable properties · Undo restores the source").clicked(){self.layer_cmd("source.rasterize",json!({}),"Rasterize text/vector");ui.close_menu();}
                    });
                    ui.menu_button("Select",|ui| {
                        let learned = crate::segmentation::configured() && !doc.read_only && !self.busy;
                        if ui.add_enabled(learned, egui::Button::new("Select subject")).on_hover_text("Uses the externally configured local selection provider").clicked(){self.segment_subject(&doc,false);ui.close_menu();}
                        if ui.add_enabled(learned && doc.selection.is_some(), egui::Button::new("Select subject in region")).clicked(){self.segment_subject(&doc,true);ui.close_menu();}
                        if ui.add_enabled(doc.selection.is_some()&&!doc.read_only,egui::Button::new("Refine selection…")).clicked(){self.open_refinement(&doc,None);ui.close_menu();}
                        if ui.add_enabled(doc.selection.is_some()&&!doc.read_only,egui::Button::new("Mask from selection")).clicked(){self.layer_cmd("mask.from_selection",json!({}),"Mask from selection");ui.close_menu();}
                        if ui.add_enabled(doc.layers.iter().any(|l|l.id==self.selected&&l.mask.is_some())&&!doc.read_only,egui::Button::new("Refine layer mask…")).clicked(){self.open_refinement(&doc,Some(self.selected.clone()));ui.close_menu();}
                        for (label,op) in [("All · Ctrl+A","all"),("Deselect","selection.clear"),("Reselect · Ctrl+Shift+D","selection.reselect"),("Inverse","selection.invert")] {
                            let button=egui::Button::new(label);
                            let button=if op=="selection.invert" {button.shortcut_text(if cfg!(target_os="macos") {"⌘ ⇧ I"} else {"Ctrl+Shift+I"})} else {button};
                            if ui.add(button).clicked(){self.edit(vec![if op=="all"{json!({"op":"selection","kind":"rectangle","rect":[0,0,doc.width,doc.height]})}else{json!({"op":op})}],label);ui.close_menu();}
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
                    if [Tool::Brush, Tool::Eraser, Tool::Smudge, Tool::Clone, Tool::Heal].contains(&self.tool) {
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
                    self.retouch_toolbar(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(ui, Icon::Frame, "Frame selection or canvas · F").clicked()
                        {
                            self.frame_pending = true;
                        }
                        ui.label(
                            RichText::new(format!(
                                "{}%",
                                self.view_rect
                                    .map(|r| {
                                        let width = self.geometry.as_ref().map(|g| g.size()[0]).unwrap_or(doc.width);
                                        (r.width() / width as f32 * 100.0).round() as u32
                                    })
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
        if matches!(self.tool, Tool::Selection | Tool::MagicWand) {
            egui::TopBottomPanel::top("selection context")
                .exact_height(40.)
                .show(ctx, |ui| {
                    if self.lifecycle.is_some() || self.lifecycle_frame {
                        ui.disable();
                    }
                    self.selection_toolbar(ui);
                });
        }
        self.recovery_failure(ctx);
        egui::TopBottomPanel::bottom("status")
            .exact_height(28.0)
            .show(ctx, |ui| {
                if self.lifecycle.is_some() || self.lifecycle_frame {
                    ui.disable();
                }
                let bounds=ui.max_rect();
                let mut modes=ui.new_child(egui::UiBuilder::new().id_salt("transform modes").max_rect(Rect::from_min_max(egui::pos2(bounds.right()-150.,bounds.top()),bounds.max)));
                self.transform_controls(&mut modes);
                ui.set_max_width((bounds.width()-160.).max(0.));
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
                        let profiled = doc.icc_profile.is_some();
                        let convertible = !profiled || doc.icc_profile.as_deref().is_some_and(|p| crate::color_profile::supported(p).is_ok());
                        let label = if profiled { "Convert to sRGB copy".into() } else { format!("Edit {}-bit copy", doc.bit_depth) };
                        let hint = if profiled { "Convert the saved artwork to sRGB at its original bit depth. Out-of-gamut colors are clipped; Photoshop layers are flattened. Save under a new name." } else { "Create a flattened editable copy at the original color precision. Unsupported Photoshop layers are flattened; the source file stays untouched." };
                        if ui.add_enabled(convertible, egui::Button::new(label)).on_hover_text(hint).clicked() {
                            let shared = self.shared.clone(); let revision = doc.revision;
                            self.job(move || { server::compatible_copy_with_color(&shared, "human", Some(revision), profiled)?; Ok("Editing a flattened copy at original precision · save under a new name".into()) });
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
                                    e.take_over_tasks();
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
                    }
                });
            });
        egui::SidePanel::left("tools")
            .exact_width(60.0)
            .resizable(false)
            .show(ctx, |ui| {
                if self.lifecycle.is_some() || self.lifecycle_frame {
                    ui.disable();
                }
                ui.add_space(6.0);
                ui.spacing_mut().item_spacing.y = 4.0;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height((ui.available_height() - 70.0).max(60.0))
                    .show(ui, |ui| {
                        for (index, tool) in [
                            Tool::Brush,
                            Tool::Smudge,
                            Tool::Clone,
                            Tool::Heal,
                            Tool::Liquify,
                            Tool::Eraser,
                            Tool::Fill,
                            Tool::Gradient,
                            Tool::Rectangle,
                            Tool::Ellipse,
                            Tool::Selection,
                            Tool::Picker,
                            Tool::Pan,
                            Tool::MagicWand,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            if index == 6 {
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
                                    | (Some("clone"), Tool::Clone)
                                    | (Some("heal"), Tool::Heal)
                                    | (Some("liquify"), Tool::Liquify)
                                    | (Some("eraser"), Tool::Eraser)
                                    | (Some("move"), Tool::Move)
                                    | (Some("rotate"), Tool::Rotate)
                                    | (Some("scale"), Tool::Scale)
                                    | (Some("selection"), Tool::MagicWand)
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
                                self.select_tool(tool);
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
                if self.lifecycle.is_some() || self.lifecycle_frame {
                    ui.disable();
                }
                let mut display = self.animation.document(&doc, ui.input(|i| i.time), false);
                if let Some(edit) = &self.parameter_gesture {
                    if let Ok(preview) =
                        crate::engine::Engine::preview_edits(doc.clone(), &edit.commands)
                    {
                        display = preview;
                    }
                }
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
                if let Some((preview, texture)) = &self.tab_preview {
                    ui.label(format!(
                        "{} · {} layers · {} bit · {}",
                        preview.name,
                        preview.layers.len(),
                        preview.bit_depth,
                        if self.tab_transfer.as_ref().is_some_and(|s| s.move_layers) {
                            "Move preview"
                        } else {
                            "Copy preview · Shift to move"
                        }
                    ));
                    let size = texture.size_vec2();
                    let scale = (ui.available_width() / size.x)
                        .min(ui.available_height() / size.y)
                        .min(1.);
                    ui.add(egui::Image::new((texture.id(), size * scale)));
                    return;
                }
                let display = self.review_document(&doc);
                self.canvas(ui, display.as_ref().unwrap_or(&doc));
            });

        if self.tab_transfer.is_none() {
            self.request_preview(ctx, &doc);
        }
        if self.lifecycle.is_some() || self.lifecycle_frame {
            return;
        }
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
        self.task_history_review(ctx, &doc);
        self.proposal_window(ctx, &doc);
        self.refinement_window(ctx, &doc);
        self.geometry_window(ctx, &doc);
        self.source_window(ctx, &doc);
        self.filters_window(ctx, &doc);
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
                    ui.label("Opens in a new project tab.");
                    if ui.button("Create canvas").clicked() {
                        let result = Document::new_depth(
                            self.new_width,
                            self.new_height,
                            self.new_bit_depth,
                        )
                        .and_then(|d| {
                            let mut e = crate::engine::Engine::new();
                            e.doc = d;
                            crate::workspace::register_in(
                                &self.workspace,
                                e,
                                Some(&self.project_id),
                            )
                        });
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
            });
            self.show_connection = open;
        }
        self.recovery_window(ctx);
        let loading = self
            .load_control
            .clone()
            .or_else(|| self.shared.lock().unwrap().loading.clone());
        if let Some(control) = loading {
            if let Some((w, h, bytes)) = control.take_preview() {
                self.load_preview = Some(ctx.load_texture(
                    "loading saved image",
                    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &bytes),
                    egui::TextureOptions::LINEAR,
                ));
            }
            let status = control.status();
            egui::Window::new("Opening PSD")
                .collapsible(false)
                .resizable(false)
                .default_width(350.)
                .show(ctx, |ui| {
                    ui.label(if status.stage.is_empty() {
                        "Choose a PSD file"
                    } else {
                        &status.stage
                    });
                    if status.total > 0 {
                        ui.add(
                            egui::ProgressBar::new(status.completed as f32 / status.total as f32)
                                .show_percentage(),
                        );
                    }
                    if ui.button("Cancel opening").clicked() {
                        control.cancel();
                    }
                    if let Some(texture) = &self.load_preview {
                        ui.add(egui::Image::new(texture).max_size(egui::vec2(350., 260.)));
                        ui.label(
                            RichText::new("Saved image · loading editable sources")
                                .size(11.)
                                .color(MUTED),
                        );
                    }
                });
        } else {
            self.load_preview = None;
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
        server::checkpoint_projects(
            &self.workspace,
            &self.connection.state_dir,
            &mut Default::default(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn released_or_expired_reservation_errors_clear_without_hiding_other_work() {
        for action in ["end", "expire", "takeover"] {
            let (mut app, ctx) = fixture();
            frame(&mut app, &ctx, vec![], Default::default());
            let layer = app.selected.clone();
            let (task, other) = {
                let mut e = app.shared.lock().unwrap();
                let task = e
                    .reserve(
                        "sky-agent",
                        "Sky texture",
                        vec![crate::engine::Scope {
                            target: Some(layer.clone()),
                            rect: Some([0, 0, 16, 16]),
                        }],
                    )
                    .unwrap()
                    .id;
                let other = e
                    .reserve(
                        "detail-agent",
                        "Small detail",
                        vec![crate::engine::Scope {
                            target: Some(layer.clone()),
                            rect: Some([16, 0, 32, 16]),
                        }],
                    )
                    .unwrap()
                    .id;
                (task, other)
            };
            let before = app.shared.lock().unwrap().doc.export_png().unwrap();
            app.edit(
                vec![json!({"op":"paint","layer":layer,"points":[[4,4]],"radius":2})],
                "Paint",
            );
            assert!(app.message.starts_with("Reserved by sky-agent:"));
            frame(&mut app, &ctx, vec![], Default::default());
            assert!(app.message.starts_with("Reserved by sky-agent:"));
            {
                let mut e = app.shared.lock().unwrap();
                match action {
                    "end" => e.finish_task(&task, "ended"),
                    "expire" => e.leases.iter_mut().find(|l| l.id == task).unwrap().expires = 0,
                    _ => e.take_over_tasks(),
                }
            }
            frame(&mut app, &ctx, vec![], Default::default());
            assert!(!app.message.starts_with("Reserved by"), "{}", app.message);
            if action != "takeover" {
                let e = app.shared.lock().unwrap();
                assert!(e.leases.iter().any(|l| l.id == other));
                assert_eq!(e.activity, "Small detail");
            }
            assert_eq!(app.shared.lock().unwrap().doc.export_png().unwrap(), before);
            assert!(app.shared.lock().unwrap().undo.is_empty());
            app.message = "Cannot save this file".into();
            frame(&mut app, &ctx, vec![], Default::default());
            assert_eq!(app.message, "Cannot save this file");
        }
    }
    #[test]
    fn tabs_keep_selection_channels_view_and_background_job_results_separate() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        let first = app.project_id.clone();
        let selected = app.selected.clone();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"mask.add","layer":selected})],
                None,
                None,
                "Add tab mask",
            )
            .unwrap();
        app.mask = true;
        app.mask_step = Some("step-view".into());
        app.zoom = 1.75;
        app.pan = Vec2::new(9., -4.);
        app.collapsed.insert("folder-view".into());
        let mut engine = crate::engine::Engine::new();
        engine.doc = Document::new(24, 20).unwrap();
        let second = crate::workspace::register_in(&app.workspace, engine, Some(&first)).unwrap();
        let second_id = second.lock().unwrap().project_id.clone();
        frame(&mut app, &ctx, vec![], Default::default());
        assert_eq!(app.project_id, second_id);
        assert!(!app.mask);
        assert_eq!(app.zoom, 1.);
        let second_selected = app.selected.clone();
        app.jobs.insert(first.clone());
        app.job_tx
            .send(JobReply {
                project: first.clone(),
                result: Ok("Imported 1 image layers".into()),
            })
            .unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert_eq!(app.selected, second_selected);
        assert!(!app.busy);
        crate::workspace::select_in(&app.workspace, &first).unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert_eq!(app.selected, selected);
        assert!(app.mask);
        assert_eq!(app.mask_step.as_deref(), Some("step-view"));
        assert_eq!(app.zoom, 1.75);
        assert_eq!(app.pan, Vec2::new(9., -4.));
        assert!(app.collapsed.contains("folder-view"));
    }
    #[test]
    fn native_ui_switches_tabs_while_background_root_engine_is_locked() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        let mut engine = crate::engine::Engine::new();
        engine.doc = Document::new(24, 20).unwrap();
        let second = crate::workspace::register_in(&app.workspace, engine, None).unwrap();
        let second_id = second.lock().unwrap().project_id.clone();
        crate::workspace::select_in(&app.workspace, &second_id).unwrap();
        let root = app.workspace_root.clone();
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _guard = root.lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(3));
        });
        held_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let start = std::time::Instant::now();
        frame(&mut app, &ctx, vec![], Default::default());
        let elapsed = start.elapsed();
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(app.project_id, second_id);
        assert!(
            elapsed < Duration::from_secs(1),
            "Tab switching waited {elapsed:?} for background engine"
        );
        app.edit(
            vec![json!({"op":"layer.update","layer":app.selected,"name":"Human in second"})],
            "Rename",
        );
        assert_eq!(second.lock().unwrap().doc.layers[0].name, "Human in second");
        assert!(app.workspace_root.lock().unwrap().undo.is_empty());
    }
    #[test]
    fn tab_drag_displays_actual_destination_then_commits_one_copy_without_source_history() {
        let (mut app, ctx) = fixture();
        {
            let mut e = app.shared.lock().unwrap();
            e.doc = Document::new(32, 24).unwrap();
            e.doc.layers[0].pixels.set(3, 4, [233, 84, 32, 255]);
        }
        let second = crate::workspace::register_in(
            &app.workspace,
            {
                let mut e = crate::engine::Engine::new();
                e.doc = Document::new(32, 24).unwrap();
                e
            },
            None,
        )
        .unwrap();
        let second_id = second.lock().unwrap().project_id.clone();
        frame(&mut app, &ctx, vec![], Default::default());
        let origin = app.layer_rects[&app.selected].center();
        let tab = app.project_rects[&second_id].center();
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(origin),
                egui::Event::PointerButton {
                    pos: origin,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
            Default::default(),
        );
        app.layer_drag = Some(LayerDrag {
            gesture: crate::engine::id(),
            id: app.selected.clone(),
            revision: 0,
            offset: 0.,
            target: 0,
            ids: vec![app.selected.clone()],
            commands: vec![],
        });
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(tab)],
            Default::default(),
        );
        let reply = app
            .transfer_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        app.transfer_tx.send(reply).unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        let preview = &app.tab_preview.as_ref().unwrap().0;
        let (_, _, bytes, _) = preview.preview(None, 32, None, false).unwrap();
        assert_eq!(
            &bytes[((4 * 32 + 3) * 4)..((4 * 32 + 3) * 4 + 4)],
            &[233, 84, 32, 255]
        );
        assert_eq!(second.lock().unwrap().doc.revision, 0);
        assert!(app.shared.lock().unwrap().undo.is_empty());
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerButton {
                pos: tab,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
            Default::default(),
        );
        let reply = app
            .transfer_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        app.transfer_tx.send(reply).unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert_eq!(app.project_id, second_id);
        assert_eq!(second.lock().unwrap().undo.len(), 1);
        assert!(app.workspace_root.lock().unwrap().undo.is_empty());
    }
    use crate::engine::Engine;
    use std::sync::{Arc, Mutex};
    #[test]
    fn stale_proposal_clears_reviewed_pixels_before_current_project_preview() {
        let (mut app, ctx) = small_fixture();
        let source = app.shared.lock().unwrap().doc.clone();
        let proposal = crate::collaboration::propose(&app.shared, "draft-agent", &json!({"document_id":source.id,"expected_revision":source.revision,"commands":[{"op":"paint.fill","layer":app.selected,"rect":[0,0,4,4],"color":[0,80,255,255]}]})).unwrap();
        app.proposal_review = Some(proposal["id"].as_str().unwrap().into());
        app.proposal_rendered = Some(app.live_key());
        app.set_canvas(&ctx, 1, 1, vec![0, 80, 255, 255]);
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":app.selected,"name":"Human change"})],
                None,
                None,
                "Rename",
            )
            .unwrap();
        app.pending = true; // No new worker can replace the image during this assertion.
        app.request_preview(&ctx, &source);
        assert!(app.proposal_original);
        assert!(app.proposal_rendered.is_none());
        assert!(app.canvas_pixels.is_none());
        assert!(app.texture.is_none());
        assert!(app.proposal_error.as_deref().unwrap().contains("changed"));
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        app.close_proposal();
        assert!(app.proposal_review.is_none());
    }
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
    #[test]
    fn preset_selection_custom_editing_and_stroke_use_the_same_settings() {
        let (mut app, ctx) = fixture();
        let preset = crate::brush_library::curated()
            .into_iter()
            .find(|p| p.id == "brush-pen")
            .unwrap();
        app.select_brush_preset(&preset);
        assert_eq!(app.brush, preset.settings);
        assert_eq!(app.radius, preset.settings.radius);
        app.radius = 23.;
        app.brush.pressure_gamma = 0.6;
        app.brush_name = "My pressure pen".into();
        app.save_brush_preset(false);
        let id = app.brush_preset.clone().unwrap();
        let stored = app
            .shared
            .lock()
            .unwrap()
            .brush_library
            .lock()
            .unwrap()
            .get(&id)
            .unwrap();
        assert_eq!(stored.settings.radius, 23.);
        assert_eq!(stored.settings.pressure_gamma, 0.6);
        assert!(app.shared.lock().unwrap().undo.is_empty());
        app.brush_search = "pressure pen".into();
        app.brush_category = "Custom".into();
        app.show_brush = true;
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app.brush_thumbnails.contains_key(&id));
        let command = app.stroke_command(vec![[40., 40.], [100., 80.]], [200, 70, 20, 255]);
        assert_eq!(
            crate::brush::Settings::from_command(&command).unwrap(),
            stored.settings
        );
        let before = app.shared.lock().unwrap().doc.clone();
        let preview = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
        assert!(app.shared.lock().unwrap().undo.is_empty());
        app.edit(vec![command], "Custom brush stroke");
        assert_eq!(
            app.shared.lock().unwrap().doc.export_png().unwrap(),
            preview.export_png().unwrap()
        );
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        app.shared.lock().unwrap().undo("human").unwrap();
        assert_eq!(
            app.shared.lock().unwrap().doc.export_png().unwrap(),
            before.export_png().unwrap()
        );
    }
    #[test]
    fn bottom_right_transform_modes_click_without_editing_and_keep_ai_feedback() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        for (index, tool) in [Tool::None, Tool::Move, Tool::Rotate, Tool::Scale]
            .into_iter()
            .enumerate()
        {
            let rect = app.transform_rects[index].unwrap();
            assert!(
                rect.left() > 1180. && rect.top() > 870. && rect.bottom() <= 900.,
                "Transform mode is not at the bottom right: {rect:?}"
            );
            if index > 0 {
                assert!(rect.left() > app.transform_rects[index - 1].unwrap().right());
            }
            click(&mut app, &ctx, rect.center());
            assert!(app.tool == tool);
        }
        assert!(app.shared.lock().unwrap().undo.is_empty());
        let id = app.selected.clone();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "ai",
                &[json!({"op":"move","layer":id,"dx":12,"dy":7})],
                None,
                None,
                "AI move",
            )
            .unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert_eq!(app.animation.tool(ctx.input(|i| i.time)), Some("move"));
        assert_eq!(app.shared.lock().unwrap().doc.layers[0].x, 12);
        let q = app.transform_rects[0].unwrap().center();
        click(&mut app, &ctx, q);
        assert!(app.tool == Tool::None);
        assert!(app.animation.tool(ctx.input(|i| i.time)).is_none());
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        for size in [Vec2::new(1100., 600.), Vec2::new(1360., 900.)] {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            );
            let rect = app.transform_rects[3].unwrap();
            assert!(rect.right() <= size.x && rect.bottom() <= size.y);
        }
    }
    #[test]
    fn posterize_levels_drag_previews_native_pixels_and_typing_commits_one_edit() {
        for depth in [8, 16] {
            for typed in [false, true] {
                let (mut app, ctx) = fixture();
                let before = {
                    let mut e = app.shared.lock().unwrap();
                    e.doc = Document::new_depth(8, 8, depth).unwrap();
                    for y in 0..8 {
                        for x in 0..8 {
                            if depth == 16 {
                                e.doc.layers[0]
                                    .pixels
                                    .set16(x, y, [10001, 30003, 50007, 65535]);
                            } else {
                                e.doc.layers[0].pixels.set(x, y, [39, 117, 195, 255]);
                            }
                        }
                    }
                    let layer = e.doc.layers[0].id.clone();
                    e.edit(
                        "human",
                        &[json!({"op":"effect.add","layer":layer,"kind":"posterize"})],
                        None,
                        None,
                        "Fixture",
                    )
                    .unwrap();
                    e.undo.clear();
                    app.selected = layer.clone();
                    app.selection_layers = [layer.clone()].into_iter().collect();
                    app.effect_selected =
                        Some((layer, false, e.doc.layers[0].effects[0].id.clone()));
                    e.doc.clone()
                };
                let native = |doc: &Document| {
                    if depth == 16 {
                        crate::depth16::render(doc).unwrap().words
                    } else {
                        doc.preview(None, 8, None, false)
                            .unwrap()
                            .2
                            .into_iter()
                            .map(u16::from)
                            .collect()
                    }
                };
                let mut time = 0.;
                let mut draw = |app: &mut PeerBrush, events| {
                    time += 0.05;
                    ctx.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(1100., 900.),
                            )),
                            time: Some(time),
                            events,
                            ..Default::default()
                        },
                        |ctx| app.draw(ctx),
                    )
                };
                draw(&mut app, vec![]);
                let output = draw(&mut app, vec![]);
                let position = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == "4" => {
                            Some(Rect::from_min_size(text.pos, text.galley.size()).center())
                        }
                        _ => None,
                    })
                    .expect("Integer Levels value");
                let end = position + Vec2::new(35., 0.);
                if typed {
                    for _ in 0..2 {
                        draw(
                            &mut app,
                            vec![
                                egui::Event::PointerMoved(position),
                                button(
                                    position,
                                    egui::PointerButton::Primary,
                                    true,
                                    Default::default(),
                                ),
                            ],
                        );
                        draw(
                            &mut app,
                            vec![button(
                                position,
                                egui::PointerButton::Primary,
                                false,
                                Default::default(),
                            )],
                        );
                    }
                    assert!(ctx.wants_keyboard_input());
                    draw(&mut app, vec![egui::Event::Text("8".into())]);
                } else {
                    draw(
                        &mut app,
                        vec![
                            egui::Event::PointerMoved(position),
                            button(
                                position,
                                egui::PointerButton::Primary,
                                true,
                                Default::default(),
                            ),
                        ],
                    );
                    draw(
                        &mut app,
                        vec![egui::Event::PointerMoved(position + Vec2::new(25., 0.))],
                    );
                    draw(&mut app, vec![egui::Event::PointerMoved(end)]);
                }
                let gesture = app
                    .parameter_gesture
                    .as_ref()
                    .expect("Posterize parameter edit");
                let commands = if typed {
                    // Text remains provisional until Enter; the accepted value
                    // must match the same engine preview used by drag gestures.
                    assert_eq!(
                        app.shared.lock().unwrap().doc.layers[0].effects[0].settings["levels"],
                        4
                    );
                    vec![
                        json!({"op":"effect.update","layer":before.layers[0].id,"effect":before.layers[0].effects[0].id,"settings":{"levels":8}}),
                    ]
                } else {
                    gesture.commands.clone()
                };
                let draft = Engine::preview_edits(before.clone(), &commands).unwrap();
                assert!(draft.layers[0].effects[0].settings["levels"]
                    .as_u64()
                    .is_some());
                assert_ne!(
                    draft.layers[0].effects[0].settings["levels"],
                    before.layers[0].effects[0].settings["levels"],
                    "depth={depth} typed={typed} commands={commands:?}"
                );
                assert_ne!(native(&draft), native(&before));
                assert_eq!(
                    draft.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert!(app.shared.lock().unwrap().undo.is_empty());
                assert_eq!(app.shared.lock().unwrap().doc.revision, before.revision);
                if typed {
                    draw(
                        &mut app,
                        vec![egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: Default::default(),
                        }],
                    );
                } else {
                    draw(
                        &mut app,
                        vec![button(
                            end,
                            egui::PointerButton::Primary,
                            false,
                            Default::default(),
                        )],
                    );
                }
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.undo.len(), 1);
                assert_eq!(native(&e.doc), native(&draft));
                if typed {
                    assert_eq!(e.doc.layers[0].effects[0].settings["levels"], 8);
                }
                e.undo("human").unwrap();
                assert_eq!(native(&e.doc), native(&before));
                assert_eq!(
                    e.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
            }
        }
    }
    #[test]
    fn clamp_bounds_drag_preview_and_typed_acceptance_keep_native_sources_and_one_undo() {
        for depth in [8, 16] {
            for typed in [false, true] {
                let (mut app, ctx) = fixture();
                let before = {
                    let mut e = app.shared.lock().unwrap();
                    e.doc = Document::new_depth(8, 8, depth).unwrap();
                    for y in 0..8 {
                        for x in 0..8 {
                            if depth == 16 {
                                e.doc.layers[0]
                                    .pixels
                                    .set16(x, y, [10001, 30003, 50007, 12345]);
                            } else {
                                e.doc.layers[0].pixels.set(x, y, [39, 117, 195, 48]);
                            }
                        }
                    }
                    let layer = e.doc.layers[0].id.clone();
                    e.edit(
                        "human",
                        &[json!({"op":"effect.add","layer":layer,"kind":"channel_clamp"})],
                        None,
                        None,
                        "Fixture",
                    )
                    .unwrap();
                    e.undo.clear();
                    app.selected = layer.clone();
                    app.selection_layers = [layer.clone()].into_iter().collect();
                    app.effect_selected =
                        Some((layer, false, e.doc.layers[0].effects[0].id.clone()));
                    e.doc.clone()
                };
                let native = |doc: &Document| {
                    if depth == 16 {
                        crate::depth16::render(doc).unwrap().words
                    } else {
                        doc.preview(None, 8, None, false)
                            .unwrap()
                            .2
                            .into_iter()
                            .map(u16::from)
                            .collect()
                    }
                };
                let mut time = 0.;
                let mut draw = |app: &mut PeerBrush, events| {
                    time += 0.05;
                    ctx.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(1100., 900.),
                            )),
                            time: Some(time),
                            events,
                            ..Default::default()
                        },
                        |ctx| app.draw(ctx),
                    )
                };
                draw(&mut app, vec![]);
                let output = draw(&mut app, vec![]);
                let position = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == "0.00%" => {
                            Some(Rect::from_min_size(text.pos, text.galley.size()).center())
                        }
                        _ => None,
                    })
                    .expect("Clamp minimum value");
                let end = position + Vec2::new(35., 0.);
                if typed {
                    for _ in 0..2 {
                        draw(
                            &mut app,
                            vec![
                                egui::Event::PointerMoved(position),
                                button(
                                    position,
                                    egui::PointerButton::Primary,
                                    true,
                                    Default::default(),
                                ),
                            ],
                        );
                        draw(
                            &mut app,
                            vec![button(
                                position,
                                egui::PointerButton::Primary,
                                false,
                                Default::default(),
                            )],
                        );
                    }
                    assert!(ctx.wants_keyboard_input());
                    draw(&mut app, vec![egui::Event::Text("90".into())]);
                } else {
                    draw(
                        &mut app,
                        vec![
                            egui::Event::PointerMoved(position),
                            button(
                                position,
                                egui::PointerButton::Primary,
                                true,
                                Default::default(),
                            ),
                        ],
                    );
                    draw(
                        &mut app,
                        vec![egui::Event::PointerMoved(position + Vec2::new(25., 0.))],
                    );
                    draw(&mut app, vec![egui::Event::PointerMoved(end)]);
                }
                let gesture = app
                    .parameter_gesture
                    .as_ref()
                    .expect("Clamp parameter edit");
                let commands = if typed {
                    // Text remains provisional until Enter; the accepted value
                    // must match the same engine preview used by drag gestures.
                    assert_eq!(
                        app.shared.lock().unwrap().doc.layers[0].effects[0].settings["minimum"],
                        0.0
                    );
                    vec![
                        json!({"op":"effect.update","layer":before.layers[0].id,"effect":before.layers[0].effects[0].id,"settings":{"channel":"r","minimum":0.9,"maximum":1.0}}),
                    ]
                } else {
                    gesture.commands.clone()
                };
                let draft = Engine::preview_edits(before.clone(), &commands).unwrap();
                assert!(draft.layers[0].effects[0].settings["minimum"]
                    .as_f64()
                    .is_some());
                assert_ne!(
                    draft.layers[0].effects[0].settings["minimum"],
                    before.layers[0].effects[0].settings["minimum"],
                    "depth={depth} typed={typed} commands={commands:?}"
                );
                assert_ne!(native(&draft), native(&before));
                assert_eq!(
                    draft.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert!(app.shared.lock().unwrap().undo.is_empty());
                assert_eq!(app.shared.lock().unwrap().doc.revision, before.revision);
                if typed {
                    draw(
                        &mut app,
                        vec![egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: Default::default(),
                        }],
                    );
                } else {
                    draw(
                        &mut app,
                        vec![button(
                            end,
                            egui::PointerButton::Primary,
                            false,
                            Default::default(),
                        )],
                    );
                }
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.undo.len(), 1);
                assert_eq!(native(&e.doc), native(&draft));
                if typed {
                    assert_eq!(e.doc.layers[0].effects[0].settings["minimum"], 0.9);
                }
                e.undo("human").unwrap();
                assert_eq!(native(&e.doc), native(&before));
                assert_eq!(
                    e.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
            }
        }
    }
    #[test]
    fn clamp_channel_buttons_commit_exact_selected_channels_and_undo_at_both_depths() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture();
            let before = {
                let mut e = app.shared.lock().unwrap();
                e.doc = Document::new_depth(8, 8, depth).unwrap();
                for y in 0..8 {
                    for x in 0..8 {
                        if depth == 16 {
                            e.doc.layers[0]
                                .pixels
                                .set16(x, y, [10001, 30003, 50007, 12345]);
                        } else {
                            e.doc.layers[0].pixels.set(x, y, [39, 117, 195, 48]);
                        }
                    }
                }
                let layer = e.doc.layers[0].id.clone();
                e.edit("human", &[json!({"op":"effect.add","layer":layer,"kind":"channel_clamp","settings":{"channel":"r","minimum":0.8,"maximum":0.9}})], None, None, "Fixture").unwrap();
                e.undo.clear();
                app.selected = layer.clone();
                app.selection_layers = [layer.clone()].into_iter().collect();
                app.effect_selected = Some((layer, false, e.doc.layers[0].effects[0].id.clone()));
                e.doc.clone()
            };
            let mut time = 0.0;
            let mut draw = |app: &mut PeerBrush, events| {
                time += 0.05;
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100., 900.))),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            draw(&mut app, vec![]);
            for (channel, label) in [("g", "G"), ("b", "B"), ("o", "O")] {
                let output = draw(&mut app, vec![]);
                let point = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == label => {
                            Some(Rect::from_min_size(text.pos, text.galley.size()).center())
                        }
                        _ => None,
                    })
                    .expect("Clamp channel button");
                draw(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(point),
                        button(
                            point,
                            egui::PointerButton::Primary,
                            true,
                            Default::default(),
                        ),
                    ],
                );
                draw(
                    &mut app,
                    vec![button(
                        point,
                        egui::PointerButton::Primary,
                        false,
                        Default::default(),
                    )],
                );
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.doc.layers[0].effects[0].settings["channel"], channel);
                assert_eq!(e.undo.len(), 1);
                let expected=Engine::preview_edits(before.clone(),&[json!({"op":"effect.update","layer":before.layers[0].id,"effect":before.layers[0].effects[0].id,"settings":{"channel":channel,"minimum":0.8,"maximum":0.9}})]).unwrap();
                assert_eq!(
                    e.doc.preview(None, 8, None, false).unwrap().2,
                    expected.preview(None, 8, None, false).unwrap().2
                );
                assert_eq!(
                    e.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                e.undo("human").unwrap();
                assert_eq!(
                    e.doc.preview(None, 8, None, false).unwrap().2,
                    before.preview(None, 8, None, false).unwrap().2
                );
            }
        }
    }
    #[test]
    fn inline_effect_strength_previews_pixels_and_commits_once_at_both_depths() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let (mut app, ctx) = fixture();
                let before = {
                    let mut engine = app.shared.lock().unwrap();
                    engine.doc = Document::new_depth(32, 32, depth).unwrap();
                    for y in 0..32 {
                        for x in 0..32 {
                            if depth == 16 {
                                engine.doc.layers[0].pixels.set16(
                                    x,
                                    y,
                                    [12347, 33559, 51237, 65535],
                                );
                            } else {
                                engine.doc.layers[0].pixels.set(x, y, [48, 131, 200, 255]);
                            }
                        }
                    }
                    let layer = engine.doc.layers[0].id.clone();
                    let commands = if mask {
                        vec![
                            json!({"op":"mask.add","layer":layer,"value":255}),
                            json!({"op":"mask.step.add","layer":layer,"kind":"invert"}),
                        ]
                    } else {
                        vec![json!({"op":"effect.add","layer":layer,"kind":"invert"})]
                    };
                    engine
                        .edit("human", &commands, None, None, "Fixture")
                        .unwrap();
                    engine.undo.clear();
                    app.selected = layer.clone();
                    app.selection_layers = [layer].into_iter().collect();
                    engine.doc.clone()
                };
                app.mask = mask;
                let draw = |app: &mut PeerBrush, events| {
                    ctx.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(1100., 900.),
                            )),
                            events,
                            ..Default::default()
                        },
                        |ctx| app.draw(ctx),
                    )
                };
                let output = draw(&mut app, vec![]);
                let name = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == "Invert" => {
                            Some(text.pos)
                        }
                        _ => None,
                    })
                    .unwrap();
                let slider = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text)
                            if text.galley.job.text == "100%"
                                && text.pos.x > name.x
                                && (text.pos.y - name.y).abs() < 5. =>
                        {
                            Some(Rect::from_min_size(text.pos, text.galley.size()))
                        }
                        _ => None,
                    })
                    .unwrap();
                assert!(app.effect_area_rect.unwrap().contains_rect(slider));
                assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.job.text == "Weight")));
                let start = slider.center();
                draw(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(start),
                        button(
                            start,
                            egui::PointerButton::Primary,
                            true,
                            Default::default(),
                        ),
                    ],
                );
                let end = start - Vec2::new(15., 0.);
                draw(&mut app, vec![egui::Event::PointerMoved(end)]);
                let edit = app.parameter_gesture.as_ref().expect("inline slider drag");
                let draft = Engine::preview_edits(before.clone(), &edit.commands).unwrap();
                let rendered = |doc: &Document| {
                    if depth == 16 {
                        crate::depth16::render(doc).unwrap().words
                    } else {
                        doc.preview(None, 32, None, false)
                            .unwrap()
                            .2
                            .into_iter()
                            .map(u16::from)
                            .collect()
                    }
                };
                assert_ne!(rendered(&draft), rendered(&before));
                assert_eq!(
                    draft.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert!(app.shared.lock().unwrap().undo.is_empty());
                assert_eq!(app.shared.lock().unwrap().doc.revision, before.revision);
                draw(
                    &mut app,
                    vec![button(
                        end,
                        egui::PointerButton::Primary,
                        false,
                        Default::default(),
                    )],
                );
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(engine.undo.len(), 1);
                assert_eq!(rendered(&engine.doc), rendered(&draft));
                engine.undo("human").unwrap();
                assert_eq!(rendered(&engine.doc), rendered(&before));
                assert_eq!(
                    engine.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
            }
        }
    }
    #[test]
    fn typed_effect_strength_preserves_native_sources_and_commits_only_on_accept() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let (mut app, ctx) = fixture();
                let before = {
                    let mut engine = app.shared.lock().unwrap();
                    engine.doc = Document::new_depth(8, 8, depth).unwrap();
                    if depth == 16 {
                        engine.doc.layers[0]
                            .pixels
                            .set16(2, 3, [12347, 33559, 51237, 45679]);
                    } else {
                        engine.doc.layers[0].pixels.set(2, 3, [48, 131, 200, 177]);
                    }
                    let layer = engine.doc.layers[0].id.clone();
                    let commands = if mask {
                        vec![
                            json!({"op":"mask.add","layer":layer}),
                            json!({"op":"mask.step.add","layer":layer,"kind":"invert"}),
                        ]
                    } else {
                        vec![json!({"op":"effect.add","layer":layer,"kind":"invert"})]
                    };
                    engine
                        .edit("human", &commands, None, None, "Fixture")
                        .unwrap();
                    engine.undo.clear();
                    app.selected = layer.clone();
                    app.selection_layers = [layer].into_iter().collect();
                    engine.doc.clone()
                };
                app.mask = mask;
                let mut time = 0.0;
                let mut draw = |app: &mut PeerBrush, events| {
                    time += 0.05;
                    ctx.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(1100., 900.),
                            )),
                            time: Some(time),
                            events,
                            ..Default::default()
                        },
                        |ctx| app.draw(ctx),
                    )
                };
                let output = draw(&mut app, vec![]);
                let name = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == "Invert" => {
                            Some(text.pos)
                        }
                        _ => None,
                    })
                    .unwrap();
                let pos = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text)
                            if text.galley.job.text == "100%"
                                && text.pos.x > name.x
                                && (text.pos.y - name.y).abs() < 5. =>
                        {
                            Some(Rect::from_min_size(text.pos, text.galley.size()).center())
                        }
                        _ => None,
                    })
                    .unwrap();
                for _ in 0..2 {
                    draw(
                        &mut app,
                        vec![
                            egui::Event::PointerMoved(pos),
                            button(pos, egui::PointerButton::Primary, true, Default::default()),
                        ],
                    );
                    draw(
                        &mut app,
                        vec![button(
                            pos,
                            egui::PointerButton::Primary,
                            false,
                            Default::default(),
                        )],
                    );
                }
                assert!(ctx.wants_keyboard_input());
                assert!(app.shared.lock().unwrap().undo.is_empty());
                draw(
                    &mut app,
                    vec![
                        egui::Event::Key {
                            key: egui::Key::W,
                            physical_key: Some(egui::Key::W),
                            pressed: true,
                            repeat: false,
                            modifiers: Default::default(),
                        },
                        egui::Event::Text("23.5".into()),
                    ],
                );
                assert!(app.tool == Tool::Brush, "Typing must retain tool focus");
                assert!(app.shared.lock().unwrap().undo.is_empty());
                draw(
                    &mut app,
                    vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }],
                );
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(engine.undo.len(), 1);
                let weight = if mask {
                    engine.doc.layers[0]
                        .mask
                        .as_ref()
                        .unwrap()
                        .steps
                        .last()
                        .unwrap()
                        .weight
                } else {
                    engine.doc.layers[0].effects[0].weight
                };
                assert!((weight - 0.235).abs() < 0.00001);
                assert_eq!(engine.doc.bit_depth, depth);
                assert_eq!(
                    engine.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                let rendered = |doc: &Document| {
                    if depth == 16 {
                        crate::depth16::render(doc).unwrap().words
                    } else {
                        doc.preview(None, 8, None, false)
                            .unwrap()
                            .2
                            .into_iter()
                            .map(u16::from)
                            .collect()
                    }
                };
                assert_ne!(rendered(&engine.doc), rendered(&before));
                engine.undo("human").unwrap();
                assert_eq!(rendered(&engine.doc), rendered(&before));
            }
        }
    }
    #[test]
    fn typed_parameters_cancel_on_newer_work_and_project_switches() {
        for switch in [false, true] {
            let (mut app, ctx) = fixture();
            frame(&mut app, &ctx, vec![], Default::default());
            let source = app.shared.clone();
            let before = source.lock().unwrap().doc.clone();
            let pos = app.opacity_rect.unwrap().center();
            click(&mut app, &ctx, pos);
            click(&mut app, &ctx, pos);
            let control = app.parameter_gesture.as_ref().unwrap().control;
            assert!(controls::number_editing(&ctx, control));
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::Text("35".into())],
                Default::default(),
            );
            assert!(source.lock().unwrap().undo.is_empty());
            if switch {
                let destination = crate::workspace::new_project(
                    &app.workspace_root,
                    8,
                    8,
                    8,
                    Some(&app.project_id),
                )
                .unwrap();
                let project = destination.lock().unwrap().project_id.clone();
                app.switch_project(&project);
            } else {
                source.lock().unwrap().edit("human",&[json!({"op":"layer.update","layer":app.selected,"name":"Newer human work","opacity":0.8})],None,None,"Human edit").unwrap();
            }
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
                Default::default(),
            );
            assert!(app.parameter_gesture.is_none());
            assert!(!controls::number_editing(&ctx, control));
            let original = source.lock().unwrap();
            if switch {
                assert_eq!(original.doc.revision, before.revision);
                assert_eq!(original.doc.layers[0].opacity, 1.0);
                assert!(original.undo.is_empty());
                assert!(app.shared.lock().unwrap().undo.is_empty());
            } else {
                assert_eq!(original.doc.layers[0].name, "Newer human work");
                assert_eq!(original.doc.layers[0].opacity, 0.8);
                assert_eq!(original.undo.len(), 1);
                assert!(app.message.contains("cancelled"));
            }
        }
    }
    #[test]
    fn parameter_preview_waits_for_release_and_cancels_when_source_changes() {
        let (mut app, ctx) = fixture();
        let layer = app.selected.clone();
        let before = app.shared.lock().unwrap().doc.clone();
        let mut rect = Rect::NOTHING;
        let mut draw = |app: &mut PeerBrush, events: Vec<egui::Event>| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 200.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let mut opacity = app.parameter_gesture.as_ref().map_or(100., |edit| {
                            edit.commands[0]["opacity"].as_f64().unwrap() as f32 * 100.
                        });
                        let response = controls::range(
                            ui,
                            "opacity-test",
                            &mut opacity,
                            0.0..=100.0,
                            200.,
                            "%",
                            0,
                            false,
                        );
                        rect = response.rect;
                        if response.changed() {
                            app.layer_parameter(
                                json!({"opacity":opacity/100.}),
                                "Opacity",
                                &response,
                            );
                        }
                    });
                },
            );
            rect
        };
        let bar = draw(&mut app, vec![]);
        let pos = bar.center();
        draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        draw(
            &mut app,
            vec![egui::Event::PointerMoved(pos + Vec2::new(-30., 0.))],
        );
        let edit = app.parameter_gesture.as_ref().unwrap();
        let draft = Engine::preview_edits(before.clone(), &edit.commands).unwrap();
        assert!(draft.layers[0].opacity < 0.5);
        assert_eq!(app.shared.lock().unwrap().doc.layers[0].opacity, 1.);
        assert!(app.shared.lock().unwrap().undo.is_empty());
        app.finish_parameter(false);
        assert_eq!(
            app.shared.lock().unwrap().doc.layers[0].opacity,
            draft.layers[0].opacity
        );
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        app.shared.lock().unwrap().undo("human").unwrap();
        assert_eq!(app.shared.lock().unwrap().doc.layers[0].opacity, 1.);
        let doc = app.shared.lock().unwrap().doc.clone();
        app.parameter_gesture = Some(ParameterEdit {
            control: egui::Id::new("test"),
            gesture: "test".into(),
            document: doc.id,
            revision: doc.revision,
            commands: vec![json!({"op":"layer.update","layer":layer,"opacity":0.2})],
            label: "Opacity".into(),
            typing: None,
            dirty: true,
        });
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"name":"Human rename"})],
                None,
                None,
                "Rename",
            )
            .unwrap();
        app.finish_parameter(false);
        assert_eq!(
            app.shared.lock().unwrap().doc.layers[0].name,
            "Human rename"
        );
        assert_eq!(app.shared.lock().unwrap().doc.layers[0].opacity, 1.);
        assert!(app.message.contains("cancelled"));
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
    fn effect_picker_filters_typed_names_and_adds_only_supported_stack_effects() {
        for (mask, query, label, kind) in [
            (false, "bLoOm", "Bloom", "bloom"),
            (true, "FeAtHeR", "Feather", "blur"),
            (true, "Bloom", "No matching effects", ""),
        ] {
            let (mut app, ctx) = fixture();
            let layer = app.selected.clone();
            if mask {
                app.shared
                    .lock()
                    .unwrap()
                    .edit(
                        "human",
                        &[json!({"op":"mask.add","layer":layer})],
                        None,
                        None,
                        "Mask",
                    )
                    .unwrap();
            }
            app.mask = mask;
            let draw = |app: &mut PeerBrush, events| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100., 900.))),
                        events,
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            draw(&mut app, vec![]);
            let add = app.effect_add_rect.unwrap().center();
            draw(
                &mut app,
                vec![
                    egui::Event::PointerMoved(add),
                    button(add, egui::PointerButton::Primary, true, Default::default()),
                ],
            );
            draw(
                &mut app,
                vec![button(
                    add,
                    egui::PointerButton::Primary,
                    false,
                    Default::default(),
                )],
            );
            let output = draw(&mut app, vec![]);
            let text_rect = |output: &egui::FullOutput, label: &str| {
                output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == label => {
                            Some(Rect::from_min_size(text.pos, text.galley.size()))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing picker label {label:?}: {:?}",
                            output
                                .shapes
                                .iter()
                                .filter_map(|shape| match &shape.shape {
                                    egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                        )
                    })
            };
            let search = text_rect(&output, "Search effects…").center();
            draw(
                &mut app,
                vec![
                    egui::Event::PointerMoved(search),
                    button(
                        search,
                        egui::PointerButton::Primary,
                        true,
                        Default::default(),
                    ),
                ],
            );
            draw(
                &mut app,
                vec![button(
                    search,
                    egui::PointerButton::Primary,
                    false,
                    Default::default(),
                )],
            );
            let output = draw(&mut app, vec![egui::Event::Text(query.into())]);
            let choice = text_rect(&output, label).center();
            if kind.is_empty() {
                assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
                continue;
            }
            assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,egui::Shape::Text(text) if text.galley.job.text == "Levels")));
            let before = app.shared.lock().unwrap().undo.len();
            draw(
                &mut app,
                vec![
                    egui::Event::PointerMoved(choice),
                    button(
                        choice,
                        egui::PointerButton::Primary,
                        true,
                        Default::default(),
                    ),
                ],
            );
            draw(
                &mut app,
                vec![button(
                    choice,
                    egui::PointerButton::Primary,
                    false,
                    Default::default(),
                )],
            );
            let mut engine = app.shared.lock().unwrap();
            assert_eq!(engine.undo.len(), before + 1);
            if mask {
                assert_eq!(
                    engine.doc.layers[0]
                        .mask
                        .as_ref()
                        .unwrap()
                        .steps
                        .last()
                        .unwrap()
                        .kind,
                    kind
                );
            } else {
                assert_eq!(engine.doc.layers[0].effects.last().unwrap().kind, kind);
            }
            engine.undo("human").unwrap();
            assert_eq!(engine.undo.len(), before);
        }
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
    fn import_choice_fixture(app: &mut PeerBrush, ctx: &egui::Context) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("peerbrush-choice-{}.gif", uuid::Uuid::new_v4()));
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder
            .encode_frames([[255, 0, 0, 255], [0, 255, 0, 255]].map(|color| {
                image::Frame::new(image::RgbaImage::from_pixel(2, 1, image::Rgba(color)))
            }))
            .unwrap();
        drop(encoder);
        std::fs::write(&path, bytes).unwrap();
        app.begin_import(Some(vec![path.clone()]));
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(10));
            frame(app, ctx, vec![], Default::default());
            if !app.busy {
                break;
            }
        }
        assert!(app.pending_import.is_some());
        path
    }
    fn wait_import(app: &mut PeerBrush, ctx: &egui::Context) {
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(10));
            frame(app, ctx, vec![], Default::default());
            if !app.busy {
                break;
            }
        }
        assert!(!app.busy);
    }
    #[test]
    fn import_choice_cancel_and_captured_frame_preserve_history_and_original_words() {
        let (mut app, ctx) = fixture();
        let path = import_choice_fixture(&mut app, &ctx);
        assert_eq!(app.shared.lock().unwrap().doc.revision, 0);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            Default::default(),
        );
        assert!(app.pending_import.is_none());
        assert!(app.shared.lock().unwrap().undo.is_empty());
        std::fs::remove_file(path).unwrap();
        let path = import_choice_fixture(&mut app, &ctx);
        app.pending_import.as_mut().unwrap().files[0].index = 1;
        std::fs::write(&path, b"Changed after inspection").unwrap();
        app.commit_import();
        wait_import(&mut app, &ctx);
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.doc.layers[0].pixels.get(0, 0), [0, 255, 0, 255]);
        assert_eq!(e.undo.len(), 1);
        assert_eq!(app.selected, e.doc.layers[0].id);
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers.len(), 1);
        drop(e);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn pending_import_rejects_newer_work_and_cannot_apply_to_another_or_closed_project() {
        let (mut app, ctx) = fixture();
        let path = import_choice_fixture(&mut app, &ctx);
        let layer = app.shared.lock().unwrap().doc.layers[0].id.clone();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"name":"Newer human work"})],
                None,
                None,
                "Rename",
            )
            .unwrap();
        app.commit_import();
        wait_import(&mut app, &ctx);
        assert_eq!(app.shared.lock().unwrap().doc.layers.len(), 1);
        assert_eq!(
            app.shared.lock().unwrap().doc.layers[0].name,
            "Newer human work"
        );
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        assert!(app.message.contains("changed"));
        std::fs::remove_file(path).unwrap();
        let path = import_choice_fixture(&mut app, &ctx);
        let source = app.shared.clone();
        let project = app.project_id.clone();
        let doc = source.lock().unwrap().doc.clone();
        let other = crate::workspace::register(&app.workspace_root, Engine::new(), None).unwrap();
        let other_id = other.lock().unwrap().project_id.clone();
        crate::workspace::select(&app.workspace_root, &other_id).unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        app.commit_import();
        assert!(app.pending_import.is_some());
        assert!(other.lock().unwrap().undo.is_empty());
        assert_eq!(source.lock().unwrap().undo.len(), 1);
        crate::workspace::close(
            &app.workspace_root,
            &project,
            &doc.id,
            doc.revision,
            true,
            "human",
        )
        .unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app.pending_import.is_none());
        assert!(other.lock().unwrap().undo.is_empty());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn svg_import_worker_retains_captured_source_native_depth_and_exact_single_undo() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture();
            {
                let mut e = app.shared.lock().unwrap();
                e.doc.bit_depth = depth;
                e.doc.ensure_depth();
                e.doc.layers[0]
                    .pixels
                    .set16(25, 25, [12345, 23457, 34569, 60001]);
            }
            let before = app.shared.lock().unwrap().doc.clone();
            let markup="<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'><rect width='16' height='16' fill='#ff0000'/></svg>";
            let path =
                std::env::temp_dir().join(format!("peerbrush-ui-svg-{}.svg", uuid::Uuid::new_v4()));
            std::fs::write(&path, markup).unwrap();
            let encoded = crate::image_import::Encoded::file(&path).unwrap();
            app.pending_import = Some(PendingImport {
                project: app.project_id.clone(),
                document: before.id.clone(),
                revision: before.revision,
                files: vec![ImportFile {
                    path: path.clone(),
                    encoded,
                    index: 0,
                }],
            });
            std::fs::write(&path, "Changed after inspection").unwrap();
            app.commit_import();
            wait_import(&mut app, &ctx);
            let mut e = app.shared.lock().unwrap();
            assert_eq!(e.doc.bit_depth, depth);
            assert_eq!(e.undo.len(), 1);
            assert_eq!(
                e.doc.layers[0].source.as_ref().unwrap().content,
                crate::source::Content::Svg { svg: markup.into() }
            );
            assert_eq!(e.doc.layers[0].pixels.get(4, 4), [255, 0, 0, 255]);
            assert_eq!(
                e.doc.layers[1].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(app.selected, e.doc.layers[0].id);
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            drop(e);
            std::fs::remove_file(path).unwrap();
        }
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
    fn layer_context_opens_over_child_controls_without_primary_side_effects() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture();
            let (child, other, folder) =
                {
                    let mut engine = app.shared.lock().unwrap();
                    engine.doc = Document::new_depth(8, 8, depth).unwrap();
                    let child = engine.doc.layers[0].id.clone();
                    engine.doc.layers[0]
                        .pixels
                        .set16(2, 2, [12347, 33559, 51237, 45679]);
                    let result = engine.edit("human", &[
                    json!({"op":"layer.add","name":"Outside"}),
                    json!({"op":"mask.add","layer":child}),
                    json!({"op":"group.create_selected","layers":[child],"name":"Folder"}),
                ], None, None, "Fixture").unwrap();
                    let other = engine
                        .doc
                        .layers
                        .iter()
                        .find(|l| l.name == "Outside")
                        .unwrap()
                        .id
                        .clone();
                    let folder = result["created"]
                        .as_array()
                        .unwrap()
                        .last()
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string();
                    engine.undo.clear();
                    (child, other, folder)
                };
            app.select_content(&other);
            app.selection_layers.insert(folder.clone());
            let draw = |app: &mut PeerBrush| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            for _ in 0..3 {
                draw(&mut app);
            }
            let before = app.shared.lock().unwrap().doc.export_png().unwrap();
            let selected = app.selection_layers.clone();
            let row = app.layer_rects[&child];
            let group = app.layer_rects[&folder];
            let positions = [
                (row.left_top() + Vec2::new(2., 2.), &child),
                (app.eye_rects[&child].center(), &child),
                (row.left_center() + Vec2::new(108., 0.), &child),
                (row.right_center() - Vec2::new(65., 0.), &child),
                (row.right_center() - Vec2::new(20., 0.), &child),
                (row.right_top() + Vec2::new(-2., 2.), &child),
                (
                    egui::pos2(app.eye_rects[&folder].right() + 15., group.center().y),
                    &folder,
                ),
                (group.left_center() + Vec2::new(85., 0.), &folder),
            ];
            for (position, target) in positions {
                frame(
                    &mut app,
                    &ctx,
                    vec![
                        egui::Event::PointerMoved(position),
                        button(
                            position,
                            egui::PointerButton::Secondary,
                            true,
                            Default::default(),
                        ),
                    ],
                    Default::default(),
                );
                frame(
                    &mut app,
                    &ctx,
                    vec![button(
                        position,
                        egui::PointerButton::Secondary,
                        false,
                        Default::default(),
                    )],
                    Default::default(),
                );
                let output = draw(&mut app);
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,egui::Shape::Text(text) if text.galley.job.text=="Rename")),"No menu over {position:?}");
                assert_eq!(
                    app.layer_context.as_ref().unwrap().0,
                    app.shared.lock().unwrap().doc.id
                );
                assert_eq!(app.selected, other);
                assert_eq!(app.selection_layers, selected);
                assert!(!app.mask);
                assert!(app.collapsed.is_empty());
                assert!(
                    app.layer_drag.is_none()
                        && app.eye_sweep.is_none()
                        && app.rename_edit.is_none()
                );
                assert!(app.shared.lock().unwrap().undo.is_empty());
                if target == &child && position == row.right_center() - Vec2::new(20., 0.) {
                    let lock = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.job.text == "Lock" => {
                                Some(text.visual_bounding_rect().center())
                            }
                            _ => None,
                        })
                        .unwrap();
                    click(&mut app, &ctx, lock);
                    assert!(app.layer_context.is_none());
                    let mut engine = app.shared.lock().unwrap();
                    assert!(
                        engine
                            .doc
                            .layers
                            .iter()
                            .find(|l| l.id == child)
                            .unwrap()
                            .locked
                    );
                    assert!(
                        !engine
                            .doc
                            .layers
                            .iter()
                            .find(|l| l.id == other)
                            .unwrap()
                            .locked
                    );
                    assert_eq!(engine.undo.len(), 1);
                    engine.undo("human").unwrap();
                    drop(engine);
                    app.select_content(&other);
                    app.selection_layers.insert(folder.clone());
                } else {
                    key(&mut app, &ctx, egui::Key::Escape);
                    assert!(app.layer_context.is_none());
                }
                assert_eq!(app.shared.lock().unwrap().doc.export_png().unwrap(), before);
            }
        }
    }
    #[test]
    fn layer_context_respects_scroll_clipping_outside_clicks_and_project_changes() {
        let (mut app, ctx) = small_fixture();
        {
            let mut engine = app.shared.lock().unwrap();
            let commands = (0..18)
                .map(|i| json!({"op":"layer.add","name":format!("Row {i}")}))
                .collect::<Vec<_>>();
            engine
                .edit("human", &commands, None, None, "Fixture")
                .unwrap();
            engine.undo.clear();
        }
        app.tool = Tool::None;
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let clipped = app
            .layer_rects
            .values()
            .find(|row| {
                row.center().y > app.color_tab_rect.unwrap().bottom() + 40. && row.center().y < 840.
            })
            .unwrap()
            .left_center()
            + Vec2::new(2., 0.);
        let secondary = |app: &mut PeerBrush, position| {
            frame(
                app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(position),
                    button(
                        position,
                        egui::PointerButton::Secondary,
                        true,
                        Default::default(),
                    ),
                ],
                Default::default(),
            );
            frame(
                app,
                &ctx,
                vec![button(
                    position,
                    egui::PointerButton::Secondary,
                    false,
                    Default::default(),
                )],
                Default::default(),
            );
            frame(app, &ctx, vec![], Default::default());
        };
        secondary(&mut app, clipped);
        assert!(
            app.layer_context.is_none(),
            "Clipped rows cannot open menus through the controls beneath them"
        );
        let first = app.shared.lock().unwrap().doc.layers[0].id.clone();
        let point = app.layer_rects[&first].right_top() + Vec2::new(-2., 2.);
        secondary(&mut app, point);
        assert!(app.layer_context.is_some());
        let canvas = app.view_rect.unwrap().center();
        click(&mut app, &ctx, canvas);
        assert!(app.layer_context.is_none());
        assert!(app.shared.lock().unwrap().undo.is_empty());
        secondary(&mut app, point);
        assert!(app.layer_context.is_some());
        let second = crate::workspace::register_in(&app.workspace, Engine::new(), None).unwrap();
        let second_id = second.lock().unwrap().project_id.clone();
        crate::workspace::select_in(&app.workspace, &second_id).unwrap();
        app.switch_project(&second_id);
        assert!(app.layer_context.is_none());
        frame(&mut app, &ctx, vec![], Default::default());
        let point = app.layer_rects[&app.selected].right_top() + Vec2::new(-2., 2.);
        secondary(&mut app, point);
        assert!(app.layer_context.is_some());
        app.shared.lock().unwrap().doc = Document::new(8, 8).unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app.layer_context.is_none());
        assert!(app.shared.lock().unwrap().undo.is_empty());
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
    fn folder_guides_show_nested_containment_and_collapse_without_source_edits() {
        let (mut app, ctx) = fixture();
        let outer = crate::engine::Layer::new("Outer folder", "group", 8, 8);
        let mut inner = crate::engine::Layer::new("Inner folder", "group", 8, 8);
        inner.parent = Some(outer.id.clone());
        let mut leaf = crate::engine::Layer::new("Nested paint", "paint", 8, 8);
        leaf.parent = Some(inner.id.clone());
        leaf.pixels.set(2, 2, [201, 81, 31, 177]);
        let mut sibling = crate::engine::Layer::new("Folder sibling", "paint", 8, 8);
        sibling.parent = Some(outer.id.clone());
        let loose = crate::engine::Layer::new("Outside folder", "paint", 8, 8);
        {
            let mut engine = app.shared.lock().unwrap();
            engine.doc.layers = vec![
                outer.clone(),
                inner.clone(),
                leaf.clone(),
                sibling.clone(),
                loose.clone(),
            ];
        }
        app.select_content(&loose.id);
        let before = app.shared.lock().unwrap().doc.export_png().unwrap();
        let mut time = 1.0;
        let mut draw = |app: &mut PeerBrush| {
            time += 1.0;
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360.0, 900.0))),
                    time: Some(time),
                    system_theme: Some(egui::Theme::Light),
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            )
        };
        draw(&mut app);
        let output = draw(&mut app);
        assert_eq!(
            ctx.style().visuals.panel_fill,
            Color32::from_rgb(41, 39, 43)
        );
        let guides = |output: &egui::FullOutput| {
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::LineSegment { points, stroke }
                        if stroke.color == MUTED.gamma_multiply(0.8) =>
                    {
                        Some(*points)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let lines = guides(&output);
        assert_eq!(lines.len(), 5, "Two parent trunks and three child branches");
        let mut columns = lines
            .iter()
            .filter(|points| points[0].x == points[1].x)
            .map(|points| points[0].x)
            .collect::<Vec<_>>();
        columns.sort_by(f32::total_cmp);
        assert!((columns[1] - columns[0] - 22.0).abs() < 0.1);
        let eyes = [&outer, &inner, &leaf, &sibling, &loose]
            .map(|layer| app.eye_rects[&layer.id].center().x);
        assert!(eyes.iter().all(|x| (*x - eyes[0]).abs() < 0.1));
        let nested_name = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == leaf.name => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .unwrap();
        click(&mut app, &ctx, nested_name);
        assert_eq!(app.selected, leaf.id);
        // Click the nested folder's disclosure, then the outer folder's.
        let inner_toggle = egui::pos2(columns[1], app.layer_rects[&inner.id].center().y);
        click(&mut app, &ctx, inner_toggle);
        assert!(app.collapsed.contains(&inner.id));
        let output = draw(&mut app);
        assert_eq!(guides(&output).len(), 3);
        assert!(!app.layer_rects.contains_key(&leaf.id));
        assert!(app.layer_rects.contains_key(&sibling.id));
        let outer_toggle = egui::pos2(columns[0], app.layer_rects[&outer.id].center().y);
        click(&mut app, &ctx, outer_toggle);
        assert!(app.collapsed.contains(&outer.id));
        let output = draw(&mut app);
        assert!(guides(&output).is_empty());
        assert_eq!(app.layer_rects.len(), 2);
        assert!(app.layer_rects.contains_key(&loose.id));
        let engine = app.shared.lock().unwrap();
        assert!(engine.undo.is_empty());
        assert_eq!(engine.doc.revision, 0);
        assert_eq!(engine.doc.export_png().unwrap(), before);
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
                coarse: false,
                error: None,
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
    fn command_g_groups_and_shift_g_merges_native_sources_with_one_undo_each() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture();
            let before = {
                let mut engine = app.shared.lock().unwrap();
                engine.doc = Document::new_depth(12, 10, depth).unwrap();
                let first = engine.doc.layers[0].id.clone();
                engine
                    .edit(
                        "human",
                        &[
                            json!({"op":"layer.add","name":"Middle"}),
                            json!({"op":"layer.add","name":"Top"}),
                            json!({"op":"mask.add","layer":first,"value":153}),
                        ],
                        None,
                        None,
                        "Fixture",
                    )
                    .unwrap();
                for (i, layer) in engine.doc.layers.iter_mut().enumerate() {
                    layer
                        .pixels
                        .set16(i as i32 + 2, 3, [12347, 33559, 51237, 45679]);
                }
                engine.undo.clear();
                engine.doc.clone()
            };
            let ids = before
                .layers
                .iter()
                .map(|l| l.id.clone())
                .collect::<Vec<_>>();
            app.select_content(&ids[0]);
            app.selection_layers.insert(ids[1].clone());
            frame(&mut app, &ctx, vec![], Default::default());
            let command = egui::Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            };
            // Event modifiers remain authoritative when Control was released
            // before this frame's final input state.
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::Key {
                    key: egui::Key::G,
                    physical_key: Some(egui::Key::G),
                    pressed: true,
                    repeat: false,
                    modifiers: command,
                }],
                Default::default(),
            );
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::Key {
                    key: egui::Key::G,
                    physical_key: Some(egui::Key::G),
                    pressed: false,
                    repeat: false,
                    modifiers: Default::default(),
                }],
                Default::default(),
            );
            assert!(app.tool == Tool::Brush);
            let folder = app.selected.clone();
            let grouped = {
                let engine = app.shared.lock().unwrap();
                assert_eq!(engine.undo.len(), 1);
                assert_eq!(app.selection_layers, [folder.clone()].into_iter().collect());
                assert!(engine
                    .doc
                    .layers
                    .iter()
                    .any(|l| l.id == folder && l.kind == "group"));
                for old in &before.layers {
                    let mut current = engine
                        .doc
                        .layers
                        .iter()
                        .find(|l| l.id == old.id)
                        .unwrap()
                        .clone();
                    assert_eq!(
                        current.parent.as_deref(),
                        if ids[..2].contains(&old.id) {
                            Some(folder.as_str())
                        } else {
                            None
                        }
                    );
                    current.parent = old.parent.clone();
                    assert_eq!(
                        serde_json::to_value(current).unwrap(),
                        serde_json::to_value(old).unwrap()
                    );
                }
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                engine.doc.clone()
            };
            // Selecting a child as well must not merge it twice.
            app.selection_layers.insert(ids[0].clone());
            let shifted = egui::Modifiers {
                shift: true,
                ..command
            };
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::Key {
                    key: egui::Key::G,
                    physical_key: Some(egui::Key::G),
                    pressed: true,
                    repeat: false,
                    modifiers: shifted,
                }],
                Default::default(),
            );
            let reply = app.merge_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(reply.result.is_ok(), "{:?}", reply.result);
            app.merge_tx.send(reply).unwrap();
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::Key {
                    key: egui::Key::G,
                    physical_key: Some(egui::Key::G),
                    pressed: false,
                    repeat: false,
                    modifiers: Default::default(),
                }],
                Default::default(),
            );
            assert!(!app.busy);
            assert!(app.tool == Tool::Brush);
            let mut engine = app.shared.lock().unwrap();
            assert_eq!(engine.undo.len(), 2);
            assert_eq!(engine.doc.layers.len(), 2);
            assert_eq!(
                app.selection_layers,
                [app.selected.clone()].into_iter().collect()
            );
            assert_eq!(
                engine.doc.export_png().unwrap(),
                before.export_png().unwrap()
            );
            assert_eq!(engine.doc.bit_depth, depth);
            let loaded = crate::psd::decode(&crate::psd::encode(&engine.doc).unwrap()).unwrap();
            assert!(!loaded.read_only);
            assert_eq!(loaded.bit_depth, depth);
            assert_eq!(loaded.export_png().unwrap(), before.export_png().unwrap());
            engine.undo("human").unwrap();
            assert_eq!(
                serde_json::to_value(&engine.doc.layers).unwrap(),
                serde_json::to_value(&grouped.layers).unwrap()
            );
            engine.undo("human").unwrap();
            assert_eq!(
                serde_json::to_value(&engine.doc.layers).unwrap(),
                serde_json::to_value(&before.layers).unwrap()
            );
        }
    }
    #[test]
    fn group_merge_shortcuts_preserve_text_and_reject_locked_reserved_or_stale_sources() {
        for failure in ["typing", "locked", "reserved", "stale", "replaced"] {
            let (mut app, ctx) = small_fixture();
            let top = {
                let mut engine = app.shared.lock().unwrap();
                let result = engine
                    .edit(
                        "human",
                        &[json!({"op":"layer.add","name":"Other"})],
                        None,
                        None,
                        "Fixture",
                    )
                    .unwrap();
                engine.undo.clear();
                result["created"][0].as_str().unwrap().to_string()
            };
            app.select_content(&top);
            app.selection_layers = app
                .shared
                .lock()
                .unwrap()
                .doc
                .layers
                .iter()
                .map(|layer| layer.id.clone())
                .collect();
            let old = app.shared.lock().unwrap().doc.clone();
            let selected = app.selected.clone();
            match failure {
                "typing" => {
                    app.begin_rename(&old, &selected);
                    frame(&mut app, &ctx, vec![], Default::default());
                    for shift in [false, true] {
                        modified_key(
                            &mut app,
                            &ctx,
                            egui::Key::G,
                            egui::Modifiers {
                                ctrl: true,
                                command: true,
                                shift,
                                ..Default::default()
                            },
                        );
                    }
                    assert!(app.rename_edit.is_some());
                    assert!(!app.busy);
                }
                "locked" | "reserved" => {
                    {
                        let mut engine = app.shared.lock().unwrap();
                        if failure == "locked" {
                            engine.doc.layers[0].locked = true;
                        } else {
                            engine
                                .reserve(
                                    "agent",
                                    "Layer work",
                                    vec![crate::engine::Scope::layer(&selected)],
                                )
                                .unwrap();
                        }
                    }
                    for shift in [false, true] {
                        frame(
                            &mut app,
                            &ctx,
                            vec![egui::Event::Key {
                                key: egui::Key::G,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers {
                                    ctrl: true,
                                    command: true,
                                    shift,
                                    ..Default::default()
                                },
                            }],
                            Default::default(),
                        );
                        if shift {
                            let reply = app.merge_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                            assert!(reply.result.is_err());
                            app.merge_tx.send(reply).unwrap();
                        }
                        frame(
                            &mut app,
                            &ctx,
                            vec![egui::Event::Key {
                                key: egui::Key::G,
                                physical_key: None,
                                pressed: false,
                                repeat: false,
                                modifiers: Default::default(),
                            }],
                            Default::default(),
                        );
                        assert_eq!(app.selected, selected);
                        assert!(!app.busy);
                    }
                }
                "stale" => {
                    app.shared
                        .lock()
                        .unwrap()
                        .edit(
                            "human",
                            &[json!({"op":"layer.update","layer":selected,"name":"Newer name"})],
                            None,
                            None,
                            "New work",
                        )
                        .unwrap();
                    app.create_folder(&old);
                    assert!(app.message.contains("revision"));
                }
                "replaced" => {
                    app.shared.lock().unwrap().doc = Document::new_depth(8, 8, 16).unwrap();
                    let current = app.shared.lock().unwrap().doc.export_png().unwrap();
                    app.create_folder(&old);
                    assert!(app.message.contains("document changed"));
                    assert_eq!(
                        app.shared.lock().unwrap().doc.export_png().unwrap(),
                        current
                    );
                }
                _ => unreachable!(),
            }
            let engine = app.shared.lock().unwrap();
            assert_eq!(
                engine.doc.layers.len(),
                if failure == "replaced" { 1 } else { 2 }
            );
            assert_eq!(engine.undo.len(), usize::from(failure == "stale"));
            if failure != "replaced" {
                assert_eq!(engine.doc.export_png().unwrap(), old.export_png().unwrap());
            }
        }
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
    #[test]
    fn retouch_source_alignment_live_pixels_and_one_undo_follow_native_input() {
        for shortcut in [egui::Key::C, egui::Key::J] {
            let (mut app, ctx) = small_fixture();
            {
                let mut e = app.shared.lock().unwrap();
                for y in 0..32 {
                    for x in 0..32 {
                        e.doc.layers[0].pixels.set(
                            x,
                            y,
                            [if x < 16 { 210 } else { 40 }, 80, 35, 255],
                        );
                    }
                }
                e.doc.layers[0].pixels.set(23, 16, [250, 180, 120, 255]);
            }
            key(&mut app, &ctx, shortcut);
            app.radius = 3.;
            app.brush.hardness = 1.;
            frame(&mut app, &ctx, vec![], Default::default());
            let rect = app.view_rect.unwrap();
            let scale = rect.width() / 32.;
            let source = rect.min + Vec2::new(7., 16.) * scale;
            let start = rect.min + Vec2::new(22., 16.) * scale;
            let end = rect.min + Vec2::new(24., 16.) * scale;
            click(&mut app, &ctx, start);
            assert!(app.shared.lock().unwrap().undo.is_empty());
            assert!(app.message.contains("Alt-click"));
            let alt = egui::Modifiers {
                alt: true,
                ..Default::default()
            };
            frame(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(source),
                    button(source, egui::PointerButton::Primary, true, alt),
                ],
                alt,
            );
            frame(
                &mut app,
                &ctx,
                vec![button(source, egui::PointerButton::Primary, false, alt)],
                alt,
            );
            assert!(app.retouch_source.is_some());
            assert!(app.shared.lock().unwrap().undo.is_empty());
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
            let before = app.shared.lock().unwrap().doc.clone();
            let command = app.stroke_command(app.points.clone(), app.color);
            let preview = Engine::preview_edits(before.clone(), &[command]).unwrap();
            assert_ne!(preview.export_png().unwrap(), before.export_png().unwrap());
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
            let mut e = app.shared.lock().unwrap();
            assert_eq!(e.undo.len(), 1);
            assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            drop(e);
            let doc = app.shared.lock().unwrap().doc.clone();
            assert!(app.begin_retouch(&doc, [26., 16.]));
            let next = app.stroke_command(vec![[26., 16.]], app.color);
            assert_eq!(next["source"], json!([11., 16.]));
            app.mask = true;
            assert!(!app.begin_retouch(&doc, [26., 16.]));
            assert!(app.retouch_source.is_none());
        }
    }
    #[test]
    fn editable_source_previews_render_pixels_without_painting_and_cancel_on_shared_changes() {
        for kind in ["text", "shape", "svg"] {
            let (mut app, ctx) = small_fixture();
            frame(&mut app, &ctx, vec![], Default::default());
            let doc = app.shared.lock().unwrap().doc.clone();
            app.open_source(&doc, kind);
            let editor = app.source_editor.as_mut().unwrap();
            editor.x = 0;
            editor.y = 0;
            if kind == "svg" {
                editor.source=crate::source::Source{width:16,height:16,matrix:[1.,0.,0.,1.,0.,0.],content:crate::source::Content::Svg{svg:"<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'><rect x='2' y='2' width='12' height='12' fill='red'/></svg>".into()}};
            }
            if let crate::source::Content::Text { text, size, .. } = &mut editor.source.content {
                *text = "Hi".into();
                *size = 16.;
            }
            let command = editor.command();
            let expected = Engine::preview_edits(doc.clone(), &[command.clone()]).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(4);
            loop {
                frame(&mut app, &ctx, vec![], Default::default());
                if !app.pending
                    && app
                        .canvas_pixels
                        .as_ref()
                        .is_some_and(|(_, _, p)| p.chunks_exact(4).any(|v| v[3] > 0))
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "Editable source preview did not finish"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let (w, h, pixels) = app.canvas_pixels.as_ref().unwrap();
            assert_eq!(
                pixels,
                &expected.preview(None, (*w).max(*h), None, false).unwrap().2
            );
            assert!(app.shared.lock().unwrap().undo.is_empty());
            let center = app.view_rect.unwrap().center();
            click(&mut app, &ctx, center);
            assert!(app.shared.lock().unwrap().undo.is_empty());
            app.cancel_source();
            assert!(app.source_editor.is_none());
            app.shared
                .lock()
                .unwrap()
                .edit("human", &[command], Some(doc.revision), None, "Source")
                .unwrap();
            let current = app.shared.lock().unwrap().doc.clone();
            app.select_content(&current.layers[0].id);
            app.open_source(&current, "edit");
            app.shared
                .lock()
                .unwrap()
                .edit(
                    "other",
                    &[json!({"op":"layer.update","layer":app.selected,"visible":false})],
                    None,
                    None,
                    "Shared edit",
                )
                .unwrap();
            app.request_preview(&ctx, &current);
            assert!(app.source_editor.is_none());
        }
    }
    #[test]
    fn document_geometry_previews_pixels_dimensions_and_cancels_when_source_changes() {
        for mode in ["crop", "canvas.resize", "image.resize"] {
            let (mut app, ctx) = small_fixture();
            {
                let mut e = app.shared.lock().unwrap();
                for y in 0..32 {
                    for x in 0..32 {
                        e.doc.layers[0]
                            .pixels
                            .set(x, y, [x as u8 * 7, y as u8 * 7, 53, 255]);
                    }
                }
            }
            frame(&mut app, &ctx, vec![], Default::default());
            let original = app.shared.lock().unwrap().doc.clone();
            app.open_geometry(&original, mode);
            let editor = app.geometry.as_mut().unwrap();
            editor.rect = [8, 4, 24, 28];
            editor.width = 48;
            editor.height = 24;
            editor.proportional = false;
            let command = editor.command();
            let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(4);
            loop {
                frame(&mut app, &ctx, vec![], Default::default());
                if !app.pending
                    && app.canvas_pixels.as_ref().is_some_and(|(w, h, _)| {
                        (*w as f32 / *h as f32 - expected.width as f32 / expected.height as f32)
                            .abs()
                            < 0.005
                    })
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "Geometry preview did not finish"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let (w, h, pixels) = app.canvas_pixels.as_ref().unwrap();
            let rendered = expected.preview(None, (*w).max(*h), None, false).unwrap();
            assert_eq!(pixels, &rendered.2);
            let view = app.view_rect.unwrap();
            assert!(
                (view.width() / view.height() - expected.width as f32 / expected.height as f32)
                    .abs()
                    < 0.0001
            );
            assert!(app.shared.lock().unwrap().undo.is_empty());
            assert_eq!(
                app.shared.lock().unwrap().doc.export_png().unwrap(),
                original.export_png().unwrap()
            );
            let center = view.center();
            click(&mut app, &ctx, center);
            assert!(
                app.shared.lock().unwrap().undo.is_empty(),
                "Geometry preview must not paint"
            );
            app.cancel_geometry();
            assert!(app.geometry.is_none());
            assert!(app.shared.lock().unwrap().undo.is_empty());
            app.open_geometry(&original, mode);
            app.shared
                .lock()
                .unwrap()
                .edit(
                    "other",
                    &[json!({"op":"paint.fill","layer":app.selected,"color":[17,61,91,255]})],
                    None,
                    None,
                    "Newer work",
                )
                .unwrap();
            app.request_preview(&ctx, &original);
            assert!(
                app.geometry.is_none(),
                "A source change must discard the stale geometry editor"
            );
            assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        }
    }
    fn small_fixture() -> (PeerBrush, egui::Context) {
        let (mut app, ctx) = fixture();
        let doc = Document::new(32, 32).unwrap();
        let id = doc.layers[0].id.clone();
        app.shared.lock().unwrap().doc = doc;
        app.select_content(&id);
        (app, ctx)
    }
    #[test]
    fn magic_wand_selects_without_creating_or_changing_native_masks() {
        for depth in [8, 16] {
            for existing_mask in [false, true] {
                let (mut app, ctx) = fixture();
                let before = {
                    let mut engine = app.shared.lock().unwrap();
                    engine.doc = Document::new_depth(8, 8, depth).unwrap();
                    for y in 0..8 {
                        for x in 0..8 {
                            engine.doc.layers[0].pixels.set16(
                                x,
                                y,
                                if x < 4 {
                                    [12347, 33559, 51237, 65535]
                                } else {
                                    [51239, 12349, 33561, 65535]
                                },
                            );
                        }
                    }
                    let id = engine.doc.layers[0].id.clone();
                    if existing_mask {
                        engine
                            .edit(
                                "human",
                                &[json!({"op":"mask.add","layer":id})],
                                None,
                                None,
                                "Fixture mask",
                            )
                            .unwrap();
                    }
                    engine.undo.clear();
                    engine.doc.clone()
                };
                app.select_content(&before.layers[0].id);
                app.mask = existing_mask;
                app.selection_tolerance = 0.0;
                app.selection_merged = false;
                frame(&mut app, &ctx, vec![], Default::default());
                modified_key(&mut app, &ctx, egui::Key::K, Default::default());
                assert!(app.tool == Tool::MagicWand);
                assert_eq!(app.selection_kind, "wand");
                let canvas = app.view_rect.unwrap();
                let point = canvas.min + Vec2::new(1.5, 2.5) * (canvas.width() / 8.0);
                click(&mut app, &ctx, point);
                assert_eq!(app.mask, existing_mask, "Wand preserves the active channel");
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(engine.doc.selection, Some([0, 0, 4, 8]));
                assert_eq!(
                    engine.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert_eq!(
                    serde_json::to_value(&engine.doc.layers[0].mask).unwrap(),
                    serde_json::to_value(&before.layers[0].mask).unwrap()
                );
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                assert_eq!(engine.doc.bit_depth, depth);
                assert_eq!(engine.undo.len(), 1);
                engine.undo("human").unwrap();
                assert!(engine.doc.selection.is_none());
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    before.export_png().unwrap()
                );
            }
        }
    }
    #[test]
    fn delete_after_wand_and_layer_row_click_erases_only_selected_native_pixels() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let before = {
                let mut e = app.shared.lock().unwrap();
                e.doc = Document::new_depth(8, 8, depth).unwrap();
                for y in 0..8 {
                    for x in 0..8 {
                        e.doc.layers[0].pixels.set16(
                            x,
                            y,
                            if x < 4 {
                                [12347, 33559, 51237, 65535]
                            } else {
                                [51239, 12349, 33561, 65535]
                            },
                        );
                    }
                }
                e.doc.clone()
            };
            app.select_content(&before.layers[0].id);
            app.selection_tolerance = 0.;
            app.selection_merged = false;
            frame(&mut app, &ctx, vec![], Default::default());
            modified_key(&mut app, &ctx, egui::Key::K, Default::default());
            let canvas = app.view_rect.unwrap();
            click(
                &mut app,
                &ctx,
                canvas.min + Vec2::new(1.5, 2.5) * (canvas.width() / 8.),
            );
            let row = app.layer_rects[&app.selected];
            click(&mut app, &ctx, row.right_center() - Vec2::new(30., 0.));
            assert!(app.layer_clipboard);
            let history = app.shared.lock().unwrap().undo.len();
            modified_key(&mut app, &ctx, egui::Key::Delete, Default::default());
            let mut e = app.shared.lock().unwrap();
            assert_eq!(e.doc.selection, Some([0, 0, 4, 8]));
            assert_eq!(e.doc.layers.len(), 1);
            assert_eq!(e.undo.len(), history + 1);
            for y in 0..8 {
                for x in 0..8 {
                    assert_eq!(
                        e.doc.layers[0].pixels.get16(x, y),
                        if x < 4 {
                            [0; 4]
                        } else {
                            before.layers[0].pixels.get16(x, y)
                        }
                    );
                }
            }
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        }
    }
    #[test]
    fn command_shift_i_inverts_feathered_coverage_once_and_preserves_text_entry() {
        for depth in [8, 16] {
            for modifiers in [
                egui::Modifiers {
                    ctrl: true,
                    shift: true,
                    ..Default::default()
                },
                egui::Modifiers {
                    command: true,
                    shift: true,
                    ..Default::default()
                },
            ] {
                let (mut app, ctx) = small_fixture();
                let before = {
                    let mut e = app.shared.lock().unwrap();
                    e.doc = Document::new_depth(32, 32, depth).unwrap();
                    e.doc.layers[0]
                        .pixels
                        .set16(4, 4, [12347, 33559, 51237, 65535]);
                    let layer = e.doc.layers[0].id.clone();
                    e.edit(
                        "human",
                        &[json!({"op":"mask.add","layer":layer})],
                        None,
                        None,
                        "Mask",
                    )
                    .unwrap();
                    e.edit("human",&[json!({"op":"selection","kind":"ellipse","rect":[4,5,20,22],"feather":2})],None,None,"Select").unwrap();
                    e.undo.clear();
                    e.doc.clone()
                };
                app.select_content(&before.layers[0].id);
                app.mask = true;
                frame(&mut app, &ctx, vec![], Default::default());
                let original = crate::selection::current(&before).unwrap();
                modified_key(&mut app, &ctx, egui::Key::I, modifiers);
                {
                    let mut e = app.shared.lock().unwrap();
                    let inverse = crate::selection::current(&e.doc).unwrap();
                    for y in 0..32 {
                        for x in 0..32 {
                            assert!((original.value(x, y) + inverse.value(x, y) - 1.).abs() < 1e-6);
                        }
                    }
                    assert_eq!(e.undo.len(), 1);
                    assert_eq!(e.doc.bit_depth, depth);
                    assert_eq!(
                        e.doc.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                    assert!(app.tool == Tool::Brush);
                    assert!(app.mask);
                    e.undo("human").unwrap();
                    assert_eq!(
                        crate::selection::current(&e.doc).unwrap().mask.rgba(),
                        original.mask.rgba()
                    );
                    e.redo("human").unwrap();
                }
                let current = app.shared.lock().unwrap().doc.clone();
                app.begin_rename(&current, &app.selected.clone());
                frame(&mut app, &ctx, vec![], Default::default());
                let history = app.shared.lock().unwrap().undo.len();
                modified_key(&mut app, &ctx, egui::Key::I, modifiers);
                let e = app.shared.lock().unwrap();
                assert_eq!(e.undo.len(), history);
                assert_eq!(
                    crate::selection::current(&e.doc).unwrap().mask.rgba(),
                    crate::selection::current(&current).unwrap().mask.rgba()
                );
            }
        }
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
    fn wand_selection_overrides_layer_row_copy_intent_at_both_depths() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            {
                let mut e = app.shared.lock().unwrap();
                e.doc = Document::new_depth(32, 32, depth).unwrap();
                for y in 0..32 {
                    for x in 0..32 {
                        e.doc.layers[0].pixels.set16(
                            x,
                            y,
                            if (4..12).contains(&x) && (5..13).contains(&y) {
                                [12347, 23459, 34571, 65535]
                            } else {
                                [51239, 12349, 33561, 65535]
                            },
                        );
                    }
                }
                app.selected = e.doc.layers[0].id.clone();
            }
            app.select_content(&app.selected.clone());
            app.selection_tolerance = 0.;
            app.selection_merged = false;
            frame(&mut app, &ctx, vec![], Default::default());
            modified_key(&mut app, &ctx, egui::Key::K, Default::default());
            let canvas = app.view_rect.unwrap();
            click(
                &mut app,
                &ctx,
                canvas.min + Vec2::new(6.5, 7.5) * (canvas.width() / 32.),
            );
            let row = app.layer_rects[&app.selected];
            click(&mut app, &ctx, row.right_center() - Vec2::new(30., 0.));
            let doc = app.shared.lock().unwrap().doc.clone();
            assert_eq!(doc.selection, Some([4, 5, 12, 13]));
            assert!(app.layer_clipboard);
            let Some(crate::clipboard::Request::Copy {
                doc: snapshot,
                target,
                mask,
                merged,
            }) = app.copy_request(&doc, false, false)
            else {
                panic!("A layer-row click must not copy the whole layer while pixels are selected");
            };
            let image = crate::clipboard::Image::copy(&snapshot, &target, mask, merged).unwrap();
            assert_eq!(
                (image.width, image.height, image.origin),
                (8, 8, Some([4, 5]))
            );
            assert_eq!(image.samples16.is_some(), depth == 16);
            assert!(
                matches!(app.copy_request(&doc, true, false),
                Some(crate::clipboard::Request::Cut { target, .. }) if target == app.selected),
                "Selected cut must erase pixels rather than delete the layer"
            );
            let baseline = doc.export_png().unwrap();
            let mut engine = app.shared.lock().unwrap();
            let history = engine.undo.len();
            engine
                .edit(
                    "human",
                    &[image.command(&target).unwrap()],
                    Some(doc.revision),
                    None,
                    "Paste selected pixels",
                )
                .unwrap();
            let pasted = engine
                .doc
                .layers
                .iter()
                .find(|l| l.name == "Pasted image")
                .unwrap();
            assert_eq!(
                (
                    pasted.x,
                    pasted.y,
                    pasted.pixels.width,
                    pasted.pixels.height
                ),
                (4, 5, 8, 8)
            );
            assert_eq!(pasted.pixels.get16(0, 0), doc.layers[0].pixels.get16(4, 5));
            assert_eq!(engine.undo.len(), history + 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.export_png().unwrap(), baseline);
            assert_eq!(engine.doc.selection, doc.selection);
        }
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
    fn connected_add_mask_batches_only_missing_masks_and_undo_restores_color() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture();
            let (before, existing) = {
                let mut engine = app.shared.lock().unwrap();
                engine.doc = Document::new_depth(8, 8, depth).unwrap();
                engine
                    .edit(
                        "human",
                        &[
                            json!({"op":"layer.add","name":"Second"}),
                            json!({"op":"layer.add","name":"Third"}),
                        ],
                        None,
                        None,
                        "Fixture",
                    )
                    .unwrap();
                for (index, layer) in engine.doc.layers.iter_mut().enumerate() {
                    layer
                        .pixels
                        .set16(index as i32 + 1, 2, [12347, 33559, 51237, 45679]);
                }
                let existing = engine.doc.layers[1].id.clone();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"mask.add","layer":existing,"value":129})],
                        None,
                        None,
                        "Existing mask",
                    )
                    .unwrap();
                engine.undo.clear();
                (engine.doc.clone(), existing)
            };
            app.select_content(&before.layers[0].id);
            app.selection_layers = before.layers.iter().map(|layer| layer.id.clone()).collect();
            let draw = |app: &mut PeerBrush| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            draw(&mut app);
            let output = draw(&mut app);
            let labels = |output: &egui::FullOutput| {
                output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                        _ => None,
                    })
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                labels(&output)
                    .iter()
                    .filter(|label| label.as_str() == "Add mask")
                    .count(),
                1
            );
            assert!(!labels(&output).iter().any(|label| label == "Add Mask"));
            let color = app.color_tab_rect.unwrap();
            let add = app.mask_tab_rect.unwrap();
            assert!((add.center().y - color.center().y).abs() < 1.0);
            assert!((add.left() - color.right()).abs() < 1.0);
            click(&mut app, &ctx, add.center());
            assert!(app.mask);
            let output = draw(&mut app);
            assert!(labels(&output).iter().any(|label| label == "Mask"));
            assert!(!labels(&output)
                .iter()
                .any(|label| label == "Add mask" || label == "Add Mask"));
            {
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(engine.undo.len(), 1);
                assert!(engine.doc.layers.iter().all(|layer| layer.mask.is_some()));
                assert!(engine
                    .doc
                    .layers
                    .iter()
                    .flat_map(|layer| &layer.mask.as_ref().unwrap().steps)
                    .all(|step| step.pixels.depth == depth));
                let kept = engine
                    .doc
                    .layers
                    .iter()
                    .find(|layer| layer.id == existing)
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(&kept.mask).unwrap(),
                    serde_json::to_value(&before.layers[1].mask).unwrap()
                );
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                engine.undo("human").unwrap();
                assert_eq!(
                    engine
                        .doc
                        .layers
                        .iter()
                        .filter(|layer| layer.mask.is_some())
                        .count(),
                    1
                );
                assert_eq!(
                    engine.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
            }
            let output = draw(&mut app);
            assert!(!app.mask);
            assert!(labels(&output).iter().any(|label| label == "Add mask"));
        }
    }
    #[test]
    fn connected_add_mask_keeps_color_and_sources_on_lock_reservation_or_stale_failure() {
        for failure in ["locked", "reserved", "stale"] {
            let (mut app, ctx) = small_fixture();
            let id = app.selected.clone();
            let before = app.shared.lock().unwrap().doc.clone();
            {
                let mut engine = app.shared.lock().unwrap();
                match failure {
                    "locked" => engine.doc.layers[0].locked = true,
                    "reserved" => {
                        engine
                            .reserve("agent", "Mask work", vec![crate::engine::Scope::layer(&id)])
                            .unwrap();
                    }
                    "stale" => {
                        engine
                            .edit(
                                "human",
                                &[json!({"op":"layer.update","layer":id,"name":"New human name"})],
                                None,
                                None,
                                "Newer work",
                            )
                            .unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            if failure == "stale" {
                app.add_selected_masks(&before);
            } else {
                frame(&mut app, &ctx, vec![], Default::default());
                let add = app.mask_tab_rect.unwrap().center();
                click(&mut app, &ctx, add);
            }
            assert!(!app.mask);
            let engine = app.shared.lock().unwrap();
            assert!(engine.doc.layers[0].mask.is_none());
            assert_eq!(
                engine.doc.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(engine.undo.len(), usize::from(failure == "stale"));
            if failure == "stale" {
                assert_eq!(engine.doc.layers[0].name, "New human name");
            }
            assert!(!app.message.is_empty());
        }
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
    fn native_control_clicks_and_folder_button_group_native16_sources_once() {
        let (mut app, ctx) = small_fixture();
        let ids = {
            let mut e = app.shared.lock().unwrap();
            e.doc.bit_depth = 16;
            e.doc.ensure_depth();
            e.edit(
                "human",
                &[json!({"op":"layer.add","name":"Second"})],
                None,
                None,
                "Setup",
            )
            .unwrap();
            for (index, layer) in e.doc.layers.iter_mut().enumerate() {
                layer
                    .pixels
                    .set16(index as i32 + 3, 5, [12347, 23459, 34571, 65535]);
            }
            e.undo.clear();
            e.doc
                .layers
                .iter()
                .map(|l| l.id.clone())
                .collect::<Vec<_>>()
        };
        app.select_content(&ids[0]);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let baseline = app.shared.lock().unwrap().doc.export_png().unwrap();
        let position = app.layer_rects[&ids[1]].right_center() - Vec2::new(40., 0.);
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        // A released Ctrl can already be absent from the frame's final modifier
        // state; the pointer event must retain the click's original chord.
        for count in [2, 1, 2] {
            frame(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(position),
                    button(position, egui::PointerButton::Primary, true, ctrl),
                ],
                Default::default(),
            );
            frame(
                &mut app,
                &ctx,
                vec![button(position, egui::PointerButton::Primary, false, ctrl)],
                Default::default(),
            );
            assert_eq!(app.selection_layers.len(), count);
            assert!(app.rename_edit.is_none());
        }
        let folder = app.new_folder_rect.unwrap().center();
        click(&mut app, &ctx, folder);
        let e = app.shared.lock().unwrap();
        let group = e.doc.layers.iter().find(|l| l.id == app.selected).unwrap();
        assert_eq!(group.kind, "group");
        assert_eq!(group.pixels.depth, 16);
        assert_eq!(e.undo.len(), 1);
        assert_eq!(
            app.selection_layers,
            [group.id.clone()].into_iter().collect()
        );
        assert_eq!(e.doc.export_png().unwrap(), baseline);
        assert!(ids.iter().all(|id| e
            .doc
            .layers
            .iter()
            .any(|l| &l.id == id && l.parent.as_deref() == Some(group.id.as_str()))));
    }
    #[test]
    fn delete_key_removes_selected_roots_once_and_preserves_native_undo() {
        let (mut app, ctx) = small_fixture();
        let (roots, child, baseline) = {
            let mut e = app.shared.lock().unwrap();
            e.doc.bit_depth = 16;
            e.doc.ensure_depth();
            let child = e.doc.layers[0].id.clone();
            e.doc.layers[0]
                .pixels
                .set16(3, 5, [12347, 23459, 34571, 65535]);
            e.edit(
                "human",
                &[
                    json!({"op":"group.create_selected","layer":child,"name":"Folder"}),
                    json!({"op":"layer.add","name":"Outside"}),
                    json!({"op":"layer.add","name":"Keep"}),
                ],
                None,
                None,
                "Setup",
            )
            .unwrap();
            let roots = e
                .doc
                .layers
                .iter()
                .filter(|l| ["Folder", "Outside"].contains(&l.name.as_str()))
                .map(|l| l.id.clone())
                .collect::<Vec<_>>();
            e.undo.clear();
            (roots, child, e.doc.clone())
        };
        app.select_content(&roots[0]);
        app.selection_layers.extend(roots.iter().cloned());
        app.selection_layers.insert(child);
        frame(&mut app, &ctx, vec![], Default::default());
        key(&mut app, &ctx, egui::Key::Delete);
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.doc.layers.len(), 1);
        assert_eq!(e.doc.layers[0].name, "Keep");
        assert_eq!(e.undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(
            serde_json::to_value(&e.doc.layers).unwrap(),
            serde_json::to_value(&baseline.layers).unwrap()
        );
        assert_eq!(e.doc.export_png().unwrap(), baseline.export_png().unwrap());
    }
    #[test]
    fn delete_key_preserves_typing_and_unfinished_selection_anchors() {
        for typing in [true, false] {
            let (mut app, ctx) = small_fixture();
            frame(&mut app, &ctx, vec![], Default::default());
            let id = app.selected.clone();
            if typing {
                let doc = app.shared.lock().unwrap().doc.clone();
                app.begin_rename(&doc, &id);
                frame(&mut app, &ctx, vec![], Default::default());
            } else {
                app.tool = Tool::Selection;
                app.selection_kind = "polygon".into();
                app.selection_path = vec![[2., 2.], [8., 4.]];
            }
            key(&mut app, &ctx, egui::Key::Delete);
            let e = app.shared.lock().unwrap();
            assert_eq!(e.doc.layers.len(), 1);
            assert_eq!(e.doc.layers[0].id, id);
            assert!(e.undo.is_empty());
            if !typing {
                assert_eq!(app.selection_path.len(), 1);
            }
        }
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
    fn folder_gizmo_wer_previews_and_commits_only_the_wand_selection() {
        for depth in [8, 16] {
            for tool in [Tool::Move, Tool::Rotate, Tool::Scale] {
                let (mut app, ctx) = small_fixture();
                let (root, child) = {
                    let mut e = app.shared.lock().unwrap();
                    e.doc = Document::new_depth(32, 32, depth).unwrap();
                    let mut folder = crate::engine::Layer::new("Folder", "group", 32, 32);
                    folder.pixels = crate::raster::Raster::new_depth(32, 32, depth);
                    let root = folder.id.clone();
                    let child = e.doc.layers[0].id.clone();
                    e.doc.layers[0].parent = Some(root.clone());
                    for y in 5..11 {
                        for x in 4..12 {
                            e.doc.layers[0]
                                .pixels
                                .set16(x, y, [12347, 23459, 34571, 65535]);
                        }
                    }
                    e.doc.layers[0]
                        .pixels
                        .set16(25, 25, [51239, 12349, 33561, 65535]);
                    e.doc.layers.push(folder);
                    e.edit("human", &[json!({"op":"selection","kind":"wand","layer":child,"point":[6,7],"sample_merged":false,"tolerance":0})], None, None, "Select").unwrap();
                    e.undo.clear();
                    (root, child)
                };
                app.select_content(&root);
                app.select_tool(tool);
                frame(&mut app, &ctx, vec![], Default::default());
                let rect = app.view_rect.unwrap();
                let center = rect.min + Vec2::new(8., 8.) * (rect.width() / 32.);
                let (start, end) = if tool == Tool::Rotate {
                    (center + Vec2::new(58., 0.), center + Vec2::new(0., 58.))
                } else {
                    (center, center + Vec2::new(35., 0.))
                };
                let before = app.shared.lock().unwrap().doc.clone();
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
                assert_eq!(app.transient.len(), 1);
                assert_eq!(app.transient[0]["selection_only"], true);
                let preview = Engine::preview_edits(before.clone(), &app.transient).unwrap();
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
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.undo.len(), 1, "{}: {}", tool.label(), app.message);
                assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
                let layer = e.doc.layers.iter().find(|l| l.id == child).unwrap();
                assert_eq!(
                    layer.pixels.get16(25 - layer.x, 25 - layer.y),
                    before.layers[0].pixels.get16(25, 25)
                );
                assert_ne!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                assert_eq!(e.doc.selection, before.selection);
            }
        }
    }
    #[test]
    fn folder_gizmo_transforms_selected_root_once_and_previews_actual_children() {
        let (mut app, ctx) = small_fixture();
        let (root, child) = {
            let mut e = app.shared.lock().unwrap();
            let mut folder = crate::engine::Layer::new("Folder", "group", 32, 32);
            folder.pixels = crate::raster::Raster::new(32, 32);
            let root = folder.id.clone();
            let child = e.doc.layers[0].id.clone();
            e.doc.layers[0].parent = Some(root.clone());
            for y in 5..13 {
                for x in 4..12 {
                    e.doc.layers[0].pixels.set(x, y, [200, 87, 31, 255]);
                }
            }
            e.doc.layers[0].pixels.set(4, 5, [31, 87, 200, 255]);
            e.doc.layers.push(folder);
            e.undo.clear();
            (root, child)
        };
        app.selected = root.clone();
        app.selection_layers = [root.clone(), child].into_iter().collect();
        app.tool = Tool::Rotate;
        frame(&mut app, &ctx, vec![], Default::default());
        let rect = app.view_rect.unwrap();
        let scale = rect.width() / 32.;
        let center = rect.min + Vec2::new(8. * scale, 9. * scale);
        let start = center + Vec2::new(58., 0.);
        let end = center + Vec2::new(0., 58.);
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
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(transforms.len(), 1);
        assert_eq!(transforms[0]["layer"], root);
        assert_eq!(transforms[0]["pivot"], json!([8., 9.]));
        let doc = app.shared.lock().unwrap().doc.clone();
        let preview = Engine::preview_edits(doc.clone(), &transforms).unwrap();
        assert!(app.shared.lock().unwrap().undo.is_empty());
        // An asymmetric test mark distinguishes the rendered rotation from a bounding-box update.
        assert_eq!(
            preview
                .layers
                .iter()
                .find(|l| l.kind == "paint")
                .unwrap()
                .pixels
                .get(0, 0),
            [200, 87, 31, 255]
        );
        assert_eq!(
            preview
                .layers
                .iter()
                .find(|l| l.kind == "paint")
                .unwrap()
                .pixels
                .get(7, 0),
            [31, 87, 200, 255]
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
        let e = app.shared.lock().unwrap();
        assert_eq!(e.undo.len(), 1);
        assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
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
    fn command_d_duplicates_active_pixels_or_layer_and_preserves_typing() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture();
            {
                let mut engine = app.shared.lock().unwrap();
                engine.doc = Document::new_depth(16, 16, depth).unwrap();
                for y in 0..16 {
                    for x in 0..16 {
                        engine.doc.layers[0]
                            .pixels
                            .set16(x, y, [12347, 33559, 51237, 45679]);
                    }
                }
            }
            let id = app.shared.lock().unwrap().doc.layers[0].id.clone();
            app.select_content(&id);
            app.shared
                .lock()
                .unwrap()
                .edit(
                    "human",
                    &[json!({"op":"selection","kind":"ellipse","rect":[2,2,12,12],"feather":1})],
                    None,
                    None,
                    "Select",
                )
                .unwrap();
            app.shared.lock().unwrap().undo.clear();
            frame(&mut app, &ctx, vec![], Default::default());
            app.layer_clipboard = true;
            let mods = egui::Modifiers {
                command: true,
                ctrl: true,
                ..Default::default()
            };
            modified_key(&mut app, &ctx, egui::Key::D, mods);
            {
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(engine.doc.layers.len(), 2);
                assert_eq!(engine.doc.selection, Some([1, 1, 13, 13]));
                assert_eq!(app.selected, engine.doc.layers[0].id);
                assert_eq!((engine.doc.layers[0].x, engine.doc.layers[0].y), (1, 1));
                assert_eq!(engine.doc.layers[0].pixels.depth, depth);
                assert_eq!(engine.undo.len(), 1);
                engine.undo("human").unwrap();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"selection.clear"})],
                        None,
                        None,
                        "Deselect",
                    )
                    .unwrap();
                engine.undo.clear();
            }
            app.select_content(&id);
            modified_key(&mut app, &ctx, egui::Key::D, mods);
            {
                let engine = app.shared.lock().unwrap();
                assert_eq!(engine.doc.layers.len(), 2);
                assert_eq!(
                    engine.doc.layers[0].pixels.rgba16(),
                    engine.doc.layers[1].pixels.rgba16()
                );
                assert_eq!(engine.undo.len(), 1);
            }
            // Ctrl+D remains ordinary text input when a numeric/text editor owns focus.
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut String::new())
                            .id(egui::Id::new("duplicate typing")),
                    )
                    .request_focus();
                });
            });
            modified_key(&mut app, &ctx, egui::Key::D, mods);
            assert_eq!(app.shared.lock().unwrap().doc.layers.len(), 2);
            assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        }
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
                    coarse: false,
                    error: None,
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
        app.shared.lock().unwrap().doc.layers[0]
            .pixels
            .set(4, 4, [200, 80, 30, 255]);
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
        assert_eq!(app.shared.lock().unwrap().doc.selection, Some([2, 2, 8, 8]));
        assert_eq!(app.shared.lock().unwrap().doc.layers.len(), 2);
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        assert!(app.tool == Tool::Selection);
    }
    #[test]
    fn refinement_previews_real_cutout_without_history_and_cancels_on_source_changes() {
        let (mut app, ctx) = small_fixture();
        {
            let mut e = app.shared.lock().unwrap();
            for y in 0..16 {
                for x in 0..16 {
                    e.doc.layers[0].pixels.set(x, y, [233, 84, 32, 255]);
                }
            }
            e.edit(
                "human",
                &[json!({"op":"selection","rect":[4,4,12,12]})],
                None,
                None,
                "Select",
            )
            .unwrap();
        }
        let doc = app.shared.lock().unwrap().doc.clone();
        let count = app.shared.lock().unwrap().undo.len();
        app.open_refinement(&doc, None);
        app.refinement.as_mut().unwrap().values[1] = 2.;
        frame(&mut app, &ctx, vec![], Default::default());
        assert_eq!(app.transient.len(), 1);
        let preview = crate::engine::Engine::preview_edits(doc.clone(), &app.transient).unwrap();
        let (w, h, mut pixels, _) = preview.preview(None, 64, None, false).unwrap();
        let original = pixels.clone();
        crate::selection::display::apply(&preview, w, h, &mut pixels, "cutout");
        assert!(pixels != original, "Cutout must alter rendered alpha");
        assert_eq!(app.shared.lock().unwrap().undo.len(), count);
        assert_eq!(app.shared.lock().unwrap().doc.revision, doc.revision);
        app.pending = false;
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(
            app.last_preview
                .as_ref()
                .unwrap()
                .2
                .contains("\"feather\":2.0"),
            "Canvas preview lost the live refinement command"
        );
        app.cancel_refinement();
        assert!(app.transient.is_empty());
        app.open_refinement(&doc, None);
        {
            let mut e = app.shared.lock().unwrap();
            e.edit(
                "human",
                &[json!({"op":"layer.update","layer":app.selected,"name":"Human rename"})],
                None,
                None,
                "Rename",
            )
            .unwrap();
        }
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app.refinement.is_none());
        assert!(app.transient.is_empty());
        assert_eq!(app.shared.lock().unwrap().undo.len(), count + 1);
    }
}
