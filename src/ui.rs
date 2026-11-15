use crate::icons::{self, Icon};
mod about;
mod activation;
mod animation;
mod associations;
pub(crate) use associations::welcome::validate_file as validate_association_preferences;
mod brush;
#[cfg(test)]
mod brush_toolbar_tests;
mod calibration;
mod color;
mod compatibility;
mod contour_points;
mod dialogs;
mod duplicate_repeat;
mod edit_job;
#[cfg(test)]
mod action_tests;
mod effect_color;
mod effects;
mod exports;
mod file_drop;
mod file_ownership;
mod filename;
mod filters;
mod free_transform;
mod geometry;
mod gradient_stops;
mod history;
mod image_open;
mod layers;
mod lifecycle;
mod liquify;
mod native_preview;
mod native_dispatch;
mod native_canvas;
mod native_texture;
mod new_canvas;
mod pattern_source;
mod pen;
#[cfg(test)]
mod pen_tests;
mod preview_quality;
mod preview_worker;
mod primitive_drag;
pub(crate) use preview_quality::validate_file as validate_preview_preferences;
mod protection;
mod recovery;
mod refinement;
mod retouch;
mod saved_versions;
mod saving;
mod selection;
mod shortcuts;
pub(crate) use shortcuts::validate_file as validate_shortcut_preferences;
#[cfg(test)]
mod input_performance_diagnostic;
#[cfg(test)]
mod parameter_preview_tests;
mod source;
mod style_settings;
mod tabs;
#[cfg(test)]
mod text_tests;
mod tool_settings;
#[cfg(test)]
mod tool_settings_tests;
mod transform_snapping;
mod transform_bounds;
mod color_sample;
mod mask_thumbnail;
mod sample_input;
mod sampled_input;
mod magnetic_input;
mod updates;
mod view;
#[cfg(test)]
mod workspace_tests;
#[cfg(test)]
mod frame_tests;
use crate::{controls, thumbnails};
use crate::{
    engine::{
        runtime_value::RuntimeValue,
        view::{DocumentSnapshot, DocumentView, LayerView},
        Document,
    },
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
const INVENTORY_RETRY_DELAY: Duration = Duration::from_millis(100);
const AI_BLUE: Color32 = Color32::from_rgb(90, 170, 255);
const MUTED: Color32 = Color32::from_rgb(188, 183, 189);
fn primary_button_samples(input: &egui::InputState) -> impl DoubleEndedIterator<Item = (bool, Pos2, egui::Modifiers)> + '_ {
    input.events.iter().filter_map(primary_button_sample)
}

fn primary_button_sample(event: &egui::Event) -> Option<(bool, Pos2, egui::Modifiers)> {
    match event {
        egui::Event::PointerButton {
            pos, button: egui::PointerButton::Primary, pressed, modifiers,
        } => Some((*pressed, *pos, *modifiers)),
        _ => None,
    }
}

fn primary_release_samples(input: &egui::InputState) -> impl DoubleEndedIterator<Item = (Pos2, egui::Modifiers)> + '_ {
    primary_button_samples(input).filter_map(|(pressed, pos, modifiers)| (!pressed).then_some((pos, modifiers)))
}

fn canvas_contact(
    input: &egui::InputState,
    held: bool,
) -> (Option<(Pos2, egui::Modifiers)>, Option<(Pos2, egui::Modifiers)>) {
    let (start, release) = canvas_contact_range(input, held);
    let press = canvas_contact_press(input, start);
    let release = release.and_then(|index| primary_button_sample(&input.events[index]))
        .map(|(_, pos, modifiers)| (pos, modifiers));
    (press, release)
}

fn canvas_contact_press(input: &egui::InputState, start: usize) -> Option<(Pos2, egui::Modifiers)> {
    let (pressed, pos, modifiers) = primary_button_sample(input.events.get(start.checked_sub(1)?)?)?;
    pressed.then_some((pos, modifiers))
}

fn canvas_contact_range(input: &egui::InputState, held: bool) -> (usize, Option<usize>) {
    // A held drag ends at the first release. A fresh drag owns only the
    // events after its first press through the following release.
    let start = if held {
        0
    } else {
        let Some(index) = input.events.iter().position(|event| {
            primary_button_sample(event).is_some_and(|(pressed, _, _)| pressed)
        }) else { return (input.events.len(), None) };
        index + 1
    };
    let release = input.events[start..].iter().position(|event| {
        primary_button_sample(event).is_some_and(|(pressed, _, _)| !pressed)
    }).map(|offset| start + offset);
    (start, release)
}

fn canvas_button_samples(input: &egui::InputState, dragging: bool) -> (Option<Pos2>, Option<(Pos2, egui::Modifiers)>) {
    let (press, release) = canvas_contact(input, dragging);
    (press.map(|(pos, _)| pos), release)
}

fn canvas_press_hits(ui: &egui::Ui, response: &egui::Response, pos: Pos2) -> bool {
    response.interact_rect.contains(pos) && ui.ctx().layer_id_at(pos) == Some(ui.layer_id())
}

fn canvas_click_sample(ui: &egui::Ui, response: &egui::Response) -> Option<(Pos2, egui::Modifiers)> {
    if !response.clicked_by(egui::PointerButton::Primary) {
        return None;
    }
    ui.input(|input| primary_release_samples(input).next_back())
        .or_else(|| response.interact_pointer_pos().map(|pos| (pos, ui.input(|input| input.modifiers))))
        .filter(|&(pos, _)| canvas_press_hits(ui, response, pos))
}

fn canvas_sample_input(ui: &egui::Ui) -> bool {
    !PeerBrush::pointer_contact_cancelled(ui.ctx()) && ui.input(|input| {
        input.focused && !input.key_down(egui::Key::Space) && !input.key_pressed(egui::Key::Escape)
            && !input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))
    })
}

fn command_modifiers(input: &egui::InputState, key: egui::Key) -> Option<egui::Modifiers> {
    input.events.iter().find_map(|event| match event {
        egui::Event::Key {
            key: k,
            pressed: true,
            modifiers,
            ..
        } if *k == key && (modifiers.command || modifiers.ctrl) => Some(*modifiers),
        _ => None,
    })
}
fn command_key(input: &egui::InputState, key: egui::Key) -> bool {
    command_modifiers(input, key).is_some()
}
enum ClipboardShortcut {
    Copy(bool),
    Cut,
    Paste,
}

fn clipboard_shortcut(event: &egui::Event) -> Option<ClipboardShortcut> {
    match event {
        egui::Event::Copy => Some(ClipboardShortcut::Copy(false)),
        egui::Event::Cut => Some(ClipboardShortcut::Cut),
        egui::Event::Paste(_) => Some(ClipboardShortcut::Paste),
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } if (modifiers.command || modifiers.ctrl) && !modifiers.alt => match key {
            egui::Key::C => Some(ClipboardShortcut::Copy(modifiers.shift)),
            egui::Key::X => Some(ClipboardShortcut::Cut),
            egui::Key::V => Some(ClipboardShortcut::Paste),
            _ => None,
        },
        _ => None,
    }
}
fn paint_tool(painter: &egui::Painter, rect: Rect, tool: Tool, color: Color32) {
    use crate::icons::{self, Icon};
    let icon = match tool {
        Tool::None => Icon::Cursor,
        Tool::Pen => Icon::Curves,
        Tool::Text => Icon::Text,
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
    icons::paint_tinted(painter, rect, icon, color);
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
    Pen,
    Text,
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
            Self::Pen => "Pen · P",
            Self::Text => "Text · T",
            Self::None => "Deselect · Q",
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
struct ResidentPreviewStroke {
    id: String,
    fields: Value,
    points: Vec<[f32; 2]>,
    pressures: Option<Vec<f32>>,
}

impl ResidentPreviewStroke {
    fn matches(&self, fields: &Value, points: &[[f32; 2]], pressures: Option<&[f32]>) -> bool {
        self.fields == *fields
            && bytemuck::cast_slice::<_, u8>(&self.points) == bytemuck::cast_slice::<_, u8>(points)
            && self.pressures.as_deref().map(bytemuck::cast_slice::<f32, u8>)
                == pressures.map(bytemuck::cast_slice::<f32, u8>)
    }

    fn command(&self) -> Value {
        brush::with_stroke_samples(self.fields.clone(), &self.points, self.pressures.as_deref())
    }
}

struct Preview {
    job: u64,
    parameter: Option<ParameterRequest>,
    parameter_document: Option<Result<Arc<Document>, String>>,
    #[cfg(test)]
    parameter_replayed: bool,
    coarse: bool,
    error: Option<String>,
    live: String,
    revision: u64,
    doc_id: String,
    w: u32,
    h: u32,
    bytes: Vec<u8>,
    patch: Option<crate::preview::CanvasPatch>,
    selection: Option<[i32; 4]>,
    selection_polygon: Option<Vec<[f32; 2]>>,
    selection_coverage: Option<crate::selection::Coverage>,
    text: Option<([u8; 32], Option<crate::source::text_position::TextCarets>)>,
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
    source: ImportSource,
    index: usize,
    icon_sizes: Vec<[u16; 2]>,
}
enum ImportSource {
    Resident(crate::image_import::Encoded),
    Native {
        format: String,
        choice: Option<(&'static str, usize)>,
        digest: [u8; 32],
    },
}
impl ImportFile {
    fn choice(&self) -> Option<(&'static str, usize)> {
        match &self.source {
            ImportSource::Resident(encoded) => encoded.info.choice.map(|key| (key, encoded.info.count)),
            ImportSource::Native { choice, .. } => *choice,
        }
    }

    fn format(&self) -> &str {
        match &self.source {
            ImportSource::Resident(encoded) => &encoded.info.format,
            ImportSource::Native { format, .. } => format,
        }
    }
}
struct PendingImport {
    project: String,
    document: String,
    revision: u64,
    native: Option<crate::engine::native::NativeDocument>,
    files: Vec<ImportFile>,
}
struct ImportReply {
    project: String,
    document: String,
    revision: u64,
    native: Option<crate::engine::native::NativeDocument>,
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
struct TransformGestureSource {
    document: String,
    revision: u64,
    native: Option<crate::engine::native::DocumentKey>,
    roots: Vec<String>,
    tool: Tool,
    mask: bool,
    step: Option<String>,
}
impl TransformGestureSource {
    fn observes(&self, doc: DocumentView<'_>) -> bool {
        self.document == doc.id()
            && self.revision == doc.revision()
            && self.native == doc.native().map(|native| native.key())
    }
}
struct DocumentFrame {
    document: DocumentSnapshot,
    proposal: Option<history::ProposalFrame>,
    loading: Option<crate::loading::Control>,
    ai_change: Option<RuntimeValue<crate::engine::AiChange>>,
    saved_revision: u64,
    has_save_path: bool,
    reservation: Option<(String, bool)>,
    ai_scopes: Vec<crate::engine::Scope>,
    human_regions: Vec<[i32; 4]>,
}
impl DocumentFrame {
    fn capture(engine: &crate::engine::Engine) -> Self {
        let mut ai_scopes = Vec::new();
        let mut human_regions = Vec::new();
        let now = crate::engine::now();
        for lease in engine.leases.iter().filter(|lease| lease.expires > now) {
            if lease.owner == "human" {
                human_regions.extend(lease.scopes.iter().filter_map(|scope| scope.rect));
            } else {
                ai_scopes.extend(lease.scopes.iter().cloned());
            }
        }
        Self {
            document: engine.doc_snapshot(),
            proposal: None,
            loading: engine.loading.clone(),
            ai_change: engine.ai_change.clone(),
            saved_revision: engine.saved_revision,
            has_save_path: engine.path.is_some(),
            reservation: engine.leases.first().map(|lease| {
                (lease.description.clone(), lease.owner == "human")
            }),
            ai_scopes,
            human_regions,
        }
    }
}
fn show_busy_project(ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.label("Working on this project… Switch tabs to continue elsewhere.");
    });
    ctx.request_repaint_after(Duration::from_millis(16));
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
    Preview {
        coarse: bool,
        result: Result<TabPreviewOutput, String>,
    },
    Commit(Result<Value, String>),
}
struct DeferredSelectionShortcuts {
    request: Arc<edit_job::Request>,
    committed: Option<(u64, crate::engine::native::DocumentKey)>,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    tool: Tool,
    layer: String,
    mask: bool,
    step: Option<String>,
    _charge: Arc<crate::psd::memory36::MetadataCharge>,
}
enum TabPreviewOutput {
    Resident(crate::workspace::TransferImage),
    Native(crate::workspace::TransferSummary, native_texture::Prepared),
}
enum TabPreviewTexture {
    Resident(native_texture::RgbaTexture),
    Native(native_texture::Canvas),
}
impl TabPreviewTexture {
    fn size(&self) -> [u32; 2] {
        match self {
            Self::Resident(texture) => texture.size(),
            Self::Native(canvas) => [canvas.key().width, canvas.key().height],
        }
    }
    fn paint(&self, painter: &egui::Painter, project: impl Fn([f32; 2]) -> Pos2) {
        match self {
            Self::Resident(texture) => texture.paint(painter, project),
            Self::Native(canvas) => canvas.paint_normalized(painter, project, Color32::WHITE),
        }
    }
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
    effect_expanded: HashSet<(String, bool, String)>,
    isolate: bool,
    collapsed: HashSet<String>,
    folder_view_document: String,
    zoom: f32,
    pan: Vec2,
    view: view::View,
    message: RuntimeValue<String>,
}
struct LayerDrag {
    gesture: String,
    document: String,
    id: String,
    revision: u64,
    native: Option<crate::engine::native::DocumentKey>,
    offset: f32,
    target: usize,
    ids: Vec<String>,
    commands: Vec<Value>,
    drop_plan: Option<Arc<layers::LayerDropPlan>>,
}
struct EyeSweep {
    gesture: String,
    document: String,
    previous: Pos2,
    revision: u64,
    native: Option<crate::engine::native::DocumentKey>,
    visible: bool,
    ids: HashSet<String>,
}

#[derive(Debug)]
struct BlendHover {
    layers: Vec<String>,
    setting: BlendSetting,
}

#[derive(Debug, Clone, Copy)]
enum BlendSetting {
    Mode(&'static str),
    Channels([bool; 3]),
}

impl BlendHover {
    fn commands(&self) -> impl Iterator<Item = Value> + '_ {
        self.layers.iter().map(|layer| match self.setting {
            BlendSetting::Mode(mode) => json!({"op":"layer.update","layer":layer,"blend":mode}),
            BlendSetting::Channels(channels) => json!({"op":"layer.update","layer":layer,"blend_channels":channels}),
        })
    }
}

struct AltSample {
    engine: std::sync::Weak<std::sync::Mutex<crate::engine::Engine>>,
    project: String,
    document: String,
    revision: u64,
    point: [i32; 2],
    selected: String,
    mask: bool,
    step: Option<String>,
    value: crate::render::sample::Color,
    native: Option<crate::engine::native::DocumentKey>,
    _charge: Option<Arc<crate::psd::memory36::MetadataCharge>>,
}
#[derive(Clone)]
struct ParameterRequest {
    engine: std::sync::Weak<std::sync::Mutex<crate::engine::Engine>>,
    document: String,
    revision: u64,
    gesture: String,
    commands: Vec<Value>,
}
impl ParameterRequest {
    fn matches(&self, edit: &ParameterEdit, shared: &Shared, source: &Document) -> bool {
        self.engine.as_ptr() == Arc::as_ptr(shared)
            && edit.engine.as_ptr() == self.engine.as_ptr()
            && self.document == source.id
            && self.revision == source.revision
            && self.document == edit.document
            && self.revision == edit.revision
            && self.gesture == edit.gesture
            && self.commands == edit.commands
    }
}
struct ParameterEdit {
    control: egui::Id,
    gesture: String,
    engine: std::sync::Weak<std::sync::Mutex<crate::engine::Engine>>,
    document: String,
    revision: u64,
    commands: Vec<Value>,
    // Derived from this edit's source and commands; never authorizes a commit.
    preview: Option<Arc<Document>>,
    #[cfg(test)]
    preview_runs: usize,
    label: String,
    typing: Option<egui::Context>,
    dirty: bool,
}
pub struct PeerBrush {
    // Drain previews before any document, workspace or profile owner drops.
    preview_worker: preview_worker::Worker,
    fonts: filename::FontSetup,
    shared: Shared,
    workspace_root: Shared,
    workspace: crate::workspace::Registry,
    project_id: String,
    project_views: HashMap<String, ProjectView>,
    project_rects: HashMap<String, Rect>,
    project_drop_rect: Option<Rect>,
    project_labels: HashMap<String, tabs::Label>,
    tab_active: String,
    tab_drag: Option<tabs::TabDrag>,
    project_picker_rect: Option<Rect>,
    doc_snapshot: DocumentSnapshot,
    canvas_press_document: Option<String>,
    canvas_input_region: Option<(Rect, egui::LayerId)>,
    jobs: HashSet<String>,
    updates: updates::Panel,
    about: about::About,
    lifecycle: Option<lifecycle::Review>,
    recovery: recovery::Browser,
    saved_versions: Option<saved_versions::Browser>,
    open_file_warning: Option<String>,
    lifecycle_frame: bool,
    exit_approved: bool,
    connected: bool,
    tab_transfer: Option<TabTransfer>,
    tab_preview_requested: Option<TabTransfer>,
    tab_preview_pending: Option<crate::engine::native::Cancellation>,
    tab_transfer_committing: bool,
    tab_preview: Option<(crate::workspace::TransferSummary, TabPreviewTexture)>,
    transfer_tx: mpsc::Sender<TransferReply>,
    transfer_rx: mpsc::Receiver<TransferReply>,
    connection: Connection,
    selected: String,
    selection_layers: HashSet<String>,
    duplicate_repeat: Option<duplicate_repeat::Repeat>,
    pending_duplicate_repeat: Option<duplicate_repeat::Repeat>,
    duplicate_repeat_history: Option<duplicate_repeat::RepeatHistory>,
    selection_anchor: String,
    eye_sweep: Option<EyeSweep>,
    mask: bool,
    isolate: bool,
    solo_target: Option<(String, bool)>,
    tool: Tool,
    selection_kind: String,
    selection_mode: String,
    selection_feather: f32,
    selection_tolerance: f32,
    selection_color_similarity: bool,
    selection_contiguous: bool,
    selection_merged: bool,
    selection_live_wand: bool,
    selection_width: f32,
    selection_path: Vec<[f32; 2]>,
    selection_gesture_mode: Option<String>,
    selection_shortcut_keys: HashSet<egui::Key>,
    project_settings: Option<new_canvas::ProjectDimensions>,
    show_global_light: bool,
    geometry: Option<geometry::Editor>,
    free_transform: Option<free_transform::Editor>,
    free_transform_frame: bool,
    free_transform_cancelled_pointer: bool,
    source_editor: Option<source::Editor>,
    native_source_preview: Option<native_dispatch::SharedCommand>,
    native_source_preview_at: usize,
    edit_worker: edit_job::Worker<server::EditReply>,
    edit_control: Option<server::EditControl>,
    edit_context: egui::Context,
    pen: pen::State,
    straight: brush::Straight,
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
    color16: Option<crate::raster::Pixel16>,
    mask_value: u8,
    mask_value16: Option<u16>,
    radius: f32,
    size_drag: Option<(Pos2, f32, f32)>,
    live_gesture: Option<String>,
    primitive_drag: Option<primitive_drag::Drag>,
    fill_press: Option<primitive_drag::FillPress>,
    workspace_stroke: Option<brush::WorkspaceStroke>,
    brush: crate::brush::Settings,
    tool_settings_store: Arc<std::sync::Mutex<crate::tool_settings::Store>>,
    tool_profile: tool_settings::Cache,
    tool_feedback: Option<tool_settings::Feedback>,
    liquify: liquify::Settings,
    liquify_effect: Option<String>,
    stroke_pressures: Vec<f32>,
    stroke_has_pressure: bool,
    current_pressure: Option<f32>,
    show_calibration: bool,
    show_view: bool,
    calibration: calibration::Pad,
    shortcuts: shortcuts::Shortcuts,
    preview_quality: preview_quality::Quality,
    background: Pixel,
    background16: Option<crate::raster::Pixel16>,
    mask_background: u8,
    mask_background16: Option<u16>,
    gradient_direction: crate::engine::primitive::GradientDirection,
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
    texture: Option<native_texture::RgbaTexture>,
    canvas_pixels: Option<(u32, u32, Vec<u8>)>,
    canvas_preview_job: Option<u64>,
    canvas_generation: u64,
    native_receipt: Option<native_dispatch::Receipt>,
    alt_sample: Option<AltSample>,
    #[cfg(test)]
    color_pick_runs: usize,
    canvas_preview: Option<(String, u64, String, bool)>,
    capture_ready_at: f64,
    canvas_selection: Option<CanvasSelection>,
    animation: animation::Animation,
    preview_rx: mpsc::Receiver<Preview>,
    preview_tx: mpsc::Sender<Preview>,
    pending: bool,
    preview_epoch: Arc<std::sync::atomic::AtomicU64>,
    preview_job: u64,
    native_preview: native_preview::Presentation,
    native_dispatch: native_dispatch::Dispatch,
    native_canvas: native_texture::Presenter,
    #[cfg(test)]
    parameter_worker_gate: Option<Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>>,
    #[cfg(test)]
    parameter_worker_runs: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    preview_variant_builds: std::cell::Cell<usize>,
    preview_cache: Arc<std::sync::Mutex<crate::preview::Cache>>,
    resident_preview_stroke: Option<Arc<ResidentPreviewStroke>>,
    last_preview: Option<(String, u64, String, bool)>,
    thumbs: HashMap<String, (u64, TextureHandle)>,
    thumb_worker: Option<thumbnails::Worker>,
    thumb_pending: HashMap<String, u64>,
    thumb_document: String,
    thumb_native: bool,
    parameter_gesture: Option<ParameterEdit>,
    cancelled_parameter: Option<egui::Id>,
    blend_hover: Option<BlendHover>,
    transient: Vec<Value>,
    points: Vec<[f32; 2]>,
    drag_start: Option<[f32; 2]>,
    view_rect: Option<Rect>,
    canvas_viewport: Option<Rect>,
    view: view::View,
    message: RuntimeValue<String>,
    last_status: RuntimeValue<String>,
    import_tx: mpsc::Sender<ImportReply>,
    import_rx: mpsc::Receiver<ImportReply>,
    pending_import: Option<PendingImport>,
    image_open_tx: mpsc::Sender<image_open::Pending>,
    image_open_rx: mpsc::Receiver<image_open::Pending>,
    pending_image_open: Option<image_open::Pending>,
    job_rx: mpsc::Receiver<JobReply>,
    job_tx: mpsc::Sender<JobReply>,
    busy: bool,
    load_control: Option<crate::loading::Control>,
    load_preview: Option<TextureHandle>,
    show_new: bool,
    new_size: new_canvas::Suggestion,
    associations: associations::Choice,
    read_only_info: Option<String>,
    read_only_patterns: Option<String>,
    selection_commit_failed: bool,
    selection_commit_revision: Option<u64>,
    deferred_selection_shortcuts: Option<DeferredSelectionShortcuts>,
    selection_pointer_cancelled: bool,
    selection_gesture_source: Option<selection::GestureSource>,
    save_report: Option<crate::compatibility::PreparedReport>,
    save_loss_review: Option<compatibility::SaveReview>,
    export_editor: Option<exports::Editor>,
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
    folder_view_document: String,
    mask_step: Option<String>,
    effect_selected: Option<(String, bool, String)>,
    effect_expanded: HashSet<(String, bool, String)>,
    activity_text: String,
    activity_changed: std::time::Instant,
    gizmo_handle: u8,
    gizmo_source: Option<TransformGestureSource>,
    drag_revision: Option<u64>,
    gizmo_bounds: Option<(String, u64, [i32; 4])>,
    transform_bounds: transform_bounds::Query,
    color_sampler: color_sample::Sampler,
    mask_thumbnails: mask_thumbnail::Thumbnails,
    layer_thumbnails: mask_thumbnail::Thumbnails,
    alt_sampler: color_sample::Sampler,
    sampled_input: Option<sampled_input::Input>,
    sampled_input_frame: bool,
    magnetic_sampler: color_sample::Sampler<[f32; 2]>,
    magnetic_input: Option<magnetic_input::Input>,
    color_pick: Option<color_sample::Pick>,
    sample_press: Option<color_sample::Press>,
}
const BRAND_MARK: &[u8; 256 * 256 * 4] = include_bytes!(concat!(env!("OUT_DIR"), "/window-icon.rgba"));
const WORDMARK_WIDTH: usize = 576;
const WORDMARK: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/workspace-wordmark.rgba"));

pub fn window_icon() -> egui::IconData {
    egui::IconData { width: 256, height: 256, rgba: BRAND_MARK.to_vec() }
}

impl PeerBrush {
    #[cfg(any(windows, target_os = "macos"))]
    pub fn set_update_profile(&mut self, profile: Option<crate::settings::UpdateProfile>) {
        self.updates.set_update_profile(profile);
    }

    pub fn enable_first_launch(&mut self, preferences: &Path) {
        self.associations = associations::Choice::load(preferences);
    }

    pub fn new_with_preferences(
        cc: &eframe::CreationContext<'_>,
        shared: Shared,
        connection: Connection,
        preferences: &Path,
    ) -> Self {
        #[cfg(target_os = "macos")]
        crate::activation::macos::repaint_with(&cc.egui_ctx);
        if let Some(state) = &cc.wgpu_render_state {
            crate::gpu::install_background(state, &cc.egui_ctx);
        }
        Self::init_with_preferences(&cc.egui_ctx, shared, connection, preferences)
    }

    pub fn new(cc: &eframe::CreationContext<'_>, shared: Shared, connection: Connection) -> Self {
        let preferences = connection.state_dir.clone();
        Self::new_with_preferences(cc, shared, connection, &preferences)
    }

    #[cfg(test)]
    fn init(ctx: &egui::Context, shared: Shared, connection: Connection) -> Self {
        let preferences = connection.state_dir.clone();
        Self::init_with_preferences(ctx, shared, connection, &preferences)
    }

    fn init_with_preferences(ctx: &egui::Context, shared: Shared, connection: Connection, preferences: &Path) -> Self {
        crate::activation::enable(&shared);
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
        let fonts = filename::FontSetup::new(ctx, fonts);
        let logo = ctx.load_texture(
            "PeerBrush mark",
            egui::ColorImage::from_rgba_unmultiplied([256, 256], BRAND_MARK),
            egui::TextureOptions::LINEAR,
        );
        let wordmark = ctx.load_texture(
            "PeerBrush wordmark",
            egui::ColorImage::from_rgba_unmultiplied(
                [WORDMARK_WIDTH, WORDMARK.len() / (WORDMARK_WIDTH * 4)],
                WORDMARK,
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
        let selected = crate::engine::view::current(&shared.lock().unwrap())
            .layer(0).map(|layer| layer.id().to_owned()).unwrap_or_default();
        let tool_settings_store = shared.lock().unwrap().tool_settings.clone();
        let tool_profile = tool_settings::initial(&tool_settings_store);
        let initial_tool_profile = tool_profile.snapshot.profile.clone();
        let (preview_tx, preview_rx) = mpsc::channel();
        let preview_epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (job_tx, job_rx) = mpsc::channel();
        let (import_tx, import_rx) = mpsc::channel();
        let (image_open_tx, image_open_rx) = mpsc::channel();
        let (connect_tx, connect_rx) = mpsc::channel();
        let (merge_tx, merge_rx) = mpsc::channel();
        let (transfer_tx, transfer_rx) = mpsc::channel();
        let workspace = crate::workspace::attach(&shared);
        let (project_id, doc_snapshot) = {
            let e = shared.lock().unwrap();
            (e.project_id.clone(), e.doc_snapshot())
        };
        Self {
            preview_worker: preview_worker::Worker::new(preview_epoch.clone()),
            fonts,
            recovery: recovery::Browser::new(&workspace, &connection.state_dir, ctx),
            saved_versions: None,
            open_file_warning: None,
            workspace_root: shared.clone(),
            workspace,
            project_id,
            project_views: HashMap::new(),
            project_rects: HashMap::new(),
            project_drop_rect: None,
            project_labels: HashMap::new(),
            tab_active: String::new(),
            tab_drag: None,
            project_picker_rect: None,
            doc_snapshot,
            canvas_press_document: None,
            canvas_input_region: None,
            jobs: HashSet::new(),
            lifecycle: None,
            lifecycle_frame: false,
            exit_approved: false,
            connected: false,
            tab_transfer: None,
            tab_preview_requested: None,
            tab_preview_pending: None,
            tab_transfer_committing: false,
            tab_preview: None,
            transfer_tx,
            transfer_rx,
            shared,
            shortcuts: shortcuts::Shortcuts::load(preferences),
            preview_quality: preview_quality::Quality::load(preferences),
            updates: updates::Panel::load(preferences),
            about: about::About::load(),
            connection,
            selection_layers: [selected.clone()].into_iter().collect(),
            duplicate_repeat: None,
            pending_duplicate_repeat: None,
            duplicate_repeat_history: None,
            selection_anchor: selected.clone(),
            eye_sweep: None,
            selected,
            mask: false,
            isolate: false,
            solo_target: None,
            tool: Tool::Brush,
            selection_kind: "rectangle".into(),
            selection_mode: "replace".into(),
            selection_feather: 0.,
            selection_tolerance: 24.,
            selection_color_similarity: false,
            selection_contiguous: true,
            selection_merged: false,
            selection_live_wand: true,
            selection_width: 12.,
            selection_path: vec![],
            selection_gesture_mode: None,
            selection_shortcut_keys: HashSet::new(),
            project_settings: None,
            show_global_light: false,
            geometry: None,
            free_transform: None,
            free_transform_frame: false,
            free_transform_cancelled_pointer: false,
            source_editor: None,
            native_source_preview: None,
            native_source_preview_at: 0,
            edit_worker: Default::default(),
            edit_control: None,
            edit_context: ctx.clone(),
            pen: pen::State::default(),
            straight: brush::Straight::default(),
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
            color16: None,
            mask_value: 0,
            mask_value16: None,
            radius: initial_tool_profile.settings.radius,
            size_drag: None,
            live_gesture: None,
            primitive_drag: None,
            fill_press: None,
            workspace_stroke: None,
            brush: tool_profile.snapshot.profile.settings,
            tool_profile,
            tool_settings_store,
            tool_feedback: None,
            liquify: liquify::Settings::default(),
            liquify_effect: None,
            stroke_pressures: vec![],
            stroke_has_pressure: false,
            current_pressure: None,
            show_calibration: false,
            show_view: false,
            calibration: calibration::Pad::default(),
            background: [245, 242, 240, 255],
            background16: None,
            mask_background: 255,
            mask_background16: None,
            gradient_direction: Default::default(),
            show_brush: false,
            brush_preview: None,
            brush_search: String::new(),
            brush_category: "All".into(),
            brush_preset: initial_tool_profile.preset.clone(),
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
            #[cfg(not(test))]
            clipboard: crate::clipboard::Worker::new(ctx.clone()),
            #[cfg(test)]
            clipboard: crate::clipboard::Worker::memory(ctx.clone()),
            zoom: 1.0,
            pan: Vec2::ZERO,
            frame_pending: false,
            logo,
            wordmark,
            texture: None,
            canvas_pixels: None,
            canvas_preview_job: None,
            canvas_generation: 0,
            native_receipt: None,
            alt_sample: None,
            #[cfg(test)]
            color_pick_runs: 0,
            canvas_preview: None,
            capture_ready_at: 0.,
            canvas_selection: None,
            animation: animation::Animation::default(),
            preview_rx,
            preview_tx,
            pending: false,
            preview_epoch,
            preview_job: 0,
            native_preview: native_preview::Presentation::default(),
            native_dispatch: native_dispatch::Dispatch::default(),
            native_canvas: native_texture::Presenter::default(),
            #[cfg(test)]
            parameter_worker_gate: None,
            #[cfg(test)]
            parameter_worker_runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            preview_variant_builds: std::cell::Cell::new(0),
            preview_cache: Arc::new(std::sync::Mutex::new(crate::preview::Cache::default())),
            resident_preview_stroke: None,
            last_preview: None,
            thumbs: HashMap::new(),
            thumb_worker: None,
            thumb_pending: HashMap::new(),
            thumb_document: String::new(),
            thumb_native: false,
            parameter_gesture: None,
            cancelled_parameter: None,
            blend_hover: None,
            transient: vec![],
            points: vec![],
            drag_start: None,
            view_rect: None,
            canvas_viewport: None,
            view: view::View::default(),
            message: "Ready".into(),
            last_status: "Ready".into(),
            import_tx,
            import_rx,
            image_open_tx,
            image_open_rx,
            pending_image_open: None,
            pending_import: None,
            job_rx,
            job_tx,
            busy: false,
            load_control: None,
            load_preview: None,
            show_new: false,
            new_size: Default::default(),
            associations: Default::default(),
            read_only_info: None,
            read_only_patterns: None,
            selection_commit_failed: false,
            selection_commit_revision: None,
            deferred_selection_shortcuts: None,
            selection_pointer_cancelled: false,
            selection_gesture_source: None,
            save_report: None,
            save_loss_review: None,
            export_editor: None,
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
            folder_view_document: String::new(),
            mask_step: None,
            effect_selected: None,
            effect_expanded: HashSet::new(),
            activity_text: String::new(),
            activity_changed: std::time::Instant::now(),
            gizmo_handle: 0,
            gizmo_source: None,
            drag_revision: None,
            gizmo_bounds: None,
            transform_bounds: transform_bounds::Query::default(),
            color_sampler: color_sample::Sampler::default(),
            mask_thumbnails: mask_thumbnail::Thumbnails::default(),
            layer_thumbnails: mask_thumbnail::Thumbnails::default(),
            alt_sampler: color_sample::Sampler::default(),
            sampled_input: None,
            sampled_input_frame: false,
            magnetic_sampler: Default::default(),
            magnetic_input: None,
            color_pick: None,
            sample_press: None,
        }
    }
    fn edit(&mut self, commands: Vec<Value>, label: &str) {
        if !self.edit_worker.pending() {
            self.cancel_free_transform();
            self.close_proposal();
        }
        self.apply_action(commands, label, None, edit_job::Origin::Action);
    }
    fn apply_action(
        &mut self,
        commands: Vec<Value>,
        label: &str,
        observed: Option<(&str, u64)>,
        origin: edit_job::Origin,
    ) {
        let duplicate = commands.len() == 1
            && matches!(
                commands[0]["op"].as_str(),
                Some("layer.duplicate" | "layer.duplicate_selection")
            );
        let selection_edit = commands.iter().any(crate::selection::command_may_change_coverage);
        if self.edit_worker.pending() {
            self.selection_commit_failed |= selection_edit;
            self.message = "An edit is still being applied; wait for its result".into();
            return;
        }
        let origin = if duplicate && origin == edit_job::Origin::Action {
            edit_job::Origin::Duplicate
        } else {
            origin
        };
        let (request, native, source) = {
            let engine = self.shared.lock().unwrap();
            let view = crate::engine::view::current(&engine);
            let (document, revision) = observed.unwrap_or((view.id(), view.revision()));
            if origin.is_duplicate() && self.pending_duplicate_repeat.is_none()
                && view.id() == document && view.revision() == revision
            {
                self.pending_duplicate_repeat = Some(self.duplicate_start(view));
            }
            let (native, source) = match origin {
                edit_job::Origin::FreeTransform(_, key)
                | edit_job::Origin::Geometry(_, key)
                | edit_job::Origin::Filter(_, key) => {
                    let source = key.is_none().then(|| {
                        if self.doc_snapshot.view().same_observation(view) {
                            self.doc_snapshot.clone()
                        } else {
                            engine.doc_snapshot()
                        }
                    });
                    (key.is_some(), source)
                }
                _ => (view.native().is_some() || matches!(origin,
                    edit_job::Origin::CanvasGesture(_) | edit_job::Origin::Pen(_, Some(_))
                    | edit_job::Origin::NativeDuplicateRepeat(_)
                ), None),
            };
            (
                edit_job::Request {
                    project: self.project_id.clone(),
                    document: document.into(),
                    revision,
                    label: label.into(),
                    selection_edit,
                    origin,
                },
                native,
                source,
            )
        };
        if native {
            let ctx = self.edit_context.clone();
            if let Err(error) =
                self.submit_edit_job(request, edit_job::Commands::resident(commands), &ctx)
            {
                self.selection_commit_failed |= selection_edit;
                if let edit_job::Origin::Parameter(control) = origin {
                    self.cancelled_parameter = Some(control);
                }
                self.finish_editor_edit(origin, Err(error.clone()));
                if origin.is_duplicate() {
                    self.pending_duplicate_repeat = None;
                }
                self.message = error.into();
            }
        } else {
            let source = source.as_ref().map(DocumentSnapshot::view);
            let result = if source.is_some_and(|view| view.native().is_some()) {
                Err("Observed edit source changed; current work preserved".into())
            } else {
                server::edit_shared_controlled(
                    &self.shared, "human", &commands, Some(request.revision), None, &request.label,
                    None, Some(&request.document), source, &server::EditControl::default(),
                )
            };
            self.show_edit_result(&request, &result, &commands);
        }
    }
    fn submit_source_edit(
        &mut self,
        command: source::EditCommand,
        token: source::EditToken,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let editor = self
            .source_editor
            .as_ref()
            .filter(|editor| editor.token() == token)
            .ok_or("Source draft changed before applying")?;
        let request = edit_job::Request {
            project: self.project_id.clone(),
            document: editor.document.clone(),
            revision: editor.revision,
            label: "Edit text/vector source".into(),
            selection_edit: false,
            origin: edit_job::Origin::Source(token.nonce()),
        };
        let commands = match command {
            source::EditCommand::Resident(command) => edit_job::Commands::resident(vec![command]),
            source::EditCommand::Native(command) => edit_job::Commands::Native(command),
        };
        self.submit_edit_job(request, commands, ctx)
    }
    fn history_action(&mut self, redo: bool) {
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; wait for its result".into();
            return;
        }
        let direction = if redo {
            crate::engine::native::Direction::Redo
        } else {
            crate::engine::native::Direction::Undo
        };
        let (request, native) = {
            let engine = self.shared.lock().unwrap();
            let view = crate::engine::view::current(&engine);
            (
                edit_job::Request {
                    project: self.project_id.clone(),
                    document: view.id().into(),
                    revision: view.revision(),
                    label: if redo { "Redo" } else { "Undo" }.into(),
                    selection_edit: true,
                    origin: edit_job::Origin::History(direction),
                },
                view.native().is_some(),
            )
        };
        if native {
            let ctx = self.edit_context.clone();
            if let Err(error) =
                self.submit_edit_job(request, edit_job::Commands::resident(vec![]), &ctx)
            {
                self.message = error.into();
            }
        } else {
            let result = server::history_shared(
                &self.shared,
                "human",
                direction,
                Some(request.revision),
                Some(&request.document),
            );
            self.show_edit_result(&request, &result, &[]);
        }
    }
    fn submit_edit_job(
        &mut self,
        request: edit_job::Request,
        commands: edit_job::Commands,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let shared = self.shared.clone();
        let source = if let edit_job::Origin::CanvasGesture(key)
            | edit_job::Origin::FreeTransform(_, Some(key))
            | edit_job::Origin::Pen(_, Some(key))
            | edit_job::Origin::NativeDuplicateRepeat(key)
            | edit_job::Origin::Geometry(_, Some(key))
            | edit_job::Origin::Filter(_, Some(key)) = request.origin {
            Some(self.doc_snapshot.view().native()
                .filter(|doc| doc.key() == key)
                .cloned()
                .ok_or("Canvas gesture source changed; current work preserved")?)
        } else {
            None
        };
        let proposal = if let edit_job::Origin::Proposal(token) = request.origin {
            let id = self.proposal_review.as_deref()
                .filter(|id| egui::Id::new(id) == token)
                .ok_or("Proposal review changed before acceptance")?;
            let engine = shared.lock().unwrap();
            let document = engine.native_document().ok_or("Proposal source is no longer native")?;
            Some(crate::engine::native::copy_admitted::<_, String>(
                &id, document.budget(), true, &|| Ok(()),
            )?)
        } else {
            None
        };
        let control = server::EditControl::default();
        let worker_control = control.clone();
        let ctx = ctx.clone();
        self.edit_worker.submit(
            request,
            commands,
            move |request, commands| {
                if let Some(id) = &proposal {
                    return crate::collaboration::native::accept_stored(
                        &shared, "human", id, &request.document, request.revision,
                        &worker_control,
                    );
                }
                if let edit_job::Origin::History(direction) = request.origin {
                    return server::history_shared_controlled(
                        &shared,
                        "human",
                        direction,
                        Some(request.revision),
                        Some(&request.document),
                        &worker_control,
                    );
                }
                server::edit_shared_controlled(
                    &shared,
                    "human",
                    commands,
                    Some(request.revision),
                    None,
                    &request.label,
                    None,
                    Some(&request.document),
                    source.as_ref().map(DocumentView::from),
                    &worker_control,
                )
            },
            move || ctx.request_repaint(),
        )?;
        self.edit_control = Some(control);
        self.lifecycle_frame = true;
        self.message = "Applying changes…".into();
        Ok(())
    }
    fn poll_edit_job(&mut self) {
        if !self.edit_worker.pending() {
            return;
        }
        let document = match self.shared.try_lock() {
            Ok(engine) => crate::engine::view::current(&engine).id().to_owned(),
            Err(_) => return,
        };
        let Some(completed) = self.edit_worker.poll() else {
            return;
        };
        self.edit_control = None;
        let request = &completed.request;
        if let edit_job::Origin::Source(nonce) = request.origin {
            self.source_edit_finished_nonce(
                nonce,
                completed.result.as_ref().map(|_| ()).map_err(Clone::clone),
            );
        }
        let current = request.project == self.project_id && document == request.document;
        if let Some(queued) = self.deferred_selection_shortcuts.as_mut() {
            if Arc::ptr_eq(&queued.request, request) {
                queued.committed = completed.result.as_ref().ok().filter(|_| current).and_then(|reply| {
                    Some((reply.data()["revision"].as_u64()?, reply.native_key()?))
                });
                if queued.committed.is_none() { self.deferred_selection_shortcuts = None; }
            }
        }
        if !current {
            if request.origin.is_duplicate() {
                self.pending_duplicate_repeat = None;
            }
            self.finish_editor_edit(
                request.origin,
                completed.result.as_ref().map_err(Clone::clone),
            );
        }
        if current {
            self.show_edit_result(request, &completed.result, completed.commands());
        } else if let Some(view) = self.project_views.get_mut(&request.project) {
            view.message = completed
                .result
                .as_ref()
                .map(|_| request.label.clone())
                .unwrap_or_else(Clone::clone)
                .into();
        }
    }
    fn finish_editor_edit(
        &mut self,
        origin: edit_job::Origin,
        result: Result<&server::EditReply, String>,
    ) {
        let revision = result
            .as_ref()
            .ok()
            .and_then(|reply| reply.data()["revision"].as_u64());
        if let edit_job::Origin::SampledStroke(id) = origin {
            self.sampled_stroke_finished(
                id,
                result.and_then(|_| {
                    revision.ok_or_else(|| "Stroke receipt is missing its revision".into())
                }),
            );
            return;
        }
        let result = result.map(|_| ());
        match origin {
            edit_job::Origin::Proposal(token) if self.proposal_review.as_deref().is_some_and(|id| egui::Id::new(id) == token) => {
                match result {
                    Ok(()) => self.close_proposal(),
                    Err(error) => {
                        self.proposal_error = Some(error);
                        self.proposal_rendered = None;
                        self.preview_epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        self.native_dispatch.clear();
                        self.pending = false;
                    }
                }
            }
            edit_job::Origin::Stroke(token) => self.stroke_finished(token, revision),
            edit_job::Origin::Magnetic(token) => self.magnetic_finished(token, result),
            edit_job::Origin::FreeTransform(token, _) => self.free_transform_finished(token, result),
            edit_job::Origin::Filter(token, _) => self.filter_edit_finished(token, result),
            edit_job::Origin::Geometry(token, _) => self.geometry_finished(token, result),
            edit_job::Origin::Refinement(token) => self.refinement_finished(token, result),
            _ => {}
        }
    }
    fn show_edit_result(
        &mut self,
        request: &edit_job::Request,
        result: &Result<server::EditReply, String>,
        commands: &[Value],
    ) {
        self.finish_editor_edit(
            request.origin,
            result.as_ref().map_err(Clone::clone),
        );
        match result {
            Ok(value) => {
                if request.selection_edit {
                    self.selection_commit_revision = value.data()["revision"].as_u64();
                }
                self.message = value.data()["cleanup_warning"]
                    .as_str()
                    .map(|warning| format!("{}; {warning}", request.label))
                    .unwrap_or_else(|| request.label.clone())
                    .into();
                if request.origin.is_duplicate() && !self.duplicate_result_still_current(request, value) {
                    self.pending_duplicate_repeat = None;
                    self.duplicate_repeat = None;
                    self.duplicate_repeat_history = None;
                    self.last_preview = None;
                    return;
                }
                if request.origin.is_duplicate() || request.origin == edit_job::Origin::Group {
                    let roots: Vec<_> = value.data()["created_roots"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    let active_copy = self.pending_duplicate_repeat.as_ref()
                        .filter(|_| request.origin.is_duplicate())
                        .and_then(|repeat| repeat.active_copy.as_deref())
                        .and_then(|copy| roots.iter().find(|id| id.as_str() == copy));
                    if let Some(id) = active_copy.or_else(|| roots.first()) {
                        self.select_content(id);
                        if request.origin == edit_job::Origin::Group {
                            self.collapsed.remove(id);
                        }
                        self.selection_layers = roots.into_iter().collect();
                    }
                    self.layer_clipboard = true;
                } else if request.origin == edit_job::Origin::Masks {
                    self.select_channel(true);
                    self.mask_step = None;
                } else if let Some(id) = value.data()["created"]
                    .as_array()
                    .and_then(|ids| ids.first())
                    .and_then(Value::as_str)
                {
                    self.selected = id.into();
                    self.selection_layers = [id.to_owned()].into_iter().collect();
                    self.selection_anchor = id.into();
                    self.mask = false;
                    self.mask_step = None;
                }
                if let edit_job::Origin::Pen(index, _) = request.origin {
                    self.pen_edit_finished(index);
                }
                self.record_duplicate_transform(request, value, commands);
                self.native_source_preview = None;
                self.last_preview = None;
            }
            Err(error) => {
                if request.origin.is_duplicate() {
                    self.pending_duplicate_repeat = None;
                }
                self.selection_commit_failed |= request.selection_edit;
                if let edit_job::Origin::Parameter(control) = request.origin {
                    self.cancelled_parameter = Some(control);
                }
                self.message = error.clone().into();
            }
        }
    }
    fn cancel_pending_edit(&mut self) {
        let Some(control) = &self.edit_control else {
            return;
        };
        self.message = if control.cancel() {
            "Cancelling edit before publication…".into()
        } else {
            "Waiting for the edit result…".into()
        };
    }
    fn select_tool(&mut self, tool: Tool) {
        self.cancel_color_pick();
        self.sample_press = None;
        self.alt_sample = None;
        if let Some(editor) = &mut self.free_transform {
            if matches!(tool, Tool::Move | Tool::Rotate | Tool::Scale) {
                editor.set_mode(tool);
                return;
            }
            self.cancel_free_transform();
        }
        if self
            .source_editor
            .as_ref()
            .is_some_and(|editor| editor.placement.is_some())
        {
            self.cancel_source();
        }
        self.cancel_selection_draft(false);
        if self.tool != tool {
            self.straight.reset();
            self.reset_duplicate_transform();
        } else {
            self.straight.cancel();
        }
        self.store_current_tool_profile();
        self.restore_tool_profile(tool);
        self.tool = tool;
        if !matches!(
            tool,
            Tool::Brush | Tool::Eraser | Tool::Smudge | Tool::Clone | Tool::Heal
        ) {
            self.show_brush = false;
        }
        if tool == Tool::MagicWand {
            self.selection_kind = "wand".into();
        }
        self.cancel_pen();
        self.primitive_drag = None;
        self.fill_press = None;
        self.workspace_stroke = None;
        self.gizmo_bounds = None;
        self.transform_bounds.clear();
        self.gizmo_handle = 0;
        self.gizmo_source = None;
        self.stroke_pressures.clear();
        self.stroke_has_pressure = false;
        self.current_pressure = None;
        self.transient.clear();
        self.size_drag = None;
        self.interrupt_animation();
        self.last_preview = None;
    }
    fn transform_controls<'a>(&mut self, ui: &mut egui::Ui, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        ui.spacing_mut().item_spacing.x = 3.0;
        // Standard button minimums exceed the footer's inner height.
        let height = ui.available_height().clamp(14.0, 20.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (index, (tool, _default_key)) in [
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
                let key = self.shortcuts.tool_label(tool).replace("Shift+", "⇧");
                let caption = self.tool_caption(tool);
                let response = response.on_hover_text(caption.as_str());
                self.transform_rects[index] = Some(response.rect);
                let active = self
                    .free_transform
                    .as_ref()
                    .map_or(Some(self.tool), |editor| editor.mode())
                    == Some(tool);
                response.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::RadioButton, response.enabled(), active, &caption)
                });
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
                    if !ui.is_enabled() {
                        Color32::from_gray(100)
                    } else if ai {
                        AI_BLUE
                    } else if active {
                        ACCENT
                    } else {
                        Color32::from_rgb(245, 242, 240)
                    },
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
                icons::paint_focus(ui, &response);
                if response.clicked() {
                    if tool == Tool::None {
                        self.deselect_area(doc, true);
                    } else {
                        self.select_tool(tool);
                    }
                }
            }
        });
    }
    fn selected_layer_ids<'a>(&self, doc: impl Into<DocumentView<'a>>) -> Vec<String> {
        let doc = doc.into();
        doc.layers()
            .filter(|l| self.selection_layers.contains(l.id()))
            .map(|l| l.id().to_owned())
            .collect()
    }
    fn copy_request(
        &self,
        snapshot: &DocumentSnapshot,
        cut: bool,
        merged: bool,
    ) -> Option<crate::clipboard::EditRequest> {
        let doc = snapshot.view();
        // An active pixel selection takes precedence over the last layer-row click.
        if self.layer_clipboard && doc.selection().is_none() && (cut || !merged) {
            Some(crate::clipboard::EditRequest::Layers {
                doc: snapshot.clone(),
                ids: self.selected_layer_ids(doc),
                cut: cut.then(|| self.shared.clone()),
            })
        } else if cut && doc.selection().is_none() {
            None
        } else {
            Some(crate::clipboard::EditRequest::Pixels {
                shared: self.shared.clone(),
                doc: snapshot.clone(),
                target: self.selected.clone(),
                mask: self.mask,
                merged: !cut && merged,
                cut,
                step: self.mask_step.clone(),
            })
        }
    }
    fn create_panel_layer<'a>(
        &mut self,
        doc: impl Into<DocumentView<'a>>,
        adjustment: Option<(&str, &str)>,
    ) {
        let doc = doc.into();
        if self.busy || self.edit_worker.pending() || self.parameter_gesture.is_some() {
            self.message = "Finish the current edit before adding a layer".into();
            return;
        }
        let charge = doc
            .native()
            .map(|native| {
                let bytes = doc
                    .layers()
                    .try_fold(16384usize, |sum, layer| {
                        layer
                            .id()
                            .len()
                            .checked_mul(8)
                            .and_then(|n| sum.checked_add(n))
                    })
                    .filter(|n| *n <= 64 * 1024 * 1024)
                    .ok_or("Layer creation metadata exceeds admission")?;
                crate::psd::memory36::MetadataCharge::reserve(native.budget(), bytes, &|| Ok(()))
            })
            .transpose();
        let _charge = match charge {
            Ok(charge) => charge,
            Err(error) => {
                self.message = error.into();
                return;
            }
        };
        let (command, label) = match adjustment {
            Some((kind, name)) => (
                json!({"op":"adjustment.add","layers":self.selected_layer_ids(doc),"kind":kind,"name":name}),
                "New adjustment",
            ),
            None => (
                json!({"op":"layer.add","kind":"paint","name":format!("Paint {}",doc.layer_count()+1)}),
                "New paint layer",
            ),
        };
        self.apply_action(
            vec![command],
            label,
            Some((doc.id(), doc.revision())),
            edit_job::Origin::Action,
        );
    }
    fn create_folder<'a>(&mut self, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        if self.busy {
            return;
        }
        if !self.edit_worker.pending() {
            self.close_proposal();
            self.finish_parameter(true);
        }
        let ids = self.selected_layer_ids(doc);
        self.apply_action(
            vec![json!({"op":"group.create_selected","layers":ids,"name":"Folder"})],
            "Group selected layers",
            Some((doc.id(), doc.revision())),
            edit_job::Origin::Group,
        );
    }
    fn selection_display<'a>(
        &'a self,
        doc: DocumentView<'a>,
    ) -> (
        Option<[i32; 4]>,
        Option<&'a [[f32; 2]]>,
        Option<&'a [Vec<[f32; 2]>]>,
    ) {
        if let Some(canvas) = self
            .native_canvas
            .current()
            .filter(|canvas| canvas.key().identity.document_id == doc.id()
                && self.native_proposal_canvas_current(doc))
        {
            let outline = canvas.outline();
            return (outline.bounds(), outline.polygon(), outline.contours());
        }
        if let Some(preview) = self
            .canvas_selection
            .as_ref()
            .filter(|preview| preview.document == doc.id() && preview.revision == doc.revision())
        {
            return (
                preview.bounds,
                preview.polygon.as_deref(),
                preview
                    .coverage
                    .as_ref()
                    .filter(|coverage| preview.bounds == Some(coverage.bounds))
                    .map(|coverage| coverage.contours.as_slice()),
            );
        }
        (
            doc.selection(),
            doc.selection_polygon(),
            doc.selection_contours(),
        )
    }
    fn duplicate_layers<'a>(&mut self, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        let ids = self.selected_layer_ids(doc);
        self.apply_action(
            vec![json!({"op":"layer.duplicate","layers":ids})],
            "Duplicate layers",
            Some((doc.id(), doc.revision())),
            edit_job::Origin::Duplicate,
        );
    }
    fn duplicate_selection<'a>(&mut self, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        let mask = self.mask && doc.selection().is_some();
        if !self.edit_worker.pending() {
            self.close_proposal();
        }
        self.apply_action(
            vec![json!({"op":"layer.duplicate_selection","layer":self.selected,"mask":mask,"document_id":doc.id(),"source_revision":doc.revision()})],
            if mask { "Duplicate mask selection" } else { "Duplicate layer selection" },
            Some((doc.id(), doc.revision())),
            doc.native().map_or(edit_job::Origin::Duplicate, |native| {
                edit_job::Origin::NativeDuplicateRepeat(native.key())
            }),
        );
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
                let engine = self.shared.lock().unwrap();
                crate::tree::roots_view(crate::engine::view::current(&engine), &self.selection_layers)
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
    fn pending_parameter_document(
        &mut self,
        ctx: &egui::Context,
        doc: &Document,
    ) -> Option<Arc<Document>> {
        let edit = self.parameter_gesture.as_mut()?;
        if edit.engine.as_ptr() != Arc::as_ptr(&self.shared)
            || edit.document != doc.id
            || edit.revision != doc.revision
        {
            edit.preview = None;
            return None;
        }
        {
            let Ok(engine) = self.shared.try_lock() else {
                ctx.request_repaint_after(Duration::from_millis(16));
                return None;
            };
            if crate::workspace::guard(&engine, &edit.document, edit.revision).is_err() {
                edit.preview = None;
                return None;
            }
        }
        edit.preview.clone()
    }
    fn invalidate_parameter_preview(&mut self) {
        self.preview_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.last_preview = None;
    }
    fn finish_parameter(&mut self, cancel: bool) {
        if !cancel && self.edit_worker.pending() && self.parameter_gesture.is_some() {
            self.message = "An edit is still being applied; parameter draft preserved".into();
            return;
        }
        let Some(edit) = self.parameter_gesture.take() else {
            return;
        };
        if cancel {
            self.cancelled_parameter = Some(edit.control);
        }
        self.invalidate_parameter_preview();
        if let Some(ctx) = &edit.typing {
            controls::cancel_number_edit(ctx, edit.control);
        }
        if cancel || !edit.dirty {
            return;
        }
        let selection_edit = edit.commands.iter().any(crate::selection::command_may_change_coverage);
        if edit.engine.as_ptr() != Arc::as_ptr(&self.shared) {
            self.cancelled_parameter = Some(edit.control);
            self.selection_commit_failed |= selection_edit;
            self.message = "Image changed during parameter edit · preview cancelled".into();
            return;
        }
        let current = {
            let engine = self.shared.lock().unwrap();
            crate::workspace::guard(&engine, &edit.document, edit.revision).is_ok()
        };
        if !current {
            self.cancelled_parameter = Some(edit.control);
            self.selection_commit_failed |= selection_edit;
            self.message = "Image changed during parameter edit · preview cancelled".into();
            return;
        }
        self.apply_action(
            edit.commands,
            &edit.label,
            Some((&edit.document, edit.revision)),
            edit_job::Origin::Parameter(edit.control),
        );
    }
    fn layer_parameter(&mut self, extra: Value, label: &str, response: &egui::Response) {
        if response
            .ctx
            .data_mut(|data| data.remove_temp::<bool>(response.id.with("cancel parameter")))
            .unwrap_or(false)
        {
            self.finish_parameter(true);
            return;
        }
        // A cancelled control must not rebind a still-held pointer to newer work.
        // The next physical press clears this transient guard in draw().
        if self.cancelled_parameter == Some(response.id) {
            return;
        }
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; parameter draft preserved".into();
            return;
        }
        if self
            .parameter_gesture
            .as_ref()
            .is_some_and(|edit| edit.engine.as_ptr() != Arc::as_ptr(&self.shared))
        {
            self.finish_parameter(true);
            self.message = "Image changed during parameter edit · preview cancelled".into();
            return;
        }
        let down = response.is_pointer_button_down_on() || response.dragged();
        let mut command = extra;
        let explicit_target = command.get("layer").is_some();
        if !matches!(command["op"].as_str(), Some("selection.wand_update" | "document.global_light")) {
            command["op"] = json!(if command.get("filter").is_some() {
                "filter.update"
            } else if command.get("effect").is_some() {
                "effect.update"
            } else if command.get("step").is_some() {
                "mask.step.update"
            } else {
                "layer.update"
            });
            if command["op"] != "filter.update" && !explicit_target {
                command["layer"] = json!(self.selected);
            }
        }
        let mut commands = if command["op"] == "layer.update" && !explicit_target {
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
        if controls::number_editing(&response.ctx, response.id)
            && !self
                .parameter_gesture
                .as_ref()
                .is_some_and(|edit| edit.control == response.id && edit.typing.is_some())
        {
            self.finish_parameter(false);
            if self.edit_worker.pending() || self.cancelled_parameter == Some(response.id) {
                controls::cancel_number_edit(&response.ctx, response.id);
                self.cancelled_parameter = Some(response.id);
                return;
            }
            let (document, revision) = {
                let engine = self.shared.lock().unwrap();
                let view = crate::engine::view::current(&engine);
                (view.id().to_owned(), view.revision())
            };
            self.parameter_gesture = Some(ParameterEdit {
                control: response.id,
                gesture: crate::engine::id(),
                engine: Arc::downgrade(&self.shared),
                document,
                revision,
                commands,
                preview: None,
                #[cfg(test)]
                preview_runs: 0,
                label: label.into(),
                typing: Some(response.ctx.clone()),
                dirty: response.changed(),
            });
            self.interrupt_animation();
            self.invalidate_parameter_preview();
            return;
        }
        if self
            .parameter_gesture
            .as_ref()
            .is_some_and(|edit| edit.control == response.id && edit.typing.is_some())
        {
            if response.changed() {
                let edit = self.parameter_gesture.as_mut().unwrap();
                if edit.commands != commands {
                    edit.commands = commands;
                    edit.preview = None;
                }
                edit.dirty = true;
                self.invalidate_parameter_preview();
                if !controls::number_editing(&response.ctx, response.id) {
                    self.finish_parameter(false);
                }
            }
            return;
        }
        let releasing = response.drag_stopped_by(egui::PointerButton::Primary)
            && self.parameter_gesture.as_ref().is_some_and(|edit| edit.control == response.id);
        if down || releasing {
            if self
                .parameter_gesture
                .as_ref()
                .is_some_and(|edit| edit.control != response.id)
            {
                self.finish_parameter(false);
                if self.edit_worker.pending() {
                    self.cancelled_parameter = Some(response.id);
                    return;
                }
            }
            let (current_document, current_revision, native) = {
                let engine = self.shared.lock().unwrap();
                let view = crate::engine::view::current(&engine);
                (view.id().to_owned(), view.revision(), view.native().is_some())
            };
            let (document, revision) = self
                .parameter_gesture
                .as_ref()
                .map(|edit| (edit.document.clone(), edit.revision))
                .unwrap_or((current_document.clone(), current_revision));
            for command in &mut commands {
                command["document_id"] = json!(document);
                command["source_revision"] = json!(revision);
            }
            let unchanged = self.parameter_gesture.as_ref().is_some_and(|edit| {
                edit.engine.as_ptr() == Arc::as_ptr(&self.shared)
                    && edit.control == response.id
                    && edit.document == current_document
                    && edit.revision == current_revision
                    && edit.commands == commands
            });
            if unchanged {
                let edit = self.parameter_gesture.as_mut().unwrap();
                edit.dirty |= response.changed();
            } else {
                let gesture = self
                    .parameter_gesture
                    .as_ref()
                    .map(|edit| edit.gesture.clone())
                    .unwrap_or_else(crate::engine::id);
                self.parameter_gesture = Some(ParameterEdit {
                    control: response.id,
                    gesture,
                    engine: Arc::downgrade(&self.shared),
                    document,
                    revision,
                    commands,
                    preview: None,
                    #[cfg(test)]
                    preview_runs: self
                        .parameter_gesture
                        .as_ref()
                        .map_or(0, |edit| edit.preview_runs),
                    label: label.into(),
                    typing: None,
                    dirty: response.changed()
                        || self
                            .parameter_gesture
                            .as_ref()
                            .is_some_and(|edit| edit.dirty),
                });
                self.interrupt_animation();
                // Native capture can finish a compatible pointer preview before
                // scheduling these values. Release still invalidates its epoch.
                if !native {
                    self.invalidate_parameter_preview();
                }
            }
            if releasing {
                self.finish_parameter(false);
            }
        } else {
            self.finish_parameter(true);
            self.apply_action(
                commands,
                label,
                None,
                edit_job::Origin::Parameter(response.id),
            );
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
        let executable = match crate::packaged::mcp_command() {
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
        self.message = self.connect_feedback.clone().into();
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
            .join(" ")
            .into();
        // Client presence belongs to the live MCP handshake, never to a setup result.
    }
    fn merge_layers<'a>(&mut self, ctx: &egui::Context, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        if self.busy {
            return;
        }
        self.close_proposal();
        self.finish_parameter(true);
        let mut ids = doc
            .layers()
            .filter(|l| self.selection_layers.contains(l.id()))
            .map(|l| l.id().to_owned())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            ids.push(self.selected.clone());
        }
        let document = doc.id().to_owned();
        let project = self.project_id.clone();
        let revision = doc.revision();
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
            self.message = format!("Could not start merging layers: {error}").into();
        }
    }
    fn open(&mut self) {
        if self.busy {
            return;
        }
        self.open_job(None, false);
    }
    fn recover_previous_save(&mut self) {
        if self.busy {
            return;
        }
        self.show_previous_saves();
    }
    fn open_job(&mut self, path: Option<PathBuf>, recover: bool) {
        self.open_named_job(path, recover, None);
    }
    fn open_named_job(&mut self, path: Option<PathBuf>, recover: bool, name: Option<String>) {
        if self.busy || self.pending_image_open.is_some() {
            return;
        }
        let workspace = self.workspace.clone();
        let initiating = self.project_id.clone();
        let control = crate::loading::Control::default();
        self.load_control = Some(control.clone());
        self.load_preview = None;
        let image_open_tx = self.image_open_tx.clone();
        self.job(move || {
            let path = path.or_else(|| {
                rfd::FileDialog::new()
                    .add_filter(
                        "Images and Photoshop documents",
                        &std::iter::once("psd")
                            .chain(std::iter::once("psb"))
                            .chain(crate::image_import::EXTENSIONS.iter().copied())
                            .collect::<Vec<_>>(),
                    )
                    .add_filter("Photoshop documents", &["psd", "psb"])
                    .add_filter("Images", crate::image_import::EXTENSIONS)
                    .pick_file()
            });
            let Some(path) = path else {
                return Ok("Open canceled · current projects preserved".into());
            };
            if !recover {
                match server::begin_image_project_in(&workspace, &path, &control, &initiating)? {
                    server::ImageOpen::Opened => return Ok("Image opened — save as PSD or PSB".into()),
                    server::ImageOpen::Choose(choice) => {
                        image_open_tx.send(image_open::Pending {
                            source: image_open::Source::Native(choice),
                            control,
                            initiating,
                            index: 0,
                        }).map_err(|_| "Open workspace closed")?;
                        return Ok("Choose image content to open".into());
                    }
                    server::ImageOpen::Other => {}
                }
            }
            if !recover && crate::image_open::photoshop(&path) {
                server::open_project_in(&workspace, &path, &control, Some(&initiating))?;
                return Ok("Photoshop document opened".into());
            }
            let s = Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
            if !recover {
                let project = s.lock().unwrap().project_id.clone();
                let prepared = match crate::image_open::Prepared::read(&path, &project, &control) {
                    Ok(p) => p,
                    Err(error) => {
                        crate::workspace::duplicate_warning(&workspace, &path, &error, true);
                        return Err(error);
                    }
                };
                let icon_sizes = prepared.encoded.icon_sizes()?;
                let pending = image_open::Pending {
                    source: image_open::Source::Resident { shared: s, prepared, icon_sizes },
                    control,
                    initiating,
                    index: 0,
                };
                if pending.source.choice().0.is_some() {
                    image_open_tx
                        .send(pending)
                        .map_err(|_| "Open workspace closed")?;
                    return Ok("Choose image content to open".into());
                }
                return image_open::finish(pending, &workspace);
            }
            if let Err(error) = server::open_progress(&s, &path, &control) {
                crate::workspace::duplicate_warning(&workspace, &path, &error, true);
                return Err(error);
            }
            if recover {
                let mut e = s.lock().unwrap();
                e.path = None;
                e.file_version = None;
                e.release_file_ownership();
                e.saved_revision = u64::MAX;
                if let Some(name) = name {
                    e.authority.resident_state_mut()?.document.name = format!("Recovered — {name}");
                }
            }
            crate::workspace::register_opened(&workspace, s, Some(&initiating))?;
            Ok(if recover {
                "Recovered version — save to your chosen PSD or PSB"
            } else {
                "Photoshop document opened"
            }
            .into())
        });
    }
    fn save(&mut self, save_as: bool) {
        if self.free_transform.is_some() {
            self.message = "Apply or cancel Free Transform before saving.".into();
            return;
        }
        if self.busy {
            self.message = "Finish the current file operation before saving".into();
            return;
        }
        if self.save_loss_review.is_some() {
            return;
        }
        let report = match crate::compatibility::report_snapshot(&self.doc_snapshot) {
            Ok(report) => report,
            Err(error) => {
                self.message = error.into();
                return;
            }
        };
        if !report.data().save_allowed {
            self.open_save_report();
            self.message =
                "Protected source · create an explicit compatible copy before saving".into();
            return;
        }
        if report.data().has_editability_losses {
            self.save_loss_review = Some(compatibility::SaveReview::new(
                self.project_id.clone(),
                self.doc_snapshot.view().name().to_owned(),
                report,
                compatibility::SaveIntent::Save { save_as },
            ));
            self.lifecycle_frame = true;
            return;
        }
        self.save_frozen(
            save_as,
            self.doc_snapshot.view().id().to_owned(),
            self.doc_snapshot.view().revision(),
            false,
        );
    }
    fn save_frozen(&mut self, save_as: bool, document: String, revision: u64, accepted: bool) {
        let s = self.shared.clone();
        let current = if save_as {
            None
        } else {
            s.lock().unwrap().path.clone()
        };
        self.job(move || {
            let overwrite = current.is_none();
            let path = current.or_else(|| {
                rfd::FileDialog::new()
                    .add_filter("PSD document", &["psd"])
                    .add_filter("PSB document", &["psb"])
                    .set_file_name("Untitled.psd")
                    .save_file()
            });
            let Some(path) = path else {
                return Ok("Save canceled · project remains open".into());
            };
            if accepted {
                server::save_accepted_source(&s, &path, &document, revision, overwrite)?;
            } else {
                server::save_confirmed_source(&s, &path, &document, revision, overwrite)?;
            }
            Ok("Photoshop document saved".into())
        });
    }
    fn export(&mut self, format: crate::production_export::Format) {
        use crate::production_export::Format;
        let s = self.shared.clone();
        let document = self.doc_snapshot.view().id().to_owned();
        let revision = self.doc_snapshot.view().revision();
        let depth = self.doc_snapshot.view().depth();
        self.job(move || {
            let (label, title) = match format {
                Format::Png => ("PNG", format!("Export native {depth}-bit PNG")),
                Format::Jxl => (
                    "JPEG XL",
                    format!("Export lossless native {depth}-bit JPEG XL"),
                ),
                Format::Webp => (
                    "WebP · lossless 8-bit",
                    format!("Export 8-bit WebP · {depth}-bit project unchanged"),
                ),
            };
            let Some(path) = rfd::FileDialog::new()
                .add_filter(label, &[format.extension()])
                .set_title(title)
                .set_file_name(format!("PeerBrush.{}", format.extension()))
                .save_file()
            else {
                return Ok("Ready".into());
            };
            if Format::from_path(&path)? != format {
                return Err(format!(
                    "Choose a .{} destination for this export",
                    format.extension()
                ));
            }
            server::export_confirmed_source(&s, &path, &document, revision)?;
            Ok(match format {
                Format::Png => "PNG exported",
                Format::Jxl => "Lossless JPEG XL exported",
                Format::Webp => "8-bit WebP exported · working document unchanged",
            }
            .into())
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
        let (document, revision, native) = {
            let e = self.shared.lock().unwrap();
            let view = crate::engine::view::current(&e);
            (view.id().to_owned(), view.revision(), e.native_document().cloned())
        };
        self.busy = true;
        self.jobs.insert(project.clone());
        let tx = self.import_tx.clone();
        let shared = self.shared.clone();
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
                let runtime = (native.is_some() && !paths.is_empty())
                    .then(|| crate::workspace::native_dispatch::runtime_for(&shared)).transpose()?;
                let session = runtime
                    .as_ref()
                    .map(|runtime| runtime.begin_foreground())
                    .transpose()?;
                let mut bytes = 0usize;
                let mut native_remaining = 128 * 1024 * 1024;
                paths
                    .into_iter()
                    .map(|path| {
                        if let Some(session) = &session {
                            let extension = path.extension().and_then(|ext| ext.to_str())
                                .ok_or("Missing image extension")?;
                            let format = crate::image_import::native::OpenFormat::from_path(&path)
                                .ok_or("Unsupported image format")?;
                            let check = || session.check();
                            let (choice, digest, icon_sizes) = match format {
                                crate::image_import::native::OpenFormat::Png => {
                                    let inspection = crate::image_import::prepared_png::apng::inspect_layer(
                                        session, &path, &check,
                                    )?;
                                    (
                                        inspection.frames.map(|count| ("frame", count as usize)),
                                        inspection.digest,
                                        Vec::new(),
                                    )
                                }
                                _ => format.inspect_layer(session, &path, &mut native_remaining, &check)?,
                            };
                            let source = ImportSource::Native {
                                format: extension.to_ascii_uppercase(),
                                choice,
                                digest,
                            };
                            return Ok(ImportFile {
                                path,
                                source,
                                index: 0,
                                icon_sizes,
                            });
                        }
                        let encoded = crate::image_import::Encoded::file(&path)?;
                        bytes += encoded.encoded_len();
                        if bytes > 128 * 1024 * 1024 {
                            return Err("Encoded image batch exceeds 128 MiB".into());
                        }
                        let icon_sizes = encoded.icon_sizes()?;
                        Ok(ImportFile { path, source: ImportSource::Resident(encoded), index: 0, icon_sizes })
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
                native,
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
            if let Some(document) = pending.native {
                let mut commands = Vec::with_capacity(pending.files.len());
                for file in pending.files {
                    let ImportSource::Native { choice, digest, .. } = file.source else {
                        return Err("Native import batch source changed".into());
                    };
                    let mut command = json!({"op":"image.import", "path":file.path, "source_sha256":digest});
                    if let Some((key, _)) = choice {
                        command[key] = json!(file.index);
                    }
                    commands.push(command);
                }
                server::edit_shared_observed(&shared, "human", &commands, (&document).into(), None, "Import images")?;
                return Ok(format!("Imported {} image layers", commands.len()));
            }
            let mut budget = crate::image_import::BUDGET; let mut commands = Vec::new(); let mut pixels = Vec::new();
            for file in pending.files {
                let mut command = json!({"op":"image.import","path":file.path,"document_id":pending.document,"source_revision":pending.revision});
                let ImportSource::Resident(encoded) = file.source else { return Err("Import batch source changed".into()) };
                if let Some(choice) = encoded.info.choice {command[choice]=json!(file.index);}
                let image = encoded.prepare(&command,budget)?;
                budget = budget.checked_sub(image.pixels.stored_bytes()).ok_or("Image batch exceeds 256 MiB")?;
                commands.push(command); pixels.push(image);
            }
            let count = commands.len(); let mut engine = shared.lock().unwrap();
            crate::workspace::guard(&engine, &pending.document, pending.revision)
                .map_err(|_| "The source project changed; import the images again")?;
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
                egui::ScrollArea::vertical().max_height(360.).show(ui, |ui| {
                    for (row, file) in pending.files.iter_mut().enumerate() {
                        ui.push_id(row, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(file.path.file_name().unwrap_or_default().to_string_lossy());
                                if let Some((choice, count)) = file.choice() {
                                    image_open::content_choice(ui, choice, count, &file.icon_sizes, &mut file.index);
                                } else {
                                    ui.weak(file.format());
                                }
                            });
                        });
                    }
                });
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
                    .is_some_and(|e| e.eq_ignore_ascii_case("psd") || e.eq_ignore_ascii_case("psb"))
            })
            .count();
        if psds > 0 && paths.len() != 1 {
            self.message = "Drop one PSD/PSB to open it, or image files to add layers".into();
        } else if psds == 1 {
            self.open_job(Some(paths[0].clone()), false);
        } else if !paths.is_empty() {
            self.begin_import(Some(paths));
        }
    }
    fn liquify_command(&self, points: &[[f32; 2]]) -> Value {
        brush::with_stroke_samples(self.liquify_command_fields(), points, None)
    }
    fn liquify_command_fields(&self) -> Value {
        let mut command = liquify::command_fields(&self.selected, self.radius, &self.liquify);
        command["mask"] = json!(self.mask);
        if let Some(effect) = &self.liquify_effect {
            command["effect"] = json!(effect);
        }
        if let (Some(document), Some(revision)) = (&self.straight.document, self.drag_revision) {
            command["document_id"] = json!(document);
            command["source_revision"] = json!(revision);
        }
        command
    }
    fn activate_liquify(&mut self, effect: Option<String>) {
        self.select_tool(Tool::Liquify);
        self.mask = false;
        self.liquify_effect = effect;
        self.message =
            "Liquify: paint on the canvas · Choose Push, Expand, Pinch or Restore above".into();
    }
    fn has_preview_stroke(&self) -> bool {
        !self.points.is_empty()
            && self.proposal_review.is_none()
            && (self.tool == Tool::Liquify || self.brush_tool() && !self.sampled_stroke_waiting())
    }
    fn preview_stroke(&self) -> Option<Value> {
        self.has_preview_stroke().then(|| {
            if self.tool == Tool::Liquify {
                self.liquify_command(&self.points)
            } else {
                self.stroke_command(&self.points, self.stroke_color())
            }
        })
    }
    fn resident_base_preview_target(&self) -> Option<&str> {
        (self.proposal_review.is_none()
            && self.parameter_gesture.is_none()
            && self.transient.is_empty()
            && self.sampled_preview_commands().is_empty()
            && self.blend_hover.is_none()
            && self.refinement.is_none()
            && self.selection_drag_preview_key().is_none()
            && !self.has_preview_stroke())
            .then(|| if self.isolate { self.selected.as_str() } else { "" })
    }
    fn preview_variant(&mut self) -> String {
        #[cfg(test)]
        self.preview_variant_builds
            .set(self.preview_variant_builds.get() + 1);
        if self.has_preview_stroke() {
            let (fields, pressures) = if self.tool == Tool::Liquify {
                (self.liquify_command_fields(), None)
            } else {
                (
                    self.stroke_command_fields(self.points.first().copied(), self.stroke_color()),
                    self.pressures_for_stroke(self.points.len()),
                )
            };
            if self.resident_preview_stroke.as_ref().is_none_or(|stroke| {
                !stroke.matches(&fields, &self.points, pressures)
            }) {
                self.resident_preview_stroke = Some(Arc::new(ResidentPreviewStroke {
                    id: crate::engine::id(),
                    fields,
                    points: self.points.clone(),
                    pressures: pressures.map(<[f32]>::to_vec),
                }));
            }
        } else {
            self.resident_preview_stroke = None;
        }
        if let Some(id) = &self.proposal_review {
            return format!("proposal:{id}:{}", self.proposal_original);
        }
        if let Some(target) = self.resident_base_preview_target() {
            return format!("base:{target}");
        }
        let target = if self.isolate {
            self.selected.as_str()
        } else {
            ""
        };
        format!(
            "{target};{:?};{};{};{}",
            self.blend_hover,
            self.resident_preview_stroke.as_ref().map_or("", |stroke| stroke.id.as_str()),
            serde_json::to_string(&(
                &self.transient,
                self.parameter_gesture.as_ref().map(|edit| &edit.commands),
                self.sampled_preview_key()
            ))
            .unwrap(),
            self.refinement
                .as_ref()
                .map(|r| r.preview.as_str())
                .unwrap_or("")
        )
    }
    fn live_key(&self) -> String {
        if let Some(editor) = &self.free_transform {
            return editor.live_key();
        }
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
        if let Some(gesture) = self.crop_preview_key() {
            return format!("crop:{gesture}");
        }
        if let Some(sweep) = &self.eye_sweep {
            return format!("visibility:{}:{}", sweep.gesture, sweep.revision);
        }
        if let Some(drag) = &self.layer_drag {
            return format!("reorder:{}:{}", drag.gesture, drag.revision);
        }
        if let Some(id) = self.primitive_drag.as_ref().and_then(primitive_drag::Drag::live_key) {
            return format!("primitive:{id}:{}:{}:{}", self.selected, self.mask, self.isolate);
        }
        if !self.points.is_empty() || self.gizmo_handle != 0 || self.pen.in_gesture() {
            if let Some(id) = &self.live_gesture {
                return format!("{id}:{}:{}:{}", self.selected, self.mask, self.isolate);
            }
        }
        String::new()
    }
    fn set_canvas(&mut self, ctx: &egui::Context, w: u32, h: u32, bytes: Vec<u8>) -> bool {
        let update = if let Some(texture) = &mut self.texture {
            texture.update(ctx, "canvas", [w, h], &bytes)
        } else {
            native_texture::RgbaTexture::new(ctx, "canvas", [w, h], &bytes)
                .map(|texture| self.texture = Some(texture))
        };
        if let Err(error) = update {
            self.message = error.into();
            return false;
        }
        self.native_receipt = None;
        self.canvas_preview_job = None;
        self.canvas_generation = self.canvas_generation.wrapping_add(1);
        self.canvas_pixels = Some((w, h, bytes));
        true
    }
    fn apply_canvas_patch(&mut self, preview: &Preview, patch: crate::preview::CanvasPatch) -> bool {
        let Some((w, h, pixels)) = self.canvas_pixels.as_mut() else {
            return false;
        };
        let Some(texture) = self.texture.as_mut() else {
            return false;
        };
        let (width, height) = (*w as usize, *h as usize);
        let [left, top, right, bottom] = patch.bounds.map(|value| value as usize);
        if self.canvas_preview_job != Some(patch.base_job)
            || (*w, *h) != (preview.w, preview.h)
            || texture.size() != [*w, *h]
            || width.checked_mul(height).and_then(|n| n.checked_mul(4)) != Some(pixels.len())
            || left >= right
            || top >= bottom
            || right > width
            || bottom > height
        {
            return false;
        }
        let row_bytes = (right - left) * 4;
        if preview.bytes.len() != row_bytes * (bottom - top) {
            return false;
        }
        for (y, row) in (top..bottom).zip(preview.bytes.chunks_exact(row_bytes)) {
            let start = (y * width + left) * 4;
            pixels[start..start + row_bytes].copy_from_slice(row);
        }
        texture.patch(pixels, patch.bounds);
        self.native_receipt = None;
        self.canvas_preview_job = Some(preview.job);
        self.canvas_generation = self.canvas_generation.wrapping_add(1);
        true
    }
    fn request_preview<'a>(&mut self, ctx: &egui::Context, doc: impl Into<DocumentView<'a>>) {
        let (snapshot, project, proposal) = {
            let Ok(mut engine) = self.shared.try_lock() else {
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            };
            let proposal = if engine.native_document().is_none() && !self.proposal_original {
                self.proposal_review.as_deref().map(|id| engine.proposal_snapshot(id))
            } else {
                None
            };
            (engine.doc_snapshot(), engine.project_id.clone(), proposal)
        };
        let current = snapshot.view();
        if !current.same_observation(doc.into()) {
            self.blend_hover = None;
        }
        if self.gizmo_source.as_ref().is_some_and(|source| !source.observes(current))
            || self.primitive_drag.as_ref().is_some_and(|source| !source.observes(self, current))
            || self.selection_gesture_stale(current)
            || self.pen_source_stale(current)
        {
            ctx.request_repaint();
            return;
        }
        if self.sampled_publication_pending(current) {
            ctx.request_repaint();
            return;
        }
        self.sync_workspace_stroke(current, ctx);
        if self.parameter_gesture.as_ref().is_some_and(|edit| {
            edit.engine.as_ptr() != Arc::as_ptr(&self.shared)
                || edit.document != current.id()
                || edit.revision != current.revision()
        }) {
            self.finish_parameter(true);
            self.message = "Image changed during parameter edit · preview cancelled".into();
        }
        self.sync_free_transform(ctx, &current);
        if self.free_transform.as_ref().is_some_and(free_transform::Editor::edits_text_draft)
            && self.transient.is_empty()
            && self.native_source_preview.is_none()
        {
            return;
        }
        if self
            .filter_editor
            .as_ref()
            .is_some_and(|e| !e.observes(current))
            && !self.filter_apply_pending()
        {
            self.cancel_filters();
            self.message = "Filter preview cancelled · project changed".into();
        }
        if self
            .source_editor
            .as_ref()
            .is_some_and(|e| e.document != current.id() || e.revision != current.revision())
        {
            self.cancel_source();
        }
        if self.source_editor.as_mut().is_some_and(|editor| editor.finish_typing_preview(ctx)) {
            self.last_preview = None;
        }
        if self
            .geometry
            .as_ref()
            .is_some_and(|g| !g.observes(current))
            && !self.geometry_pending()
        {
            self.cancel_geometry();
        }
        if !self.refinement_pending()
            && (self
                .refinement
                .as_ref()
                .is_some_and(|r| r.document != current.id() || r.revision != current.revision())
                || (self.refinement.is_some()
                    && (!self.points.is_empty()
                        || self.gizmo_handle != 0
                        || self.layer_drag.is_some()
                        || self.eye_sweep.is_some()
                        || self.blend_hover.is_some()
                        || self
                            .transient
                            .iter()
                            .any(|c| c["op"] != "selection.refine" && c["op"] != "mask.refine"))))
        {
            self.cancel_refinement();
        }
        self.sync_solo(current);
        let current_owner = match snapshot {
            crate::engine::view::DocumentSnapshot::Native(document) => {
                self.resident_preview_stroke = None;
                self.request_native_canvas(ctx, &project, &document);
                return;
            }
            crate::engine::view::DocumentSnapshot::Resident(document) => document,
        };
        let current = current_owner.as_ref();
        let doc = current;
        self.refresh_sampled_preview(doc.into());
        let proposal_doc = proposal.and_then(|result| self.resolve_proposal_document(result));
        let preview_mask = self.mask && self.isolate && self.proposal_review.is_none();
        let time = ctx.input(|i| i.time);
        let display_edge = preview_quality::display_edge(self.view_rect, ctx.pixels_per_point());
        let mut live = self.live_key();
        let mut live_draft = false;
        if live.is_empty() && self.transient.len() == 1 && self.sampled_preview_commands().is_empty()
            && self.blend_hover.is_none() && !self.has_preview_stroke()
        {
            let target = if self.isolate { self.selected.as_str() } else { "" };
            let key = match self.transient[0]["op"].as_str() {
                Some("filter.add" | "filter.update") if self.source_editor.is_none() => self.filter_editor.as_ref()
                    .and_then(|editor| editor.pointer_preview_key(ctx, doc.into()))
                    .map(|key| format!("filter:{key:?}:{target}")),
                Some("source.add" | "source.update") if self.filter_editor.is_none() => self.source_editor.as_ref()
                    .and_then(|editor| editor.canvas_preview_key(ctx, doc.into()))
                    .map(|key| format!("canvas text:{key:?}:{target}")),
                _ => None,
            };
            if let Some(key) = key {
                live = key;
                live_draft = true;
            }
        }
        live_draft |= self.selection_drag_preview_key().is_some();
        let editing = self.proposal_review.is_none() && (!live.is_empty()
            || !self.sampled_preview_commands().is_empty()
            || !self.transient.is_empty()
            || self.blend_hover.is_some()
            || self.source_editor.is_some()
            || self.filter_editor.is_some()
            || self.geometry.is_some()
            || self.animation.progress(time) < 1.
            || self.animation.fading());
        let source_size = [doc.width, doc.height];
        let [width, height] = if editing && self.preview_quality.reduced() {
            let commands = self.transient.iter()
                .chain(self.sampled_preview_commands().iter().map(|command| &**command))
                .chain(self.parameter_gesture.iter().flat_map(|edit| &edit.commands));
            // The worker still handles invalid drafts and reports their errors.
            geometry::canvas_size(source_size, commands, crate::raster::CanvasLimits::Resident)
                .unwrap_or(source_size)
        } else {
            source_size
        };
        let reduced_edge = self.preview_quality.reduced_edge(width.max(height), display_edge, editing);
        let mut preview_variant = reduced_edge.map(|_| self.preview_variant());
        let full_edge = crate::raster::MAX_EDGE;
        let edge = self.preview_quality.edge(
            preview_variant.as_deref().unwrap_or_default().as_bytes(),
            time,
            reduced_edge,
            full_edge,
        );
        self.preview_quality.repaint_when_settled(ctx, time, edge != full_edge);
        let animation_key = self.animation.render_key(time);
        let mut make_variant = |app: &mut Self| {
            let variant = preview_variant.take().unwrap_or_else(|| app.preview_variant());
            format!("{variant};{animation_key};{edge}")
        };
        let mut variant = None;
        if live.is_empty() {
            let ai = self.animation.live_key();
            if !ai.is_empty() {
                live = format!("{ai}:{}:{}:{}", self.selected, self.mask, self.isolate);
            }
        }
        while let Ok(mut p) = self.preview_rx.try_recv() {
            if p.job != self.preview_job {
                continue;
            }
            if !p.coarse {
                self.pending = false;
            }
            if p.job
                != self
                    .preview_epoch
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                self.last_preview = None;
                continue;
            }
            let parameter_matches = match &p.parameter {
                Some(request) => self
                    .parameter_gesture
                    .as_ref()
                    .is_some_and(|edit| request.matches(edit, &self.shared, doc)),
                None => self.parameter_gesture.is_none(),
            };
            if parameter_matches
                && p.doc_id == doc.id
                && p.revision == doc.revision
                && p.parameter.is_some()
                && !p.coarse
            {
                #[cfg(test)]
                if p.parameter_replayed {
                    self.parameter_gesture.as_mut().unwrap().preview_runs += 1;
                }
                if let Some(preview) = p.parameter_document.take() {
                    match preview {
                        Ok(preview) => {
                            self.parameter_gesture.as_mut().unwrap().preview = Some(preview);
                        }
                        Err(error) => {
                            self.finish_parameter(true);
                            self.message = error.into();
                            ctx.request_repaint();
                            return;
                        }
                    }
                }
            }
            if !parameter_matches || p.doc_id != doc.id || p.revision != doc.revision {
                self.last_preview = None;
                continue;
            }
            let variant = variant.get_or_insert_with(|| make_variant(self));
            if (p.target == *variant || (!live.is_empty() && p.live == live))
                && p.target.rsplit(';').next() == Some(edge.to_string().as_str())
                && (p.parameter.is_none() || p.target == *variant)
                && p.mask == preview_mask
                && (self.free_transform.is_none() || (p.target == *variant && p.live == live))
                && ((self.source_editor.is_none() && self.filter_editor.is_none())
                    || p.live == live)
            {
                if let Some(patch) = p.patch {
                    if self.animation.active(time)
                        || self.animation.fading()
                        || !self.apply_canvas_patch(&p, patch)
                    {
                        self.canvas_preview_job = None;
                        self.last_preview = None;
                        continue;
                    }
                }
                let has_pixels = !p.bytes.is_empty();
                if has_pixels
                    && p.patch.is_none()
                    && !self.animation.fade_preview(self.canvas_pixels.as_ref(), &p, time)
                {
                    if !self.set_canvas(ctx, p.w, p.h, std::mem::take(&mut p.bytes)) {
                        self.canvas_preview = None;
                        continue;
                    }
                    self.canvas_preview_job = (!p.coarse).then_some(p.job);
                }
                if let Some(editor) = &mut self.free_transform {
                    editor.error = p.error.clone();
                    editor.rendered = if edge == full_edge && !p.coarse && p.error.is_none() && has_pixels {
                        editor.key().ok()
                    } else {
                        None
                    };
                }
                if let Some(editor) = &mut self.geometry {
                    editor.error = p.error.clone();
                }
                if let Some(editor) = &mut self.refinement {
                    editor.error = p.error.clone();
                }
                if let Some(editor) = &mut self.source_editor {
                    if p.target == *variant {
                        if editor.error != p.error {
                            ctx.request_repaint();
                        }
                        editor.error = p.error.clone();
                    }
                    if let Some((signature, layout)) = p.text.take() {
                        editor.accept_resident_carets(signature, layout);
                    }
                }
                if let Some(editor) = self.filter_editor.as_mut().filter(|_| p.target == *variant) {
                    editor.error = p.error.clone();
                }
                if self.proposal_review.is_some() {
                    self.proposal_error = p.error.clone();
                    if p.error.is_none() && has_pixels {
                        self.proposal_rendered = Some(p.live.clone());
                    }
                }
                if has_pixels {
                    self.canvas_preview = (!p.coarse && !live_draft && edge == full_edge)
                        .then(|| (p.doc_id.clone(), p.revision, p.target.clone(), p.mask));
                    self.canvas_selection = Some(CanvasSelection {
                        document: p.doc_id.clone(),
                        revision: p.revision,
                        bounds: p.selection,
                        polygon: p.selection_polygon.clone(),
                        coverage: p.selection_coverage.clone(),
                    });
                }
                if p.revision != doc.revision {
                    self.last_preview = None;
                }
            } else {
                self.last_preview = None;
            }
        }
        if let Some((w, h, bytes)) = self.animation.fade_pixels(time) {
            if !self.set_canvas(ctx, w, h, bytes) {
                self.canvas_preview = None;
                self.last_preview = None;
            }
        }
        if self.animation.fading() || self.animation.progress(time) < 1.0 {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        // A compatible in-flight frame can still show progress during a live
        // gesture. A new source or sampling grid supersedes it immediately.
        if self.pending
            && self.last_preview.as_ref().is_some_and(|(id, revision, variant, mask)| {
                id == &doc.id && *revision == doc.revision && *mask == preview_mask
                    && variant.rsplit(';').next().and_then(|value| value.parse::<u32>().ok()) == Some(edge)
            })
        {
            return;
        }
        if variant.is_none()
            && animation_key.is_empty()
            && edge == full_edge
            && self.resident_base_preview_target().is_some_and(|target| {
                self.last_preview.as_ref().is_some_and(|(id, revision, variant, mask)| {
                    id == &doc.id && *revision == doc.revision && *mask == preview_mask
                        && variant.strip_prefix("base:")
                            .and_then(|rest| rest.strip_prefix(target))
                            .and_then(|rest| rest.strip_prefix(";;"))
                            .is_some_and(|old_edge| old_edge.parse::<u32>() == Ok(edge))
                })
            })
        {
            self.resident_preview_stroke = None;
            return;
        }
        let variant = variant.unwrap_or_else(|| make_variant(self));
        if !self.last_preview.as_ref().is_some_and(|(id, revision, target, mask)| {
            id == &doc.id && *revision == doc.revision && target == &variant && *mask == preview_mask
        }) {
            self.pending = true;
            self.last_preview = Some((doc.id.clone(), doc.revision, variant.clone(), preview_mask));
            let job = self
                .preview_epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .wrapping_add(1);
            self.preview_job = job;
            let epoch = self.preview_epoch.clone();
            let parameter = self
                .parameter_gesture
                .as_ref()
                .map(|edit| ParameterRequest {
                    engine: edit.engine.clone(),
                    document: edit.document.clone(),
                    revision: edit.revision,
                    gesture: edit.gesture.clone(),
                    commands: edit.commands.clone(),
                });
            let mut doc = proposal_doc.unwrap_or_else(|| {
                if parameter.is_some() || self.proposal_review.is_some() {
                    current_owner.clone()
                } else {
                    match self.animation.document(doc, time, true) {
                        std::borrow::Cow::Borrowed(_) => current_owner.clone(),
                        std::borrow::Cow::Owned(document) => Arc::new(document),
                    }
                }
            });
            let source_revision = current.revision;
            let mut commands = self.transient.clone();
            commands.extend(self.sampled_preview_commands().iter().map(|command| (**command).clone()));
            if let Some(edit) = &self.parameter_gesture {
                commands.extend(edit.commands.clone());
            }
            if let Some(hover) = &self.blend_hover {
                commands.extend(hover.commands());
            }
            let stroke = self.resident_preview_stroke.clone();
            if self.proposal_review.is_some() {
                commands.clear();
            }
            let selection_drag = self.selection_drag_preview_key().is_some();
            let selection_base = selection_drag
                && self.refinement.is_none() && parameter.is_none() && stroke.is_none()
                && commands.len() == 1 && Arc::ptr_eq(&doc, &current_owner);
            // Only a pure parameter result can be reused by either parameter panel.
            // Mixed tool/hover/transient previews keep their existing command order.
            let parameter_only = stroke.is_none() && parameter.as_ref().is_some_and(|p| p.commands == commands);
            if parameter_only {
                if let Some(preview) = self
                    .parameter_gesture
                    .as_ref()
                    .and_then(|e| e.preview.as_ref())
                {
                    doc = preview.clone();
                    commands.clear();
                }
            }
            #[cfg(test)]
            let parameter_replayed = parameter.is_some() && (!commands.is_empty() || stroke.is_some());
            #[cfg(test)]
            let worker_gate = self
                .parameter_worker_gate
                .clone()
                .filter(|_| parameter_replayed || !self.points.is_empty());
            #[cfg(test)]
            let worker_runs = self.parameter_worker_runs.clone();
            let cache = self.preview_cache.clone();
            let tx = self.preview_tx.clone();
            let ctx = ctx.clone();
            let layer = if self.isolate && self.proposal_review.is_none() {
                Some(self.selected.clone())
            } else {
                None
            };
            let mask = self.mask && self.isolate && self.proposal_review.is_none();
            let projection = refinement::preview_projection(self.refinement.as_ref(),
                self.parameter_gesture.as_ref(), selection_drag);
            let (refinement_mask, selection_view) = match projection {
                Some(refinement::PreviewProjection::Selection(mode)) => (None, Some(mode.to_owned())),
                Some(refinement::PreviewProjection::LayerMask(layer)) => (Some(layer.to_owned()), None),
                None => (None, None),
            };
            let presented = self.canvas_preview_job.filter(|_| {
                self.canvas_pixels.is_some()
                    && self.texture.is_some()
                    && !self.animation.active(time)
                    && !self.animation.fading()
            });
            let failure_context = ctx.clone();
            let request_context = ctx.clone();
            let failure_tx = tx.clone();
            let failure_message =
                "Canvas preview could not complete; current work preserved".to_owned();
            let text_request = self.source_editor.as_ref().and_then(source::Editor::resident_caret_request);
            let failure_reply = Preview {
                job,
                parameter: parameter.clone(),
                parameter_document: parameter_only.then(|| Err(failure_message.clone())),
                #[cfg(test)]
                parameter_replayed: false,
                coarse: false,
                error: Some(failure_message),
                live: live.clone(),
                revision: source_revision,
                doc_id: doc.id.clone(),
                w: 0,
                h: 0,
                bytes: Vec::new(),
                patch: None,
                selection: None,
                selection_polygon: None,
                selection_coverage: None,
                text: None,
                target: variant.clone(),
                mask,
            };
            let requested = self.preview_worker.request(
                move || {
                    let id = doc.id.clone();
                    let revision = source_revision;
                    let mut parameter_document = None;
                    let mut text = None;
                    let check = || {
                        if epoch.load(std::sync::atomic::Ordering::Relaxed) == job {
                            Ok(())
                        } else {
                            Err("Preview superseded".to_string())
                        }
                    };
                    let output = (|| {
                        #[cfg(test)]
                        if let Some(gate) = &worker_gate {
                            let (lock, ready) = &**gate;
                            let mut released = lock.lock().unwrap();
                            while !*released {
                                check()?;
                                released = ready
                                    .wait_timeout(released, Duration::from_millis(25))
                                    .unwrap()
                                    .0;
                            }
                        }
                        check()?;
                        if let Some(stroke) = stroke {
                            commands.push(stroke.command());
                        }
                        check()?;
                        let selection_base = selection_base.then(|| doc.clone());
                        let mut dirty = None;
                        // A replay delta belongs to these retained pixels. Keep
                        // worker replay/render together across project switches.
                        let mut cache = cache
                            .lock()
                            .map_err(|_| "Preview worker cache unavailable")?;
                        check()?;
                        let doc = if !commands.is_empty() || doc.revision != revision {
                            #[cfg(test)]
                            if parameter_replayed {
                                worker_runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            let mut draft = Arc::unwrap_or_clone(doc);
                            draft.revision = revision;
                            if !commands.is_empty() {
                                (draft, dirty) = cache.edit_with_region(draft, &commands, &live)?;
                            }
                            Arc::new(draft)
                        } else {
                            doc
                        };
                        check()?;
                        if parameter_only {
                            parameter_document = Some(doc.clone());
                        }
                        let mut rendered = if let (Some(base), Some(mode)) = (&selection_base, &selection_view) {
                            cache.render_selection_canvas(base, &doc, &live, edge, layer.as_deref(), mask, mode, &check)?
                        } else if let Some(target) = &refinement_mask {
                            let (width, height, bytes, _) = doc.preview_checked(None, edge, Some(target), true, &check)?;
                            crate::preview::CanvasFrame { width, height, bytes, patch: None }
                        } else if selection_view.is_none() {
                            cache.render_canvas(
                                &doc, &live, dirty, edge, layer.as_deref(), mask, presented, job, &check,
                            )?
                        } else {
                            let frame = cache.render_checked(
                                &doc, &live, dirty, edge, layer.as_deref(), mask, &check,
                            )?;
                            crate::preview::CanvasFrame {
                                width: frame.width,
                                height: frame.height,
                                bytes: frame.bytes,
                                patch: None,
                            }
                        };
                        if let Some(mode) = selection_view.as_ref().filter(|_| selection_base.is_none()) {
                            crate::selection::display::apply_region_checked(
                                &doc, rendered.width, rendered.height, &mut rendered.bytes,
                                [0, 0, doc.width as i32, doc.height as i32], mode, &check,
                            )?;
                        }
                        check()?;
                        if let Some(signature) = text_request.filter(|_| commands.len() == 1) {
                            let layout = crate::source::parse(&commands[0]).ok().and_then(|source| {
                                crate::source::text_position::TextCarets::resident(&source).ok()
                            });
                            check()?;
                            text = Some((signature, layout));
                        }
                        Ok::<_, String>((
                            rendered.width,
                            rendered.height,
                            rendered.bytes,
                            rendered.patch,
                            doc.selection,
                            doc.selection_polygon.clone(),
                            doc.selection_coverage.clone(),
                        ))
                    })();
                    let error = output.as_ref().err().cloned();
                    let (w, h, bytes, patch, selection, selection_polygon, selection_coverage) =
                        output.unwrap_or((0, 0, vec![], None, None, None, None));
                    let _ = tx.send(Preview {
                        job,
                        parameter,
                        parameter_document: if parameter_only {
                            parameter_document
                                .map(Ok)
                                .filter(|_| error.is_none())
                                .or_else(|| error.as_ref().map(|error| Err(error.clone())))
                        } else {
                            None
                        },
                        #[cfg(test)]
                        parameter_replayed,
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
                        patch,
                        selection,
                        selection_polygon,
                        selection_coverage,
                        text,
                    });
                    ctx.request_repaint();
                },
                move || {
                    let _ = failure_tx.send(failure_reply);
                    failure_context.request_repaint();
                },
            );
            if let Err(error) = requested {
                self.pending = false;
                self.last_preview = None;
                self.message = format!(
                    "Canvas preview worker could not start: {error}; current work preserved"
                )
                .into();
                request_context.request_repaint();
            }
        }
    }
    fn thumbnail<'a>(
        &mut self,
        ui: &egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        id: &str,
        mask: bool,
    ) -> Option<TextureHandle> {
        let doc = doc.into();
        if let Some(native) = doc.native() {
            match self.layer_thumbnails.get_layer(
                &self.shared,
                &self.project_id,
                native,
                id,
                mask,
                ui.ctx(),
            ) {
                Ok(texture) => texture,
                Err(error) => {
                    self.message = error.into();
                    None
                }
            }
        } else {
            self.thumbnail_for(ui.ctx(), doc.resident().unwrap(), id, mask, None)
        }
    }
    fn thumbnail_for(
        &mut self,
        ctx: &egui::Context,
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
        // Reuse the committed frame owner across thumbnail requests for this revision.
        if let DocumentSnapshot::Resident(source) = &self.doc_snapshot {
            if !ready && self.thumb_pending.get(&key) != Some(&doc.revision)
                && source.id == doc.id && source.revision == doc.revision
            {
                let worker = self.thumb_worker.get_or_insert_with(|| {
                    let worker = thumbnails::Worker::new(ctx.clone());
                    worker.document(source);
                    worker
                });
                if worker.request(thumbnails::Request {
                    key: key.clone(),
                    document: source.clone(),
                    layer: id.into(),
                    mask,
                    step,
                }) {
                    self.thumb_pending.insert(key.clone(), doc.revision);
                }
            }
        }
        self.thumbs.get(&key).map(|(_, texture)| texture.clone())
    }
    fn layer_menu<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        l: impl Into<LayerView<'a>>,
        reserved: bool,
    ) {
        let doc = doc.into();
        let l = l.into();
        if self.layer_menu_contents(ui, doc, l, reserved) {
            if l.source_kind() == Some("text") {
                self.select_tool(Tool::Text);
            }
            self.open_source(doc, "edit");
        }
    }
    fn layer_menu_contents<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        l: impl Into<LayerView<'a>>,
        reserved: bool,
    ) -> bool {
        let doc = doc.into();
        let l = l.into();
        let mut edit_source = false;
        if ui
            .add_enabled(
                !self.busy && !doc.read_only(),
                egui::Button::new("Group layers").shortcut_text(if cfg!(target_os = "macos") {
                    "⌘ G"
                } else {
                    "Ctrl+G"
                }),
            )
            .clicked()
        {
            if !self.selection_layers.contains(l.id()) {
                self.select_content(l.id());
            }
            self.create_folder(doc);
            ui.close_menu();
        }
        if ui
            .add_enabled(
                !self.busy && !doc.read_only(),
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
            if !self.selection_layers.contains(l.id()) {
                self.select_content(l.id());
            }
            self.merge_layers(ui.ctx(), doc);
            ui.close_menu();
        }
        ui.separator();
        for (title, op, extra) in [
            ("Rename", "rename", json!({})),
            ("Duplicate", "layer.duplicate", json!({})),
            (
                if l.locked() { "Unlock" } else { "Lock" },
                "layer.update",
                json!({"locked":!l.locked()}),
            ),
            ("Add mask", "mask.add", json!({"value":255})),
        ] {
            if ui
                .add_enabled(
                    !(op == "layer.duplicate" && l.kind() == "group")
                        && !(op == "mask.add" && l.mask().is_some()),
                    egui::Button::new(title),
                )
                .clicked()
            {
                self.selected = l.id().to_owned();
                if op == "rename" {
                    self.begin_rename(doc, l.id());
                } else {
                    let extra = if op == "mask.add" {
                        json!({"value":255,"document_id":doc.id(),"source_revision":doc.revision()})
                    } else {
                        extra
                    };
                    self.layer_cmd(op, extra, title);
                }
                ui.close_menu();
            }
        }
        if l.mask().is_some() && ui.button("Remove mask").clicked() {
            self.selected = l.id().to_owned();
            self.layer_cmd("mask.remove", json!({}), "Remove mask");
            self.mask = false;
            ui.close_menu();
        }
        let source_action = if l.source_kind() == Some("text") { "Edit text on canvas" } else { "Edit vector properties…" };
        if l.source().is_some() && ui.button(source_action).clicked() {
            self.select_content(l.id());
            edit_source = true;
            ui.close_menu();
        }
        ui.add_enabled_ui(
            !doc.read_only() && !self.busy && !l.locked() && !reserved,
            |ui| {
                ui.menu_button("Color label", |ui| {
                    for &label in crate::psd::LAYER_COLOR_LABELS {
                        let title = format!("{}{}", label[..1].to_ascii_uppercase(), &label[1..]);
                        let option = match layers::label_color(label) {
                            Some(color) => RichText::new(format!("●  {title}")).color(color),
                            None => RichText::new(format!("○  {title}")),
                        };
                        let selected = l.color_label() == label;
                        let response = ui.selectable_label(selected, option);
                        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), selected, &title));
                        if response.clicked() {
                            self.edit(vec![json!({"op":"layer.update", "layer":l.id(), "color_label":label,
                                "document_id":doc.id(), "source_revision":doc.revision()})], "Layer color label");
                            ui.close_menu();
                        }
                    }
                });
            },
        );
        ui.separator();
        if l.parent().is_some() && ui.button("Move out of group").clicked() {
            self.selected = l.id().to_owned();
            self.layer_cmd("layer.parent", json!({"parent":null}), "Move out of group");
            ui.close_menu();
        }
        ui.menu_button("Move into group", |ui| {
            for group in doc
                .layers()
                .filter(|g| g.kind() == "group" && g.id() != l.id())
            {
                if ui.button(group.name()).clicked() {
                    self.selected = l.id().to_owned();
                    self.layer_cmd(
                        "layer.parent",
                        json!({"parent":group.id()}),
                        "Move into group",
                    );
                    ui.close_menu();
                }
            }
        });
        if ui
            .button(if l.clip_to().is_some() {
                "Release clipping"
            } else {
                "Clip to layer below"
            })
            .clicked()
        {
            let base = if l.clip_to().is_some() {
                None
            } else {
                doc.layers()
                    .skip_while(|b| b.id() != l.id())
                    .skip(1)
                    .find(|b| b.parent() == l.parent())
                    .map(|b| b.clip_to().unwrap_or_else(|| b.id()).to_owned())
            };
            self.selected = l.id().to_owned();
            self.layer_cmd("layer.clip", json!({"base":base}), "Layer clipping");
            ui.close_menu();
        }
        let mut grouped = l.blend_clipped_as_group();
        if (!grouped || crate::psd::clipping::supports_disabled(
            l.kind(), l.blend(), l.effect_count() != 0, l.fill_opacity(),
        ) && doc.layers().any(|layer| layer.clip_to() == Some(l.id())))
            && ui.checkbox(&mut grouped, "Blend Clipped Layers As Group").changed()
        {
            self.edit(vec![json!({
                "op":"layer.update", "layer":l.id(), "blend_clipped_as_group":grouped,
            })], "Clipping group blending");
            ui.close_menu();
        }
        let clip_base = doc.layers().any(|layer| layer.clip_to() == Some(l.id()));
        let unsupported_base = clip_base && !crate::psd::blend_channels::supports_clip_base(
            l.kind(), (0..l.effect_count()).filter_map(|i| l.effect(i).map(|effect| effect.kind())),
            l.blend_clipped_as_group(),
        );
        if crate::psd::blend_channels::supported(
            l.kind(), l.blend(), (0..l.effect_count()).filter_map(|i| l.effect(i).map(|effect| effect.kind())),
            unsupported_base,
        )
            && l.clip_to().is_none_or(|id| doc.find_layer(id).is_some_and(|base|
                crate::psd::blend_channels::supports_clipped_member(
                    l.kind(), base.kind(),
                    (0..base.effect_count()).filter_map(|i| base.effect(i).map(|effect| effect.kind())),
                    base.blend_clipped_as_group(),
                )
            ))
        {
            ui.menu_button("Blend channels", |ui| {
                let enabled = l.blend_channels();
                let can_edit = ui.is_enabled() && !doc.read_only() && !self.busy && !l.locked() && !reserved;
                let mut hovered = None;
                let mut focused = None;
                ui.label("Off channels keep the backdrop color.");
                for (channel, ((name, _), color)) in controls::RGB_CHANNELS
                    .into_iter()
                    .zip(controls::RGB_COLORS)
                    .enumerate()
                {
                    let response = ui.add_enabled_ui(can_edit, |ui| {
                        controls::blend_channel(ui, name, color, enabled[channel])
                    }).inner;
                    if response.hovered() {
                        hovered = Some(channel);
                    }
                    if response.has_focus() {
                        focused = Some(channel);
                    }
                    if response.clicked() {
                        let mut next = enabled;
                        next[channel] = !next[channel];
                        if !self.edit_worker.pending() {
                            self.cancel_free_transform();
                            self.close_proposal();
                        }
                        self.apply_action(vec![json!({"op":"layer.update", "layer":l.id(), "blend_channels":next})],
                            "Blend channels", Some((doc.id(), doc.revision())), edit_job::Origin::Action);
                        ui.close_menu();
                        return;
                    }
                }
                if let Some(channel) = hovered.or(focused).filter(|_| can_edit) {
                    let mut next = enabled;
                    next[channel] = !next[channel];
                    self.blend_hover = Some(BlendHover {
                        layers: vec![l.id().to_owned()],
                        setting: BlendSetting::Channels(next),
                    });
                }
            });
        }
        ui.separator();
        if ui
            .button(RichText::new("Delete layer").color(Color32::from_rgb(255, 154, 144)))
            .clicked()
        {
            self.selected = l.id().to_owned();
            self.layer_cmd("layer.delete", json!({}), "Delete layer");
            ui.close_menu();
        }
        edit_source
    }
    fn layer_footer<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        l: impl Into<LayerView<'a>>,
    ) {
        let doc = doc.into();
        let l = l.into();
        let numbers = self.animation.controls(doc, ui.input(|i| i.time));
        let unsupported_base = !crate::psd::blend_ranges::supports_clip_base(
            l.kind(), l.blend(), l.effect_count() != 0, l.blend_clipped_as_group(), l.fill_opacity(),
        ) && doc.layers().any(|layer| layer.clip_to() == Some(l.id()));
        let blend_if = crate::psd::blend_ranges::supported(
            l.kind(), l.blend(), (0..l.effect_count()).filter_map(|i| l.effect(i).map(|effect| effect.enabled())),
            l.clip_to().is_some(), unsupported_base,
        )
            && l.clip_to().is_none_or(|id| doc.find_layer(id).is_some_and(|base| {
                crate::psd::clipping::supports_advanced_member(
                    l.kind(), base.kind(), base.effect_count() != 0, base.blend_clipped_as_group(),
                )
            }));
        ui.horizontal(|ui| {
            let blend = l.blend();
            let mut preview = None;
            let mut keyboard_preview = None;
            let combo = egui::ComboBox::from_id_salt("blend")
                .selected_text(
                    crate::raster::layer_blends(l.kind())
                        .find(|(mode, _)| *mode == blend)
                        .map(|(_, label)| label)
                        .unwrap_or("Normal"),
                )
                .width(ui.available_width() - if blend_if { 82.0 } else { 10.0 })
                .show_ui(ui, |ui| {
                    let selected = self.selection_layers.contains(&self.selected);
                    let targets = doc.layers()
                        .filter(|layer| if selected { self.selection_layers.contains(layer.id()) }
                            else { layer.id() == self.selected })
                        .take(101)
                        .collect::<Vec<_>>();
                    let same_targets = !targets.is_empty() && targets.len() <= 100
                        && !doc.read_only() && !self.busy && l.id() == self.selected;
                    if targets.len() > 100 {
                        ui.label("Select up to 100 layers to change their blend mode.");
                    }
                    for (name, label) in crate::raster::layer_blends(l.kind()) {
                        let supported = same_targets && (name != "pass_through"
                            || targets.iter().all(|layer| layer.kind() == "group"));
                        let response = controls::blend_choice(ui, name, label, blend == name, supported);
                        if !supported {
                            if name == "pass_through" && same_targets {
                                response.on_disabled_hover_text("Pass Through applies to folders.");
                            }
                            continue;
                        }
                        if response.hovered() {
                            preview = Some(name);
                        }
                        if response.has_focus() {
                            keyboard_preview = Some(name);
                        }
                        if response.clicked() {
                            ui.close_menu();
                            let blend = BlendHover {
                                layers: targets.iter().map(|layer| layer.id().to_owned()).collect(),
                                setting: BlendSetting::Mode(name),
                            };
                            if !self.edit_worker.pending() {
                                self.cancel_free_transform();
                                self.close_proposal();
                            }
                            self.apply_action(blend.commands().collect(), "Layer blend",
                                Some((doc.id(), doc.revision())), edit_job::Origin::Action);
                            return;
                        }
                    }
                    if let Some(mode) = preview.or(keyboard_preview) {
                        if targets.iter().any(|layer| layer.blend() != mode) {
                            self.blend_hover = Some(BlendHover {
                                layers: targets.iter().map(|layer| layer.id().to_owned()).collect(),
                                setting: BlendSetting::Mode(mode),
                            });
                        }
                    }
                });
            self.blend_rect = Some(combo.response.rect);
            if blend_if {
                ui.menu_button("Blend If", |ui| {
                    let mut pairs = l.blend_if().unwrap_or([crate::psd::blend_ranges::DEFAULT; 4]);
                    let pending = self.parameter_gesture.as_ref().and_then(|edit| {
                        let command = edit.commands.first()?;
                        if edit.engine.as_ptr() != Arc::as_ptr(&self.shared)
                            || edit.document != doc.id() || edit.revision != doc.revision()
                            || command["layer"].as_str() != Some(l.id())
                        {
                            return None;
                        }
                        serde_json::from_value::<[[u8; 8]; 4]>(command.get("blend_if")?.clone()).ok()
                    });
                    if let Some(pending) = pending {
                        pairs = pending;
                    }
                    let channel_id = ui.id().with("channel");
                    let mut channel = ui.ctx().data(|data| data.get_temp::<usize>(channel_id)).unwrap_or(0).min(3);
                    let _ = controls::swatch_choices(
                        ui,
                        "Channel",
                        &mut channel,
                        std::iter::once(("Gray", controls::GRAYSCALE)).chain(controls::RGB_CHANNELS),
                    );
                    ui.ctx().data_mut(|data| data.insert_temp(channel_id, channel));
                    for (offset, title) in [(0, "This Layer"), (4, "Underlying Layer")] {
                        ui.label(title);
                        let id = ui.make_persistent_id(("blend_if ramp", l.id(), channel, offset));
                        let mut endpoints = std::array::from_fn(|i| pairs[channel][offset + i]);
                        let displayed = std::array::from_fn(|i| numbers.blend_if(l, channel, offset + i, endpoints[i]));
                        if let Some(response) = layers::blend_range(ui, id, channel, &mut endpoints, displayed) {
                            pairs[channel][offset..offset + 4].copy_from_slice(&endpoints);
                            self.layer_parameter(json!({"layer":l.id(), "blend_if":pairs}), "Blend If", &response);
                        }
                        egui::CollapsingHeader::new("Exact values").id_salt(id).show(ui, |ui| {
                            for (index, label) in layers::BLEND_ENDPOINTS.into_iter().enumerate() {
                                let endpoint = offset + index;
                                let low = if index == 0 { 0 } else { pairs[channel][endpoint - 1] };
                                let high = if index == 3 { 255 } else { pairs[channel][endpoint + 1] };
                                let mut value = numbers.blend_if(l, channel, endpoint, pairs[channel][endpoint]);
                                ui.horizontal(|ui| {
                                    ui.label(label);
                                    let response = controls::range(
                                        ui, ("blend_if", l.id(), channel, endpoint), &mut value,
                                        f32::from(low)..=f32::from(high), 100., "", 0, false,
                                    );
                                    if response.changed() || response.double_clicked() {
                                        pairs[channel][endpoint] = (value.round() as u8).clamp(low, high);
                                        self.layer_parameter(json!({"layer":l.id(), "blend_if":pairs}), "Blend If", &response);
                                    }
                                });
                            }
                        });
                    }
                    if l.blend_if().is_some() && ui.button("Reset").clicked() {
                        self.edit(vec![json!({"op":"layer.update", "layer":l.id(), "blend_if":null})], "Reset Blend If");
                        ui.close_menu();
                    }
                });
            }
        });
        ui.horizontal(|ui| {
            let label = ui.label(RichText::new("Opacity").size(12.0).color(MUTED));
            let mut percent = numbers.opacity(l) * 100.0;
            let width = (ui.available_width() - 3.0).max(60.0);
            let response = controls::range(
                ui,
                ("opacity", l.id()),
                &mut percent,
                0.0..=100.0,
                width,
                "%",
                0,
                false,
            ).labelled_by(label.id);
            self.opacity_rect = Some(response.rect);
            if response.changed() || response.double_clicked() {
                self.layer_parameter(json!({"opacity":percent/100.0}), "Layer opacity", &response);
            }
        });
        if crate::psd::fill_opacity::supported(
            l.kind(), l.blend(), (0..l.effect_count()).filter_map(|i| l.effect(i).map(|effect| effect.enabled())),
        ) {
            ui.horizontal(|ui| {
                let label = ui.label(RichText::new("Fill").size(12.0).color(MUTED));
                let mut percent = numbers.fill_opacity(l) * (100. / 255.);
                let width = (ui.available_width() - 3.0).max(60.0);
                let response = controls::range(
                    ui, ("fill_opacity", l.id()), &mut percent, 0.0..=100.0, width, "%", 0, false,
                ).labelled_by(label.id);
                if response.changed() || response.double_clicked() {
                    let fill = (percent * (255. / 100.)).round() as u8;
                    self.layer_parameter(json!({"layer":l.id(),"fill_opacity":fill}), "Layer fill opacity", &response);
                }
            });
        }
    }
    fn add_selected_masks<'a>(&mut self, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        let mut ids = self.selected_layer_ids(doc);
        if !ids.contains(&self.selected) {
            ids.push(self.selected.clone());
        }
        let commands: Vec<_> = doc.layers()
            .filter(|layer| ids.iter().any(|id|id==layer.id()) && layer.mask().is_none())
            .map(|layer| json!({"op":"mask.add","layer":layer.id(),"value":255,"document_id":doc.id(),"source_revision":doc.revision()}))
            .collect();
        self.apply_action(
            commands,
            "Add masks",
            Some((doc.id(), doc.revision())),
            edit_job::Origin::Masks,
        );
    }
    fn channel_tabs<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        has_mask: bool,
    ) {
        let doc = doc.into();
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
            self.select_channel(false);
        }
        let mask = ui.add_enabled(
            has_mask || !doc.read_only(),
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
                self.select_channel(true);
            } else {
                self.add_selected_masks(doc);
            }
        }
        mask.on_hover_text(if has_mask {
            "Edit mask"
        } else if doc.selection().is_some() {
            "Create masks from the selection for selected layers that do not have one"
        } else {
            "Create white masks for selected layers that do not have one"
        });
    }

    fn layers<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        reserved: &HashSet<&str>,
    ) {
        let doc = doc.into();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Layers").size(15.0).strong());
            ui.label(
                RichText::new(doc.layer_count().to_string())
                    .size(11.0)
                    .color(MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let folder = ui
                    .add_enabled_ui(!self.busy && !doc.read_only(), |ui| {
                        icons::button(ui, Icon::Folder, "Group selected layers · Ctrl/Cmd+G")
                    })
                    .inner;
                self.new_folder_rect = Some(folder.rect);
                if folder.clicked() {
                    self.create_folder(doc);
                }
                let adjustment = egui::menu::menu_custom_button(
                    ui,
                    egui::Button::new("").min_size(Vec2::splat(30.)),
                    |ui| {
                        for (kind, label) in [
                            ("color_balance", "Color balance"),
                            (crate::effects::photoshop_color_balance::KIND, "Color Balance (Photoshop)"),
                            ("hsl", "Hue / saturation"),
                            (crate::effects::photoshop_hue_saturation::KIND, "Hue/Saturation (Photoshop)"),
                            (crate::effects::color_lookup::KIND, "Color Lookup"),
                            ("levels", "Levels"),
                            ("curves", "Curves"),
                            ("posterize", "Posterize"),
                            ("threshold", "Threshold"),
                            (crate::effects::modern_brightness::KIND, "Brightness/Contrast"),
                            (crate::effects::exposure::KIND, "Exposure"),
                            (crate::effects::channel_mixer::KIND, "Channel Mixer"),
                            (crate::effects::black_white::KIND, "Black & White"),
                            (crate::effects::selective_color::KIND, "Selective Color"),
                            (crate::effects::vibrance::KIND, "Vibrance (Legacy)"),
                            (crate::effects::photo_filter::KIND, "Photo Filter"),
                            (crate::effects::gradient_map::KIND, "Gradient Map"),
                            (crate::effects::legacy_brightness::KIND, "Legacy Brightness/Contrast"),
                        ] {
                            if effects::menu_effect(ui, kind, label, None).clicked() {
                                self.create_panel_layer(doc, Some((kind, label)));
                                ui.close_menu();
                            }
                        }
                    },
                )
                .response
                .on_hover_text("Add clipped adjustment · multiple layers become a shared color group");
                adjustment.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, adjustment.enabled(), "Add adjustment layer")
                });
                icons::paint(ui.painter(), adjustment.rect.shrink(4.), Icon::Adjust, adjustment.enabled());
                icons::paint_focus(ui, &adjustment);
                if icons::button(ui, Icon::AddLayer, "New paint layer").clicked() {
                    self.create_panel_layer(doc, None);
                }
            });
        });
        if let Some(caption) = self.solo_caption(doc) {
            ui.add(egui::Label::new(RichText::new(&caption).size(11.).color(ACCENT)).truncate())
                .on_hover_text(caption);
        }
        ui.add_space(12.0);
        let selected = doc.find_layer(&self.selected);
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        self.blend_hover = None;
        self.layer_list(ui, doc, reserved);
        ui.add_space(10.0);
        self.effect_add_rect = None;
        self.opacity_rect = None;
        self.blend_rect = None;
        self.color_tab_rect = None;
        self.mask_tab_rect = None;
        self.effect_area_rect = None;
        if let Some(l) = selected {
            if l.mask().is_none() {
                self.select_channel(false);
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
                        self.channel_tabs(ui, doc, l.mask().is_some());
                        if l.mask().is_some() {
                            self.mask_link_button(ui, doc, l, reserved.contains(l.id()));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .selectable_label(
                                    self.isolate,
                                    if self.isolate { "Exit solo" } else { "Solo" },
                                )
                                .on_hover_text(if self.isolate {
                                    "Return to the full document"
                                } else {
                                    "View only the selected Color or Mask target"
                                })
                                .clicked()
                            {
                                self.set_solo(!self.isolate);
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);
                    if l.kind() == "fill" && !self.mask {
                        let color = self.animation.controls(doc, ui.input(|i| i.time)).color(l);
                        ui.horizontal(|ui| {
                            controls::label(ui, "Fill color");
                            let mut rgb = [color[0], color[1], color[2]].map(|value| f64::from(value) / 255.);
                            let mut alpha = f64::from(color[3]) / 255.;
                            if let Some(response) =
                                effect_color::control(ui, (l.id(), "fill"), "Fill", &mut rgb, &mut alpha, Some("Alpha"), false)
                            {
                                let rgba = [rgb[0], rgb[1], rgb[2], alpha]
                                    .map(|value| (value.clamp(0., 1.) * 255.).round() as u8);
                                self.layer_parameter(
                                    json!({"layer":l.id(),"color":rgba}),
                                    "Fill color",
                                    &response,
                                );
                            }
                        });
                    }
                    if !self.mask {
                        self.color_stack(ui, l, doc);
                    } else if l.mask().is_some() {
                        self.mask_stack(ui, doc, l);
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
            self.layer_footer(ui, doc, l);
        }
    }
    fn cancel_gizmo(&mut self, message: &str) {
        if self.gizmo_handle == 0 {
            return;
        }
        self.gizmo_handle = 0;
        self.gizmo_source = None;
        self.drag_start = None;
        self.drag_revision = None;
        self.live_gesture = None;
        self.transient.clear();
        self.last_preview = None;
        self.canvas_selection = None;
        self.message = message.to_owned().into();
    }
    fn gizmo<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        rect: Rect,
        scale: f32,
        available: Rect,
        response: &egui::Response,
    ) {
        let doc = doc.into();
        let roots = crate::tree::roots_view(doc, &self.selection_layers);
        let (press, release) = ui.input(|input| canvas_button_samples(input, self.gizmo_handle != 0));
        let interrupted = Self::pointer_contact_cancelled(ui.ctx()) || ui.input(|input| {
            !input.focused
                || input.key_pressed(egui::Key::Escape)
                || input.key_down(egui::Key::Space)
                || (self.gizmo_handle != 0 && release.is_none() && !input.pointer.primary_down())
                || input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::WindowFocused(false)))
        });
        let stale = self.gizmo_source.as_ref().is_some_and(|source| {
            !source.observes(doc)
                || source.roots != roots
                || source.tool != self.tool
                || source.mask != self.mask
                || source.step != self.mask_step
        });
        if interrupted || stale {
            self.cancel_gizmo(if stale {
                "Transform source changed · preview cancelled; start a new drag"
            } else {
                "Transform gesture cancelled · start a new drag"
            });
            return;
        }
        let targets = doc
            .layers()
            .filter(|l| roots.iter().any(|id| id == l.id()))
            .collect::<Vec<_>>();
        let tree = crate::transform::tree_ids_view(doc, &roots);
        if targets.is_empty() {
            return;
        }
        let selection = doc.selection().filter(|_| !self.transform_entire_pixel_copy(doc));
        let b = match (selection, doc) {
            (Some(bounds), _) => bounds,
            (None, DocumentView::Resident(resident)) => {
                let key = targets.iter().map(|l| l.id()).collect::<Vec<_>>().join(";");
                if self
                    .gizmo_bounds
                    .as_ref()
                    .is_none_or(|(ids, rev, _)| *ids != key || *rev != doc.revision())
                {
                    let b = crate::transform::tree_bounds(resident, &roots).unwrap_or([
                        0,
                        0,
                        doc.width() as i32,
                        doc.height() as i32,
                    ]);
                    self.gizmo_bounds = Some((key, doc.revision(), b));
                }
                self.gizmo_bounds.as_ref().unwrap().2
            }
            (None, DocumentView::Native(native)) => match self.transform_bounds.get(
                &self.shared,
                &self.project_id,
                native,
                &roots,
                self.mask,
                self.mask_step.as_deref(),
                ui.ctx(),
            ) {
                Ok(Some(geometry)) => geometry.bounds,
                Ok(None) => return,
                Err(error) => {
                    self.message = error.into();
                    return;
                }
            },
        };
        let selection_only = selection.is_some();
        let pivot = [(b[0] + b[2]) as f32 / 2.0, (b[1] + b[3]) as f32 / 2.0];
        let view = self.view;
        let screen = |p: [f32; 2]| view.point(p, rect, scale);
        let center = screen(pivot);
        let xend = center + Vec2::new(65.0, 0.0);
        let yend = center - Vec2::new(0.0, 65.0);
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let hit = press.or(pointer)
            .filter(|&p| canvas_press_hits(ui, response, p))
            .map(|p| {
                let p = center + view.inverse(p - center);
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
        let xend = center + view.vector(xend - center);
        let yend = center + view.vector(yend - center);
        if hit != 0 {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if let Some(pos) = press.filter(|_| hit != 0 && doc
                .layers()
                .filter(|l| tree.iter().any(|id| id == l.id()))
                .all(|l| !l.locked()))
        {
            self.gizmo_handle = hit;
            self.live_gesture = Some(crate::engine::id());
            self.drag_start = Some(view.document(pos, rect, scale));
            self.drag_revision = Some(doc.revision());
            self.gizmo_source = Some(TransformGestureSource {
                document: doc.id().to_owned(),
                revision: doc.revision(),
                native: doc.native().map(|native| native.key()),
                roots: roots.clone(),
                tool: self.tool,
                mask: self.mask,
                step: self.mask_step.clone(),
            });
        }
        let mut dx = 0.0;
        let mut dy = 0.0;
        let mut angle = 0.0f32;
        let mut sx = 1.0f32;
        let mut sy = 1.0f32;
        let shift = release.map_or_else(|| ui.input(|i| i.modifiers.shift), |(_, modifiers)| modifiers.shift);
        if let (Some(a), Some(pos)) = (self.drag_start, release.map(|(pos, _)| pos).or(pointer)) {
            if self.gizmo_handle != 0 {
                let p = view.document(pos, rect, scale);
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
                } else {
                    let delta = view.inverse(pos - screen(a));
                    let amount = ((delta.x - delta.y) * 0.012).exp().clamp(0.05, 20.0);
                    if self.gizmo_handle != 2 {
                        sx = amount;
                    }
                    if self.gizmo_handle != 1 {
                        sy = amount;
                    }
                    if shift {
                        sx = amount;
                        sy = amount;
                    }
                }
            }
        }
        if self.gizmo_handle != 0 {
            for target in &targets {
                let command = if self.tool == Tool::Move {
                    json!({"op":"move","layer":target.id(),"dx":dx,"dy":dy,"snap":shift,"mask":self.mask,"step":if targets.len()==1 {self.mask_step.clone()}else{None},"selection_only":selection_only})
                } else {
                    json!({"op":"transform","layer":target.id(),"angle":angle,"scale_x":sx,"scale_y":sy,"snap":shift,"pivot":pivot,"mask":self.mask,"step":if targets.len()==1 {self.mask_step.clone()}else{None},"selection_only":selection_only})
                };
                match crate::transform::resolve_command(&command) {
                    Ok(mut command) => {
                        if let Some(source) = &self.gizmo_source {
                            command["document_id"] = json!(source.document);
                            command["source_revision"] = json!(source.revision);
                        }
                        if self.tool == Tool::Move {
                            dx = command["dx"].as_f64().unwrap() as f32;
                            dy = command["dy"].as_f64().unwrap() as f32;
                            dx = dx.round();
                            dy = dy.round();
                            command["dx"] = json!(dx);
                            command["dy"] = json!(dy);
                        } else {
                            angle = command["angle"].as_f64().unwrap() as f32;
                            sx = command["scale_x"].as_f64().unwrap() as f32;
                            sy = command["scale_y"].as_f64().unwrap() as f32;
                        }
                        self.transient.push(command);
                    }
                    Err(error) => {
                        self.message = error.into();
                        self.transient.clear();
                    }
                }
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
        if self.gizmo_handle != 0 {
            let value = match self.tool {
                Tool::Move => format!("X {dx:.0} · Y {dy:.0} px"),
                Tool::Rotate => format!("{angle:.0}°"),
                _ => format!("X {:.0}% · Y {:.0}%", sx * 100., sy * 100.),
            };
            let label = format!(
                "{value}{}",
                if shift {
                    " · Snap"
                } else {
                    ""
                }
            );
            let position = center + Vec2::new(0., 88.);
            painter.text(
                position + Vec2::splat(1.),
                egui::Align2::CENTER_TOP,
                &label,
                egui::FontId::proportional(12.),
                Color32::BLACK,
            );
            painter.text(
                position,
                egui::Align2::CENTER_TOP,
                label,
                egui::FontId::proportional(12.),
                ACCENT,
            );
        }
        if self.gizmo_handle != 0 && release.is_some() {
            let commands = std::mem::take(&mut self.transient);
            if let Some(source) = self.gizmo_source.take() {
                self.apply_action(
                    commands,
                    self.tool.label(),
                    Some((&source.document, source.revision)),
                    source.native.map_or(edit_job::Origin::Action, edit_job::Origin::CanvasGesture),
                );
            } else {
                self.message = "Transform source changed; start a new drag".into();
            }
            self.last_preview = None;
            self.drag_start = None;
            self.gizmo_handle = 0;
            self.gizmo_source = None;
            self.drag_revision = None;
            self.live_gesture = None;
        }
    }
    #[cfg(test)]
    fn pick_color(&mut self, doc: &Document, point: [f32; 2]) -> bool {
        #[cfg(test)]
        {
            self.color_pick_runs += 1;
        }
        let Ok(point) = crate::render::sample::point([doc.width, doc.height], point) else {
            return false;
        };
        let target = self.mask.then_some(self.selected.as_str());
        let Ok(pixel) = crate::render::sample::resident_color(doc, point, target, &|| Ok(())) else {
            return false;
        };
        self.accept_color_sample(pixel);
        true
    }

    #[cfg(test)]
    fn pick_alt_color(&mut self, doc: &Document, project: &str, point: [f32; 2]) {
        // Resident oracle for cache and precision regressions.
        let Ok(native) = crate::render::sample::point([doc.width, doc.height], point) else {
            self.alt_sample = None;
            return;
        };
        if let Some(sample) = self.alt_sample.as_ref().filter(|sample| {
            sample.engine.as_ptr() == Arc::as_ptr(&self.shared)
                && sample.project == project
                && sample.document == doc.id
                && sample.revision == doc.revision
                && sample.point == native
                && sample.selected == self.selected
                && sample.mask == self.mask
                && sample.step == self.mask_step
        }) {
            self.accept_color_sample(sample.value);
            return;
        }
        self.alt_sample = None;
        if self.pick_color(doc, point) {
            self.alt_sample = Some(AltSample {
                native: None,
                _charge: None,
                engine: Arc::downgrade(&self.shared),
                project: project.into(),
                document: doc.id.clone(),
                revision: doc.revision,
                point: native,
                selected: self.selected.clone(),
                mask: self.mask,
                step: self.mask_step.clone(),
                value: if self.mask {
                    self.mask_value16.map_or(
                        crate::render::sample::Color::Byte([self.mask_value; 4]),
                        |gray| crate::render::sample::Color::Word([gray, gray, gray, u16::MAX]),
                    )
                } else {
                    self.color16.map_or(crate::render::sample::Color::Byte(self.color), crate::render::sample::Color::Word)
                },
            });
        }
    }
    fn canvas<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        ai_scopes: &[crate::engine::Scope],
        human_regions: &[[i32; 4]],
    ) {
        let doc = doc.into();
        if self.gizmo_handle != 0 && Self::pointer_contact_cancelled(ui.ctx()) {
            self.cancel_gizmo("Transform gesture cancelled · start a new drag");
        }
        let primitive_drag = self.primitive_drag.take();
        let fill_press = self.fill_press.take();
        let sample_press = self.sample_press.take();
        self.sync_workspace_stroke(doc, ui.ctx());
        let workspace_stroke = self.workspace_stroke.take();
        if (self.brush_tool() || self.tool == Tool::Liquify)
            && !self.points.is_empty()
            && (self.drag_revision.is_some_and(|v| v != doc.revision())
                || self
                    .straight
                    .document
                    .as_ref()
                    .is_some_and(|v| *v != doc.id()))
        {
            self.points.clear();
            self.drag_start = None;
            self.drag_revision = None;
            self.straight.reset();
            self.live_gesture = None;
            self.straight.cancelled = true;
            self.stroke_pressures.clear();
            self.stroke_has_pressure = false;
            self.current_pressure = None;
            self.message = if self.tool == Tool::Liquify {
                "Image changed during Liquify stroke · preview cancelled"
            } else {
                "Image changed during brush stroke · preview cancelled"
            }.into();
        }
        if [Tool::Clone, Tool::Heal].contains(&self.tool)
            && !self.points.is_empty()
            && self.retouch_revision != Some(doc.revision())
        {
            self.points.clear();
            self.drag_start = None;
            self.live_gesture = None;
            self.message = "Image changed during retouch · stroke cancelled".into();
        }
        self.transient.clear();
        if let Some(editor) = self.free_transform.as_ref().filter(|editor| editor.observes(doc)) {
            self.transient.extend(editor.commands().unwrap_or_default());
        }
        self.native_source_preview = None;
        self.native_source_preview_at = self.transient.len();
        if let Some(editor) = self.source_editor.as_ref()
            .filter(|editor| editor.previewing() && editor.matches_source(doc))
        {
            if editor.is_native() {
                self.native_source_preview = editor.owned_command();
            } else {
                self.transient.push(editor.command());
            }
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
        self.canvas_input_region = Some((available, ui.layer_id()));
        let press = ui.input(|input| primary_button_samples(input).find_map(|(down, pos, _)| down.then_some(pos)));
        if press.is_some_and(|pos| canvas_press_hits(ui, &response, pos)) {
            self.selection_pointer_cancelled = false;
            self.layer_clipboard = false;
            if ui.input(|i| i.pointer.primary_down()) {
                self.canvas_press_document = Some(doc.id().to_owned());
            }
        }
        let [canvas_width, canvas_height] = self.displayed_canvas_size(doc);
        let view = self.view;
        let fit = view.fit_scale(available, [canvas_width, canvas_height]);
        let zoom_limits = view::zoom_limits(fit, ui.ctx().pixels_per_point());
        let view_locked = self.canvas_view_gesture_active()
            || primitive_drag.is_some()
            || fill_press.is_some()
            || sample_press.is_some()
            || workspace_stroke.is_some();
        if self.frame_pending && !view_locked {
            let target = doc.selection().filter(|r| {
                [canvas_width, canvas_height] == [doc.width(), doc.height()]
                    && r[2] > r[0] && r[3] > r[1]
            });
            if let Some(r) = target {
                let width = (r[2] - r[0]) as f32;
                let height = (r[3] - r[1]) as f32;
                // Ten percent breathing room, fitted in document coordinates.
                let extent = view.extent(Vec2::new(width, height));
                let scale = (available.width() / (extent.x * 1.12))
                    .min(available.height() / (extent.y * 1.12));
                self.zoom = (scale / fit).clamp(*zoom_limits.start(), *zoom_limits.end());
                let center = Vec2::new(
                    (r[0] as f32 + r[2] as f32) * 0.5,
                    (r[1] as f32 + r[3] as f32) * 0.5,
                );
                self.pan = view
                    .vector(Vec2::new(doc.width() as f32, doc.height() as f32) * 0.5 - center)
                    * fit
                    * self.zoom;
            } else {
                self.zoom = 1.0;
                self.pan = Vec2::ZERO;
            }
            self.frame_pending = false;
        }
        let size_mode = [
            Tool::Brush,
            Tool::Eraser,
            Tool::Smudge,
            Tool::Clone,
            Tool::Heal,
            Tool::Liquify,
        ]
        .contains(&self.tool)
            && !ui
                .ctx()
                .memory(|m| m.focused())
                .is_some_and(|id| egui::TextEdit::load_state(ui.ctx(), id).is_some())
            && !self.shortcuts.open
            && self.points.is_empty()
            && self.drag_start.is_none()
            && self.gizmo_handle == 0
            && ui.input(|i| {
                i.focused
                    && !i
                        .events
                        .iter()
                        .any(|event| matches!(event, egui::Event::WindowFocused(false)))
                    && self.shortcuts.size_held(i)
            });
        let text_navigation = self.tool == Tool::Text && self.source_editor.as_ref().is_some_and(|editor| {
            editor.canvas_text && editor.matches_source(doc) && !editor.moving_text_field()
        });
        let navigate = self.lifecycle.is_none()
            && !self.lifecycle_frame
            && self.proposal_review.is_none()
            && self.geometry.is_none()
            && (self.source_editor.is_none() || text_navigation)
            && self.filter_editor.is_none()
            && !view_locked
            && !(self.brush_tool() && self.straight.cancelled)
            && !(size_mode && response.hovered())
            && self.size_drag.is_none();
        let (alt, text_navigation_pointer) = ui.input(|i| {
            let middle = i.pointer.middle_down() || i.pointer.button_released(egui::PointerButton::Middle);
            let secondary = i.pointer.secondary_down()
                || i.pointer.button_released(egui::PointerButton::Secondary);
            (i.modifiers.alt, (middle || i.modifiers.alt && secondary)
                && !i.pointer.primary_down() && !i.pointer.primary_pressed())
        });
        let text_navigation_contact = navigate && text_navigation && text_navigation_pointer;
        let navigate_pointer = navigate && (!text_navigation || text_navigation_contact);
        let zoom_drag = navigate_pointer
            && alt
            && response.dragged_by(egui::PointerButton::Secondary);
        let pan_drag = navigate_pointer
            && !zoom_drag
            && (response.dragged_by(egui::PointerButton::Middle)
                || (!text_navigation
                    && (self.tool == Tool::Pan || ui.input(|i| i.key_down(egui::Key::Space)))
                    && response.dragged_by(egui::PointerButton::Primary)));
        // Apply navigation before building the image mesh and document-coordinate mapping.
        if navigate {
            if response.hovered() && !text_navigation {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    let old = self.zoom;
                    self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(*zoom_limits.start(), *zoom_limits.end());
                    if let Some(anchor) = ui.input(|i| i.pointer.hover_pos()) {
                        self.pan +=
                            (anchor - (available.center() + self.pan)) * (1.0 - self.zoom / old);
                    }
                }
            }
            if zoom_drag {
                let delta = response.drag_delta();
                let old = self.zoom;
                self.zoom = (self.zoom * ((delta.x - delta.y) * 0.008).exp()).clamp(*zoom_limits.start(), *zoom_limits.end());
                if let Some(anchor) = response.interact_pointer_pos() {
                    self.pan +=
                        (anchor - (available.center() + self.pan)) * (1.0 - self.zoom / old);
                }
            } else if pan_drag {
                self.pan += response.drag_delta();
            }
        }
        let scale = fit * self.zoom;
        let size = Vec2::new(canvas_width as f32 * scale, canvas_height as f32 * scale);
        let rect = Rect::from_center_size(available.center() + self.pan, size);
        let (rect, scale) = self.crop_canvas_input(ui, doc, &response, rect, scale);
        let [canvas_width, canvas_height] = self.displayed_canvas_size(doc);
        if let Some(editor) = self.geometry.as_ref().filter(|editor| editor.observes(doc)) {
            self.transient.insert(self.native_source_preview_at, editor.command());
            self.native_source_preview_at += 1;
        }
        if (self.view_rect != Some(rect) || self.canvas_viewport != Some(available))
            && doc.native().is_some()
        {
            ui.ctx().request_repaint();
        }
        self.view_rect = Some(rect);
        self.canvas_viewport = Some(available);
        let painter = ui.painter().with_clip_rect(available);
        let projection = view.projection(rect, scale);
        let to_screen = |point| projection.point(point);
        let to_doc = |point| projection.document(point);
        let corners = projection.corners();
        let bounds = Rect::from_points(&corners);
        let visible = bounds.intersect(available);
        if bounds.intersect(painter.clip_rect()).is_positive() {
            view::paint_checker(&painter, corners, view::checker(ui.ctx()).id());
        }
        if let Some(canvas) = self
            .native_canvas
            .current()
            .filter(|c| c.key().identity.document_id == doc.id() && self.native_proposal_canvas_current(doc))
        {
            canvas.paint_with(
                &painter,
                to_screen,
                Color32::WHITE,
            );
        } else if let Some(texture) = self.texture.as_ref().filter(|_| {
            self.proposal_review.is_none()
                || self.proposal_rendered.as_deref() == Some(self.live_key().as_str())
        }) {
            texture.paint(&painter, |[x, y]| {
                to_screen([x * canvas_width as f32, y * canvas_height as f32])
            });
        }
        if bounds.intersects(painter.clip_rect())
            || bounds.expand(1. + 2. / ui.ctx().pixels_per_point()).intersects(painter.clip_rect())
        {
            painter.add(egui::Shape::closed_line(
                corners.to_vec(),
                Stroke::new(1.0_f32, Color32::from_rgb(90, 84, 93)),
            ));
        }
        if let Some(caption) = self.solo_caption(doc) {
            let painter = painter.with_clip_rect(visible);
            let point = visible.left_top() + Vec2::new(8., 8.);
            for (offset, color) in [
                (Vec2::splat(1.), Color32::BLACK),
                (Vec2::ZERO, Color32::WHITE),
            ] {
                painter.text(
                    point + offset,
                    egui::Align2::LEFT_TOP,
                    &caption,
                    egui::FontId::proportional(12.),
                    color,
                );
            }
        }
        if self.lifecycle.is_some() || (self.lifecycle_frame && !self.sampled_canvas_input()) {
            return;
        }
        if (self.brush_tool() || self.tool == Tool::Liquify)
            && Self::pointer_contact_cancelled(ui.ctx())
        {
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
        if self.free_transform.is_some() {
            self.free_transform_canvas(ui, doc, rect, scale, available);
            return;
        }
        if self.free_transform_frame {
            return;
        }
        if self.free_transform_cancelled_pointer {
            if ui.input(|input| input.pointer.primary_pressed()) {
                self.free_transform_cancelled_pointer = false;
            } else {
                if !ui.input(|input| input.pointer.primary_down()) {
                    self.free_transform_cancelled_pointer = false;
                }
                return;
            }
        }
        if self.tool == Tool::Text
            && !zoom_drag
            && !pan_drag
            && !text_navigation_contact
            && (self.source_editor.is_some() || !ui.input(|input| input.key_down(egui::Key::Space)))
            && self.geometry.is_none()
            && self.filter_editor.is_none()
            && self
                .source_editor
                .as_ref()
                .is_none_or(|editor| editor.placement.is_some() || editor.canvas_text)
        {
            self.text_canvas(ui, doc, &response, &painter, rect, scale);
            return;
        }
        if text_navigation_contact {
            if let Some(editor) = &mut self.source_editor {
                editor.pause_canvas_pointer_for_navigation(ui.ctx());
            }
        }
        if self.geometry.is_some() || self.source_editor.is_some() || self.filter_editor.is_some() {
            self.paint_crop_handles(ui, doc, &painter, &projection);
            self.text_field_outline(&painter, rect, scale);
            return;
        }
        if (self.brush_tool() || self.tool == Tool::Liquify) && self.straight.cancelled {
            self.cancel_contact_stroke();
            self.straight.cancelled = ui.input(|i| i.pointer.primary_down());
            return;
        }
        if self.brush_tool()
            && self.points.is_empty()
            && self.drag_start.is_none()
            && !ui.input(|i| i.pointer.primary_down())
        {
            // Sampling and size controls can finish without a canvas stroke release.
            self.straight.cancel();
            self.drag_revision = None;
        }
        if size_mode && (response.hovered() || self.size_drag.is_some()) {
            if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
                let (anchor, start, hardness) =
                    *self
                        .size_drag
                        .get_or_insert((pointer, self.radius, self.brush.hardness));
                self.radius = (start * ((pointer.x - anchor.x) * 0.012).exp()).clamp(0.5, 512.0);
                if self.tool != Tool::Liquify {
                    self.brush.hardness = (hardness - (pointer.y - anchor.y) * 0.005).clamp(0., 1.);
                }
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
                    if self.tool == Tool::Liquify {
                        format!("{} px", (self.radius * 2.0).round())
                    } else {
                        format!(
                            "{} px · hardness {:.0}%",
                            (self.radius * 2.0).round(),
                            self.brush.hardness * 100.
                        )
                    },
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
        self.paint_selection(ui, &painter, doc.into(), &projection);
        self.canvas_reservations(ui, &painter, doc, &projection, ai_scopes, human_regions);
        if response.hovered() {
            if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                if ([
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
        if (self.tool == Tool::Picker && self.color_pick.is_some() || self.sampled_stroke_waiting()) && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Progress);
        }
        if zoom_drag || pan_drag {
            self.cancel_gizmo("Transform gesture cancelled · start a new drag");
            if zoom_drag {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
            }
            return;
        }
        if [Tool::Clone, Tool::Heal].contains(&self.tool) {
            if let Some(source) = &self.retouch_source {
                if source.project == doc.id() {
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
            let alt_press = ui.input(|input| primary_button_samples(input).find_map(|(down, pos, mods)| {
                (down && mods.alt).then_some(pos)
            }));
            if alt_press.is_some() || ui.input(|i| i.modifiers.alt) || self.retouch_pick {
                let point = if self.retouch_pick {
                    self.canvas_sample_release(ui, doc, &response, &projection, sample_press)
                } else {
                    alt_press.filter(|&pos| canvas_press_hits(ui, &response, pos))
                        .map(to_doc)
                        .filter(|&point| canvas_sample_input(ui)
                            && crate::render::sample::point([doc.width(), doc.height()], point).is_ok())
                };
                if let Some(point) = point {
                    self.retouch_source = Some(retouch::Anchor {
                        project: doc.id().to_owned(),
                        layer: self.selected.clone(),
                        mask: self.mask,
                        point,
                        destination: None,
                    });
                    self.message = "Clone/heal source set · paint to retouch".into();
                    self.retouch_pick = false;
                }
                self.cancel_contact_stroke();
                return;
            }
        }
        if self.tool == Tool::Brush && ui.input(|i| i.modifiers.alt) {
            if !self.points.is_empty() {
                let color = self.stroke_color();
                if !self.straight.line {
                    let command = self.stroke_command(&self.points, color);
                    if !self.submit_stroke(command, "Brush stroke") { return; }
                }
                self.points.clear();
                self.stroke_pressures.clear();
                self.stroke_has_pressure = false;
                self.current_pressure = None;
                self.live_gesture = None;
            }
            if response.hovered() {
                if let Some(pointer) = ui
                    .input(|i| i.pointer.hover_pos())
                    .filter(|p| projection.contains(*p))
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
                        if ui.input(|i| i.focused && !i.key_pressed(egui::Key::Escape)) {
                            self.queue_alt_sample(ui.ctx(), doc, to_doc(pointer));
                        }
                    }
                }
            }
            self.drag_start = None;
            return;
        }
        if matches!(self.tool, Tool::Selection | Tool::MagicWand) {
            self.selection_canvas(ui, doc, &response, &painter, &projection);
            return;
        }
        if self.tool == Tool::Pen {
            self.pen_canvas(ui, doc, &response, &painter, rect, scale);
            return;
        }
        if [Tool::Move, Tool::Rotate, Tool::Scale].contains(&self.tool) {
            self.gizmo(ui, doc, rect, scale, available, &response);
            return;
        }
        if [Tool::Rectangle, Tool::Ellipse, Tool::Gradient].contains(&self.tool) {
            self.primitive_canvas(ui, doc, &response, &painter, &projection, primitive_drag);
            return;
        }
        if self.tool == Tool::Fill {
            self.fill_canvas(ui, doc, &response, fill_press);
            return;
        }
        if self.tool == Tool::Liquify && response.hovered() {
            if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
                liquify::cursor(&painter, pointer, self.radius, scale);
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            }
        }
        if self.brush_tool() || self.tool == Tool::Liquify {
            self.stroke_canvas(ui, doc, &response, &projection, workspace_stroke);
            return;
        }
        if self.tool == Tool::Picker {
            if let Some(point) = self.canvas_sample_release(ui, doc, &response, &projection, sample_press) {
                self.queue_color_pick(ui.ctx(), doc, point);
            }
        }
    }
}
impl PeerBrush {
    fn switch_project(&mut self, id: &str) {
        if (self.edit_worker.pending() || self.sampled_input_pending()) && id != self.project_id {
            self.message = "Wait for the edit result before switching projects".into();
            return;
        }
        if id == self.project_id {
            return;
        }
        self.cancel_color_pick();
        self.sample_press = None;
        self.blend_hover = None;
        self.mask_thumbnails.clear();
        self.layer_thumbnails.clear();
        self.transform_bounds.clear();
        self.cancel_free_transform();
        let Some((_, _, shared)) = crate::workspace::entries_in(&self.workspace)
            .into_iter()
            .find(|(project, _, _)| project == id)
        else {
            return;
        };
        self.canvas_press_document = None;
        self.selection_pointer_cancelled = false;
        self.canvas_input_region = None;
        self.deferred_selection_shortcuts = None;
        self.duplicate_repeat = None;
        self.pending_duplicate_repeat = None;
        self.duplicate_repeat_history = None;
        self.project_views.insert(
            self.project_id.clone(),
            ProjectView {
                selected: self.selected.clone(),
                selection_layers: self.selection_layers.clone(),
                selection_anchor: self.selection_anchor.clone(),
                mask: self.mask,
                mask_step: self.mask_step.clone(),
                effect_selected: self.effect_selected.clone(),
                effect_expanded: self.effect_expanded.clone(),
                isolate: self.isolate,
                collapsed: self.collapsed.clone(),
                folder_view_document: self.folder_view_document.clone(),
                zoom: self.zoom,
                pan: self.pan,
                view: self.view,
                message: self.message.clone(),
            },
        );
        self.select_tool(self.tool);
        self.finish_parameter(true);
        self.rename_edit = None;
        self.eye_sweep = None;
        self.layer_drag = None;
        self.clear_tab_transfer_preview();
        if !self.tab_transfer_committing {
            self.tab_transfer = None;
        }
        self.layer_context = None;
        self.geometry = None;
        self.source_editor = None;
        self.filter_editor = None;
        self.refinement = None;
        self.retouch_source = None;
        self.task_undo_review = None;
        self.proposal_review = None;
        self.proposal_rendered = None;
        self.clear_selection_path();
        self.selection_gesture_source = None;
        self.primitive_drag = None;
        self.fill_press = None;
        self.workspace_stroke = None;
        self.selection_gesture_mode = None;
        self.project_settings = None;
        self.show_global_light = false;
        self.shared = shared;
        self.project_id = id.into();
        self.busy = self.jobs.contains(id);
        if let Some(view) = self.project_views.remove(id) {
            // A fit requested for a new tab must not replace this tab's saved view.
            self.frame_pending = false;
            self.selected = view.selected;
            self.selection_layers = view.selection_layers;
            self.selection_anchor = view.selection_anchor;
            self.mask = view.mask;
            self.mask_step = view.mask_step;
            self.effect_selected = view.effect_selected;
            self.effect_expanded = view.effect_expanded;
            self.set_solo(view.isolate);
            self.collapsed = view.collapsed;
            self.folder_view_document = view.folder_view_document;
            self.zoom = view.zoom;
            self.pan = view.pan;
            self.view = view.view;
            self.message = view.message;
        } else {
            self.selected = String::new();
            self.selection_layers.clear();
            self.selection_anchor = String::new();
            self.mask = false;
            self.mask_step = None;
            self.effect_selected = None;
            self.effect_expanded.clear();
            self.set_solo(false);
            self.collapsed.clear();
            self.folder_view_document.clear();
            self.zoom = 1.;
            self.pan = Vec2::ZERO;
            self.view = view::View::default();
            self.frame_pending = true;
            self.message = "Ready".into();
        }
        self.texture = None;
        self.canvas_pixels = None;
        self.canvas_preview_job = None;
        self.canvas_selection = None;
        self.last_preview = None;
        self.pending = false;
        self.preview_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.preview_cache = Arc::new(std::sync::Mutex::new(crate::preview::Cache::default()));
        self.resident_preview_stroke = None;
        self.native_dispatch.clear();
        self.native_preview.clear();
        self.native_canvas.clear();
        self.native_receipt = None;
        self.thumbs.clear();
        self.thumb_pending.clear();
        self.thumb_document.clear();
        self.thumb_native = false;
        self.last_status = "".into();
    }
    fn draw_header(&mut self, ctx: &egui::Context) {
        let (connected, activity) = {
            if let Ok(e) = self.workspace_root.try_lock() {
                self.connected = e
                    .mcp_clients
                    .values()
                    .any(|expiry| *expiry > crate::engine::now());
            }
            let activity = self
                .shared
                .try_lock()
                .ok()
                .map(|e| e.activity.clone())
                .unwrap_or_else(|| self.activity_text.clone());
            (self.connected, activity)
        };
        let available = self.connection.is_available();
        let connected = connected && available;
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
                    self.task_history_menu(ui);
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
                            "Advertise PeerBrush and set up installed AI clients. Green MCP available means the server is ready; MCP connected confirms an active AI client."
                        } else { &self.connect_feedback });
                        let color = if available {
                            Color32::from_rgb(166, 215, 157)
                        } else {
                            Color32::from_rgb(255, 154, 144)
                        };
                        ui.label(
                            RichText::new(if !available {
                                "MCP unavailable"
                            } else if connected {
                                "MCP connected"
                            } else {
                                "MCP available"
                            })
                            .size(11.0)
                            .color(color),
                        ).on_hover_text(if !available {
                            "The local MCP server is unavailable. Restart PeerBrush to restore the connection."
                        } else if connected {
                            "The local MCP server is available and an AI client is connected."
                        } else {
                            "The local MCP server is available; no AI client is connected yet. Connect AI sets up discovery and installed clients."
                        });
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
    fn clear_tab_transfer_preview(&mut self) {
        if let Some(pending) = &self.tab_preview_pending {
            pending.cancel();
        }
        self.tab_preview = None;
        self.tab_preview_requested = None;
    }

    fn start_tab_transfer_commit(&self, ctx: &egui::Context) {
        let spec = self.tab_transfer.clone().expect("queued tab drop");
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
    }

    fn project_tabs(&mut self, ctx: &egui::Context) {
        let release = ctx.input(|input| primary_release_samples(input).next());
        let (select, close, hovered) = self.draw_project_tabs(ctx, release);
        if !self.tab_transfer_committing && (self.tab_transfer != hovered || release.is_some()) {
            self.clear_tab_transfer_preview();
        }
        while let Ok(reply) = self.transfer_rx.try_recv() {
            match reply.output {
                TransferOutput::Preview { coarse, result } => {
                    let current = self
                        .tab_preview_pending
                        .as_ref()
                        .is_some_and(|job| job.check().is_ok());
                    // A terminal reply comes after provisional sources have retired.
                    // Start a queued drop only after that worker releases the service.
                    if !coarse
                        && self.tab_preview_pending.take().is_some()
                        && self.tab_transfer_committing
                    {
                        drop(result);
                        self.start_tab_transfer_commit(ctx);
                        continue;
                    }
                    if current
                        && self.tab_transfer.as_ref() == Some(&reply.spec)
                        && !self.tab_transfer_committing
                        && (!coarse || self.preview_quality.reduced())
                    {
                        match result.and_then(|output| match output {
                            TabPreviewOutput::Resident(image) => {
                                let texture = native_texture::RgbaTexture::new(
                                    ctx, "Cross-project preview", [image.width, image.height], image.bytes(),
                                )?;
                                Ok((image.summary, TabPreviewTexture::Resident(texture)))
                            }
                            TabPreviewOutput::Native(summary, prepared) => {
                                Ok((summary, TabPreviewTexture::Native(prepared.publish(ctx, &mut || Ok(()))?)))
                            }
                        }) {
                            Ok(preview) => self.tab_preview = Some(preview),
                            Err(error) => {
                                if !coarse {
                                    self.tab_preview = None;
                                }
                                self.message = error.into();
                            }
                        }
                    }
                }
                TransferOutput::Commit(result) => {
                    self.jobs.remove(&reply.spec.source);
                    self.busy = self.jobs.contains(&self.project_id);
                    self.tab_transfer_committing = false;
                    self.tab_transfer = None;
                    self.clear_tab_transfer_preview();
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
                            let message = if reply.spec.move_layers {
                                "Moved layers between projects"
                            } else {
                                "Copied layers to project"
                            };
                            self.message = result["cleanup_warning"].as_str().unwrap_or(message).into();
                        }
                        Err(error) => self.message = error.into(),
                    }
                    self.last_preview = None;
                }
            }
        }
        if !self.tab_transfer_committing {
            if let Some(spec) = hovered.filter(|spec| spec.source == self.project_id) {
                if release.is_some() {
                    self.layer_drag = None;
                    self.tab_transfer = Some(spec.clone());
                    self.tab_transfer_committing = true;
                    self.jobs.insert(spec.source.clone());
                    self.busy = true;
                    if self.tab_preview_pending.is_none() {
                        self.start_tab_transfer_commit(ctx);
                    }
                } else {
                    self.tab_transfer = Some(spec.clone());
                    if self.tab_preview_pending.is_none()
                        && self.tab_preview_requested.as_ref() != Some(&spec)
                    {
                        self.tab_preview_requested = Some(spec.clone());
                        let cancel = crate::engine::native::Cancellation::default();
                        self.tab_preview_pending = Some(cancel.clone());
                        let workspace = self.workspace.clone();
                        let tx = self.transfer_tx.clone();
                        let ctx = ctx.clone();
                        let side = ctx.input(|input| input.max_texture_side);
                        let reduced = self.preview_quality.reduced();
                        std::thread::spawn(move || {
                            let result = crate::workspace::render_transfer_with(
                                &workspace,
                                &spec.request(),
                                crate::raster::NATIVE_EDGE,
                                reduced.then_some(1400),
                                &|| cancel.check(),
                                TabPreviewOutput::Resident,
                                |preview| {
                                    let (summary, output) = preview.render_canvas(|request, check| {
                                        let retirement = native_texture::Retirement::new(request.budget())?;
                                        native_texture::Prepared::from_one_shot_request(
                                            request, &retirement, side, &ctx, &mut || check(),
                                        )
                                    })?;
                                    Ok(TabPreviewOutput::Native(summary, output))
                                },
                                |image| {
                                    let _ = tx.send(TransferReply {
                                        spec: spec.clone(),
                                        output: TransferOutput::Preview {
                                            coarse: true,
                                            result: Ok(image),
                                        },
                                    });
                                    ctx.request_repaint();
                                },
                            );
                            let _ = tx.send(TransferReply {
                                spec,
                                output: TransferOutput::Preview {
                                    coarse: false,
                                    result,
                                },
                            });
                            ctx.request_repaint();
                        });
                    }
                }
            } else if self.tab_transfer.take().is_some() {
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
    // Commands before selection completion must not act on old coverage. Require
    // an explicit retry; never retain delayed destructive/clipboard work.
    fn hold_canvas_selection_shortcuts<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) -> Option<Vec<egui::Event>> {
        let doc = doc.into();
        let pending = self.edit_worker.request().is_some_and(|request| {
            request.selection_edit && request.project == self.project_id && request.document == doc.id()
        });
        if (!matches!(self.tool, Tool::Selection | Tool::MagicWand) && !pending)
            || self.lifecycle.is_some()
            || self.lifecycle_frame
            || self.shortcuts.open
            || self.filter_editor.is_some()
            || self.rename_edit.is_some()
            || ctx
                .memory(|m| m.focused())
                .is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
        {
            return None;
        }
        // A cancelled contact still owns input through its release. Otherwise
        // a shortcut in that frame could fall back to the old selection.
        let cancelled = self.selection_pointer_cancelled;
        let owner = cancelled || self.canvas_press_document.as_deref() == Some(doc.id());
        let (release, press, later_press, focused, primary_down, event_count) = ctx.input(|input| {
            let (start, release) = canvas_contact_range(input, owner);
            let press = canvas_contact_press(input, start).map(|(pos, _)| (start, pos));
            let later_press = release.and_then(|index| {
                input.events[index + 1..].iter().enumerate().find_map(|(offset, event)| {
                    let (pressed, pos, _) = primary_button_sample(event)?;
                    pressed.then_some((index + offset + 2, pos))
                })
            });
            (
                release,
                press,
                later_press,
                input.focused,
                input.pointer.primary_down(),
                input.events.len(),
            )
        });
        let path_tool = self.tool == Tool::Selection
            && ["polygon", "magnetic"].contains(&self.selection_kind.as_str());
        if !focused || !path_tool && release.is_none() && !primary_down {
            return None;
        }
        let hits_canvas = |(_, pos): &(usize, Pos2)| {
            self.canvas_input_region.is_some_and(|(rect, layer)| {
                rect.contains(*pos) && ctx.layer_id_at(*pos) == Some(layer)
            })
        };
        let press = press.filter(hits_canvas);
        let start = if owner || path_tool && self.selection_path_pending() {
            0
        } else {
            press.or_else(|| later_press.filter(hits_canvas))?.0
        };
        // A path spans clicks and remains unfinished after mouse release.
        // Enter/double-click can finish it later in this frame; require a new
        // shortcut after that commit rather than using previous coverage.
        // A later press supersedes the release in this input batch. A fresh
        // release while another selection edit is pending also cannot own
        // shortcuts bound to that earlier worker request.
        let interrupted = cancelled || Self::pointer_contact_cancelled(ctx)
            || ctx.input(|input| input.key_pressed(egui::Key::Escape));
        let end = if path_tool || later_press.is_some() || pending || interrupted {
            event_count
        } else {
            release.unwrap_or(event_count)
        };
        let preserve_anchor = path_tool && self.selection_path_pending();
        let shortcuts = &self.shortcuts;
        let hold = |index, event: &egui::Event| {
            index >= start
                && index < end
                && !(preserve_anchor
                    && matches!(event,
                        egui::Event::Key { key: egui::Key::Delete, pressed: true, modifiers, .. }
                            if !modifiers.any()))
                && shortcuts.selection_dependent_event(event)
        };
        ctx.input_mut(|input| {
            if !input
                .events
                .iter()
                .enumerate()
                .any(|(index, event)| hold(index, event))
            {
                return None;
            }
            let original = std::mem::take(&mut input.events);
            input.events = original
                .iter()
                .enumerate()
                .filter(|(index, event)| {
                    let held = hold(*index, event);
                    if held {
                        if let egui::Event::Key {
                            key, physical_key, ..
                        } = event
                        {
                            self.selection_shortcut_keys.insert(*key);
                            if let Some(key) = physical_key {
                                self.selection_shortcut_keys.insert(*key);
                            }
                        }
                    }
                    !held
                })
                .map(|(_, event)| event.clone())
                .collect();
            Some(original)
        })
    }
    // Canvas and parameter-point gestures can commit during drawing. Shortcuts
    // delivered after their mouse release need the resulting document revision.
    fn defer_canvas_shortcuts<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
        parameter_point: bool,
    ) -> Option<(Vec<egui::Event>, Vec<egui::Event>, egui::Modifiers)> {
        let doc = doc.into();
        // A quick click may deliver both buttons in one frame. Use the prior
        // canvas hit region and layer to exclude floating windows and panels.
        let held = parameter_point || self.canvas_press_document.as_deref() == Some(doc.id());
        let (press, release) = ctx.input(|input| {
            let (start, release) = canvas_contact_range(input, held);
            (canvas_contact_press(input, start).map(|(pos, _)| pos), release)
        });
        let pressed_canvas = press.is_some_and(|pos| {
            self.canvas_input_region.is_some_and(|(rect, layer)| {
                rect.contains(pos) && ctx.layer_id_at(pos) == Some(layer)
            })
        });
        ctx.input_mut(|i| {
            let owner = if release.is_some() || !i.focused || !i.pointer.primary_down() {
                self.canvas_press_document.take()
            } else {
                None
            };
            let release = release?;
            if !i.focused
                || i.events
                    .iter()
                    .any(|event| matches!(event, egui::Event::WindowFocused(false)))
                || self.selection_gesture_stale(doc)
                || (!parameter_point && owner.as_deref() != Some(doc.id()) && !pressed_canvas)
            {
                return None;
            }
            let shortcut = |event: &egui::Event| match event {
                egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) => true,
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    shortcuts::Shortcuts::allowed(*key)
                        || (*key == egui::Key::Delete
                            && self.tool != Tool::Pen
                            && !(self.tool == Tool::Selection
                                && ["polygon", "magnetic"].contains(&self.selection_kind.as_str()))
                            && !self.selection_path_pending())
                        || (*key == egui::Key::Backspace && modifiers.alt && !modifiers.command)
                }
                _ => false,
            };
            if !i.events[release + 1..].iter().any(&shortcut) {
                return None;
            }
            let original = std::mem::take(&mut i.events);
            let mut late = Vec::new();
            for (index, event) in original.iter().enumerate() {
                if index > release && shortcut(event) {
                    late.push(event.clone());
                } else {
                    i.events.push(event.clone());
                }
            }
            let release_modifiers = match original[release] {
                egui::Event::PointerButton { modifiers, .. } => modifiers,
                _ => unreachable!(),
            };
            Some((original, late, release_modifiers))
        })
    }

    fn selection_shortcut_events(&self, events: &[egui::Event]) -> Vec<egui::Event> {
        events.iter().filter(|event| {
            !matches!(event, egui::Event::Key { repeat: true, .. })
                && self.shortcuts.selection_dependent_event(event)
        }).take(8).map(|event| match event {
            // The document clipboard reads the system image on request. Do
            // not keep the platform's unrelated text payload in a replay.
            egui::Event::Paste(_) => egui::Event::Paste(String::new()),
            event => event.clone(),
        }).collect()
    }

    fn queue_selection_shortcuts(&mut self, doc: DocumentView<'_>, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
        if events.is_empty() || self.deferred_selection_shortcuts.is_some() { return; }
        let Some(document) = doc.native() else { return; };
        let Some(request) = self.edit_worker.request_owner().filter(|request| {
            request.selection_edit && request.project == self.project_id && request.document == doc.id()
        }) else { return; };
        let bytes = 2048 + self.selected.len() + self.mask_step.as_ref().map_or(0, String::len);
        let charge = match crate::psd::memory36::MetadataCharge::reserve(document.budget(), bytes, &|| Ok(())) {
            Ok(charge) => charge,
            Err(error) => { self.message = error.into(); return; }
        };
        self.deferred_selection_shortcuts = Some(DeferredSelectionShortcuts {
            request, committed: None, events, modifiers, tool: self.tool, layer: self.selected.clone(),
            mask: self.mask, step: self.mask_step.clone(), _charge: charge,
        });
    }

    fn replay_selection_shortcuts(&mut self, ctx: &egui::Context, doc: DocumentView<'_>) {
        if self.edit_worker.pending() { return; }
        let Some(queued) = self.deferred_selection_shortcuts.take() else { return; };
        let Some((revision, source)) = queued.committed else { return; };
        if queued.request.project != self.project_id || queued.request.document != doc.id()
            || queued.tool != self.tool || queued.layer != self.selected
            || queued.mask != self.mask || queued.step != self.mask_step
            || self.document_shortcut_typing(ctx) || self.free_transform.is_some() || self.free_transform_frame
        { return; }
        let current = self.shared.try_lock().ok().map(|engine| {
            let live = crate::engine::view::current(&engine);
            live.id() == doc.id() && live.revision() == revision
                && live.native().is_some_and(|document| document.key() == source)
        });
        if current == Some(false) { return; }
        if current.is_none() || doc.revision() < revision || self.lifecycle_frame {
            self.deferred_selection_shortcuts = Some(queued);
            ctx.request_repaint();
            return;
        }
        if doc.revision() != revision || doc.native().is_none_or(|document| document.key() != source)
            || ctx.input(|input| input.events.iter().any(|event| {
                !matches!(event, egui::Event::Key { repeat: true, .. }) && self.shortcuts.selection_dependent_event(event)
            }))
        { return; }
        if doc.selection().is_none() {
            self.message = "Selection is empty; shortcut skipped".into();
            return;
        }
        let (events, modifiers) = ctx.input_mut(|input| {
            (std::mem::replace(&mut input.events, queued.events), std::mem::replace(&mut input.modifiers, queued.modifiers))
        });
        self.document_shortcuts(ctx, doc);
        ctx.input_mut(|input| { input.events = events; input.modifiers = modifiers; });
    }

    fn document_shortcut_typing(&self, ctx: &egui::Context) -> bool {
        self.lifecycle.is_some()
            || self.source_editor.is_some()
            || (self.tool == Tool::Text
                && ctx.input(|input| input.pointer.primary_down() || input.pointer.primary_pressed()))
            || self.shortcuts.open || self.filter_editor.is_some() || self.rename_edit.is_some()
            || self.save_loss_review.is_some() || self.show_new
            || ctx.memory(|m| m.focused()).is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
    }

    fn hold_keyboard_selection_shortcuts(
        &mut self,
        events: &mut Vec<egui::Event>,
    ) -> (Option<Vec<egui::Event>>, bool) {
        if self.selection_shortcut_keys.is_empty()
            && !events.iter().any(|event| self.shortcuts.selection_change_event(event))
        {
            return (None, false);
        }
        let original = events.clone();
        let mut selection_change = false;
        let mut skipped = false;
        events.retain(|event| {
            let hold = selection_change && self.shortcuts.selection_dependent_event(event);
            if let egui::Event::Key { key, physical_key, pressed, repeat, .. } = event {
                // OS repeats do not retry a shortcut held during a selection change.
                for key in std::iter::once(key).chain(physical_key.as_ref()) {
                    if hold {
                        self.selection_shortcut_keys.insert(*key);
                    } else if !pressed || !repeat {
                        self.selection_shortcut_keys.remove(key);
                    }
                }
                if !hold && *pressed && *repeat && (self.selection_shortcut_keys.contains(key)
                    || physical_key.is_some_and(|key| self.selection_shortcut_keys.contains(&key)))
                {
                    return false;
                }
            }
            skipped |= hold;
            selection_change |= self.shortcuts.selection_change_event(event);
            !hold
        });
        ((events.len() != original.len()).then_some(original), skipped)
    }

    fn document_shortcuts<'a>(&mut self, ctx: &egui::Context, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        if self.free_transform.is_some() {
            self.free_transform_keys(ctx, doc);
            return;
        }
        if self.free_transform_frame {
            return;
        }
        let stale_selection = self.selection_gesture_stale(doc);
        if ctx.input(|i| {
            !i.focused
                || i.events
                    .iter()
                    .any(|event| matches!(event, egui::Event::WindowFocused(false)))
        }) || stale_selection || self.selection_commit_failed
        {
            // Events already in the frame may outlive focus or the selection
            // source. A failed worker reply can also arrive before shortcuts
            // are dispatched. Keep those keys held until an explicit retry.
            ctx.input(|input| {
                for event in &input.events {
                    if let egui::Event::Key {
                        key,
                        physical_key,
                        pressed,
                        ..
                    } = event
                    {
                        for key in std::iter::once(*key).chain(*physical_key) {
                            if *pressed {
                                self.selection_shortcut_keys.insert(key);
                            } else {
                                self.selection_shortcut_keys.remove(&key);
                            }
                        }
                    }
                }
            });
            if stale_selection && !self.selection_commit_failed {
                self.message =
                    "Image changed during selection — draft cancelled; retry the shortcut".into();
            }
            return;
        }
        if self.source_editor.as_ref().is_some_and(|source| source.transform_draft(doc).is_some())
            && Self::free_transform_shortcut(ctx)
            && !ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Ime(_))))
        {
            self.open_free_transform(ctx, doc);
            return;
        }
        if self.document_shortcut_typing(ctx) || self.lifecycle_frame {
            return;
        }
        // The frame snapshot predates selection edits dispatched below.
        // Preserve earlier shortcuts and require a fresh press for later ones.
        let (original, skipped) = ctx.input_mut(|input| {
            self.hold_keyboard_selection_shortcuts(&mut input.events)
        });
        self.dispatch_document_shortcuts(ctx, doc);
        if let Some(original) = original {
            ctx.input_mut(|input| input.events = original);
        }
        if skipped && !self.selection_commit_failed {
            self.message = "Press the shortcut again after the selection edit".into();
        }
    }

    fn free_transform_shortcut(ctx: &egui::Context) -> bool {
        ctx.input(|input| input.events.iter().any(|event| {
            matches!(event, egui::Event::Key {
                key: egui::Key::T, pressed: true, repeat: false, modifiers, ..
            } if (modifiers.command || modifiers.ctrl) && !modifiers.alt && !modifiers.shift)
        }))
    }

    fn dispatch_document_shortcuts(&mut self, ctx: &egui::Context, doc: DocumentView<'_>) {
        if self.edit_worker.request().is_some_and(|request| request.selection_edit) {
            if ctx.input(|input| input.pointer.primary_pressed()) { return; }
            let (events, modifiers) = ctx.input(|input| (self.selection_shortcut_events(&input.events), input.modifiers));
            self.queue_selection_shortcuts(doc, events, modifiers);
        } else {
            if Self::free_transform_shortcut(ctx) {
                self.open_free_transform(ctx, doc);
                return;
            }
            if self.tool == Tool::Selection
                && ["polygon", "magnetic"].contains(&self.selection_kind.as_str())
                && self.selection_path_pending()
            {
                // Anchor keys may continue repeating after Enter, cancellation
                // or removal of the last anchor. Preserve canvas handling, but
                // require a new press before they become document edits.
                // Canvas anchor removal accepts modified keys too; record every
                // available anchor press before its modifiers can change.
                ctx.input(|input| {
                    for event in &input.events {
                        if let egui::Event::Key {
                            key,
                            physical_key,
                            pressed: true,
                            ..
                        } = event
                        {
                            if matches!(key, egui::Key::Delete | egui::Key::Backspace) {
                                self.selection_shortcut_keys.insert(*key);
                                if let Some(key) = physical_key {
                                    self.selection_shortcut_keys.insert(*key);
                                }
                            }
                        }
                    }
                });
            }
            let (copy, cut, paste) = ctx.input(|i| {
                let (mut copy, mut cut, mut paste) = (None, false, false);
                for event in &i.events {
                    if matches!(event, egui::Event::Key { repeat: true, .. }) {
                        continue;
                    }
                    match clipboard_shortcut(event) {
                        Some(ClipboardShortcut::Copy(merged)) => {
                            copy.get_or_insert(merged);
                        }
                        Some(ClipboardShortcut::Cut) => cut = true,
                        Some(ClipboardShortcut::Paste) => paste = true,
                        None => {}
                    }
                }
                (copy, cut, paste)
            });
            let merged = copy.unwrap_or(false);
            if copy.is_some() || cut {
                if let Some(request) = self.copy_request(&self.doc_snapshot, cut, merged) {
                    self.message = self
                        .clipboard
                        .request_snapshot(request)
                        .map(|_| {
                            if cut {
                                if doc.selection().is_some() {
                                    "Cutting selected pixels…".into()
                                } else {
                                    "Cutting layers…".into()
                                }
                            } else if self.layer_clipboard && doc.selection().is_none() && !merged {
                                "Copying layers…".into()
                            } else {
                                "Copying selection…".into()
                            }
                        })
                        .unwrap_or_else(|e| e)
                        .into();
                }
            }
            if paste {
                self.message = self
                    .clipboard
                    .request_snapshot(crate::clipboard::EditRequest::Paste {
                        shared: self.shared.clone(),
                        doc: self.doc_snapshot.clone(),
                        target: self.selected.clone(),
                    })
                    .map(|_| "Pasting image…".into())
                    .unwrap_or_else(|e| e)
                    .into();
            }
            ctx.input(|i| {
                if i.events.iter().any(|event| matches!(event,
                    egui::Event::Key { key: egui::Key::B | egui::Key::Backspace, pressed: true, modifiers, .. }
                        if modifiers.alt && !modifiers.command)) {
                    let color=if self.mask {[self.mask_value,self.mask_value,self.mask_value,255]}else{self.color};
                    let mut command = json!({"color":color,"mask":self.mask,"step":self.mask_step});
                    if let Some(mut words) = self.foreground_words(doc.depth()) {
                        if self.mask {
                            words[3] = u16::MAX;
                        }
                        command["color16"] = json!(words);
                    }
                    self.layer_cmd("paint.fill", command, "Fill foreground");
                }
                if let Some(modifiers) = command_modifiers(i,egui::Key::Z) {
                    self.history_action(modifiers.shift);
                }
                if let Some(modifiers) = command_modifiers(i,egui::Key::S) {
                    self.save(modifiers.shift);
                }
                if command_key(i,egui::Key::O) {
                    self.open();
                }
                if let Some(modifiers) = command_modifiers(i,egui::Key::G) {
                    if modifiers.shift {
                        self.merge_layers(ctx, doc);
                    } else {
                        self.create_folder(doc);
                    }
                }
                if i.events.iter().any(|event| matches!(event,
                    egui::Event::Key { key: egui::Key::Delete, pressed: true, modifiers, .. }
                        if !modifiers.any()))
                    && self.tool != Tool::Pen
                    && !self.selection_path_pending()
                {
                    if doc.selection().is_some() {
                        self.edit(vec![json!({"op":"paint.clear_selection","layer":self.selected,"mask":self.mask,"step":self.mask_step,"document_id":doc.id(),"source_revision":doc.revision()})],"Clear selected pixels");
                    } else {
                        self.layer_cmd("layer.delete",json!({}),"Delete selected layers");
                    }
                }
                if let Some(mods) = command_modifiers(i,egui::Key::D) {
                    if mods.alt && !mods.shift {
                        self.edit(vec![json!({"op":"selection.reselect"})],"Reselect");
                    } else if mods.shift && !mods.alt {
                        self.deselect_area(doc, false);
                    }
                }
                if i.events.iter().any(|event| self.shortcuts.duplicate_transform_event(event)) {
                    self.duplicate_with_transform(doc);
                }
                if command_key(i,egui::Key::A) {self.edit(vec![json!({"op":"selection","kind":"rectangle","rect":[0,0,doc.width(),doc.height()]})],"Select all");}
                if command_modifiers(i,egui::Key::I).is_some_and(|mods| mods.shift && !mods.alt) {
                    self.edit(vec![json!({"op":"selection.invert"})],"Invert selection");
                }
                if command_key(i,egui::Key::J) && self.layer_clipboard {self.duplicate_layers(doc);}
                self.configured_shortcuts(i,doc);
                if i.key_pressed(egui::Key::Escape) {
                    let cancelled=self.straight.line || !self.points.is_empty();
                    self.straight.cancel();self.straight.cancelled=cancelled;self.drag_revision=None;
                    self.points.clear();self.stroke_pressures.clear();self.stroke_has_pressure=false;self.current_pressure=None;
                    self.cancel_gizmo("Transform gesture cancelled · start a new drag");
                    self.drag_start = None;
                    self.live_gesture = None;
                }
                if i.key_pressed(egui::Key::Escape) {
                    self.layer_drag = None;
                    self.eye_sweep = None;
                }
            });
        }
    }
    fn poll_clipboard(&mut self, ctx: &egui::Context) {
        let shared = self.shared.clone();
        loop {
            let Ok(engine) = shared.try_lock() else {
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            };
            let Ok(mut reply) = self.clipboard.replies.try_recv() else {
                return;
            };
            let doc = crate::engine::view::current(&engine);
            if engine.ensure_open().is_err() || doc.id() != reply.document {
                continue;
            }
            reply
                .selected_layers
                .retain(|id| doc.find_layer(id).is_some());
            reply.selected = reply.selected.filter(|id| doc.find_layer(id).is_some());
            drop(engine);
            if reply.result.is_ok() {
                if let Some(first) = reply.selected_layers.first() {
                    self.select_content(first);
                    self.selection_layers = reply.selected_layers.into_iter().collect();
                    self.layer_clipboard = true;
                } else if let Some(id) = reply.selected {
                    self.select_content(&id);
                    self.layer_clipboard = false;
                }
            }
            self.message = reply.result.unwrap_or_else(|error| error).into();
            self.last_preview = None;
        }
    }
    fn poll_capture_panel(&mut self, ctx: &egui::Context) {
        let shared = self.shared.clone();
        let root = self.workspace_root.clone();
        let (panel, document, has_mask, snapshot) = {
            let Ok(mut engine) = root.try_lock() else {
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            };
            if engine.capture_panel.is_none() {
                return;
            }
            let selected = if Arc::ptr_eq(&root, &shared) {
                None
            } else {
                let Ok(selected) = shared.try_lock() else {
                    ctx.request_repaint_after(Duration::from_millis(16));
                    return;
                };
                Some(selected)
            };
            let Ok(workspace) = self.workspace.try_lock() else {
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            };
            if !workspace.is_active_engine(&shared) {
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            let source = selected.as_deref().unwrap_or(&engine);
            let doc = crate::engine::view::current(source);
            let panel = engine.capture_panel.as_deref().unwrap();
            let snapshot = (matches!(panel, "exports" | "pen")
                || panel == "filters" && self.filter_editor.is_none())
            .then(|| source.doc_snapshot());
            let document = doc.id().to_owned();
            let has_mask = doc
                .find_layer(&self.selected)
                .is_some_and(|layer| layer.mask().is_some());
            (
                engine.capture_panel.take().unwrap(),
                document,
                has_mask,
                snapshot,
            )
        };
        // A newly opened egui window fades in during presentation. Capture
        // its settled controls without requiring a second client request.
        self.capture_ready_at =
            ctx.input(|i| i.time) + f64::from(ctx.style().animation_time).max(0.25) + 0.05;
        if panel == "saved_versions" {
            self.recover_previous_save();
        }
        if panel == "exports" {
            self.open_exports(snapshot.as_ref().unwrap());
        }
        if panel == "save_report" {
            self.open_save_report();
        } else {
            self.save_report = None;
        }
        self.read_only_info = if panel == "document_info" || panel == "document_sources" {
            let id = document.clone();
            ctx.data_mut(|data| {
                data.insert_temp(
                    egui::Id::new(("protected-source-scroll", id.as_str())),
                    panel == "document_sources",
                )
            });
            Some(id)
        } else {
            None
        };
        self.show_brush = panel == "brush";
        self.read_only_patterns = (panel == "document_patterns").then(|| document.clone());
        self.show_calibration = panel == "calibration";
        self.show_view = panel == "view";
        self.shortcuts.open = panel == "shortcuts";
        self.updates.open = panel == "updates";
        self.about.open = panel == "application_info";
        if panel == "solo_color" || panel == "solo_mask" {
            self.mask = panel == "solo_mask";
            let active = !self.mask || has_mask;
            self.set_solo(active);
            if !self.isolate {
                self.message = "Selected layer has no mask for solo viewing".into();
            }
            self.last_preview = None;
        }
        if panel == "selection" {
            self.select_tool(Tool::MagicWand);
            self.selection_kind = "wand".into();
        }
        if panel == "pen" {
            self.capture_pen(snapshot.as_ref().unwrap());
        }
        if panel == "color" {
            self.open_color(color::Target::Foreground);
        } else {
            self.color_editor = None;
        }
        if panel == "filters" && self.filter_editor.is_none() {
            self.open_filters(snapshot.as_ref().unwrap());
        }
    }
    fn draw(&mut self, ctx: &egui::Context) {
        self.shortcuts.record_key(ctx);
        self.selection_commit_failed = false;
        if (self.layer_drag.is_some() || self.eye_sweep.is_some())
            && layers::layer_gesture_interrupted(ctx)
        {
            self.layer_drag = None;
            self.eye_sweep = None;
            self.last_preview = None;
        }
        if (self.brush_tool() || self.tool == Tool::Liquify) && Self::pointer_contact_cancelled(ctx) {
            self.cancel_contact_stroke();
        }
        // Drop superseded shortcuts before polling a selection that may commit this frame.
        if self.deferred_selection_shortcuts.is_some() && ctx.input(|input| {
            !input.focused || input.pointer.primary_pressed() || input.viewport().close_requested()
                || input.key_pressed(egui::Key::Escape)
                || input.events.iter().any(|event| {
                    matches!(event, egui::Event::WindowFocused(false))
                        || self.shortcuts.selection_change_event(event)
                })
        }) {
            self.deferred_selection_shortcuts = None;
        }
        self.sampled_input_frame = self.sampled_input.is_some();
        self.poll_color_pick(ctx);
        self.poll_edit_job();
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) && self.sampled_input.is_some() {
            self.cancel_sampled_input();
        }
        if self.sampled_input.is_some()
            && ctx.input(|input| {
                !input.focused
                    || input.events.iter().any(|event| {
                        matches!(event, egui::Event::WindowFocused(false))
                    })
            })
        {
            self.sampled_focus_lost();
        }
        if self.edit_worker.pending() && ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.cancel_pending_edit();
        }
        if (self.edit_worker.pending() || self.sampled_input_pending())
            && ctx.input(|input| input.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.message = "Finish or discard pending edits before closing PeerBrush".into();
        }
        if self.update_exit_committed() {
            return;
        }
        if self.tool != Tool::Brush
            || ctx.input(|input| {
                !input.focused
                    || !input.modifiers.alt
                    || !input.pointer.primary_down()
                    || input.pointer.primary_pressed()
                    || input.key_pressed(egui::Key::Escape)
            })
        {
            self.alt_sample = None;
        }
        self.free_transform_frame = self.free_transform.is_some();
        self.updates_tick(ctx);
        if self.update_exit_committed() {
            return;
        }
        self.about.dismiss_escape(ctx);
        self.fonts.refresh(ctx);
        let drawn_generation = self.canvas_generation;
        self.sync_tool_profile(ctx);
        self.lifecycle_frame =
            self.save_loss_review.is_some() || self.show_new || self.edit_worker.pending() || self.sampled_input_pending();
        if !self.lifecycle_frame && self.free_transform.is_none() {
            self.temporary_pen_tool(ctx);
        }
        if let Some(warning) = self.workspace.lock().unwrap().open_warning.take() {
            self.open_file_warning = Some(warning);
        }
        if let Some(warning) = self.open_file_warning.clone() {
            egui::Window::new("File already open")
                .id(egui::Id::new("duplicate_file_warning"))
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    dialogs::body(ui, 440.);
                    dialogs::heading(ui, "File already open", true);
                    ui.label(warning);
                    ui.add_space(3.);
                    if ui.add(dialogs::primary("OK")).clicked() {
                        self.open_file_warning = None;
                    }
                });
        }
        self.poll_capture_panel(ctx);
        {
            if let Ok(mut e) = self.workspace_root.try_lock() {
                if e.focus_requested {
                    e.focus_requested = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
            }
        }
        let screenshots = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| {
                    let egui::Event::Screenshot { user_data, image, .. } = event else {
                        return None;
                    };
                    let path = user_data.data.as_ref()?.downcast_ref::<PathBuf>()?;
                    Some((path.clone(), image.clone()))
                })
                .collect::<Vec<_>>()
        });
        for (path, image) in screenshots {
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            if let Ok(png) = crate::raster::png(image.size[0] as u32, image.size[1] as u32, &bytes) {
                let _ = server::atomic_write(&path, &png);
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
                        .unwrap_or_else(|e| e)
                        .into();
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
                            crate::engine::view::current(&e).id() == reply.document && crate::engine::view::current(&e).find_layer(&id).is_some()
                        };
                        if still_present {
                            self.select_content(&id);
                        }
                    }
                    self.message = "Merged layers".into();
                    self.last_preview = None;
                }
                Err(error) => {
                    self.message = error.into();
                }
            }
        }
        if self.connecting {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        self.poll_clipboard(ctx);
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
                    let needs_choice = files.iter().any(|f| f.choice().is_some());
                    self.pending_import = Some(PendingImport {
                        project: reply.project,
                        document: reply.document,
                        revision: reply.revision,
                        native: reply.native,
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
                        self.message = error.into();
                    } else if let Some(view) = self.project_views.get_mut(&reply.project) {
                        view.message = error.into();
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
            if self.recovery.record_job.as_deref() == Some(reply.project.as_str()) {
                self.recovery.finish_record_job(
                    result.as_ref().err().cloned(),
                    &self.workspace,
                    &self.connection.state_dir,
                    ctx,
                );
            }
            if reply.project != self.project_id {
                if let Some(view) = self.project_views.get_mut(&reply.project) {
                    view.message = result.unwrap_or_else(|e| e).into();
                }
                continue;
            }
            if result
                .as_ref()
                .is_ok_and(|msg| msg.starts_with("Imported ") || msg.starts_with("Added "))
            {
                let first = { let engine = self.shared.lock().unwrap(); crate::engine::view::current(&engine).layer(0).map(|layer| layer.id().to_owned()) };
                if let Some(id) = first {
                    self.selected = id.clone();
                    self.selection_layers = [id.clone()].into_iter().collect();
                    self.selection_anchor = id;
                    self.mask = false;
                    self.mask_step = None;
                }
            }
            self.message = result.unwrap_or_else(|e| e).into();
            self.last_preview = None;
        }
        if self.save_loss_review.is_some() {
            self.save_loss_dialog(ctx);
        } else {
            self.lifecycle_dialog(ctx);
        }
        // eframe treats any CancelClose as final for this native close frame.
        // Validate all projects first; a clean approved exit needs no cancellation.
        if ctx.input(|input| input.viewport().close_requested()) && !self.exit_approved {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
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
        if self.parameter_gesture.is_some() && self.shared.try_lock().is_err() {
            show_busy_project(ctx);
            return;
        }
        pattern_source::poll(self, ctx);
        if ctx.input(|i| {
            i.pointer.any_pressed()
                || (!i.pointer.any_down()
                    && i.events.iter().any(|event| {
                        matches!(event, egui::Event::Key { pressed: true, repeat: false, .. })
                    }))
        }) {
            self.cancelled_parameter = None;
        }
        let parameter_focus_lost = ctx.input(|i| {
            !i.focused
                || i.events
                    .iter()
                    .any(|event| matches!(event, egui::Event::WindowFocused(false)))
        });
        let point_release = self.parameter_gesture.as_ref()
            .filter(|edit| edit.typing.is_none() && controls::point_release_pending(ctx, edit.control))
            .map(|edit| edit.control);
        if self.lifecycle.is_some() || self.lifecycle_frame {
            // The review owns keyboard/canvas input for the entire frame.
        } else if parameter_focus_lost || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.finish_parameter(true);
        } else if point_release.is_none() && !ctx.input(|i| i.pointer.any_down())
            && !self.parameter_gesture.as_ref().is_some_and(|edit| {
                edit.typing
                    .as_ref()
                    .is_some_and(|ctx| controls::number_editing(ctx, edit.control))
            })
        {
            self.finish_parameter(false);
        }
        let frame = {
            let Ok(mut engine) = self.shared.try_lock() else {
                show_busy_project(ctx);
                return;
            };
            engine.expire();
            let mut frame = DocumentFrame::capture(&engine);
            frame.proposal = self.proposal_review.as_deref()
                .map(|id| history::ProposalFrame::capture(&engine, id));
            frame
        };
        self.draw_document(ctx, frame, drawn_generation, point_release);
    }
    fn draw_document(
        &mut self,
        ctx: &egui::Context,
        frame: DocumentFrame,
        drawn_generation: u64,
        point_release: Option<egui::Id>,
    ) {
        self.doc_snapshot = frame.document.clone();
        let doc = frame.document.view();
        if self.folder_view_document != doc.id() {
            if !self.folder_view_document.is_empty() {
                self.collapsed.clear();
            }
            self.collapsed.extend(doc.layers().filter(|layer| layer.photoshop_folder_is_closed())
                .map(|layer| layer.id().to_owned()));
            doc.id().clone_into(&mut self.folder_view_document);
        }
        self.poll_edit_job();
        self.process_sampled_input(ctx);
        self.cancel_interrupted_text_drag(ctx, doc);
        self.sync_free_transform(ctx, doc);
        if self.lifecycle.is_none() && !self.lifecycle_frame {
            self.import_dialog(ctx);
            self.image_open_dialog(ctx);
            self.open_activated_file(ctx);
        }
        if self.rename_edit.as_ref().is_some_and(|edit| {
            edit.document != doc.id() || !doc.layers().any(|l| l.id() == edit.layer)
        }) {
            self.rename_edit = None;
        }
        let time = ctx.input(|i| i.time);
        if self.animation.observe(&self.doc_snapshot, frame.ai_change, time) {
            // Reveal the destination so an AI hierarchy move remains visible.
            for scope in self.animation.scopes(time) {
                let mut parent = scope.target.and_then(|id| {
                    doc.layers()
                        .find(|l| l.id() == id)
                        .and_then(|l| l.parent().map(str::to_owned))
                });
                for _ in 0..16 {
                    let Some(id) = parent else { break };
                    self.collapsed.remove(&id);
                    parent = doc.layers()
                        .find(|l| l.id() == id)
                        .and_then(|l| l.parent().map(str::to_owned));
                }
            }
            ctx.request_repaint();
        }
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.interrupt_animation();
        }
        if self.parameter_gesture.as_ref().is_some_and(|edit| {
            edit.engine.as_ptr() != Arc::as_ptr(&self.shared)
                || edit.document != doc.id()
                || edit.revision != doc.revision()
        }) {
            self.finish_parameter(true);
            self.message = "Image changed during parameter edit · preview cancelled".into();
        }
        self.effect_expanded.retain(|(layer, mask, effect)| {
            doc.layers()
                .find(|entry| entry.id() == layer)
                .is_some_and(|entry| {
                    if *mask {
                        entry.mask().is_some_and(|channel| {
                            (0..channel.step_count()).any(|i| channel.step(i).unwrap().id() == effect)
                        })
                    } else {
                        (0..entry.effect_count()).any(|i| entry.effect(i).unwrap().id() == effect)
                    }
                })
        });
        if let Some((worker, resident)) = self.thumb_worker.as_ref().zip(doc.resident()) {
            worker.document(resident);
        }
        if self.thumb_document != doc.id() || self.thumb_native != doc.native().is_some() {
            self.retouch_source = None;
            self.retouch_revision = None;
            self.retouch_pick = false;
            self.thumbs.clear();
            self.thumb_pending.clear();
            self.thumb_document = doc.id().to_owned();
            self.thumb_native = doc.native().is_some();
        }
        while let Some(reply) = self.thumb_worker.as_ref().and_then(|worker| worker.replies.try_recv().ok()) {
            if self.thumb_pending.get(&reply.key) == Some(&reply.revision) {
                self.thumb_pending.remove(&reply.key);
            }
            if doc.resident().is_some() && reply.document == doc.id() && reply.revision == doc.revision() {
                if let Some((w, h, bytes)) = reply.pixels {
                    let image =
                        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &bytes);
                    if let Some((revision, texture)) = self.thumbs.get_mut(&reply.key) {
                        texture.set(image, egui::TextureOptions::LINEAR);
                        *revision = doc.revision();
                    } else {
                        self.thumbs.insert(
                            reply.key.clone(),
                            (
                                doc.revision(),
                                ctx.load_texture(reply.key, image, egui::TextureOptions::LINEAR),
                            ),
                        );
                    }
                }
            }
        }
        if self.selection_layers.len() > 1 {
            let present: HashSet<_> = doc.layers()
                .filter_map(|layer| self.selection_layers.contains(layer.id()).then_some(layer.id()))
                .collect();
            self.selection_layers.retain(|id| present.contains(id.as_str()));
        } else {
            self.selection_layers.retain(|id| doc.find_layer(id).is_some());
        }
        let selected_exists = doc.find_layer(&self.selected).is_some();
        if self.selection_layers.is_empty() && selected_exists {
            self.selection_layers.insert(self.selected.clone());
        }
        if !selected_exists {
            self.set_solo(false);
            self.selected = doc.layer(0).map(|l| l.id().to_owned()).unwrap_or_default();
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
            if self.file_drop_on_tabs(ctx) {
                self.drop_project_file(dropped);
            } else {
                self.drop_files(dropped);
            }
        }
        self.file_drop_feedback(ctx);
        self.replay_selection_shortcuts(ctx, doc);
        let held_selection_shortcuts = self.hold_canvas_selection_shortcuts(ctx, doc);
        let point_release = point_release.filter(|control| self.parameter_gesture.as_ref().is_some_and(|edit| edit.control == *control));
        let deferred_shortcuts = self.defer_canvas_shortcuts(ctx, doc, point_release.is_some());
        let selection_release = point_release.is_none()
            && matches!(self.tool, Tool::Selection | Tool::MagicWand);
        let show_held_shortcut_message = held_selection_shortcuts.is_some()
            && deferred_shortcuts
                .as_ref()
                .is_none_or(|(_, late, _)| late.is_empty());
        self.document_shortcuts(ctx, doc);
        egui::TopBottomPanel::top("context")
            .exact_height(48.0)
            .show(ctx, |ui| {
                if self.lifecycle.is_some() || self.lifecycle_frame {
                    ui.disable();
                }
                ui.horizontal_centered(|ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.;
                    egui::menu::menu_custom_button(ui, egui::Button::new("File").corner_radius(egui::CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 }), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.;
                        if ui.button("New canvas").clicked() {
                            self.open_new_canvas();
                            ui.close_menu();
                        }
                        if ui.button("Open PSD/PSB").clicked() {
                            self.open();
                            ui.close_menu();
                        }
                        if ui.button("Recover autosaved work…").clicked() {
                            self.recovery.refresh(&self.workspace, &self.connection.state_dir, ctx, true);
                            ui.close_menu();
                        }
                        if ui.button("Save PSD/PSB").clicked() {
                            self.save(false);
                            ui.close_menu();
                        }
                        if ui.add_enabled(frame.has_save_path && !self.busy, egui::Button::new("Recover previous save…")).clicked() {
                            self.recover_previous_save();
                            ui.close_menu();
                        }
                        if ui.button("Save As...").clicked() {
                            self.save(true);
                            ui.close_menu();
                        }
                        if ui.button("PSD compatibility…").clicked() {
                            self.open_save_report();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Import images...").clicked() {
                            self.import();
                            ui.close_menu();
                        }
                        if ui.button("Export PNG...").clicked() {
                            self.export(crate::production_export::Format::Png);
                            ui.close_menu();
                        }
                        if ui.button("Export WebP (8-bit)…").clicked() {
                            self.export(crate::production_export::Format::Webp);
                            ui.close_menu();
                        }
                        if ui.button("Export JPEG XL (lossless)…").clicked() {
                            self.export(crate::production_export::Format::Jxl);
                            ui.close_menu();
                        }
                        if ui.button("Production export…").clicked() {
                            self.open_exports(doc);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Settings…").clicked() {
                            self.preview_quality.open = true;
                            ui.close_menu();
                        }
                        if ui.button("Keyboard and pen shortcuts…").clicked() {
                            self.shortcuts.open = true;
                            ui.close_menu();
                        }
                        self.associations.menu(ui);
                        if ui.button("Updates…").clicked() {
                            self.updates.open = true;
                            ui.close_menu();
                        }
                        self.about.menu(ui);
                    });
                    if ui.add(egui::Button::new("Canvas view…").corner_radius(egui::CornerRadius::same(0))).clicked() {
                        self.show_view = true;
                    }
                    egui::menu::menu_custom_button(ui, egui::Button::new("Image").corner_radius(egui::CornerRadius::same(0)),|ui| {
                        ui.spacing_mut().item_spacing.x = 8.;
                        if ui.add_enabled(!doc.read_only()&&!self.busy,egui::Button::new("Whole-image filters…")).clicked(){self.open_filters(doc);ui.close_menu();}
                        if doc.filter_count() != 0{ui.menu_button("Filter stack",|ui|self.document_filter_stack(ui,doc));}
                        for (label,mode) in [("Crop…","crop"),("Canvas size…","canvas.resize"),("Image size…","image.resize")] {
                            if ui.add_enabled(!doc.read_only() && !self.busy,egui::Button::new(label)).clicked() {self.open_geometry(doc,mode);ui.close_menu();}
                        }
                    });
                    egui::menu::menu_custom_button(ui, egui::Button::new("Layer").corner_radius(egui::CornerRadius::same(0)),|ui| {
                        ui.spacing_mut().item_spacing.x = 8.;
                        if ui.add_enabled(!self.busy&&!doc.read_only(),egui::Button::new("Free Transform...").shortcut_text(if cfg!(target_os="macos") {"Cmd+T"}else{"Ctrl+T"})).clicked(){self.open_free_transform(ctx,doc);ui.close_menu();}
                        let duplicate = egui::Button::new(shortcuts::DUPLICATE_TRANSFORM)
                            .shortcut_text(self.shortcuts.duplicate_transform_shortcut());
                        if ui.add_enabled(self.can_duplicate_with_transform(doc), duplicate)
                            .on_disabled_hover_text("Finish the active edit and select editable content")
                            .clicked() {
                            self.duplicate_with_transform(doc);
                            ui.close_menu();
                        }
                        if ui.add_enabled(!self.busy&&!doc.read_only(),egui::Button::new("Group layers").shortcut_text(if cfg!(target_os="macos") {"⌘ G"} else {"Ctrl+G"})).clicked(){self.create_folder(doc);ui.close_menu();}
                        if ui.add_enabled(!self.busy&&!doc.read_only(),egui::Button::new("Merge layers").shortcut_text(if cfg!(target_os="macos") {"⌘ ⇧ G"} else {"Ctrl+Shift+G"})).clicked(){self.merge_layers(ctx,doc);ui.close_menu();}
                        ui.separator();
                        let selected_text = doc.find_layer(&self.selected).is_some_and(|layer| layer.source_kind() == Some("text"));
                        let edit_source = if selected_text { "Edit text on canvas" } else { "Edit vector properties…" };
                        for (label,kind) in [("New text…","text"),("New vector shape…","shape"),(edit_source,"edit")] {
                            let enabled=!doc.read_only()&&!self.busy && (kind!="edit"||doc.layers().any(|l|l.id()==self.selected&&l.source().is_some()));
                            if ui.add_enabled(enabled,egui::Button::new(label)).clicked() {
                                if kind == "text" {
                                    self.select_tool(Tool::Text);
                                    self.message = "Click or drag on the canvas to place text".into();
                                } else {
                                    if kind == "edit" && selected_text {
                                        self.select_tool(Tool::Text);
                                    }
                                    self.open_source(doc,kind);
                                }
                                ui.close_menu();
                            }
                        }
                        if ui.add_enabled(!doc.read_only()&&doc.layers().any(|l|l.id()==self.selected&&l.source().is_some()),egui::Button::new("Rasterize text/vector")).on_hover_text("Keep the current pixels and remove editable properties · Undo restores the source").clicked(){self.layer_cmd("source.rasterize",json!({}),"Rasterize text/vector");ui.close_menu();}
                    });
                    egui::menu::menu_custom_button(ui, egui::Button::new("Select").corner_radius(egui::CornerRadius { nw: 0, sw: 0, ne: 6, se: 6 }),|ui| {
                        ui.spacing_mut().item_spacing.x = 8.;
                        let learned = crate::segmentation::configured() && !doc.read_only() && !self.busy;
                        if ui.add_enabled(learned, egui::Button::new("Select subject")).on_hover_text("Uses the externally configured local selection provider").clicked(){self.segment_subject(doc,false);ui.close_menu();}
                        if ui.add_enabled(learned && doc.selection().is_some(), egui::Button::new("Select subject in region")).clicked(){self.segment_subject(doc,true);ui.close_menu();}
                        if ui.add_enabled(doc.selection().is_some()&&!doc.read_only(),egui::Button::new("Refine selection…")).clicked(){self.open_refinement(doc,None);ui.close_menu();}
                        if ui.add_enabled(doc.selection().is_some()&&!doc.read_only(),egui::Button::new("Mask from selection")).clicked(){self.layer_cmd("mask.from_selection",json!({}),"Mask from selection");ui.close_menu();}
                        if ui.add_enabled(doc.layers().any(|l|l.id()==self.selected&&l.mask().is_some())&&!doc.read_only(),egui::Button::new("Refine layer mask…")).clicked(){self.open_refinement(doc,Some(self.selected.clone()));ui.close_menu();}
                        for (label,op) in [("All · Ctrl+A","all"),("Deselect · Ctrl+Shift+D","selection.clear"),("Reselect · Ctrl+Alt+D","selection.reselect"),("Inverse","selection.invert")] {
                            let button=egui::Button::new(label);
                            let button=if op=="selection.invert" {button.shortcut_text(if cfg!(target_os="macos") {"⌘ ⇧ I"} else {"Ctrl+Shift+I"})} else {button};
                            if ui.add(button).clicked(){if op=="selection.clear" {self.deselect_area(doc,false);}else{self.edit(vec![if op=="all"{json!({"op":"selection","kind":"rectangle","rect":[0,0,doc.width(),doc.height()]})}else{json!({"op":op})}],label);}ui.close_menu();}
                        }
                        ui.add_enabled_ui(doc.selection().is_some() && !doc.read_only() && !self.busy, |ui| {
                            for (label, icon, mode, help) in [
                                ("Expand by 1 px", Icon::SelectionExpand, "expand", "Grow the current selection by one pixel."),
                                ("Contract by 1 px", Icon::SelectionContract, "contract", "Shrink the current selection by one pixel."),
                            ] {
                                if icons::menu_choice(ui, icon, label, None)
                                    .on_hover_text(format!("{help} Refine selection previews larger changes.")).clicked()
                                {
                                    self.edit(vec![json!({"op":"selection.modify","mode":mode,"radius":1})], label);
                                    ui.close_menu();
                                }
                            }
                        });
                    });
                    });
                    if ui
                        .add_enabled_ui(!self.busy, |ui| {
                            icons::button(ui, Icon::Save, "Save PSD/PSB · Ctrl/Cmd+S")
                        })
                        .inner
                        .on_hover_text("Save PSD/PSB · Ctrl/Cmd+S")
                        .clicked()
                    {
                        self.save(false);
                    }
                    ui.separator();
                    let (_, icon) = ui.allocate_space(Vec2::splat(24.0));
                    paint_tool(ui.painter(), icon, self.tool, if ui.is_enabled() { ACCENT } else { Color32::from_gray(100) });
                    ui.label(RichText::new(self.tool_caption(self.tool)).size(12.0));
                    ui.add_space(6.0);
                    if self.mask {
                        ui.label(RichText::new("MASK").size(10.0).color(ACCENT));
                    }
                    if [Tool::Brush, Tool::Eraser, Tool::Smudge, Tool::Clone, Tool::Heal].contains(&self.tool) {
                        controls::label(ui, "Size");
                        let time=ui.input(|i|i.time);
                        let mut size=self.radius*2.;
                        let display=self.tool_display_number("radius",self.radius,time)*2.;
                        if tool_settings::number(ui,"brush size",&mut size,display,1.0..=1024.,130.," px",0,true,self.tool_preferences_ai(time)).changed() {self.radius=size/2.;}
                        self.toolbar_brush_opacity(ui);
                        if icons::button(ui, Icon::Adjust, "Brush settings").clicked() {
                            self.show_brush = !self.show_brush;
                        }
                    }
                    if self.tool == Tool::Gradient {
                        self.gradient_toolbar(ui);
                    }
                    if self.tool == Tool::Liquify {
                        let time=ui.input(|i|i.time);
                        let radius=self.tool_display_number("radius",self.radius,time);
                        let strength=self.tool_display_number("opacity",self.liquify.strength,time);
                        let ai=self.tool_preferences_ai(time);
                        liquify::toolbar(ui, &mut self.radius, &mut self.liquify,radius,strength,ai);
                    }
                    self.transform_snap_toolbar(ui, doc);
                    self.pen_toolbar(ui,doc);
                    self.retouch_toolbar(ui);
                    if self.busy {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spinner();
                        });
                    }
                });
            });
        if self.tool == Tool::Text
            || self.free_transform.as_ref().is_some_and(|editor| editor.edits_text_draft())
        {
            let text_toolbar_height = match self.source_editor.as_ref() {
                Some(editor) if editor.canvas_text && editor.show_field_geometry => 106.,
                Some(editor) if editor.canvas_text => 76.,
                _ => 40.,
            };
            egui::TopBottomPanel::top("text context")
                .exact_height(text_toolbar_height)
                .show(ctx, |ui| {
                    if self.lifecycle.is_some() || self.lifecycle_frame {
                        ui.disable();
                    }
                    self.text_toolbar(ui, doc);
                });
        }
        if matches!(self.tool, Tool::Selection | Tool::MagicWand) {
            egui::TopBottomPanel::top("selection context")
                .exact_height(40.)
                .show(ctx, |ui| {
                    if self.lifecycle.is_some() || self.lifecycle_frame {
                        ui.disable();
                    }
                    if self.animation.layer("@selection", ui.input(|i| i.time)) {
                        ui.style_mut().visuals.selection.stroke.color = AI_BLUE;
                    }
                    self.selection_toolbar(ui, doc);
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
                self.transform_controls(&mut modes, doc);
                ui.set_max_width((bounds.width()-160.).max(0.));
                ui.horizontal_centered(|ui| {
                    ui.add(egui::Label::new(RichText::new(format!("{} × {} · {} bit", doc.width(), doc.height(), doc.depth())).color(MUTED)).sense(egui::Sense::click())).on_hover_text("Right-click for project settings").context_menu(|ui| {
                        if ui.button("Project dimensions and depth…").clicked() {
                            self.project_settings = Some(new_canvas::ProjectDimensions::new(doc));
                            ui.close_menu();
                        }
                        if !doc.read_only() && doc.global_light_angle().is_some() && ui.button("Global Light…").clicked() {
                            self.show_global_light = true;
                            ui.close_menu();
                        }
                    });
                    ui.separator();
                    filename::label(ui, &format!(
                            "{}{}",
                            doc.name(),
                            if frame.saved_revision != doc.revision() {
                                " *"
                            } else {
                                ""
                            }
                        ), 11., MUTED, 300.,
                    );
                    ui.separator();
                    ui.label(RichText::new(self.message.as_str()).size(11.0));
                    if doc.read_only() {
                        ui.label(RichText::new("READ ONLY").size(10.0).color(ACCENT))
                            .on_hover_text(doc.warnings().join("\n"));
                        if ui.small_button("Why read only?").clicked() {
                            self.read_only_info = Some(doc.id().to_owned());
                        }
                        let profiled = doc.profile().is_some();
                        let convertible = !profiled || doc.profile().is_some_and(|p| crate::color_profile::supported(p).is_ok());
                        let label = if profiled { "Convert to sRGB copy".into() } else { format!("Edit {}-bit copy", doc.depth()) };
                        let hint = if profiled { "Convert the saved artwork to sRGB at its original bit depth. Out-of-gamut colors are clipped; Photoshop layers are flattened. Save under a new name." } else { "Create a flattened editable copy at the original color precision. Unsupported Photoshop layers are flattened; the source file stays untouched." };
                        if ui.add_enabled(convertible, egui::Button::new(label)).on_hover_text(hint).clicked() {
                            let shared = self.shared.clone();
                            let revision = doc.revision();
                            let document = doc.id().to_owned();
                            self.job(move || {
                                server::compatible_copy_guarded(&shared, "human", Some(revision), profiled, Some(&document))?;
                                Ok("Editing a flattened copy at original precision · save under a new name".into())
                            });
                        }
                    }
                    if let Some((description, human)) = &frame.reservation {
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
                            ui.label(RichText::new(description).color(
                                if *human {
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
                    // Tool clicks must not drag the strip when it overflows.
                    .drag_to_scroll(false)
                    .auto_shrink([false, false])
                    .max_height((ui.available_height() - 70.0).max(60.0))
                    .show(ui, |ui| {
                        let mut buttons = Vec::with_capacity(16);
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
                            Tool::Pen,
                            Tool::Text,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            if index == 6 {
                                ui.add_space(4.0);
                                ui.separator();
                                ui.add_space(4.0);
                            }
                            let caption = self.tool_caption(tool);
                            let r = ui
                                .add_sized([42.0, 39.0], egui::Button::new("").frame(false))
                                .on_hover_text(caption.as_str());
                            if r.clicked() {
                                self.select_tool(tool);
                                ui.ctx().request_repaint();
                            }
                            buttons.push((tool, r, caption));
                        }
                        // Resolve input before painting either the old or new selection.
                        for (tool, r, caption) in buttons {
                            r.widget_info(|| {
                                egui::WidgetInfo::selected(egui::WidgetType::RadioButton, r.enabled(), self.tool == tool, &caption)
                            });
                            if self.tool == tool {
                                ui.painter()
                                    .rect_filled(r.rect, 6, Color32::from_rgb(74, 48, 71));
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
                                    | (Some("pen"), Tool::Pen)
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
                                if !ui.is_enabled() {
                                    Color32::from_gray(100)
                                } else if ai_active {
                                    AI_BLUE
                                } else if self.tool == tool {
                                    ACCENT
                                } else {
                                    Color32::from_rgb(245, 242, 240)
                                },
                            );
                            icons::paint_focus(ui, &r);
                        }
                    });
                ui.add_space((ui.available_height() - 64.0).max(0.0));
                self.palette(ui);
            });
        self.sync_solo(doc);
        let reserved = crate::engine::view::reserved_layers(doc, &frame.ai_scopes);
        egui::SidePanel::right("layer_panel")
            .default_width(320.0)
            .min_width(300.0)
            .max_width(470.0)
            .show(ctx, |ui| {
                if self.lifecycle.is_some() || self.lifecycle_frame {
                    ui.disable();
                }
                let display = doc.resident().and_then(|resident| self.pending_parameter_document(ui.ctx(), resident));
                if reserved.contains(self.selected.as_str()) || self.animation.layer(&self.selected, time) {
                    ui.style_mut().visuals.selection.stroke.color = AI_BLUE;
                }
                self.layers(ui, display.as_deref().map(DocumentView::from).unwrap_or(doc), &reserved);
            });
        if self.lifecycle.is_none() && !self.lifecycle_frame {
            self.global_light_window(ctx, doc);
        }
        if point_release.is_some_and(|control| self.parameter_gesture.as_ref().is_some_and(|edit| edit.control == control)) {
            self.finish_parameter(false);
        }
        self.sync_solo(doc);
        let shortcut_modifiers = deferred_shortcuts
            .as_ref()
            .map(|(_, _, release_modifiers)| {
                ctx.input_mut(|i| {
                    let original = i.modifiers;
                    i.modifiers = *release_modifiers;
                    original
                })
            });
        // Only an edit committed by this canvas pass can authorize its late shortcuts.
        self.selection_commit_revision = None;
        let edit_pending_before_canvas = self.edit_worker.pending();
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
                        preview.layers,
                        preview.bit_depth,
                        if self.tab_transfer.as_ref().is_some_and(|s| s.move_layers) {
                            "Move preview"
                        } else {
                            "Copy preview · Shift to move"
                        }
                    ));
                    let [width, height] = texture.size();
                    let size = Vec2::new(width as f32, height as f32);
                    let scale = (ui.available_width() / size.x)
                        .min(ui.available_height() / size.y)
                        .min(1.);
                    let (rect, _) = ui.allocate_exact_size(size * scale, egui::Sense::hover());
                    texture.paint(ui.painter(), |[x, y]| rect.min + rect.size() * Vec2::new(x, y));
                    return;
                }
                let display = if doc.resident().is_some() && !self.proposal_original {
                    frame.proposal.as_ref()
                        .filter(|proposal| self.proposal_review.as_deref() == Some(proposal.id.as_str()))
                        .and_then(|proposal| self.resolve_proposal_document(proposal.resident_draft()))
                } else {
                    None
                };
                self.canvas(
                    ui,
                    display.as_deref().map(DocumentView::from).unwrap_or(doc),
                    &frame.ai_scopes,
                    &frame.human_regions,
                );
            });

        if let Some(modifiers) = shortcut_modifiers {
            ctx.input_mut(|i| i.modifiers = modifiers);
        }
        if let Some((original, mut late, _)) = deferred_shortcuts {
            let current = self.shared.lock().unwrap().doc_snapshot();
            if current.view().id() == doc.id() && self.tab_transfer.is_none() && !self.selection_commit_failed {
                if self.edit_worker.pending() {
                    if point_release.is_some() {
                        self.message = "Parameter edit is applying. Retry the shortcut after it finishes.".into();
                    } else if !edit_pending_before_canvas && !self.document_shortcut_typing(ctx) {
                        let (_, skipped) = self.hold_keyboard_selection_shortcuts(&mut late);
                        if skipped {
                            self.message = "Press the shortcut again after the selection edit".into();
                        }
                        let events = self.selection_shortcut_events(&late);
                        self.queue_selection_shortcuts(current.view(), events, ctx.input(|input| input.modifiers));
                    }
                } else {
                    let committed = self.selection_commit_revision == Some(current.view().revision());
                    if selection_release && (!committed || current.view().selection().is_none()) {
                        let count = late.len();
                        late.retain(|event| {
                            if !self.shortcuts.selection_dependent_event(event) {
                                return true;
                            }
                            if let egui::Event::Key { key, physical_key, .. } = event {
                                self.selection_shortcut_keys.extend(std::iter::once(*key).chain(*physical_key));
                            }
                            false
                        });
                        if late.len() != count && late.is_empty() {
                            self.message = if committed {
                                "Selection is empty; shortcut skipped".into()
                            } else {
                                "No current selection result; press the shortcut again".into()
                            };
                        }
                    }
                    ctx.input_mut(|i| i.events = late);
                    self.doc_snapshot = current.clone();
                    self.document_shortcuts(ctx, current.view());
                    ctx.request_repaint();
                }
            }
            ctx.input_mut(|i| i.events = original);
        }
        if let Some(original) = held_selection_shortcuts {
            ctx.input_mut(|i| i.events = original);
            if show_held_shortcut_message && !self.selection_commit_failed {
                self.message = if self.tool == Tool::Selection
                    && ["polygon", "magnetic"].contains(&self.selection_kind.as_str())
                {
                    if !self.selection_path_pending() {
                        "Selection path ended; retry the shortcut"
                    } else {
                        "Finish or cancel the selection path, then retry the shortcut"
                    }
                } else if ctx.input(|i| i.pointer.primary_down()) {
                    "Release the selection gesture, then retry the shortcut"
                } else {
                    "Selection gesture ended; retry the shortcut"
                }
                .into();
            }
        }

        if self.tab_transfer.is_none() {
            self.request_preview(ctx, doc);
            // The canvas was already painted before the preview was presented.
            if drawn_generation != self.canvas_generation {
                ctx.request_repaint();
            }
        }
        self.dispatch_sampled_input(ctx);
        self.sampled_input_window(ctx);
        self.magnetic_input_window(ctx);
        if self.lifecycle.is_some() || self.lifecycle_frame {
            if self.edit_worker.pending() {
                if self.free_transform_pending() {
                    self.free_transform_keys(ctx, doc);
                    self.free_transform_window(ctx, doc, drawn_generation);
                }
                if self.filter_apply_pending() {
                    self.filters_window(ctx, doc);
                }
                if self.geometry_pending() {
                    self.geometry_window(ctx, doc);
                }
                if self.refinement_pending() {
                    self.refinement_window(ctx, doc);
                }
            }
            if self.lifecycle.is_none() && self.save_loss_review.is_none() {
                self.new_canvas_window(ctx);
            }
            self.finish_capture(ctx, drawn_generation);
            // Shared-state requests must remain observable while the dialog owns input.
            ctx.request_repaint_after(Duration::from_millis(200));
            return;
        }
        if self.liquify_effect.as_ref().is_some_and(|id| {
            !doc.layers()
                .find(|l| l.id() == self.selected)
                .is_some_and(|l| {
                    (0..l.effect_count()).filter_map(|i| l.effect(i)).any(|e| e.id() == id && e.kind() == "liquify" && e.enabled())
                })
        }) {
            self.liquify_effect = None;
        }
        self.project_dimensions_window(ctx, doc);
        if self.show_brush {
            self.brush_settings(ctx);
        }
        self.color_window(ctx);
        self.task_history_review(ctx, doc);
        self.proposal_window(ctx, doc, frame.proposal.as_ref());
        self.refinement_window(ctx, doc);
        self.geometry_window(ctx, doc);
        self.free_transform_window(ctx, doc, drawn_generation);
        self.source_window(ctx, doc);
        self.filters_window(ctx, doc);
        self.exports_window(ctx, doc);
        self.protection_window(ctx, doc);
        self.patterns_window(ctx, doc);
        self.compatibility_window(ctx, doc);
        self.associations.show(ctx, &self.logo);
        self.new_canvas_window(ctx);
        if self.show_connection {
            let mut open = true;
            egui::Window::new("AI connection settings").open(&mut open).default_width(550.0).show(ctx, |ui| {
                ui.label("Connect AI handles discovery and installed clients. Manual configuration is available here for other clients.");
                if !self.connect_feedback.is_empty() { ui.label(&self.connect_feedback); }
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.connection_codex, true, "Codex");
                    ui.selectable_value(&mut self.connection_codex, false, "Other clients");
                });
                let exe = match crate::packaged::mcp_command() {
                    Ok(path) => path.to_string_lossy().into_owned(),
                    Err(error) => { ui.label(error); return; }
                };
                ui.label(RichText::new("MCP is ready").color(Color32::from_rgb(166, 215, 157)).strong());
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
        self.saved_versions_window(ctx);
        self.save_progress_windows(ctx);
        let loading = self
            .load_control
            .clone()
            .or(frame.loading);
        if let Some(control) = loading {
            if let Some((w, h, bytes)) = control.take_preview() {
                self.load_preview = Some(ctx.load_texture(
                    "loading saved image",
                    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &bytes),
                    egui::TextureOptions::LINEAR,
                ));
            }
            let status = control.status();
            egui::Window::new("Opening Photoshop document")
                .collapsible(false)
                .resizable(false)
                .default_width(350.)
                .show(ctx, |ui| {
                    ui.label(if status.stage.is_empty() {
                        "Choose a PSD or PSB file"
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
        self.pressure_calibration(ctx);
        self.canvas_view_window(ctx, doc);
        self.shortcuts_window(ctx);
        self.preview_quality_window(ctx);
        self.updates_dialog(ctx);
        self.about.show(ctx);
        self.finish_capture(ctx, drawn_generation);
        self.sync_tool_profile(ctx);
        if self.tool_preferences_ai(ctx.input(|i| i.time)) {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        // Preview workers wake the UI when results arrive. Background project
        // activity still needs the workspace's periodic refresh.
        if self.busy || !self.points.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
    fn global_light_window(&mut self, ctx: &egui::Context, doc: DocumentView<'_>) {
        let Some(angle) = doc.global_light_angle().filter(|_| self.show_global_light && !doc.read_only()) else {
            return;
        };
        let mut open = true;
        let animated = self.animation.controls(doc, ctx.input(|i| i.time)).global_light(angle);
        egui::Window::new("Global Light")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                let draft = self.parameter_gesture.as_ref()
                    .and_then(|edit| edit.commands.first())
                    .filter(|command| command["op"] == "document.global_light")
                    .and_then(|command| command["angle"].as_u64())
                    .map(|angle| angle as u32);
                if draft.is_none() && animated.is_some() {
                    ui.visuals_mut().override_text_color = Some(AI_BLUE);
                }
                let mut angle = f64::from(draft.or(animated).unwrap_or(angle));
                let response = controls::whole_turn_angle(
                    ui,
                    ("global light angle", doc.id()),
                    &mut angle,
                    "Global Light angle",
                    "Set the direction of the light for all linked shadows. 0° points right and 90° points up.",
                );
                let releasing = response.drag_stopped_by(egui::PointerButton::Primary)
                    && self.parameter_gesture.as_ref().is_some_and(|edit| edit.control == response.id);
                if response.changed() || controls::number_editing(ctx, response.id) || releasing {
                    self.layer_parameter(json!({"op":"document.global_light","angle":angle as u32}), "Global Light angle", &response);
                }
            });
        self.show_global_light = open;
    }

    fn canvas_capture_ready(&self, doc: DocumentView<'_>) -> bool {
        match doc {
            DocumentView::Resident(_) => !self.animation.fading()
                && self.canvas_preview.as_ref().is_some_and(|key| {
                    key.0 == doc.id()
                    && key.1 == doc.revision()
                    && self.last_preview.as_ref() == Some(key)
                }),
            // The viewport contains no artwork samples to verify. The UI
            // frame must still have observed the current document generation.
            DocumentView::Native(_) if self.native_canvas_offscreen() => {
                self.doc_snapshot.view().same_observation(doc)
            }
            DocumentView::Native(native) => self
                .native_receipt
                .as_ref()
                .zip(self.native_canvas.current())
                .is_some_and(|(receipt, canvas)| {
                    let region = self.native_canvas_region(native);
                    let key = canvas.key();
                    let covers = canvas.tag().matches_region(region) || region.is_some_and(|visible| {
                        let [left, top, right, bottom] = key.rect;
                        key.width == (right - left) as u32 && key.height == (bottom - top) as u32
                            && native_canvas::covers_rect(key.rect, visible)
                    });
                    covers
                        && self.native_dispatch.capture_ready(
                        receipt,
                        &self.project_id,
                        native,
                        self.preview_epoch
                            .load(std::sync::atomic::Ordering::Relaxed),
                        canvas.tag(),
                    )
                }),
        }
    }
    fn finish_capture(&mut self, ctx: &egui::Context, drawn_generation: u64) {
        let time = ctx.input(|i| i.time);
        let presentation_ready = !self.pending
            && self.exports_capture_ready()
            && drawn_generation == self.canvas_generation
            && time >= self.capture_ready_at
            && !self.animation.fading()
            && self.animation.progress(time) >= 1.;
        // Keep selection and both documents stable through dequeue. Every lock
        // here is nonblocking; root may itself be the selected engine.
        if let Ok(mut engine) = self.workspace_root.try_lock() {
            if engine.capture_ui.is_some() {
                ctx.request_repaint_after(Duration::from_millis(16));
                let selected = if std::sync::Arc::ptr_eq(&self.workspace_root, &self.shared) {
                    None
                } else {
                    let Ok(selected) = self.shared.try_lock() else {
                        return;
                    };
                    Some(selected)
                };
                let Ok(workspace) = self.workspace.try_lock() else {
                    return;
                };
                let doc = crate::engine::view::current(selected.as_deref().unwrap_or(&engine));
                let ready = presentation_ready
                    && engine.capture_panel.is_none()
                    && workspace.is_active_engine(&self.shared)
                    && self.canvas_capture_ready(doc);
                if ready {
                    let path = engine.capture_ui.take().unwrap();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                        path,
                    )));
                } else {
                    ctx.request_repaint_after(Duration::from_millis(16));
                }
            }
        } else {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

// The disposable health workspace renders the real app but must never accept
// human document edits which would disappear when the probe exits.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn isolate_probe_input(ctx: &eframe::egui::Context) {
    ctx.input_mut(|input| {
        input.events.clear();
        input.raw.events.clear();
        input.keys_down.clear();
        input.modifiers = Default::default();
        input.raw.modifiers = Default::default();
        input.pointer = Default::default();
        input.raw.dropped_files.clear();
        input.raw.hovered_files.clear();
        input.raw_scroll_delta = eframe::egui::Vec2::ZERO;
        input.smooth_scroll_delta = eframe::egui::Vec2::ZERO;
    });
}

impl eframe::App for PeerBrush {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(windows)]
        self.updates.native_window(_frame);
        self.draw(ctx);
    }
    fn on_exit(&mut self) {
        if let Some(control) = &self.edit_control {
            control.cancel();
        }
        self.clear_tab_transfer_preview();
        self.preview_worker.stop();
        self.transform_bounds.stop();
        self.color_sampler.stop();
        self.mask_thumbnails.stop();
        self.layer_thumbnails.stop();
        self.alt_sampler.stop();
        self.magnetic_sampler.stop();
        self.sampled_input = None;
        self.color_pick = None;
        self.animation.clear();
        self.native_preview.clear();
        self.native_canvas.clear();
        self.native_source_preview = None;
        self.native_dispatch.clear();
        crate::production_export::jobs::cancel_all(&self.shared.lock().unwrap().export_library);
        if self.update_exit_committed() {
            return;
        }
        #[cfg(target_os = "macos")]
        if let Err(error) = self.updates.retire_profile_capture() {
            eprintln!("PeerBrush: app update profile capture remains unusable: {error}");
        }
        self.exit_tool_profile();
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
    fn resident_canvas_patch_cannot_overwrite_an_unrelated_same_size_frame() {
        let (mut app, ctx) = small_fixture();
        let original = vec![90; 4 * 3 * 4];
        app.set_canvas(&ctx, 4, 3, original.clone());
        app.canvas_preview_job = Some(41);
        let patch = crate::preview::CanvasPatch { base_job: 41, bounds: [1, 1, 3, 3] };
        let frame = Preview {
            job: 42,
            parameter: None,
            parameter_document: None,
            parameter_replayed: false,
            coarse: false,
            error: None,
            live: String::new(),
            revision: 0,
            doc_id: String::new(),
            w: 4,
            h: 3,
            bytes: vec![170; 2 * 2 * 4],
            patch: Some(patch),
            selection: None,
            selection_polygon: None,
            selection_coverage: None,
            text: None,
            target: String::new(),
            mask: false,
        };
        let generation = app.canvas_generation;
        assert!(!app.apply_canvas_patch(&frame, crate::preview::CanvasPatch { base_job: 40, ..patch }));
        assert_eq!(app.canvas_generation, generation);
        assert_eq!(app.canvas_pixels.as_ref().unwrap().2, original);
        assert!(app.apply_canvas_patch(&frame, patch));
        let mut expected = original;
        for y in [1, 2] {
            expected[(y * 4 + 1) * 4..(y * 4 + 3) * 4].fill(170);
        }
        assert_eq!(app.canvas_pixels.as_ref().unwrap().2, expected);
        assert_eq!(app.canvas_preview_job, Some(42));
        assert_eq!(app.canvas_generation, generation + 1);
        app.set_canvas(&ctx, 4, 3, expected.clone());
        assert!(!app.apply_canvas_patch(&frame, crate::preview::CanvasPatch { base_job: 42, ..patch }));
        assert_eq!(app.canvas_pixels.as_ref().unwrap().2, expected);
    }

    #[test]
    fn capture_waits_for_current_painted_texture_target_and_settled_presentation() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.bit_depth = depth;
            let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            let key = (
                doc.id.clone(),
                doc.revision,
                "current preview".into(),
                false,
            );
            app.set_canvas(&ctx, 1, 1, vec![231, 84, 32, 255]);
            app.canvas_preview = Some(key.clone());
            app.last_preview = Some(key.clone());
            let path =
                std::env::temp_dir().join(format!("peerbrush-capture-{}.png", crate::engine::id()));
            app.workspace_root.lock().unwrap().capture_ui = Some(path.clone());
            let run = |app: &mut PeerBrush, time: f64, drawn: u64| {
                let out = ctx.run(
                    egui::RawInput {
                        time: Some(time),
                        ..Default::default()
                    },
                    |ctx| app.finish_capture(ctx, drawn),
                );
                out.viewport_output
                    .values()
                    .flat_map(|v| &v.commands)
                    .any(|c| matches!(c, egui::ViewportCommand::Screenshot(_)))
            };
            let painted = app.canvas_generation;
            app.capture_ready_at = 0.3;
            assert!(!run(&mut app, 0.1, painted));
            app.pending = true;
            assert!(!run(&mut app, 0.4, painted));
            app.pending = false;
            app.set_canvas(&ctx, 1, 1, vec![20, 140, 240, 255]);
            assert!(
                !run(&mut app, 0.5, painted),
                "upload after painting needs another frame"
            );
            let painted = app.canvas_generation;
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.revision += 1;
            assert!(
                !run(&mut app, 0.6, painted),
                "new header must not capture old canvas"
            );
            let next = (doc.id.clone(), doc.revision + 1, "new target".into(), true);
            app.last_preview = Some(next.clone());
            assert!(
                !run(&mut app, 0.7, painted),
                "new isolation/target needs its own pixels"
            );
            app.canvas_preview = Some(next);
            let root = app.workspace_root.clone();
            let registry = app.workspace.clone();
            {
                let _held = root.lock().unwrap();
                assert!(!run(&mut app, 0.71, painted), "root contention must defer");
            }
            {
                let _held = registry.lock().unwrap();
                assert!(
                    !run(&mut app, 0.72, painted),
                    "registry contention must defer"
                );
            }
            root.lock().unwrap().capture_panel = Some("workspace".into());
            assert!(
                !run(&mut app, 0.73, painted),
                "pending panel must be presented first"
            );
            root.lock().unwrap().capture_panel = None;
            let mut other_engine = Engine::new();
            other_engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
            let other = crate::workspace::register_in(&registry, other_engine, None).unwrap();
            let other_id = other.lock().unwrap().project_id.clone();
            crate::workspace::select_in(&registry, &other_id).unwrap();
            assert!(
                !run(&mut app, 0.74, painted),
                "mid-frame project selection must defer"
            );
            let original_id = root.lock().unwrap().project_id.clone();
            crate::workspace::select_in(&registry, &original_id).unwrap();
            assert!(run(&mut app, 0.8, painted));
            assert!(app.workspace_root.lock().unwrap().capture_ui.is_none());
            assert!(
                !run(&mut app, 0.9, painted),
                "one queued capture must be consumed once"
            );
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            assert!(
                !path.exists(),
                "synthetic screenshot request must not write a fake capture"
            );
            app.shared = other.clone();
            crate::workspace::select_in(&registry, &other_id).unwrap();
            let doc = other.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            let key = (doc.id, doc.revision, "owned engine".into(), false);
            app.canvas_preview = Some(key.clone());
            app.last_preview = Some(key);
            root.lock().unwrap().capture_ui = Some(path);
            {
                let _held = other.lock().unwrap();
                assert!(
                    !run(&mut app, 1.0, painted),
                    "selected engine contention must defer"
                );
            }
            assert!(run(&mut app, 1.1, painted));
            assert!(!run(&mut app, 1.2, painted));
        }
    }
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
            let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
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
            assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(), before);
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
        app.set_solo(true);
        app.zoom = 1.75;
        app.pan = Vec2::new(9., -4.);
        app.collapsed.insert("folder-view".into());
        let mut engine = crate::engine::Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = Document::new(24, 20).unwrap();
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
        assert!(app.isolate);
        assert_eq!(app.solo_target, Some((selected.clone(), true)));
        assert_eq!(app.zoom, 1.75);
        assert_eq!(app.pan, Vec2::new(9., -4.));
        assert!(app.collapsed.contains("folder-view"));
    }
    #[test]
    fn native_ui_switches_tabs_while_background_root_engine_is_locked() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        let mut engine = crate::engine::Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = Document::new(24, 20).unwrap();
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
        assert_eq!(second.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].name, "Human in second");
        assert!(app.workspace_root.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
    }
    #[test]
    fn tab_drag_displays_actual_destination_then_commits_one_copy_without_source_history() {
        tab_drag_preview_then_drop(false);
    }

    #[test]
    fn tab_drop_uses_release_position_and_shift_and_waits_for_preview_retirement() {
        tab_drag_preview_then_drop(true);
    }

    fn tab_drag_preview_then_drop(move_layers: bool) {
        let (mut app, ctx) = fixture();
        {
            let mut e = app.shared.lock().unwrap();
            e.resident_state_mut().expect("resident fixture").document = Document::new(32, 24).unwrap();
            e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(3, 4, [233, 84, 32, 255]);
        }
        let second = crate::workspace::register_in(
            &app.workspace,
            {
                let mut e = crate::engine::Engine::new();
                e.resident_state_mut().expect("resident fixture").document = Document::new(32, 24).unwrap();
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
            document: app.shared.lock().unwrap().resident_state().expect("resident fixture").document.id.clone(),
            native: None,
            id: app.selected.clone(),
            revision: 0,
            offset: 0.,
            target: 0,
            ids: vec![app.selected.clone()],
            commands: vec![],
            drop_plan: None,
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
        let TransferOutput::Preview { result: Ok(TabPreviewOutput::Resident(image)), .. } = &reply.output else {
            panic!("Expected rendered transfer preview");
        };
        let bytes = image.bytes();
        assert_eq!(
            &bytes[((4 * 32 + 3) * 4)..((4 * 32 + 3) * 4 + 4)],
            &[233, 84, 32, 255]
        );
        assert_eq!(second.lock().unwrap().resident_state().expect("resident fixture").document.revision, 0);
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        let release = egui::Event::PointerButton {
            pos: tab,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers {
                shift: move_layers,
                ..Default::default()
            },
        };
        if move_layers {
            // Hold the terminal preview reply so the release has to queue its move.
            let pending = app.tab_preview_pending.clone().unwrap();
            frame(
                &mut app,
                &ctx,
                vec![release, egui::Event::PointerMoved(origin), egui::Event::PointerGone],
                Default::default(),
            );
            assert!(pending.check().is_err());
            assert!(app.tab_transfer_committing);
            assert!(app.tab_preview.is_none());
            assert!(app.transfer_rx.try_recv().is_err());
            assert_eq!(second.lock().unwrap().history_lengths(), [0, 0]);
            app.transfer_tx.send(reply).unwrap();
            frame(&mut app, &ctx, vec![], Default::default());
        } else {
            app.transfer_tx.send(reply).unwrap();
            frame(&mut app, &ctx, vec![], Default::default());
            assert!(app.tab_preview.is_some());
            frame(&mut app, &ctx, vec![release], Default::default());
        }
        let reply = app
            .transfer_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        app.transfer_tx.send(reply).unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert_eq!(app.project_id, second_id);
        assert_eq!(second.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        assert_eq!(
            app.workspace_root.lock().unwrap().history_lengths(),
            [usize::from(move_layers), 0]
        );
    }
    use crate::engine::Engine;
    use std::sync::{Arc, Mutex};
    #[test]
    fn stale_proposal_clears_reviewed_pixels_before_current_project_preview() {
        let (mut app, ctx) = small_fixture();
        let source = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
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
    pub(super) fn fixture() -> (PeerBrush, egui::Context) {
        let ctx = egui::Context::default();
        let app = PeerBrush::init(
            &ctx,
            Arc::new(Mutex::new(Engine::new())),
            Connection {
                port: 0,
                token: String::new(),
                state_dir: std::env::temp_dir(),
                instance_lock: None,
                availability: Default::default(),
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
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        app.brush_search = "pressure pen".into();
        app.brush_category = "Custom".into();
        app.show_brush = true;
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app.brush_thumbnails.contains_key(&id));
        let command = app.stroke_command(&[[40., 40.], [100., 80.]], [200, 70, 20, 255]);
        assert_eq!(
            crate::brush::Settings::from_command(&command).unwrap(),
            stored.settings
        );
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let preview = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        app.edit(vec![command], "Custom brush stroke");
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
            preview.export_png().unwrap()
        );
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        app.shared.lock().unwrap().undo("human").unwrap();
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
            before.export_png().unwrap()
        );
    }
    #[test]
    fn basic_brush_section_comes_first_and_actual_clicks_use_shared_native_strokes() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let before = {
                let mut e = app.shared.lock().unwrap();
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(96, 64, depth).unwrap();
                e.resident_state().expect("resident fixture").document.clone()
            };
            app.select_content(&before.layers[0].id);
            app.show_brush = true;
            let draw = |app: &mut PeerBrush| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            let label_pos = |out: &egui::FullOutput, name: &str| {
                out.shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Text(t) if t.galley.job.text == name => {
                            Some(t.pos + t.galley.size() * 0.5)
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("Missing visible brush label: {name}"))
            };
            // egui sizes a newly opened window before painting its visible frame.
            for _ in 0..3 {
                let _ = draw(&mut app);
            }
            let out = draw(&mut app);
            assert!(label_pos(&out, "Basic").y < label_pos(&out, "Hard Round").y);
            let first_section = label_pos(&out, "Basic").y;
            for category in &crate::brush_library::CATEGORIES[1..] {
                assert!(out.shapes.iter().all(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.job.text == *category =>
                        t.pos.y > first_section,
                    _ => true,
                }));
            }
            for preset in crate::brush_library::curated().into_iter().take(5) {
                let out = draw(&mut app);
                let pos = label_pos(&out, &preset.name);
                click(&mut app, &ctx, pos);
                assert_eq!(app.brush_preset.as_deref(), Some(preset.id.as_str()));
                assert_eq!(app.brush, preset.settings);
                assert_eq!(app.radius, preset.settings.radius);
                assert_eq!(app.brush_thumbnails[&preset.id].0, preset.settings);
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(
                    app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                app.stroke_has_pressure = true;
                app.stroke_pressures = vec![0.25, 0.8];
                let command = app.stroke_command(&[[20., 25.], [72., 38.]], [173, 61, 29, 255]);
                let expected = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
                app.edit(vec![command], "Basic brush stroke");
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), expected.export_png().unwrap());
                assert_ne!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                let loaded = crate::psd::decode(&crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap()).unwrap();
                assert_eq!(loaded.bit_depth, depth);
                assert_eq!(
                    loaded.layers[0].pixels.rgba16(),
                    e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16()
                );
                e.undo("human").unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
            }
            app.brush_category = "Basic".into();
            app.brush_search = "pencil".into();
            let out = draw(&mut app);
            assert!(out
                .shapes
                .iter()
                .any(|s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="Pencil")));
            assert!(!out.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="Hard Round" || t.galley.job.text=="Graphite")));
        }
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
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].x, 12);
        let q = app.transform_rects[0].unwrap().center();
        click(&mut app, &ctx, q);
        assert!(app.tool == Tool::None);
        assert!(app.animation.tool(ctx.input(|i| i.time)).is_none());
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
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
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                    for y in 0..8 {
                        for x in 0..8 {
                            if depth == 16 {
                                e.resident_state_mut().expect("resident fixture").document.layers[0]
                                    .pixels
                                    .set16(x, y, [10001, 30003, 50007, 65535]);
                            } else {
                                e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(x, y, [39, 117, 195, 255]);
                            }
                        }
                    }
                    let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    e.edit(
                        "human",
                        &[json!({"op":"effect.add","layer":layer,"kind":"posterize"})],
                        None,
                        None,
                        "Fixture",
                    )
                    .unwrap();
                    e.resident_state_mut().expect("resident fixture").undo.clear();
                    app.selected = layer.clone();
                    app.selection_layers = [layer.clone()].into_iter().collect();
                    app.effect_selected =
                        Some((layer, false, e.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone()));
                    e.resident_state().expect("resident fixture").document.clone()
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
                        app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].effects[0].settings["levels"],
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
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, before.revision);
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
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(native(&e.resident_state().expect("resident fixture").document), native(&draft));
                if typed {
                    assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["levels"], 8);
                }
                e.undo("human").unwrap();
                assert_eq!(native(&e.resident_state().expect("resident fixture").document), native(&before));
                assert_eq!(
                    e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
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
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                    for y in 0..8 {
                        for x in 0..8 {
                            if depth == 16 {
                                e.resident_state_mut().expect("resident fixture").document.layers[0]
                                    .pixels
                                    .set16(x, y, [10001, 30003, 50007, 12345]);
                            } else {
                                e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(x, y, [39, 117, 195, 48]);
                            }
                        }
                    }
                    let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    e.edit(
                        "human",
                        &[json!({"op":"effect.add","layer":layer,"kind":"channel_clamp"})],
                        None,
                        None,
                        "Fixture",
                    )
                    .unwrap();
                    e.resident_state_mut().expect("resident fixture").undo.clear();
                    app.selected = layer.clone();
                    app.selection_layers = [layer.clone()].into_iter().collect();
                    app.effect_selected =
                        Some((layer, false, e.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone()));
                    e.resident_state().expect("resident fixture").document.clone()
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
                        app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].effects[0].settings["minimum"],
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
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, before.revision);
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
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(native(&e.resident_state().expect("resident fixture").document), native(&draft));
                if typed {
                    assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["minimum"], 0.9);
                }
                e.undo("human").unwrap();
                assert_eq!(native(&e.resident_state().expect("resident fixture").document), native(&before));
                assert_eq!(
                    e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
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
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                for y in 0..8 {
                    for x in 0..8 {
                        if depth == 16 {
                            e.resident_state_mut().expect("resident fixture").document.layers[0]
                                .pixels
                                .set16(x, y, [10001, 30003, 50007, 12345]);
                        } else {
                            e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(x, y, [39, 117, 195, 48]);
                        }
                    }
                }
                let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                e.edit("human", &[json!({"op":"effect.add","layer":layer,"kind":"channel_clamp","settings":{"channel":"r","minimum":0.8,"maximum":0.9}})], None, None, "Fixture").unwrap();
                e.resident_state_mut().expect("resident fixture").undo.clear();
                app.selected = layer.clone();
                app.selection_layers = [layer.clone()].into_iter().collect();
                app.effect_selected = Some((layer, false, e.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone()));
                e.resident_state().expect("resident fixture").document.clone()
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
                assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["channel"], channel);
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                let expected=Engine::preview_edits(before.clone(),&[json!({"op":"effect.update","layer":before.layers[0].id,"effect":before.layers[0].effects[0].id,"settings":{"channel":channel,"minimum":0.8,"maximum":0.9}})]).unwrap();
                assert_eq!(
                    e.resident_state().expect("resident fixture").document.preview(None, 8, None, false).unwrap().2,
                    expected.preview(None, 8, None, false).unwrap().2
                );
                assert_eq!(
                    e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                e.undo("human").unwrap();
                assert_eq!(
                    e.resident_state().expect("resident fixture").document.preview(None, 8, None, false).unwrap().2,
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
                    engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                    for y in 0..32 {
                        for x in 0..32 {
                            if depth == 16 {
                                engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
                                    x,
                                    y,
                                    [12347, 33559, 51237, 65535],
                                );
                            } else {
                                engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(x, y, [48, 131, 200, 255]);
                            }
                        }
                    }
                    let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
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
                    engine.resident_state_mut().expect("resident fixture").undo.clear();
                    app.selected = layer.clone();
                    app.selection_layers = [layer].into_iter().collect();
                    engine.resident_state().expect("resident fixture").document.clone()
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
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, before.revision);
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
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(rendered(&engine.resident_state().expect("resident fixture").document), rendered(&draft));
                engine.undo("human").unwrap();
                assert_eq!(rendered(&engine.resident_state().expect("resident fixture").document), rendered(&before));
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
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
                    engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                    if depth == 16 {
                        engine.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(2, 3, [12347, 33559, 51237, 45679]);
                    } else {
                        engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(2, 3, [48, 131, 200, 177]);
                    }
                    let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
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
                    engine.resident_state_mut().expect("resident fixture").undo.clear();
                    app.selected = layer.clone();
                    app.selection_layers = [layer].into_iter().collect();
                    engine.resident_state().expect("resident fixture").document.clone()
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
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                let weight = if mask {
                    engine.resident_state().expect("resident fixture").document.layers[0]
                        .mask
                        .as_ref()
                        .unwrap()
                        .steps
                        .last()
                        .unwrap()
                        .weight
                } else {
                    engine.resident_state().expect("resident fixture").document.layers[0].effects[0].weight
                };
                assert!((weight - 0.235).abs() < 0.00001);
                assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, depth);
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
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
                assert_ne!(rendered(&engine.resident_state().expect("resident fixture").document), rendered(&before));
                engine.undo("human").unwrap();
                assert_eq!(rendered(&engine.resident_state().expect("resident fixture").document), rendered(&before));
            }
        }
    }
    #[test]
    fn typed_parameters_cancel_on_newer_work_and_project_switches() {
        for switch in [false, true] {
            let (mut app, ctx) = fixture();
            frame(&mut app, &ctx, vec![], Default::default());
            let source = app.shared.clone();
            let before = source.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
            assert!(source.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
                assert_eq!(original.resident_state().expect("resident fixture").document.revision, before.revision);
                assert_eq!(original.resident_state().expect("resident fixture").document.layers[0].opacity, 1.0);
                assert!(original.resident_state().expect("resident fixture").undo.is_empty());
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            } else {
                assert_eq!(original.resident_state().expect("resident fixture").document.layers[0].name, "Newer human work");
                assert_eq!(original.resident_state().expect("resident fixture").document.layers[0].opacity, 0.8);
                assert_eq!(original.resident_state().expect("resident fixture").undo.len(), 1);
                assert!(app.message.contains("cancelled"));
            }
        }
    }
    #[test]
    fn parameter_preview_waits_for_release_and_cancels_when_source_changes() {
        let (mut app, ctx) = fixture();
        let layer = app.selected.clone();
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].opacity, 1.);
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        app.finish_parameter(false);
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].opacity,
            draft.layers[0].opacity
        );
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        app.shared.lock().unwrap().undo("human").unwrap();
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].opacity, 1.);
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.parameter_gesture = Some(ParameterEdit {
            control: egui::Id::new("test"),
            gesture: "test".into(),
            engine: Arc::downgrade(&app.shared),
            document: doc.id,
            revision: doc.revision,
            commands: vec![json!({"op":"layer.update","layer":layer,"opacity":0.2})],
            preview: None,
            preview_runs: 0,
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
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].name,
            "Human rename"
        );
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].opacity, 1.);
        assert!(app.message.contains("cancelled"));
    }
    #[test]
    fn all_layers_defaults_off_samples_active_layer_and_keeps_explicit_choices() {
        use egui::accesskit::{Action, ActionRequest, Role};

        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            ctx.enable_accesskit();
            assert!(!app.selection_merged);
            let before = {
                let mut e = app.shared.lock().unwrap();
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                for y in 0..8 {
                    for x in 0..8 {
                        e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
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
                let mut top = crate::engine::Layer::new("Opaque cover", "paint", 8, 8);
                top.pixels = crate::raster::Raster::new_depth(8, 8, depth);
                for y in 0..8 {
                    for x in 0..8 {
                        top.pixels.set16(x, y, [22001, 22003, 22005, 65535]);
                    }
                }
                e.resident_state_mut().expect("resident fixture").document.layers.insert(0, top);
                e.resident_state().expect("resident fixture").document.clone()
            };
            app.select_content(&before.layers[1].id);
            app.select_tool(Tool::MagicWand);
            app.selection_kind = "wand".into();
            app.selection_tolerance = 0.;
            frame(&mut app, &ctx, vec![], Default::default());
            let canvas = app.view_rect.unwrap();
            click(
                &mut app,
                &ctx,
                canvas.min + Vec2::new(1.5, 2.5) * (canvas.width() / 8.),
            );
            assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection, Some([0, 0, 4, 8]));
            fn choose_sampling(app: &mut PeerBrush, ctx: &egui::Context, label: &str) {
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                );
                let tree = output.platform_output.accesskit_update.unwrap();
                let (target, node) = tree.nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some(label))
                    .expect("Sampling choice must have an accessible label");
                assert_eq!(node.role(), Role::RadioButton);
                assert!(node.supports_action(Action::Click));
                frame(app, ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Click, target: *target, data: None,
                })], Default::default());
            }
            choose_sampling(&mut app, &ctx, "Sample all layers");
            assert!(app.selection_merged);
            {
                let e = app.shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.selection, Some([0, 0, 8, 8]));
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 2);
                assert_eq!(
                    crate::selection::wand_source(&e.resident_state().expect("resident fixture").document).unwrap().command["sample_merged"],
                    true
                );
                for (layer, original) in e.resident_state().expect("resident fixture").document.layers.iter().zip(&before.layers) {
                    assert_eq!(layer.pixels.rgba16(), original.pixels.rgba16());
                }
            }
            for kind in ["quick", "object", "wand"] {
                app.select_tool(if kind == "wand" {
                    Tool::MagicWand
                } else {
                    Tool::Selection
                });
                app.selection_kind = kind.into();
                frame(&mut app, &ctx, vec![], Default::default());
                assert!(
                    app.selection_merged,
                    "Explicit All Layers choice must survive sampler changes"
                );
            }
            choose_sampling(&mut app, &ctx, "Sample active layer");
            assert!(!app.selection_merged);
            let mut e = app.shared.lock().unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.selection, Some([0, 0, 4, 8]));
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 3);
            e.undo("human").unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.selection, Some([0, 0, 8, 8]));
            e.undo("human").unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.selection, Some([0, 0, 4, 8]));
            assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
        }
    }
    #[test]
    fn color_comparison_choice_replays_wand_once_and_respects_live_toggle() {
        use egui::accesskit::{Action, ActionRequest, Role};

        let (mut app, ctx) = small_fixture();
        ctx.enable_accesskit();
        assert!(!app.selection_color_similarity);
        let before = {
            let mut e = app.shared.lock().unwrap();
            e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, 16).unwrap();
            for y in 0..24 {
                for x in 0..32 {
                    e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
                        x,
                        y,
                        [if x < 10 { 25701 } else { 41121 }, 25703, 25705, 65535],
                    );
                }
            }
            let id = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
            e.edit("human",&[json!({"op":"selection","kind":"wand","layer":id,"point":[4,8],"sample_merged":false,"tolerance":40})],None,None,"Seed").unwrap();
            e.resident_state_mut().expect("resident fixture").undo.clear();
            e.resident_state().expect("resident fixture").document.clone()
        };
        app.select_content(&before.layers[0].id);
        app.select_tool(Tool::MagicWand);
        app.selection_kind = "wand".into();
        fn checkbox(app: &mut PeerBrush, ctx: &egui::Context, name: &str) -> Pos2 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            );
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == name => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("Checkbox absent: {name}"))
        }
        let choose_comparison = |app: &mut PeerBrush, label| {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            );
            let tree = output.platform_output.accesskit_update.unwrap();
            let (target, node) = tree.nodes.iter().find(|(_, node)| node.label() == Some(label))
                .expect("Color comparison choice must have an accessible label");
            assert_eq!(node.role(), Role::RadioButton);
            frame(app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                action: Action::Click, target: *target, data: None,
            })], Default::default());
        };
        choose_comparison(&mut app, "Color similarity by channel");
        {
            let e = app.shared.lock().unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
            assert_eq!(crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap().value(15, 12), 0.);
            assert_eq!(
                e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(
                crate::selection::wand_source(&e.resident_state().expect("resident fixture").document).unwrap().command["color_similarity"],
                true
            );
        }
        let at = checkbox(&mut app, &ctx, "Live updates");
        click(&mut app, &ctx, at);
        assert!(!app.selection_live_wand);
        choose_comparison(&mut app, "Combined RGB distance");
        assert!(!app.selection_color_similarity);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        let at = checkbox(&mut app, &ctx, "Live updates");
        click(&mut app, &ctx, at);
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 2);
        assert_eq!(crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap().value(15, 12), 1.);
        e.undo("human").unwrap();
        assert_eq!(crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap().value(15, 12), 0.);
    }
    #[test]
    fn wand_toolbar_live_drag_previews_pixels_and_toggle_preserves_selection() {
        let (mut app, ctx) = small_fixture();
        app.tool = Tool::MagicWand;
        app.selection_kind = "wand".into();
        {
            let mut e = app.shared.lock().unwrap();
            e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, 16).unwrap();
            app.selected = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
            for y in 0..24 {
                for x in 0..32 {
                    e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
                        x,
                        y,
                        if x < 10 {
                            [10001, 20003, 30007, 65535]
                        } else {
                            [16001, 26003, 36007, 65535]
                        },
                    );
                }
            }
            e.edit("human", &[json!({"op":"selection","kind":"wand","layer":app.selected,"point":[5,5],"tolerance":0,"feather":2})], None, None, "Wand").unwrap();
            e.resident_state_mut().expect("resident fixture").undo.clear();
        }
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let mut time = 0.;
        let mut draw = |app: &mut PeerBrush, events: Vec<egui::Event>| {
            time += 0.15;
            let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            let mut rects = [Rect::NOTHING; 3];
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200., 100.))),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        rects = app.selection_toolbar(ui, &doc);
                    });
                },
            );
            rects
        };
        let rects = draw(&mut app, vec![]);
        assert!(app.selection_live_wand);
        let p = rects[1].center();
        draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(p),
                button(p, egui::PointerButton::Primary, true, Default::default()),
            ],
        );
        draw(
            &mut app,
            vec![egui::Event::PointerMoved(p + Vec2::new(25., 0.))],
        );
        let commands = app.parameter_gesture.as_ref().unwrap().commands.clone();
        assert_eq!(commands[0]["op"], "selection.wand_update");
        assert_eq!(commands[0]["document_id"], before.id);
        let draft = Engine::preview_edits(before.clone(), &commands).unwrap();
        assert!(draft.selection.unwrap()[2] > before.selection.unwrap()[2]);
        assert_eq!(draft.export_png().unwrap(), before.export_png().unwrap());
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        // Inspect the actual asynchronous canvas worker, rather than only an outline.
        app.request_preview(&ctx, &before);
        let rendered = app
            .preview_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(rendered.error.is_none());
        let mut expected = draft.preview(None, 8192, None, false).unwrap().2;
        crate::selection::display::apply(&draft, rendered.w, rendered.h, &mut expected, "cutout");
        assert_eq!(rendered.bytes, expected);
        draw(
            &mut app,
            vec![button(
                p + Vec2::new(25., 0.),
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
        );
        app.finish_parameter(false);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        assert_eq!(
            crate::selection::current(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document)
                .unwrap()
                .mask
                .rgba(),
            crate::selection::current(&draft).unwrap().mask.rgba()
        );
        let rects = draw(&mut app, vec![]);
        let toggle = rects[2].center();
        draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(toggle),
                button(
                    toggle,
                    egui::PointerButton::Primary,
                    true,
                    Default::default(),
                ),
            ],
        );
        draw(
            &mut app,
            vec![button(
                toggle,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
        );
        assert!(!app.selection_live_wand);
        let feather = rects[0].center();
        draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(feather),
                button(
                    feather,
                    egui::PointerButton::Primary,
                    true,
                    Default::default(),
                ),
            ],
        );
        draw(
            &mut app,
            vec![egui::Event::PointerMoved(feather + Vec2::new(15., 0.))],
        );
        draw(
            &mut app,
            vec![button(
                feather + Vec2::new(15., 0.),
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
        );
        assert!(app.selection_feather > 0.);
        assert!(app.parameter_gesture.is_none());
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        assert_eq!(
            crate::selection::current(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document)
                .unwrap()
                .mask
                .rgba(),
            crate::selection::current(&draft).unwrap().mask.rgba()
        );
        draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(toggle),
                button(
                    toggle,
                    egui::PointerButton::Primary,
                    true,
                    Default::default(),
                ),
            ],
        );
        draw(
            &mut app,
            vec![button(
                toggle,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
        );
        assert!(app.selection_live_wand);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 2);
        assert_ne!(
            crate::selection::current(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document)
                .unwrap()
                .mask
                .rgba(),
            crate::selection::current(&draft).unwrap().mask.rgba()
        );
        let committed = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(feather),
                button(
                    feather,
                    egui::PointerButton::Primary,
                    true,
                    Default::default(),
                ),
            ],
        );
        draw(
            &mut app,
            vec![egui::Event::PointerMoved(feather - Vec2::new(10., 0.))],
        );
        assert!(app.parameter_gesture.is_some());
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":app.selected,"name":"Human rename"})],
                None,
                None,
                "Rename",
            )
            .unwrap();
        app.finish_parameter(false);
        let e = app.shared.lock().unwrap();
        assert!(app.parameter_gesture.is_none());
        assert!(app.message.contains("cancelled"));
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 3);
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].name, "Human rename");
        assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
        assert_eq!(
            crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap().mask.rgba(),
            crate::selection::current(&committed).unwrap().mask.rgba()
        );
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
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.selection = Some([100, 80, 300, 180]);
        key(&mut app, &ctx, egui::Key::F);
        let framed = app.view_rect.unwrap();
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let scale = framed.width() / doc.width as f32;
        let center = framed.min + Vec2::new(200.0, 130.0) * scale;
        assert!((center - baseline.center()).length() < 1.0);
        assert!(app.zoom > 2.0);
        assert!(200.0 * scale < baseline.width() + 60.0);
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.selection = None;
        key(&mut app, &ctx, egui::Key::F);
        assert_eq!(app.zoom, 1.0);
        assert_eq!(app.pan, Vec2::ZERO);
        assert!((app.view_rect.unwrap().center() - baseline.center()).length() < 1.0);
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
                key: egui::Key::W,
                physical_key: Some(egui::Key::Q),
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
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
                continue;
            }
            assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,egui::Shape::Text(text) if text.galley.job.text == "Levels")));
            let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
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
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), before + 1);
            if mask {
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0]
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
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].effects.last().unwrap().kind, kind);
            }
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), before);
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
        assert!(engine.resident_state().expect("resident fixture").document.layers[0].x > 0);
        assert!(engine.resident_state().expect("resident fixture").document.layers[0].y > 0);
        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
        engine.undo("human").unwrap();
        assert_eq!((engine.resident_state().expect("resident fixture").document.layers[0].x, engine.resident_state().expect("resident fixture").document.layers[0].y), (0, 0));
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
        assert_eq!(
            app.view_rect.unwrap().center(),
            b,
            "pan must render in the input frame"
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
        let before_zoom = app.view_rect.unwrap();
        let old_zoom = app.zoom;
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(a + Vec2::new(50.0, 0.0))],
            alt,
        );
        assert!(
            (app.view_rect.unwrap().width() / before_zoom.width() - app.zoom / old_zoom).abs()
                < 0.001,
            "zoom must render in the input frame"
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, 0);
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
        assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 2);
        assert_eq!(app.selected, engine.resident_state().expect("resident fixture").document.layers[0].id);
        engine.undo("human").unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 1);
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, 0);
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
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        std::fs::remove_file(path).unwrap();
        let path = import_choice_fixture(&mut app, &ctx);
        app.pending_import.as_mut().unwrap().files[0].index = 1;
        std::fs::write(&path, b"Changed after inspection").unwrap();
        app.commit_import();
        wait_import(&mut app, &ctx);
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get(0, 0), [0, 255, 0, 255]);
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
        assert_eq!(app.selected, e.resident_state().expect("resident fixture").document.layers[0].id);
        e.undo("human").unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
        drop(e);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn pending_import_rejects_newer_work_and_cannot_apply_to_another_or_closed_project() {
        let (mut app, ctx) = fixture();
        let path = import_choice_fixture(&mut app, &ctx);
        let layer = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].id.clone();
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers.len(), 1);
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].name,
            "Newer human work"
        );
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        assert!(app.message.contains("changed"));
        std::fs::remove_file(path).unwrap();
        let path = import_choice_fixture(&mut app, &ctx);
        let source = app.shared.clone();
        let project = app.project_id.clone();
        let doc = source.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let other = crate::workspace::register(&app.workspace_root, Engine::new(), None).unwrap();
        let other_id = other.lock().unwrap().project_id.clone();
        crate::workspace::select(&app.workspace_root, &other_id).unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        app.commit_import();
        assert!(app.pending_import.is_some());
        assert!(other.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        assert_eq!(source.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
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
        assert!(other.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn svg_import_worker_retains_captured_source_native_depth_and_exact_single_undo() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture();
            {
                let mut e = app.shared.lock().unwrap();
                e.resident_state_mut().expect("resident fixture").document.bit_depth = depth;
                e.resident_state_mut().expect("resident fixture").document.ensure_depth();
                e.resident_state_mut().expect("resident fixture").document.layers[0]
                    .pixels
                    .set16(25, 25, [12345, 23457, 34569, 60001]);
            }
            let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            let markup="<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'><rect width='16' height='16' fill='#ff0000'/></svg>";
            let path =
                std::env::temp_dir().join(format!("peerbrush-ui-svg-{}.svg", uuid::Uuid::new_v4()));
            std::fs::write(&path, markup).unwrap();
            let encoded = crate::image_import::Encoded::file(&path).unwrap();
            app.pending_import = Some(PendingImport {
                project: app.project_id.clone(),
                document: before.id.clone(),
                revision: before.revision,
                native: None,
                files: vec![ImportFile {
                    path: path.clone(),
                    source: ImportSource::Resident(encoded),
                    index: 0,
                    icon_sizes: Vec::new(),
                }],
            });
            std::fs::write(&path, "Changed after inspection").unwrap();
            app.commit_import();
            wait_import(&mut app, &ctx);
            let mut e = app.shared.lock().unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.bit_depth, depth);
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
            assert_eq!(
                e.resident_state().expect("resident fixture").document.layers[0].source.as_ref().unwrap().content,
                crate::source::Content::Svg { svg: markup.into() }
            );
            assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get(4, 4), [255, 0, 0, 255]);
            assert_eq!(
                e.resident_state().expect("resident fixture").document.layers[1].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(app.selected, e.resident_state().expect("resident fixture").document.layers[0].id);
            e.undo("human").unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
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
            .resident_state().expect("resident fixture").document
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
        assert!(!app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].visible);
        assert_eq!(app.selected, ids[0]);
        click(&mut app, &ctx, eye);
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].visible);
    }
    #[test]
    fn layer_context_opens_over_child_controls_without_primary_side_effects() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture();
            let (child, other, folder) =
                {
                    let mut engine = app.shared.lock().unwrap();
                    engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                    let child = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    engine.resident_state_mut().expect("resident fixture").document.layers[0]
                        .pixels
                        .set16(2, 2, [12347, 33559, 51237, 45679]);
                    let result = engine.edit("human", &[
                    json!({"op":"layer.add","name":"Outside"}),
                    json!({"op":"mask.add","layer":child}),
                    json!({"op":"group.create_selected","layers":[child],"name":"Folder"}),
                ], None, None, "Fixture").unwrap();
                    let other = engine
                        .resident_state().expect("resident fixture").document
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
                    engine.resident_state_mut().expect("resident fixture").undo.clear();
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
            let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
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
                    app.shared.lock().unwrap().resident_state().expect("resident fixture").document.id
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
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
                            .resident_state().expect("resident fixture").document
                            .layers
                            .iter()
                            .find(|l| l.id == child)
                            .unwrap()
                            .locked
                    );
                    assert!(
                        !engine
                            .resident_state().expect("resident fixture").document
                            .layers
                            .iter()
                            .find(|l| l.id == other)
                            .unwrap()
                            .locked
                    );
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                    engine.undo("human").unwrap();
                    drop(engine);
                    app.select_content(&other);
                    app.selection_layers.insert(folder.clone());
                } else {
                    key(&mut app, &ctx, egui::Key::Escape);
                    assert!(app.layer_context.is_none());
                }
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(), before);
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
            engine.resident_state_mut().expect("resident fixture").undo.clear();
        }
        app.tool = Tool::None;
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let output = ctx.run(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
            ..Default::default()
        }, |ctx| app.draw(ctx));
        let (clipped_id, clipped_row) = app
            .layer_rects
            .iter()
            .find(|(_, row)| {
                row.center().y > app.color_tab_rect.unwrap().bottom() + 40. && row.center().y < 840.
            })
            .unwrap();
        let clipped = clipped_row.left_center() + Vec2::new(2., 0.);
        let doc = app.doc_snapshot.view();
        let drawn_name = |name: &str| output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == name)
        });
        assert!(drawn_name(doc.layer(0).unwrap().name()));
        assert!(!drawn_name(doc.find_layer(clipped_id).unwrap().name()));
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
        let first = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].id.clone();
        let point = app.layer_rects[&first].right_top() + Vec2::new(-2., 2.);
        secondary(&mut app, point);
        assert!(app.layer_context.is_some());
        let canvas = app.view_rect.unwrap().center();
        click(&mut app, &ctx, canvas);
        assert!(app.layer_context.is_none());
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = Document::new(8, 8).unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app.layer_context.is_none());
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
            e.resident_state_mut().expect("resident fixture").undo.clear();
        }
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, doc.revision);
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
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[2].id, id);
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].id, id);
    }
    #[test]
    fn x_swaps_foreground_background_and_independent_mask_colors() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        let (fore, back) = (app.color, app.background);
        app.open_color(color::Target::Foreground);
        key(&mut app, &ctx, egui::Key::X);
        assert_eq!((app.color, app.background), (back, fore));
        app.color_editor = None;
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, 0);
        key(&mut app, &ctx, egui::Key::Escape);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, 0);
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
            .resident_state().expect("resident fixture").document
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
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").undo.clear();
        app.layer_cmd("layer.update", json!({"opacity":0.7}), "Opacity");
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        for id in [&ids[0], &ids[1]] {
            assert_eq!(
                app.shared
                    .lock()
                    .unwrap()
                    .resident_state().expect("resident fixture").document
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
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 2);
        let p = e
            .resident_state().expect("resident fixture").document
            .layers
            .iter()
            .filter(|l| app.selection_layers.contains(&l.id))
            .map(|l| (l.x, l.y))
            .collect::<Vec<_>>();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0], p[1]);
        assert!(p[0].0 > 0);
        e.undo("human").unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].x, 0);
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
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let id = doc.layers[0].id.clone();
        let folder = doc.layers[1].id.clone();
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").undo.clear();
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
            .resident_state().expect("resident fixture").document
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
                .resident_state().expect("resident fixture").document
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
            .resident_state().expect("resident fixture").document
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .parent
            .is_none());
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 2);
        e.undo("human").unwrap();
        assert_eq!(
            e.resident_state().expect("resident fixture").document
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
            .resident_state().expect("resident fixture").document
            .layers
            .iter()
            .map(|l| l.id.clone())
            .collect::<Vec<_>>();
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").undo.clear();
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
            .resident_state().expect("resident fixture").document
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
        assert!(e.resident_state().expect("resident fixture").document.layers.iter().all(|l| !l.visible));
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
        e.undo("human").unwrap();
        assert!(e.resident_state().expect("resident fixture").document.layers.iter().all(|l| l.visible));
        drop(e);

        // A release with a new position is the last segment, even without a move event.
        frame(&mut app, &ctx, vec![button(a, egui::PointerButton::Primary, true, Default::default())], Default::default());
        frame(&mut app, &ctx, vec![button(b, egui::PointerButton::Primary, false, Default::default())], Default::default());
        let mut engine = app.shared.lock().unwrap();
        let state = engine.resident_state().unwrap();
        assert!(state.document.layers.iter().all(|layer| !layer.visible));
        assert_eq!(state.undo.len(), 1);
        engine.undo("human").unwrap();
        drop(engine);

        frame(&mut app, &ctx, vec![button(a, egui::PointerButton::Primary, true, Default::default())], Default::default());
        assert!(app.eye_sweep.is_some());
        frame(&mut app, &ctx, vec![
            egui::Event::WindowFocused(false),
            egui::Event::WindowFocused(true),
            button(b, egui::PointerButton::Primary, false, Default::default()),
        ], Default::default());
        assert!(app.eye_sweep.is_none());
        let engine = app.shared.lock().unwrap();
        let state = engine.resident_state().unwrap();
        assert!(state.document.layers.iter().all(|layer| layer.visible));
        assert!(state.undo.is_empty());
    }
    #[test]
    fn alt_b_fills_a_selection_and_mask_and_remains_undoable() {
        let (mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.selection = Some([3, 4, 8, 9]);
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
            assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get(4, 5), app.color);
            assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get(1, 1), [0; 4]);
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
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
        assert!((e.resident_state().expect("resident fixture").document.layers[0].mask_value(4, 5) * 255.0 - 22.0).abs() < 1.0);
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get(4, 5), [0; 4]);
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, 0);
        assert!(app.points.is_empty());
    }
    #[test]
    fn size_gesture_survives_control_focus_and_preserves_text_entry_and_native_sources() {
        for depth in [8, 16] {
            for control in ["button", "slider", "text"] {
                // egui-winit maps an unrecognized Hebrew letter to physical S;
                // this covers that app input shape, not actual OS layout changes.
                for (physical, typed) in [(None, "s"), (Some(egui::Key::S), "ד")] {
                    let (mut app, ctx) = fixture();
                    app.recovery.open = false;
                    app.shortcuts = shortcuts::Shortcuts::load(Path::new(""));
                    {
                        let mut engine = app.shared.lock().unwrap();
                        engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                        engine.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(1, 1, [12001, 23003, 34007, 65003]);
                        engine.resident_state_mut().expect("resident fixture").document.revision = 7;
                    }
                    let before = serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
                    let mut text = String::new();
                    let mut slider = 0.4;
                    let mut draw = |app: &mut PeerBrush, events, focused| {
                        let mut id = egui::Id::NULL;
                        let _ = ctx.run(
                            egui::RawInput {
                                screen_rect: Some(Rect::from_min_size(
                                    Pos2::ZERO,
                                    Vec2::new(1360., 900.),
                                )),
                                events,
                                focused,
                                ..Default::default()
                            },
                            |ctx| {
                                egui::Window::new("Shortcut focus fixture")
                                    .fixed_pos(Pos2::new(20., 100.))
                                    .default_width(180.)
                                    .show(ctx, |ui| {
                                        let response = match control {
                                            "button" => ui.button("Focused button"),
                                            "slider" => ui.add(
                                                egui::Slider::new(&mut slider, 0. ..=1.)
                                                    .show_value(false),
                                            ),
                                            _ => ui.text_edit_singleline(&mut text),
                                        };
                                        id = response.id;
                                        response.request_focus();
                                    });
                                app.draw(ctx);
                            },
                        );
                        id
                    };
                    draw(&mut app, vec![], true);
                    let id = draw(&mut app, vec![], true);
                    assert_eq!(ctx.memory(|m| m.focused()), Some(id));
                    assert!(ctx.wants_keyboard_input());
                    assert_eq!(
                        egui::TextEdit::load_state(&ctx, id).is_some(),
                        control == "text"
                    );
                    app.radius = 20.;
                    app.brush.hardness = 0.7;
                    let radius = app.radius;
                    let hardness = app.brush.hardness;
                    let at = app.view_rect.unwrap().center();
                    let key = |pressed| egui::Event::Key {
                        key: egui::Key::S,
                        physical_key: physical,
                        pressed,
                        repeat: false,
                        modifiers: Default::default(),
                    };
                    draw(
                        &mut app,
                        vec![
                            egui::Event::PointerMoved(at),
                            key(true),
                            egui::Event::Text(typed.into()),
                        ],
                        true,
                    );
                    assert_eq!(
                        app.size_drag.is_some(),
                        control != "text",
                        "{control}, depth {depth}, physical {physical:?}"
                    );
                    draw(
                        &mut app,
                        vec![egui::Event::PointerMoved(at + Vec2::new(24., 20.))],
                        true,
                    );
                    assert_eq!(ctx.memory(|m| m.focused()), Some(id));
                    if control == "text" {
                        assert_eq!(app.radius, radius);
                        assert_eq!(app.brush.hardness, hardness);
                    } else {
                        assert!(app.radius > radius);
                        assert!(app.brush.hardness < hardness);
                    }
                    let stopped = (app.radius, app.brush.hardness);
                    draw(
                        &mut app,
                        vec![
                            egui::Event::WindowFocused(false),
                            egui::Event::PointerMoved(at + Vec2::new(48., 40.)),
                        ],
                        false,
                    );
                    assert!(app.size_drag.is_none());
                    assert_eq!((app.radius, app.brush.hardness), stopped);
                    draw(
                        &mut app,
                        vec![egui::Event::WindowFocused(true), key(false)],
                        true,
                    );
                    assert!(app.size_drag.is_none());
                    if control == "text" {
                        assert_eq!(text, typed);
                    }
                    assert_eq!(slider, 0.4);
                    assert!(app.points.is_empty());
                    let engine = app.shared.lock().unwrap();
                    assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
                    assert!(engine.resident_state().expect("resident fixture").undo.is_empty() && engine.resident_state().expect("resident fixture").redo.is_empty());
                }
            }
        }
    }
    #[test]
    fn alt_temporarily_samples_color_without_painting_or_switching_tools() {
        let (mut app, ctx) = fixture();
        {
            let mut e = app.shared.lock().unwrap();
            e.resident_state_mut().expect("resident fixture").document.layers[0].kind = "fill".into();
            e.resident_state_mut().expect("resident fixture").document.layers[0].color = [24, 120, 222, 255];
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
        sampled_input::tests::settle(&mut app, &ctx);
        assert_eq!(app.color, [24, 120, 222, 255]);
        assert!(app.tool == Tool::Brush);
        assert!(app.points.is_empty());
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, 0);
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        // Releasing Alt restores brush input, including mask painting.
        frame(&mut app, &ctx, vec![], Default::default());
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.layers[0].kind = "paint".into();
        app.color = [233, 84, 32, 255];
        click(&mut app, &ctx, point);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, 1);
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
            engine.resident_state_mut().expect("resident fixture").document.layers = vec![
                outer.clone(),
                inner.clone(),
                leaf.clone(),
                sibling.clone(),
                loose.clone(),
            ];
        }
        app.select_content(&loose.id);
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
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
        assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
        assert_eq!(engine.resident_state().expect("resident fixture").document.revision, 0);
        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), before);
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
        let revision = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision;
        let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
        draw_at(&mut app, 1.01);
        assert!(!app.collapsed.contains(&folder));
        assert!(app.animation.layer(&layer, ctx.input(|i| i.time)));
        draw_at(&mut app, 1.19);
        let middle = app.layer_rects[&layer];
        draw_at(&mut app, 1.60);
        let after = app.layer_rects[&layer];
        assert!(
            middle.top() > before.top() && middle.top() < after.top(),
            "Layer should visibly travel between rows: {before:?}, {middle:?}, {after:?}"
        );
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, revision);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history);
        assert_eq!(
            app.shared
                .lock()
                .unwrap()
                .resident_state().expect("resident fixture").document
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
        let revision = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision;
        for _ in 0..12 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let e = app.shared.lock().unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.revision, revision);
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].opacity, 0.17);
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
    }
    #[test]
    fn resident_base_preview_reuses_its_key_until_the_display_target_changes() {
        let (mut app, ctx) = small_fixture();
        let source = app.shared.lock().unwrap().doc_snapshot();
        let doc = source.view();
        let base = app.preview_variant();
        app.last_preview = Some((
            doc.id().into(),
            doc.revision(),
            format!("{base};;{}", crate::raster::MAX_EDGE),
            false,
        ));
        let builds = app.preview_variant_builds.get();
        let job = app.preview_job;

        app.request_preview(&ctx, doc);
        assert_eq!(app.preview_variant_builds.get(), builds);
        assert_eq!(app.preview_job, job);

        app.isolate = true;
        app.request_preview(&ctx, doc);
        assert_eq!(app.preview_variant_builds.get(), builds + 1);
        assert_ne!(app.preview_job, job);
    }
    #[test]
    fn pending_full_resolution_stroke_defers_serialization_and_accepts_live_progress() {
        let (mut app, ctx) = small_fixture();
        app.select_tool(Tool::Brush);
        app.live_gesture = Some("stroke".into());
        app.points = vec![[10.0, 10.0], [20.0, 10.0]];
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let live = app.live_key();
        let variant = format!(
            "{};;{}",
            app.preview_variant(),
            crate::raster::MAX_EDGE,
        );
        app.last_preview = Some((doc.id.clone(), doc.revision, variant.clone(), false));
        app.pending = true;
        let builds = app.preview_variant_builds.get();
        let job = app.preview_job;
        app.points.push([30.0, 10.0]);
        app.request_preview(&ctx, &doc);
        assert_eq!(app.preview_variant_builds.get(), builds);
        assert_eq!(app.preview_job, job);

        app.preview_tx
            .send(Preview {
                job: app.preview_job,
                parameter: None,
                parameter_document: None,
                parameter_replayed: false,
                coarse: false,
                error: None,
                live,
                doc_id: doc.id.clone(),
                revision: doc.revision,
                selection: doc.selection,
                selection_polygon: doc.selection_polygon.clone(),
                selection_coverage: doc.selection_coverage.clone(),
                text: None,
                target: variant,
                mask: false,
                w: 1,
                h: 1,
                patch: None,
                bytes: vec![255, 80, 20, 255],
            })
            .unwrap();
        app.request_preview(&ctx, &doc);
        assert_eq!(app.preview_variant_builds.get(), builds + 1);
        assert_eq!(
            app.canvas_pixels.as_ref().map(|(_, _, pixels)| pixels.as_slice()),
            Some(&[255, 80, 20, 255][..]),
        );
        assert_ne!(app.preview_job, job);
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
    fn compatibility_report_is_view_only_and_closes_when_the_source_changes() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            {
                let mut engine = app.shared.lock().unwrap();
                engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                engine.resident_state_mut().expect("resident fixture").document.layers[0].effects.push(crate::effects::Effect {
                    id: crate::engine::id(),
                    kind: "invert".into(),
                    enabled: true,
                    weight: 1.,
                    settings: json!({}).into(),
                });
                app.selected = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
            }
            let before = serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
            let original = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
            app.workspace_root.lock().unwrap().capture_panel = Some("save_report".into());
            let draw = |app: &mut PeerBrush| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            let mut output = draw(&mut app);
            for _ in 0..4 {
                output = draw(&mut app);
            }
            let text = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("PSD / PSB compatibility"));
            assert!(text.contains("Layer effects are baked"));
            assert!(text.contains("no profile conversion"));
            assert_eq!(
                serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                before
            );
            assert_eq!(
                app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
                original
            );
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.revision += 1;
            frame(&mut app, &ctx, vec![], Default::default());
            assert!(app.save_report.is_none());
            assert!(app.message.contains("Document changed"));
        }
    }
    #[test]
    fn protected_photoshop_report_is_view_only_and_bound_to_the_current_source() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let draw = |app: &mut PeerBrush| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            {
                let mut e = app.shared.lock().unwrap();
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                e.resident_state_mut().expect("resident fixture").document.layers[0]
                    .pixels
                    .set16(1, 2, [10001, 20003, 40007, 65535]);
                e.resident_state_mut().expect("resident fixture").document.read_only = true;
                e.resident_state_mut().expect("resident fixture").document.warnings = vec![
                    "Unsupported Photoshop feature: lmfx".into(),
                    "Unsupported global Photoshop feature: unknown".into(),
                ];
                e.resident_state_mut().expect("resident fixture").document.source_structure = vec![crate::psd::SourceLayerInfo {
                    source_index: 1,
                    parent_index: None,
                    name: "Original styled layer".into(),
                    kind: "paint".into(),
                    bounds: [-2, 1, 6, 9],
                    visible: true,
                    has_mask: true,
                    tags: vec!["lmfx".into()],
                    warnings: vec!["Unsupported Photoshop feature: lmfx".into()],
                    details_truncated: false,
                }];
                app.selected = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
            }
            let before = serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
            let original = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
            draw(&mut app);
            let output = draw(&mut app);
            let point = output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == "Why read only?" => {
                        Some(t.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .unwrap();
            click(&mut app, &ctx, point);
            let mut output = draw(&mut app);
            for _ in 0..3 {
                output = draw(&mut app);
            }
            let text = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some(t.galley.text()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("Showing the saved appearance"));
            assert!(text.contains("layer styles"));
            assert!(text.contains("Unsupported global Photoshop feature: unknown"));
            assert!(text.contains("Original source layers"));
            assert!(text.contains("Original styled layer"));
            app.workspace_root.lock().unwrap().capture_panel = Some("document_sources".into());
            for _ in 0..4 {
                output = draw(&mut app);
            }
            let source_text = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(source_text.contains("Bounds: -2, 1"));
            assert!(source_text.contains("Source mask present"));
            assert!(app.protection_visible(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document));
            assert_eq!(
                serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                before
            );
            assert_eq!(
                app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
                original
            );
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.id = crate::engine::id();
            frame(&mut app, &ctx, vec![], Default::default());
            assert!(app.read_only_info.is_none());
            app.workspace_root.lock().unwrap().capture_panel = Some("document_info".into());
            frame(&mut app, &ctx, vec![], Default::default());
            assert!(app.protection_visible(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document));
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.read_only = false;
            frame(&mut app, &ctx, vec![], Default::default());
            assert!(app.read_only_info.is_none());
        }
    }
    #[test]
    fn dropped_psb_opens_a_native_project_without_replacing_existing_work() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let original = app.project_id.clone();
            let root = app.shared.clone();
            let before = crate::psd::encode(&root.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
            let mut doc = Document::new_depth(8, 6, depth).unwrap();
            doc.layers[0]
                .pixels
                .set16(2, 3, [10001, 20003, 40007, 65535]);
            let expected = doc.export_png().unwrap();
            let directory =
                std::env::temp_dir().join(format!("peerbrush-psb-ui-{}", crate::engine::id()));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("Native artwork.PSB");
            std::fs::write(&path, crate::psd::encode_psb(&doc).unwrap()).unwrap();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                    dropped_files: vec![egui::DroppedFile {
                        path: Some(path.clone()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while std::time::Instant::now() < deadline {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                );
                if crate::workspace::entries_in(&app.workspace).len() == 2 && !app.busy {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert_eq!(
                crate::workspace::entries_in(&app.workspace).len(),
                2,
                "{}",
                app.message
            );
            assert_ne!(app.project_id, original);
            let e = app.shared.lock().unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.bit_depth, depth);
            assert!(!e.resident_state().expect("resident fixture").document.read_only);
            assert_eq!(e.path.as_ref(), Some(&path));
            assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), expected);
            assert!(e.resident_state().expect("resident fixture").undo.is_empty());
            drop(e);
            assert_eq!(
                crate::psd::encode(&root.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                before
            );
            assert!(root.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            app.drop_files(vec![path.clone(), directory.join("Image.png")]);
            assert!(app.message.contains("Drop one PSD/PSB"));
            assert_eq!(crate::workspace::entries_in(&app.workspace).len(), 2);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
    #[test]
    fn mcp_header_separates_server_availability_from_live_or_expired_clients_without_document_edits(
    ) {
        let (mut app, ctx) = small_fixture();
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let saved = crate::psd::encode(&before).unwrap();
        for (available, expiry, label, color) in [
            (
                false,
                None,
                "MCP unavailable",
                Color32::from_rgb(255, 154, 144),
            ),
            (
                true,
                None,
                "MCP available",
                Color32::from_rgb(166, 215, 157),
            ),
            (
                true,
                Some(crate::engine::now() + 90),
                "MCP connected",
                Color32::from_rgb(166, 215, 157),
            ),
            (
                false,
                Some(crate::engine::now() + 90),
                "MCP unavailable",
                Color32::from_rgb(255, 154, 144),
            ),
            (
                true,
                Some(crate::engine::now() - 1),
                "MCP available",
                Color32::from_rgb(166, 215, 157),
            ),
        ] {
            app.connection
                .availability
                .store(available, std::sync::atomic::Ordering::Release);
            {
                let mut e = app.workspace_root.lock().unwrap();
                e.mcp_clients.clear();
                if let Some(expiry) = expiry {
                    e.mcp_clients.insert("Synthetic UI client".into(), expiry);
                }
            }
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            );
            let text = out
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.job.text == label => Some(t),
                    _ => None,
                })
                .unwrap();
            assert_eq!(text.galley.rows[0].visuals.mesh.vertices[0].color, color);
            assert!(out.shapes.iter().any(
                |s| matches!(&s.shape,egui::Shape::Circle(c) if c.radius==2.5 && c.fill==color)
            ));
            assert!(!out.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text.starts_with("MCP ") && t.galley.job.text!=label && matches!(t.galley.job.text.as_str(),"MCP connected"|"MCP available"|"MCP unavailable"))));
            let e = app.shared.lock().unwrap();
            assert!(e.resident_state().expect("resident fixture").undo.is_empty());
            assert_eq!(crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap(), saved);
            assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
        }
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
    fn command_e_selects_eraser_without_editing_and_respects_text_focus() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
            let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            app.selected = before.layers[0].id.clone();
            app.select_tool(Tool::Brush);
            let command = egui::Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            };
            modified_key(&mut app, &ctx, egui::Key::E, command);
            assert!(app.tool == Tool::Eraser);
            assert!(!app.busy);
            assert!(app.merge_rx.try_recv().is_err());
            app.select_tool(Tool::Brush);
            app.begin_rename(&before, &before.layers[0].id);
            modified_key(&mut app, &ctx, egui::Key::E, command);
            assert!(app.tool == Tool::Brush);
            let engine = app.shared.lock().unwrap();
            assert_eq!(
                serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
        }
    }
    #[test]
    fn command_shift_g_merges_selected_layers_selects_result_and_keeps_one_undo_entry() {
        let (mut app, ctx) = fixture();
        {
            let mut e = app.shared.lock().unwrap();
            e.resident_state_mut().expect("resident fixture").document = Document::new(32, 32).unwrap();
            e.edit(
                "human",
                &[json!({"op":"layer.add","kind":"paint","name":"Top"})],
                None,
                None,
                "Add",
            )
            .unwrap();
            e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(4, 4, [233, 84, 32, 255]);
            e.resident_state_mut().expect("resident fixture").document.layers[1].pixels.set(8, 8, [24, 120, 222, 255]);
            e.resident_state_mut().expect("resident fixture").undo.clear();
        }
        let ids = app
            .shared
            .lock()
            .unwrap()
            .resident_state().expect("resident fixture").document
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
            shift: true,
            ..Default::default()
        };
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::G,
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
                key: egui::Key::G,
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
        assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
        assert_eq!(app.selected, e.resident_state().expect("resident fixture").document.layers[0].id);
        assert!(!ids.contains(&app.selected));
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(
            e.resident_state().expect("resident fixture").document
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
                engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(12, 10, depth).unwrap();
                let first = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
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
                for (i, layer) in engine.resident_state_mut().expect("resident fixture").document.layers.iter_mut().enumerate() {
                    layer
                        .pixels
                        .set16(i as i32 + 2, 3, [12347, 33559, 51237, 45679]);
                }
                engine.resident_state_mut().expect("resident fixture").undo.clear();
                engine.resident_state().expect("resident fixture").document.clone()
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
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(app.selection_layers, [folder.clone()].into_iter().collect());
                assert!(engine
                    .resident_state().expect("resident fixture").document
                    .layers
                    .iter()
                    .any(|l| l.id == folder && l.kind == "group"));
                for old in &before.layers {
                    let mut current = engine
                        .resident_state().expect("resident fixture").document
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
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                engine.resident_state().expect("resident fixture").document.clone()
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
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 2);
            assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 2);
            assert_eq!(
                app.selection_layers,
                [app.selected.clone()].into_iter().collect()
            );
            assert_eq!(
                engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                before.export_png().unwrap()
            );
            assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, depth);
            let loaded = crate::psd::decode(&crate::psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap()).unwrap();
            assert!(!loaded.read_only);
            assert_eq!(loaded.bit_depth, depth);
            assert_eq!(loaded.export_png().unwrap(), before.export_png().unwrap());
            engine.undo("human").unwrap();
            assert_eq!(
                serde_json::to_value(&engine.resident_state().expect("resident fixture").document.layers).unwrap(),
                serde_json::to_value(&grouped.layers).unwrap()
            );
            engine.undo("human").unwrap();
            assert_eq!(
                serde_json::to_value(&engine.resident_state().expect("resident fixture").document.layers).unwrap(),
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
                engine.resident_state_mut().expect("resident fixture").undo.clear();
                result["created"][0].as_str().unwrap().to_string()
            };
            app.select_content(&top);
            app.selection_layers = app
                .shared
                .lock()
                .unwrap()
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .map(|layer| layer.id.clone())
                .collect();
            let old = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
                            engine.resident_state_mut().expect("resident fixture").document.layers[0].locked = true;
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
                    assert!(app.message.contains("source changed"));
                }
                "replaced" => {
                    app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, 16).unwrap();
                    let current = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
                    app.create_folder(&old);
                    assert!(app.message.contains("source changed"));
                    assert_eq!(
                        app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
                        current
                    );
                }
                _ => unreachable!(),
            }
            let engine = app.shared.lock().unwrap();
            assert_eq!(
                engine.resident_state().expect("resident fixture").document.layers.len(),
                if failure == "replaced" { 1 } else { 2 }
            );
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), usize::from(failure == "stale"));
            if failure != "replaced" {
                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), old.export_png().unwrap());
            }
        }
    }
    #[test]
    fn mask_stack_displays_last_operation_on_top_without_changing_execution_order() {
        let (mut app, ctx) = fixture();
        {
            let mut e = app.shared.lock().unwrap();
            e.resident_state_mut().expect("resident fixture").document = Document::new(32, 32).unwrap();
            e.resident_state_mut().expect("resident fixture").document.layers[0].kind = "fill".into();
            let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
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
        let source = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
            e.resident_state().expect("resident fixture").document.layers[0]
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
        assert_eq!(e.resident_state().expect("resident fixture").document.preview(None, 32, None, false).unwrap().2, expected);
        // Invert then gamma 2 gives 63; reversing execution would give 191.
        assert_eq!(expected[3], 63);
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
    }
    #[test]
    fn retouch_source_alignment_live_pixels_and_one_undo_follow_native_input() {
        for shortcut in [egui::Key::C, egui::Key::J] {
            let (mut app, ctx) = small_fixture();
            {
                let mut e = app.shared.lock().unwrap();
                for y in 0..32 {
                    for x in 0..32 {
                        e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(
                            x,
                            y,
                            [if x < 16 { 210 } else { 40 }, 80, 35, 255],
                        );
                    }
                }
                e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(23, 16, [250, 180, 120, 255]);
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
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
                    egui::Event::PointerMoved(end),
                ],
                alt,
            );
            frame(
                &mut app,
                &ctx,
                vec![button(source, egui::PointerButton::Primary, false, alt)],
                alt,
            );
            let chosen = app.retouch_source.as_ref().unwrap().point;
            assert!((chosen[0] - 7.).abs() < 0.01 && (chosen[1] - 16.).abs() < 0.01);
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
            let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            let command = app.stroke_command(&app.points, app.color);
            let preview = Engine::preview_edits(before.clone(), &[command]).unwrap();
            assert_ne!(preview.export_png().unwrap(), before.export_png().unwrap());
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
            assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), preview.export_png().unwrap());
            e.undo("human").unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
            drop(e);
            let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            assert!(app.begin_retouch(&doc, [26., 16.]));
            let next = app.stroke_command(&[[26., 16.]], app.color);
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
            let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            let center = app.view_rect.unwrap().center();
            click(&mut app, &ctx, center);
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            app.cancel_source();
            assert!(app.source_editor.is_none());
            app.shared
                .lock()
                .unwrap()
                .edit("human", &[command], Some(doc.revision), None, "Source")
                .unwrap();
            let current = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
    fn missing_installed_text_retains_native_pixels_and_explicit_picker_replacement_previews_and_undoes(
    ) {
        use crate::fonts::{self, FontError, InstalledFont, TextFont};
        let font = InstalledFont {
            family: "Unavailable fixture".into(),
            postscript: "UnavailableFixture-Regular".into(),
            style: "normal".into(),
            weight: 400,
            stretch: 5,
            index: 0,
            sha256: "d".repeat(64),
        };
        let _fonts = fonts::test_fonts(vec![(font.clone(), Err(FontError::Missing))]);
        let (mut app, ctx) = small_fixture();
        let original = {
            let mut engine = app.shared.lock().unwrap();
            engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, 16).unwrap();
            engine.resident_state_mut().expect("resident fixture").document.layers[0]
                .pixels
                .set16(2, 3, [12347, 33659, 51237, 65535]);
            engine.resident_state_mut().expect("resident fixture").document.layers[0].source = Some(
                crate::source::Source {
                    width: 32,
                    height: 32,
                    matrix: [1., 0., 0., 1., 0., 0.],
                    content: crate::source::Content::Text {
                        text: "Hi".into(),
                        layout: crate::source::TextLayout::Legacy,
                        font: TextFont::Installed(font),
                        size: 16.,
                        line_height: 1.2,
                        letter_spacing: 0.,
                        align: "left".into(),
                        color: [53471, 12679, 33457, 65535],
                    },
                }
                .into(),
            );
            engine.resident_state().expect("resident fixture").document.clone()
        };
        app.select_content(&original.layers[0].id);
        app.open_source(&original, "edit");
        frame(&mut app, &ctx, vec![], Default::default());
        assert!(app.source_editor.is_some());
        assert!(
            app.transient.is_empty(),
            "Missing fonts must retain the saved raster, without failed source previews"
        );
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
            original.layers[0].pixels.rgba16()
        );
        fn control(app: &mut PeerBrush, ctx: &egui::Context, label: &str) -> Pos2 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            );
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("Source control absent: {label}"))
        }
        // Apply is disabled while the exact font is missing.
        let apply = control(&mut app, &ctx, "Apply");
        click(&mut app, &ctx, apply);
        assert!(app.source_editor.is_some());
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        let label = app.source_editor.as_ref().unwrap().source.content.clone();
        let crate::source::Content::Text { font, .. } = label else {
            panic!()
        };
        let picker = control(&mut app, &ctx, &font.label());
        click(&mut app, &ctx, picker);
        let replacement = control(&mut app, &ctx, "Ubuntu Sans medium");
        click(&mut app, &ctx, replacement);
        assert!(
            matches!(&app.source_editor.as_ref().unwrap().source.content,
            crate::source::Content::Text {font:TextFont::Bundled(name),..} if name=="medium")
        );
        let command = app.source_editor.as_ref().unwrap().command();
        let expected = Engine::preview_edits(original.clone(), &[command]).unwrap();
        assert_ne!(
            expected.layers[0].pixels.rgba16(),
            original.layers[0].pixels.rgba16()
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        loop {
            frame(&mut app, &ctx, vec![], Default::default());
            if !app.pending
                && app.canvas_pixels.as_ref().is_some_and(|(w, h, pixels)| {
                    *pixels == expected.preview(None, (*w).max(*h), None, false).unwrap().2
                })
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Replacement font must preview actual engine pixels"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        let apply = control(&mut app, &ctx, "Apply");
        click(&mut app, &ctx, apply);
        super::text_tests::wait_edit(&mut app);
        assert!(app.source_editor.is_none());
        let mut engine = app.shared.lock().unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
        assert_eq!(
            engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
            expected.layers[0].pixels.rgba16()
        );
        engine.undo("human").unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].source, original.layers[0].source);
        assert_eq!(
            engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
            original.layers[0].pixels.rgba16()
        );
        drop(engine);
        let current = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.open_source(&current, "edit");
        frame(&mut app, &ctx, vec![], Default::default());
        let cancel = control(&mut app, &ctx, "Cancel");
        click(&mut app, &ctx, cancel);
        assert!(app.source_editor.is_none());
        assert!(app.transient.is_empty());
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
            original.layers[0].pixels.rgba16()
        );
    }

    #[test]
    fn unchanged_installed_text_retries_loading_and_busy_then_previews_ready_native_pixels() {
        use crate::fonts::{self, FontError, InstalledFont, TextFont};
        for depth in [8, 16] {
            for pending in [FontError::Loading, FontError::Busy] {
                let font = InstalledFont {
                    family: "Pending fixture".into(),
                    postscript: "PendingFixture-Regular".into(),
                    style: "normal".into(),
                    weight: 400,
                    stretch: 5,
                    index: 0,
                    sha256: "e".repeat(64),
                };
                let waiting = fonts::test_fonts(vec![(font.clone(), Err(pending))]);
                let (mut app, ctx) = small_fixture();
                let original = {
                    let mut engine = app.shared.lock().unwrap();
                    engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                    engine.resident_state_mut().expect("resident fixture").document.layers[0]
                        .pixels
                        .set16(1, 2, [21347, 41259, 31357, 65535]);
                    engine.resident_state_mut().expect("resident fixture").document.layers[0].source = Some(
                        crate::source::Source {
                            width: 32,
                            height: 32,
                            matrix: [1., 0., 0., 1., 0., 0.],
                            content: crate::source::Content::Text {
                                text: "Hi".into(),
                                layout: crate::source::TextLayout::Legacy,
                                font: TextFont::Installed(font.clone()),
                                size: 16.,
                                line_height: 1.2,
                                letter_spacing: 0.,
                                align: "left".into(),
                                color: [53471, 12679, 33457, 65535],
                            },
                        }
                        .into(),
                    );
                    engine.resident_state().expect("resident fixture").document.clone()
                };
                app.select_content(&original.layers[0].id);
                app.open_source(&original, "edit");
                let command = app.source_editor.as_ref().unwrap().command();
                // A preview can lose its snapshot between UI validation and rendering.
                app.source_editor.as_mut().unwrap().error = Some(pending.to_string());
                app.last_preview = Some((
                    original.id.clone(),
                    original.revision,
                    "cached failed preview".into(),
                    false,
                ));
                let draw_source = |app: &mut PeerBrush| {
                    ctx.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(1360., 900.),
                            )),
                            ..Default::default()
                        },
                        |ctx| app.source_window(ctx, &original),
                    )
                };
                let output = draw_source(&mut app);
                assert!(
                    output.viewport_output[&egui::ViewportId::ROOT].repaint_delay
                        <= Duration::from_millis(50)
                );
                assert!(app.transient.is_empty());
                assert!(app.source_editor.as_ref().unwrap().error.is_some());
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(
                    app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    original.layers[0].pixels.rgba16()
                );
                drop(waiting);
                let _ready = fonts::test_fonts(vec![(
                    font,
                    Ok(ab_glyph::FontArc::new(fonts::bundled("regular").unwrap())),
                )]);
                draw_source(&mut app);
                assert_eq!(
                    app.source_editor.as_ref().unwrap().command(),
                    command,
                    "Readiness must not substitute or edit metadata"
                );
                assert!(app.source_editor.as_ref().unwrap().error.is_none());
                assert!(
                    app.last_preview.is_none(),
                    "A cached loading failure must be retried without another property edit"
                );
                assert_eq!(app.transient, vec![command.clone()]);
                let expected = Engine::preview_edits(original.clone(), &[command]).unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(4);
                loop {
                    frame(&mut app, &ctx, vec![], Default::default());
                    if !app.pending
                        && app.canvas_pixels.as_ref().is_some_and(|(w, h, pixels)| {
                            *pixels == expected.preview(None, (*w).max(*h), None, false).unwrap().2
                        })
                    {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Ready unchanged font must render after pending state"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(
                    app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    original.layers[0].pixels.rgba16()
                );
                app.cancel_source();
            }
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
                        e.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set(x, y, [x as u8 * 7, y as u8 * 7, 53, 255]);
                    }
                }
            }
            frame(&mut app, &ctx, vec![], Default::default());
            let original = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            assert_eq!(
                app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
                original.export_png().unwrap()
            );
            let center = view.center();
            click(&mut app, &ctx, center);
            assert!(
                app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty(),
                "Geometry preview must not paint"
            );
            app.cancel_geometry();
            assert!(app.geometry.is_none());
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
            assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        }
    }
    fn small_fixture() -> (PeerBrush, egui::Context) {
        let (mut app, ctx) = fixture();
        let doc = Document::new(32, 32).unwrap();
        let id = doc.layers[0].id.clone();
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc;
        app.select_content(&id);
        (app, ctx)
    }
    #[test]
    fn quick_canvas_touch_commits_the_contact_force_and_the_next_mouse_dab_is_full_force() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let before = Document::new_depth(32, 32, depth).unwrap();
            app.shared
                .lock()
                .unwrap()
                .resident_state_mut()
                .unwrap()
                .document = before.clone();
            app.select_content(&before.layers[0].id);
            app.select_tool(Tool::Brush);
            app.radius = 4.;
            app.brush = crate::brush::Settings {
                pressure_size: false,
                ..Default::default()
            };
            frame(&mut app, &ctx, vec![], Default::default());
            let canvas = app.view_rect.unwrap();
            let viewport = app.canvas_viewport.unwrap();
            let scale = ((viewport.width().min(viewport.height()) - 60.) / 32.).max(0.01)
                * app.zoom;
            let tap = app.view.point([8.5, 8.5], canvas, scale);
            let mouse = app.view.point([20.5, 20.5], canvas, scale);
            let tap_point = app.view.document(tap, canvas, scale);
            let touch = |id, pos, phase, force| egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(id),
                phase,
                pos,
                force: Some(force),
            };
            let primary = |pos, pressed| {
                button(pos, egui::PointerButton::Primary, pressed, Default::default())
            };
            frame(
                &mut app,
                &ctx,
                vec![
                    touch(1, tap, egui::TouchPhase::Start, 0.25),
                    egui::Event::PointerMoved(tap),
                    primary(tap, true),
                    touch(1, tap, egui::TouchPhase::End, 0.),
                    primary(tap, false),
                    egui::Event::PointerGone,
                    touch(2, mouse, egui::TouchPhase::Move, 0.9),
                ],
                Default::default(),
            );
            let mut expected = app.stroke_command(&[tap_point], app.stroke_color());
            expected["pressures"] = json!([0.25]);
            let expected = Engine::preview_edits(before, &[expected]).unwrap();
            {
                let engine = app.shared.lock().unwrap();
                assert_eq!(engine.history_lengths(), [1, 0]);
                assert_eq!(
                    engine.resident().unwrap().layers[0].pixels.rgba16(),
                    expected.layers[0].pixels.rgba16()
                );
            }

            frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(mouse), primary(mouse, true), primary(mouse, false)],
                Default::default(),
            );
            let engine = app.shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [2, 0]);
            let pixels = &engine.resident().unwrap().layers[0].pixels;
            assert_eq!(pixels.get16(8, 8), expected.layers[0].pixels.get16(8, 8));
            assert_eq!(pixels.get16(20, 20)[3], 65535);
        }
    }

    #[test]
    fn active_touch_stroke_keeps_the_last_move_before_release_and_ignores_later_hover() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let original = Document::new_depth(32, 32, depth).unwrap();
            app.shared.lock().unwrap().resident_state_mut().unwrap().document = original.clone();
            app.select_content(&original.layers[0].id);
            app.select_tool(Tool::Brush);
            app.radius = 3.;
            frame(&mut app, &ctx, vec![], Default::default());
            let canvas = app.view_rect.unwrap();
            let viewport = app.canvas_viewport.unwrap();
            let scale = ((viewport.width().min(viewport.height()) - 60.) / 32.).max(0.01)
                * app.zoom;
            let start = app.view.point([6.5, 8.5], canvas, scale);
            let end = app.view.point([20.5, 8.5], canvas, scale);
            let hover = app.view.point([24.5, 24.5], canvas, scale);
            let touch = |pos, phase, force| egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase,
                pos,
                force: Some(force),
            };
            frame(
                &mut app,
                &ctx,
                vec![
                    touch(start, egui::TouchPhase::Start, 0.25),
                    egui::Event::PointerMoved(start),
                    button(start, egui::PointerButton::Primary, true, Default::default()),
                ],
                Default::default(),
            );
            assert_eq!(app.stroke_pressures, [0.25]);
            assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
            let points = [app.points[0], app.view.document(end, canvas, scale)];
            let mut command = app.stroke_command(&points, app.stroke_color());
            command["pressures"] = json!([0.25, 0.75]);
            let expected = Engine::preview_edits(original, &[command]).unwrap();
            frame(
                &mut app,
                &ctx,
                vec![
                    touch(end, egui::TouchPhase::Move, 0.75),
                    egui::Event::PointerMoved(end),
                    touch(end, egui::TouchPhase::End, 0.),
                    primary_release(end),
                    egui::Event::PointerGone,
                    egui::Event::PointerMoved(hover),
                ],
                Default::default(),
            );
            let engine = app.shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [1, 0]);
            assert_eq!(
                engine.resident().unwrap().layers[0].pixels.rgba16(),
                expected.layers[0].pixels.rgba16()
            );
        }
    }

    #[test]
    fn rapid_workspace_stroke_keeps_ordered_force_and_the_exact_release() {
        for depth in [8, 16] {
            for split in [false, true] {
                let (mut app, ctx) = small_fixture();
                let before = Document::new_depth(32, 32, depth).unwrap();
                app.shared.lock().unwrap().resident_state_mut().unwrap().document = before.clone();
                app.select_content(&before.layers[0].id);
                app.select_tool(Tool::Brush);
                app.zoom = 0.55;
                app.view = view::View { angle: 25., mirror_h: true, ..Default::default() };
                app.radius = 2.5;
                app.brush = crate::brush::Settings {
                    pressure_size: true, pressure_opacity: true, ..Default::default()
                };
                frame(&mut app, &ctx, vec![], Default::default());
                let canvas = app.view_rect.unwrap();
                let projection = app.view.projection(canvas, canvas.width() / 32.);
                let start = projection.point([-5.4, 9.3]);
                let moved = projection.point([7.3, 10.1]);
                let release = projection.point([17.3, 9.9]);
                let later = projection.point([26.2, 24.1]);
                assert!(!projection.contains(start));
                assert!(app.canvas_input_region.unwrap().0.contains(start));
                let touch = |pos, phase, force| egui::Event::Touch {
                    device_id: egui::TouchDeviceId(1), id: egui::TouchId(1),
                    phase, pos, force: Some(force),
                };
                let press = button(start, egui::PointerButton::Primary, true, Default::default());
                frame(&mut app, &ctx, vec![
                    egui::Event::PointerMoved(start), press.clone(), primary_release(start),
                ], Default::default());
                assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
                let mut events = vec![
                    touch(start, egui::TouchPhase::Start, 0.2),
                    egui::Event::PointerMoved(start), press,
                ];
                if split {
                    frame(&mut app, &ctx, std::mem::take(&mut events), Default::default());
                    assert!(app.workspace_stroke.is_some());
                    assert!(app.points.is_empty());
                    assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
                }
                events.extend([
                    touch(moved, egui::TouchPhase::Move, 0.75), egui::Event::PointerMoved(moved),
                    touch(release, egui::TouchPhase::End, 0.), primary_release(release),
                    egui::Event::PointerGone, egui::Event::PointerMoved(later),
                ]);
                let points = [start, moved, release].map(|pos| projection.document(pos));
                let mut command = app.stroke_command(&points, app.stroke_color());
                command["pressures"] = json!([0.2, 0.75, 0.75]);
                let expected = Engine::preview_edits(before.clone(), &[command]).unwrap();
                frame(&mut app, &ctx, events, Default::default());
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(engine.history_lengths(), [1, 0]);
                let pixels = &engine.resident().unwrap().layers[0].pixels;
                assert_eq!(pixels, &expected.layers[0].pixels);
                assert_ne!(pixels, &before.layers[0].pixels);
                engine.undo("human").unwrap();
                assert_eq!(engine.resident().unwrap().layers[0].pixels, before.layers[0].pixels);
            }
        }
    }

    #[test]
    fn workspace_stroke_cancels_if_the_layer_changes_before_release() {
        let (mut app, ctx) = small_fixture();
        let mut before = Document::new_depth(32, 32, 16).unwrap();
        before.layers.push(crate::engine::Layer::new("Second", "paint", 32, 32));
        before.ensure_depth();
        app.shared.lock().unwrap().resident_state_mut().unwrap().document = before.clone();
        app.select_content(&before.layers[0].id);
        app.select_tool(Tool::Brush);
        app.zoom = 0.55;
        frame(&mut app, &ctx, vec![], Default::default());
        let canvas = app.view_rect.unwrap();
        let projection = app.view.projection(canvas, canvas.width() / 32.);
        let start = projection.point([-5., 16.]);
        let end = projection.point([16., 16.]);
        frame(&mut app, &ctx, vec![
            egui::Event::PointerMoved(start),
            button(start, egui::PointerButton::Primary, true, Default::default()),
        ], Default::default());
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)], Default::default());
        assert!(!app.points.is_empty());
        assert!(app.workspace_stroke.is_some());
        app.select_content(&before.layers[1].id);
        frame(&mut app, &ctx, vec![primary_release(end)], Default::default());
        assert!(app.points.is_empty() && app.workspace_stroke.is_none());
        let engine = app.shared.lock().unwrap();
        assert_eq!(engine.history_lengths(), [0, 0]);
        for (actual, original) in engine.resident().unwrap().layers.iter().zip(&before.layers) {
            assert_eq!(actual.pixels, original.pixels);
        }
    }

    #[test]
    fn cancelled_pointer_contact_discards_only_the_active_canvas_stroke() {
        let (mut app, ctx) = small_fixture();
        app.select_tool(Tool::Brush);
        let original = app.shared.lock().unwrap().resident().unwrap().export_png().unwrap();
        frame(&mut app, &ctx, vec![], Default::default());
        let start = app.view_rect.unwrap().center();
        let touch = |id, phase| egui::Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId(id),
            phase,
            pos: start,
            force: Some(0.4),
        };
        let primary = |pressed| {
            button(start, egui::PointerButton::Primary, pressed, Default::default())
        };
        frame(
            &mut app,
            &ctx,
            vec![
                touch(1, egui::TouchPhase::Start),
                egui::Event::PointerMoved(start),
                primary(true),
            ],
            Default::default(),
        );
        assert_eq!(app.stroke_pressures, [0.4]);
        assert!(app.live_gesture.is_some());
        frame(
            &mut app,
            &ctx,
            vec![touch(2, egui::TouchPhase::Cancel)],
            Default::default(),
        );
        assert_eq!(app.current_pressure, Some(0.4));
        assert!(app.live_gesture.is_some());
        frame(
            &mut app,
            &ctx,
            vec![touch(1, egui::TouchPhase::Cancel), egui::Event::PointerGone],
            Default::default(),
        );
        assert!(app.points.is_empty() && app.stroke_pressures.is_empty());
        assert!(app.drag_start.is_none() && app.live_gesture.is_none());
        frame(&mut app, &ctx, vec![primary(false)], Default::default());
        {
            let engine = app.shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert_eq!(engine.resident().unwrap().export_png().unwrap(), original);
        }
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(start), primary(true), primary(false)],
            Default::default(),
        );
        assert_eq!(app.shared.lock().unwrap().history_lengths(), [1, 0]);
    }

    #[test]
    fn toolbar_selection_is_fully_painted_in_the_click_frame() {
        let (mut app, ctx) = small_fixture();
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
        let mut time = 0.;
        let mut draw = |app: &mut PeerBrush, events| {
            time += 1. / 240.;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            );
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.fill == Color32::from_rgb(74, 48, 71)
                            && rect.rect.left() < 60.
                            && rect.rect.size() == Vec2::new(42., 39.) =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        draw(&mut app, vec![]);
        let selected = draw(&mut app, vec![]);
        assert_eq!(selected.len(), 1);
        let brush = selected[0];
        let eraser = brush.translate(Vec2::new(0., 5. * 43.));
        for (tool, rect) in [(Tool::Eraser, eraser), (Tool::Brush, brush)] {
            let pos = rect.center();
            draw(
                &mut app,
                vec![
                    egui::Event::PointerMoved(pos),
                    button(pos, egui::PointerButton::Primary, true, Default::default()),
                ],
            );
            let selected = draw(
                &mut app,
                vec![button(
                    pos,
                    egui::PointerButton::Primary,
                    false,
                    Default::default(),
                )],
            );
            assert!(app.tool == tool);
            assert_eq!(selected.len(), 1, "Only the current tool is highlighted");
            assert!(
                selected[0].contains(pos),
                "The clicked tool is painted without a fade: {pos:?} vs {:?}",
                selected[0]
            );
        }
        let engine = app.shared.lock().unwrap();
        assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), before);
    }
    #[test]
    fn mask_link_control_accepts_pointer_and_keyboard_without_changing_artwork_or_target() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            {
                let mut engine = app.shared.lock().unwrap();
                engine.resident_state_mut().expect("resident fixture").document.bit_depth = depth;
                let id = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"mask.add","layer":id})],
                        None,
                        None,
                        "Mask",
                    )
                    .unwrap();
                engine.resident_state_mut().expect("resident fixture").undo.clear();
            }
            app.mask = true;
            let selected = app.selected.clone();
            let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
            let draw = |app: &mut PeerBrush, events| {
                let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                let mut response = None;
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(300., 180.))),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            response = Some(app.mask_link_button(ui, &doc, &doc.layers[0], false));
                        });
                    },
                );
                response.unwrap()
            };
            let response = draw(&mut app, vec![]);
            let pos = response.rect.center();
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
            assert!(
                !app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0]
                    .mask
                    .as_ref()
                    .unwrap()
                    .linked
            );
            assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
            response.request_focus();
            for key in [egui::Key::Space, egui::Key::Enter] {
                let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0]
                    .mask
                    .as_ref()
                    .unwrap()
                    .linked;
                for pressed in [true, false] {
                    draw(
                        &mut app,
                        vec![egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers: Default::default(),
                        }],
                    );
                }
                assert_ne!(
                    app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0]
                        .mask
                        .as_ref()
                        .unwrap()
                        .linked,
                    before
                );
            }
            assert_eq!(app.selected, selected);
            assert!(app.mask);
            assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(), before);
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.layers[0].locked = true;
            assert!(!draw(&mut app, vec![]).enabled());
        }
    }
    #[test]
    fn magic_wand_selects_without_creating_or_changing_native_masks() {
        for depth in [8, 16] {
            for existing_mask in [false, true] {
                let (mut app, ctx) = fixture();
                let before = {
                    let mut engine = app.shared.lock().unwrap();
                    engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                    for y in 0..8 {
                        for x in 0..8 {
                            engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
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
                    let id = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
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
                    engine.resident_state_mut().expect("resident fixture").undo.clear();
                    engine.resident_state().expect("resident fixture").document.clone()
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
                assert_eq!(engine.resident_state().expect("resident fixture").document.selection, Some([0, 0, 4, 8]));
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert_eq!(
                    serde_json::to_value(&engine.resident_state().expect("resident fixture").document.layers[0].mask).unwrap(),
                    serde_json::to_value(&before.layers[0].mask).unwrap()
                );
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, depth);
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                engine.undo("human").unwrap();
                assert!(engine.resident_state().expect("resident fixture").document.selection.is_none());
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
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
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                for y in 0..8 {
                    for x in 0..8 {
                        e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
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
                e.resident_state().expect("resident fixture").document.clone()
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
            let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
            modified_key(&mut app, &ctx, egui::Key::Delete, Default::default());
            let mut e = app.shared.lock().unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.selection, Some([0, 0, 4, 8]));
            assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), history + 1);
            for y in 0..8 {
                for x in 0..8 {
                    assert_eq!(
                        e.resident_state().expect("resident fixture").document.layers[0].pixels.get16(x, y),
                        if x < 4 {
                            [0; 4]
                        } else {
                            before.layers[0].pixels.get16(x, y)
                        }
                    );
                }
            }
            e.undo("human").unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
        }
    }
    #[test]
    fn area_delete_targets_active_color_or_mask_with_multiple_selected_roots() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let (mut app, ctx) = small_fixture();
                let before = {
                    let mut e = app.shared.lock().unwrap();
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
                    for y in 0..24 {
                        for x in 0..32 {
                            e.resident_state_mut().expect("resident fixture").document.layers[0]
                                .pixels
                                .set16(x, y, [12347, 33559, 51237, 65535]);
                        }
                    }
                    let id = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    e.edit("human", &[json!({"op":"layer.add","name":"Untouched"}), json!({"op":"mask.add","layer":id}), json!({"op":"selection","kind":"ellipse","rect":[2,2,28,22],"feather":2}), json!({"op":"selection","kind":"rectangle","mode":"subtract","rect":[12,9,18,15]})], None, None, "Fixture").unwrap();
                    e.resident_state_mut().expect("resident fixture").document.layers[1].visible = false;
                    for y in 0..24 {
                        for x in 0..32 {
                            e.resident_state_mut().expect("resident fixture").document.layers[0]
                                .pixels
                                .set16(x, y, [12347, 33559, 51237, 65535]);
                        }
                    }
                    let active = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    e.edit(
                        "human",
                        &[json!({"op":"mask.add","layer":active})],
                        None,
                        None,
                        "Active mask",
                    )
                    .unwrap();
                    e.resident_state_mut().expect("resident fixture").undo.clear();
                    e.resident_state().expect("resident fixture").document.clone()
                };
                let id = before.layers[0].id.clone();
                let other = before.layers[1].id.clone();
                app.select_content(&id);
                frame(&mut app, &ctx, vec![], Default::default());
                app.selection_layers = [id.clone(), other.clone()].into_iter().collect();
                app.mask = mask;
                let command = json!({"op":"paint.clear_selection","layer":id,"mask":mask});
                let expected = Engine::preview_edits(before.clone(), &[command]).unwrap();
                let coverage = crate::selection::current(&before).unwrap().mask.rgba();
                // An inactive selected root's reservation must not block active-area deletion.
                app.shared
                    .lock()
                    .unwrap()
                    .reserve(
                        "agent",
                        "Other layer",
                        vec![crate::engine::Scope::layer(&other)],
                    )
                    .unwrap();
                modified_key(&mut app, &ctx, egui::Key::Delete, Default::default());
                {
                    let mut e = app.shared.lock().unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                    assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 2);
                    assert_eq!(
                        e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                        expected.layers[0].pixels.rgba16()
                    );
                    assert_eq!(
                        serde_json::to_value(&e.resident_state().expect("resident fixture").document.layers[0].mask).unwrap(),
                        serde_json::to_value(&expected.layers[0].mask).unwrap()
                    );
                    assert_eq!(
                        serde_json::to_value(&e.resident_state().expect("resident fixture").document.layers[1]).unwrap(),
                        serde_json::to_value(&before.layers[1]).unwrap()
                    );
                    assert_eq!(
                        crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap().mask.rgba(),
                        coverage
                    );
                    assert!(e.resident_state().expect("resident fixture").document.export_png().unwrap() != before.export_png().unwrap());
                    e.undo("human").unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                    assert_eq!(
                        serde_json::to_value(&e.resident_state().expect("resident fixture").document.layers).unwrap(),
                        serde_json::to_value(&before.layers).unwrap()
                    );
                    e.reserve(
                        "agent",
                        "Active layer",
                        vec![crate::engine::Scope::layer(&id)],
                    )
                    .unwrap();
                }
                modified_key(&mut app, &ctx, egui::Key::Delete, Default::default());
                let e = app.shared.lock().unwrap();
                assert!(e.resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                assert_eq!(
                    crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap().mask.rgba(),
                    coverage
                );
            }
        }
    }
    #[test]
    fn keyboard_selected_cut_paste_preserves_feather_holes_native_samples_and_history() {
        for depth in [8, 16] {
            for modifiers in [
                egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
                egui::Modifiers {
                    command: true,
                    ..Default::default()
                },
            ] {
                let (mut app, ctx) = small_fixture();
                app.clipboard = crate::clipboard::Worker::memory(ctx.clone());
                let before = {
                    let mut e = app.shared.lock().unwrap();
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
                    for y in 0..24 {
                        for x in 0..32 {
                            e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
                                x,
                                y,
                                [12347 + x as u16, 33559 + y as u16, 51237, 60001],
                            );
                        }
                    }
                    e.edit("human", &[json!({"op":"selection","kind":"ellipse","rect":[2,2,28,22],"feather":2}), json!({"op":"selection","kind":"rectangle","mode":"subtract","rect":[12,9,18,15]})], None, None, "Fixture").unwrap();
                    e.resident_state_mut().expect("resident fixture").undo.clear();
                    e.resident_state().expect("resident fixture").document.clone()
                };
                let id = before.layers[0].id.clone();
                app.select_content(&id);
                frame(&mut app, &ctx, vec![], Default::default());
                let image = crate::clipboard::Image::copy(&before, &id, false, false).unwrap();
                let cleared = Engine::preview_edits(
                    before.clone(),
                    &[json!({"op":"paint.clear_selection","layer":id})],
                )
                .unwrap();
                let area = crate::selection::current(&before).unwrap();
                assert!(area
                    .mask
                    .rgba()
                    .chunks_exact(4)
                    .any(|p| p[0] > 0 && p[0] < 255));
                assert_eq!(area.value(15, 12), 0.);
                modified_key(&mut app, &ctx, egui::Key::X, modifiers);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty() || app.busy {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Cut did not finish: {}",
                        app.message
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    frame(&mut app, &ctx, vec![], Default::default());
                }
                {
                    let e = app.shared.lock().unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
                    assert_eq!(
                        e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                        cleared.layers[0].pixels.rgba16()
                    );
                    assert_eq!(
                        e.resident_state().expect("resident fixture").document.layers[0].pixels.get16(15, 12),
                        before.layers[0].pixels.get16(15, 12)
                    );
                    assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                }
                frame(&mut app, &ctx, vec![], Default::default());
                modified_key(&mut app, &ctx, egui::Key::V, modifiers);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len() < 2 || app.busy {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Paste did not finish: {}",
                        app.message
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    frame(&mut app, &ctx, vec![], Default::default());
                }
                let mut e = app.shared.lock().unwrap();
                let pasted = e
                    .resident_state().expect("resident fixture").document
                    .layers
                    .iter()
                    .find(|l| l.name == "Pasted image")
                    .unwrap();
                assert_eq!(Some([pasted.x, pasted.y]), image.origin);
                assert_eq!(
                    pasted.pixels.rgba16(),
                    image.samples16.clone().unwrap_or_else(|| image
                        .bytes
                        .iter()
                        .map(|v| *v as u16 * 257)
                        .collect())
                );
                assert_eq!(e.resident_state().expect("resident fixture").document.bit_depth, depth);
                assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 3);
                assert_eq!(
                    e.resident_state().expect("resident fixture").document
                        .layers
                        .iter()
                        .filter(|l| l.name == "Pasted image")
                        .count(),
                    1
                );
                assert_eq!(
                    e.resident_state().expect("resident fixture").document
                        .layers
                        .iter()
                        .find(|l| l.id == id)
                        .unwrap()
                        .pixels
                        .rgba16(),
                    cleared.layers[0].pixels.rgba16()
                );
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 2);
                let saved = crate::psd::decode(&crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap()).unwrap();
                assert_eq!(saved.export_png().unwrap(), e.resident_state().expect("resident fixture").document.export_png().unwrap());
                if modifiers.ctrl {
                    if let Some(directory) = std::env::var_os("PEERBRUSH_TEST_CAPTURE_DIR") {
                        let root = std::path::PathBuf::from(directory);
                        std::fs::create_dir_all(&root).unwrap();
                        for (name, doc) in
                            [("Before", &before), ("Cut", &cleared), ("Paste", &e.resident_state().expect("resident fixture").document)]
                        {
                            std::fs::write(
                                root.join(format!("{name}{depth}.psd")),
                                crate::psd::encode(doc).unwrap(),
                            )
                            .unwrap();
                            std::fs::write(
                                root.join(format!("{name}{depth}.png")),
                                doc.export_png().unwrap(),
                            )
                            .unwrap();
                        }
                    }
                }
                let result = e.resident_state().expect("resident fixture").document.export_png().unwrap();
                e.undo("human").unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), cleared.export_png().unwrap());
                e.undo("human").unwrap();
                assert_eq!(
                    e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert_eq!(
                    crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap().mask.rgba(),
                    area.mask.rgba()
                );
                e.redo("human").unwrap();
                e.redo("human").unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), result);
            }
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
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                    e.resident_state_mut().expect("resident fixture").document.layers[0]
                        .pixels
                        .set16(4, 4, [12347, 33559, 51237, 65535]);
                    let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    e.edit(
                        "human",
                        &[json!({"op":"mask.add","layer":layer})],
                        None,
                        None,
                        "Mask",
                    )
                    .unwrap();
                    e.edit("human",&[json!({"op":"selection","kind":"ellipse","rect":[4,5,20,22],"feather":2})],None,None,"Select").unwrap();
                    e.resident_state_mut().expect("resident fixture").undo.clear();
                    e.resident_state().expect("resident fixture").document.clone()
                };
                app.select_content(&before.layers[0].id);
                app.mask = true;
                frame(&mut app, &ctx, vec![], Default::default());
                let original = crate::selection::current(&before).unwrap();
                modified_key(&mut app, &ctx, egui::Key::I, modifiers);
                {
                    let mut e = app.shared.lock().unwrap();
                    let inverse = crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap();
                    for y in 0..32 {
                        for x in 0..32 {
                            assert!((original.value(x, y) + inverse.value(x, y) - 1.).abs() < 1e-6);
                        }
                    }
                    assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                    assert_eq!(e.resident_state().expect("resident fixture").document.bit_depth, depth);
                    assert_eq!(
                        e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                    assert!(app.tool == Tool::Brush);
                    assert!(app.mask);
                    e.undo("human").unwrap();
                    assert_eq!(
                        crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap().mask.rgba(),
                        original.mask.rgba()
                    );
                    e.redo("human").unwrap();
                }
                let current = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                app.begin_rename(&current, &app.selected.clone());
                frame(&mut app, &ctx, vec![], Default::default());
                let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
                modified_key(&mut app, &ctx, egui::Key::I, modifiers);
                let e = app.shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), history);
                assert_eq!(
                    crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap().mask.rgba(),
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
    fn clipboard_requests_share_resident_and_native_frame_owners_and_keep_selection_intent() {
        use crate::clipboard::EditRequest;
        use crate::engine::{
            native::{worker::tests::Fixture, NativeDocument},
            scene::{CapturedScene, LegacyTileCache},
        };
        for depth in [8, 16] {
            for selected in [false, true] {
                let fixture = Fixture::new();
                let (runtime, connection) = fixture.runtime();
                let mut doc = Document::new_depth(9, 7, depth).unwrap();
                doc.layers[0].pixels.set16(3, 4, [12347, 23459, 34571, 49153]);
                if selected {
                    doc.selection = Some([1, 2, 6, 7]);
                }
                let cache = LegacyTileCache::new(&runtime.resident(), 16).unwrap();
                let staged = CapturedScene::from_parts(
                    doc.clone(), "clipboard-ui".into(), vec![], None, None,
                ).stage(&runtime, &cache, &|| Ok(())).unwrap();
                let native = NativeDocument::from_pending(
                    staged.scene.into_provisional_pending().unwrap(),
                ).unwrap();
                let (mut app, _) = small_fixture();
                app.select_content(&doc.layers[0].id);
                app.layer_clipboard = true;
                app.mask = true;
                app.mask_step = Some("observed-mask-step".into());
                let history = app.shared.lock().unwrap().history_lengths();
                for snapshot in [DocumentSnapshot::from(doc.clone()), native.clone().into()] {
                    for cut in [false, true] {
                        for merged in [false, true] {
                            let used = runtime.resident().used();
                            let request = app.copy_request(&snapshot, cut, merged).unwrap();
                            let captured = match &request {
                                EditRequest::Layers { doc: captured, ids, cut: deletion } => {
                                    assert!(!selected && (cut || !merged));
                                    assert_eq!(ids, &[doc.layers[0].id.clone()]);
                                    assert_eq!(deletion.is_some(), cut);
                                    captured
                                }
                                EditRequest::Pixels { doc: captured, target, mask, merged: combined, cut: deletion, step, .. } => {
                                    assert!(selected || (!cut && merged));
                                    assert_eq!(target, &doc.layers[0].id);
                                    assert!(*mask);
                                    assert_eq!(*combined, !cut && merged);
                                    assert_eq!(*deletion, cut);
                                    assert_eq!(step.as_deref(), Some("observed-mask-step"));
                                    captured
                                }
                                EditRequest::Resident(_) | EditRequest::Paste { .. } => panic!("Copy must retain its frame snapshot"),
                            };
                            match (&snapshot, captured) {
                                (DocumentSnapshot::Resident(a), DocumentSnapshot::Resident(b)) => assert!(Arc::ptr_eq(a, b)),
                                (DocumentSnapshot::Native(a), DocumentSnapshot::Native(b)) => assert_eq!(a.key(), b.key()),
                                _ => panic!("Clipboard request changed storage authority"),
                            }
                            assert_eq!(captured.view().selection(), doc.selection);
                            assert_eq!(runtime.resident().used(), used);
                        }
                    }
                    app.layer_clipboard = false;
                    assert_eq!(app.copy_request(&snapshot, true, false).is_some(), selected);
                    app.layer_clipboard = true;
                }
                assert_eq!(app.shared.lock().unwrap().history_lengths(), history);
                drop((native, staged.session, staged.seals, cache, runtime, connection));
            }
        }
    }
    #[test]
    fn layer_clipboard_intent_tracks_rows_and_canvas_without_touching_os_clipboard() {
        let (mut app, ctx) = small_fixture();
        frame(&mut app, &ctx, vec![], Default::default());
        let id = app.selected.clone();
        let row = app.layer_rects[&id];
        click(&mut app, &ctx, row.right_center() - Vec2::new(30.0, 0.0));
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        assert!(app.layer_clipboard);
        assert!(
            matches!(app.copy_request(&doc.clone().into(), false, false), Some(crate::clipboard::EditRequest::Layers { ids, cut: None, .. }) if ids == vec![id.clone()])
        );
        assert!(
            matches!(app.copy_request(&doc.clone().into(), true, false), Some(crate::clipboard::EditRequest::Layers { doc: copied, ids, cut: Some(_), .. }) if copied.view().id() == doc.id && copied.view().revision() == doc.revision && ids == vec![id.clone()])
        );
        assert!(matches!(
            app.copy_request(&doc.clone().into(), false, true),
            Some(crate::clipboard::EditRequest::Pixels { merged: true, cut: false, .. })
        ));
        app.tool = Tool::None;
        let canvas = app.view_rect.unwrap().center();
        click(&mut app, &ctx, canvas);
        assert!(!app.layer_clipboard);
        assert!(matches!(
            app.copy_request(&doc.clone().into(), false, false),
            Some(crate::clipboard::EditRequest::Pixels { merged: false, cut: false, .. })
        ));
        assert!(app.copy_request(&doc.clone().into(), true, false).is_none());
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
    }
    #[test]
    fn wand_selection_overrides_layer_row_copy_intent_at_both_depths() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            {
                let mut e = app.shared.lock().unwrap();
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                for y in 0..32 {
                    for x in 0..32 {
                        e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
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
                app.selected = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
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
            let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            assert_eq!(doc.selection, Some([4, 5, 12, 13]));
            assert!(app.layer_clipboard);
            let Some(crate::clipboard::EditRequest::Pixels {
                doc: snapshot,
                target,
                mask,
                merged,
                cut: false,
                ..
            }) = app.copy_request(&doc.clone().into(), false, false)
            else {
                panic!("A layer-row click must not copy the whole layer while pixels are selected");
            };
            let image = crate::clipboard::Image::copy(snapshot.view().resident().unwrap(), &target, mask, merged).unwrap();
            assert_eq!(
                (image.width, image.height, image.origin),
                (8, 8, Some([4, 5]))
            );
            assert_eq!(image.samples16.is_some(), depth == 16);
            assert!(
                matches!(app.copy_request(&doc.clone().into(), true, false),
                Some(crate::clipboard::EditRequest::Pixels { target, cut: true, .. }) if target == app.selected),
                "Selected cut must erase pixels rather than delete the layer"
            );
            let baseline = doc.export_png().unwrap();
            let mut engine = app.shared.lock().unwrap();
            let history = engine.resident_state().expect("resident fixture").undo.len();
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
                .resident_state().expect("resident fixture").document
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
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history + 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), baseline);
            assert_eq!(engine.resident_state().expect("resident fixture").document.selection, doc.selection);
        }
    }
    fn pressed_wand_fixture(depth: u16) -> (PeerBrush, egui::Context, Pos2, Document) {
        pressed_selection_fixture(depth, Tool::MagicWand, "wand")
    }
    fn pressed_selection_fixture(
        depth: u16,
        tool: Tool,
        kind: &str,
    ) -> (PeerBrush, egui::Context, Pos2, Document) {
        pressed_selection_fixture_with_previous(depth, tool, kind, false)
    }
    fn pressed_selection_fixture_with_previous(
        depth: u16,
        tool: Tool,
        kind: &str,
        previous: bool,
    ) -> (PeerBrush, egui::Context, Pos2, Document) {
        let (mut app, ctx) = small_fixture();
        let before = {
            let mut engine = app.shared.lock().unwrap();
            engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
            for y in 0..32 {
                for x in 0..32 {
                    engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
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
            app.selected = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
            if previous {
                engine
                    .edit(
                        "human",
                        &[json!({"op":"selection","kind":"rectangle","rect":[0,0,2,2]})],
                        None,
                        None,
                        "Previous coverage",
                    )
                    .unwrap();
                engine.resident_state_mut().expect("resident fixture").undo.clear();
            }
            engine.resident_state().expect("resident fixture").document.clone()
        };
        app.clipboard = crate::clipboard::Worker::memory(ctx.clone());
        app.select_tool(tool);
        app.selection_kind = kind.into();
        app.selection_tolerance = 0.;
        app.selection_merged = false;
        frame(&mut app, &ctx, vec![], Default::default());
        let canvas = app.view_rect.unwrap();
        let point = canvas.min + Vec2::new(6.5, 7.5) * (canvas.width() / 32.);
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
            Default::default(),
        );
        (app, ctx, point, before)
    }
    fn primary_release(point: Pos2) -> egui::Event {
        egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }
    }
    fn shortcut_press(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }
    }
    #[test]
    fn selection_without_shortcuts_keeps_the_input_event_buffer() {
        let (mut app, ctx) = small_fixture();
        let frame = app.doc_snapshot.clone();
        let doc = frame.view();
        app.select_tool(Tool::Selection);
        app.selection_kind = "rectangle".into();
        app.canvas_press_document = Some(doc.id().into());
        let point = Pos2::new(20., 20.);
        let _ = ctx.run(
            egui::RawInput {
                events: vec![
                    egui::Event::Text("ordinary input".into()),
                    primary_release(point),
                    egui::Event::PointerMoved(point),
                ],
                focused: true,
                ..Default::default()
            },
            |ctx| {
                let before = ctx.input(|input| input.events.as_ptr());
                assert!(app.hold_canvas_selection_shortcuts(ctx, doc).is_none());
                assert_eq!(ctx.input(|input| input.events.as_ptr()), before);
                assert!(app.defer_canvas_shortcuts(ctx, doc, false).is_none());
                assert_eq!(ctx.input(|input| input.events.as_ptr()), before);
                assert!(app.canvas_press_document.is_none());
            },
        );
    }
    #[test]
    fn wand_release_before_transform_shortcut_in_one_frame_keeps_the_selected_area() {
        for depth in [8, 16] {
            for (key, tool) in [
                (egui::Key::W, Tool::Move),
                (egui::Key::E, Tool::Rotate),
                (egui::Key::R, Tool::Scale),
            ] {
                let (mut app, ctx, point, before) = pressed_wand_fixture(depth);
                frame(
                    &mut app,
                    &ctx,
                    vec![primary_release(point), shortcut_press(key)],
                    Default::default(),
                );
                frame(&mut app, &ctx, vec![], Default::default());
                assert!(app.tool == tool);
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.selection,
                    Some([4, 5, 12, 13]),
                    "{depth}-bit {key:?}"
                );
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.selection, None);
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    before.export_png().unwrap()
                );
            }
        }
    }

    #[test]
    fn native_wand_replays_release_frame_clipboard_and_transform_shortcuts() {
        use crate::engine::{native::{worker::tests::Fixture, NativeDocument}, scene::{CapturedScene, LegacyTileCache}};
        for depth in [8, 16] {
            for (event, tool) in [
                (egui::Event::Copy, Tool::MagicWand),
                (shortcut_press(egui::Key::W), Tool::Move),
                (shortcut_press(egui::Key::E), Tool::Rotate),
                (shortcut_press(egui::Key::R), Tool::Scale),
            ] {
                let fixture = Fixture::new();
                let (runtime, _connection) = fixture.runtime();
                let mut doc = Document::new_depth(32, 32, depth).unwrap();
                let layer = doc.layers[0].id.clone();
                for y in 5..13 {
                    for x in 4..12 { doc.layers[0].pixels.set16(x, y, [12347, 23459, 34571, 65535]); }
                }
                let sample = doc.layers[0].pixels.get16(4, 5);
                let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
                let staged = CapturedScene::from_parts(doc, "wand-native-ui".into(), vec![], None, None)
                    .stage(&runtime, &cache, &|| Ok(())).unwrap();
                let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
                let shared = Arc::new(Mutex::new(Engine::from_native(native.clone(), 8).unwrap()));
                let workspace = crate::workspace::attach(&shared);
                workspace.lock().unwrap().source_runtime = Some(runtime);
                let (mut app, ctx) = small_fixture();
                app.project_id = shared.lock().unwrap().project_id.clone();
                app.shared = shared.clone();
                app.workspace_root = shared.clone();
                app.workspace = workspace;
                app.doc_snapshot = native.into();
                app.selected = layer.clone();
                app.selection_layers = [layer.clone()].into_iter().collect();
                app.layer_clipboard = true;
                app.clipboard = crate::clipboard::Worker::memory(ctx.clone());
                app.select_tool(Tool::MagicWand);
                app.selection_kind = "wand".into();
                app.selection_tolerance = 0.;
                app.selection_merged = false;
                app.selection_contiguous = true;
                frame(&mut app, &ctx, vec![], Default::default());
                let canvas = app.view_rect.unwrap();
                let point = canvas.min + Vec2::new(6.5, 7.5) * (canvas.width() / 32.);
                frame(&mut app, &ctx, vec![egui::Event::PointerMoved(point), egui::Event::PointerButton {
                    pos: point, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default(),
                }], Default::default());
                frame(&mut app, &ctx, vec![primary_release(point), event], Default::default());
                assert!(app.deferred_selection_shortcuts.is_some());
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while app.deferred_selection_shortcuts.is_some() || app.edit_worker.pending() {
                    assert!(std::time::Instant::now() < deadline);
                    frame(&mut app, &ctx, vec![], Default::default());
                    std::thread::yield_now();
                }
                assert!(app.tool == tool);
                let snapshot = {
                    let engine = shared.lock().unwrap();
                    assert_eq!(engine.history_lengths(), [1, 0]);
                    assert_eq!(engine.native_document().unwrap().metadata().selection(), Some([4, 5, 12, 13]));
                    engine.doc_snapshot()
                };
                if tool == Tool::MagicWand {
                    let copy = app.clipboard.replies.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert!(copy.result.is_ok(), "{:?}", copy.result);
                    app.clipboard.request_snapshot(crate::clipboard::EditRequest::Paste {
                        shared: shared.clone(), doc: snapshot, target: layer,
                    }).unwrap();
                    let paste = app.clipboard.replies.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert!(paste.result.is_ok(), "{:?}", paste.result);
                    let engine = shared.lock().unwrap();
                    let document = engine.native_document().unwrap();
                    let pasted = document.metadata().layers().find(|layer| Some(layer.id()) == paste.selected.as_deref()).unwrap();
                    let raster = pasted.pixels();
                    assert_eq!((pasted.origin(), raster.width(), raster.height(), raster.depth()), ([4, 5], 8, 8, depth));
                    let pixels = document.source(raster.slot()).unwrap().prepare_region(
                        crate::raster::backed::Region::new([0, 0, 1, 1]).unwrap(), &mut || Ok(()),
                    ).unwrap();
                    assert_eq!(pixels.pixel16(0, 0).unwrap(), sample);
                    continue;
                }

                let rect = app.view_rect.unwrap();
                let scale = rect.width() / 32.;
                let center = rect.min + Vec2::new(8., 9.) * scale;
                let (start, end) = match tool {
                    Tool::Rotate => (center + Vec2::new(58., 0.), center + Vec2::new(0., 58.)),
                    Tool::Scale => (center, center + Vec2::new(20., 0.)),
                    _ => (center, center + Vec2::new(6., 3.) * scale),
                };
                frame(&mut app, &ctx, vec![
                    egui::Event::PointerMoved(start),
                    button(start, egui::PointerButton::Primary, true, Default::default()),
                ], Default::default());
                assert_eq!(app.gizmo_source.as_ref().unwrap().native, snapshot.view().native().map(|native| native.key()));
                frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)], Default::default());
                assert_eq!(app.transient.len(), 1);
                assert_eq!(app.transient[0]["selection_only"], true);
                assert_eq!(shared.lock().unwrap().history_lengths(), [1, 0]);
                let mut release = vec![primary_release(end)];
                if tool == Tool::Move { release.push(egui::Event::Copy); }
                frame(&mut app, &ctx, release, Default::default());
                if tool == Tool::Move { assert!(app.deferred_selection_shortcuts.is_some()); }
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while app.edit_worker.pending() || app.deferred_selection_shortcuts.is_some() {
                    assert!(std::time::Instant::now() < deadline);
                    frame(&mut app, &ctx, vec![], Default::default());
                    std::thread::yield_now();
                }
                let moved = {
                    let engine = shared.lock().unwrap();
                    assert_eq!(engine.history_lengths(), [2, 0], "{}", app.message);
                    engine.doc_snapshot()
                };
                if tool == Tool::Move {
                    let copy = app.clipboard.replies.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert!(copy.result.is_ok(), "{:?}", copy.result);
                    let native = moved.view().native().unwrap();
                    assert_eq!(native.metadata().selection(), Some([10, 8, 18, 16]));
                    let layer = native.metadata().layer(0).unwrap();
                    let [x, y] = layer.origin();
                    let pixels = native.source(layer.pixels().slot()).unwrap().prepare_region(
                        crate::raster::backed::Region::new([10 - x, 8 - y, 11 - x, 9 - y]).unwrap(),
                        &mut || Ok(()),
                    ).unwrap();
                    assert_eq!(pixels.pixel16(10 - x, 8 - y).unwrap(), sample);
                    app.edit(vec![json!({"op":"selection.clear"})], "Clear selection");
                    app.queue_selection_shortcuts(moved.view(), vec![shortcut_press(egui::Key::Delete)], Default::default());
                    assert!(app.deferred_selection_shortcuts.is_some());
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    while app.deferred_selection_shortcuts.is_some() || app.edit_worker.pending() {
                        assert!(std::time::Instant::now() < deadline);
                        frame(&mut app, &ctx, vec![], Default::default());
                        std::thread::yield_now();
                    }
                    let engine = shared.lock().unwrap();
                    let document = engine.native_document().unwrap().metadata();
                    assert!(document.selection().is_none());
                    assert_eq!(document.layers().len(), 1);
                    assert_eq!(engine.history_lengths(), [3, 0]);
                    let current = engine.doc_snapshot();
                    drop(engine);
                    app.clipboard.request_snapshot(crate::clipboard::EditRequest::Paste {
                        shared: shared.clone(), doc: current, target: layer.id().to_owned(),
                    }).unwrap();
                    let paste = app.clipboard.replies.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert!(paste.result.is_ok(), "{:?}", paste.result);
                    let engine = shared.lock().unwrap();
                    let document = engine.native_document().unwrap();
                    let pasted = document.metadata().layers()
                        .find(|layer| Some(layer.id()) == paste.selected.as_deref()).unwrap();
                    let raster = pasted.pixels();
                    assert_eq!((pasted.origin(), raster.width(), raster.height(), raster.depth()), ([10, 8], 8, 8, depth));
                    let pixels = document.source(raster.slot()).unwrap().prepare_region(
                        crate::raster::backed::Region::new([0, 0, 1, 1]).unwrap(), &mut || Ok(()),
                    ).unwrap();
                    assert_eq!(pixels.pixel16(0, 0).unwrap(), sample);
                    assert_eq!(engine.history_lengths(), [4, 0]);
                }
            }
        }
    }
    #[test]
    fn rejected_wand_release_never_replays_whole_layer_clipboard_delete_or_transform() {
        for depth in [8, 16] {
            for protected in [false, true] {
                for action in ["copy", "cut", "delete", "move", "rotate", "scale"] {
                    let (mut app, ctx, point, _) = pressed_wand_fixture(depth);
                    let before = {
                        let mut e = app.shared.lock().unwrap();
                        if protected {
                            e.resident_state_mut().expect("resident fixture").document.read_only = true;
                        } else {
                            e.reserve(
                                "assistant",
                                "Synthetic selection reservation",
                                vec![crate::engine::Scope {
                                    target: None,
                                    rect: None,
                                }],
                            )
                            .unwrap();
                        }
                        e.resident_state().expect("resident fixture").document.clone()
                    };
                    let event = match action {
                        "copy" => egui::Event::Copy,
                        "cut" => egui::Event::Cut,
                        "delete" => shortcut_press(egui::Key::Delete),
                        "move" => shortcut_press(egui::Key::W),
                        "rotate" => shortcut_press(egui::Key::E),
                        _ => shortcut_press(egui::Key::R),
                    };
                    frame(
                        &mut app,
                        &ctx,
                        vec![primary_release(point), event],
                        Default::default(),
                    );
                    assert!(
                        app.selection_commit_failed,
                        "{depth}-bit {action} protected={protected}"
                    );
                    assert!(app.tool == Tool::MagicWand);
                    assert!(
                        app.message
                            .contains(if protected { "read-only" } else { "Reserved" }),
                        "{}",
                        app.message
                    );
                    let e = app.shared.lock().unwrap();
                    assert_eq!(
                        serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(),
                        serde_json::to_value(&before).unwrap()
                    );
                    assert!(e.resident_state().expect("resident fixture").undo.is_empty());
                    assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                    drop(e);
                    if matches!(action, "copy" | "cut") {
                        assert!(
                            matches!(
                                app.clipboard
                                    .replies
                                    .recv_timeout(Duration::from_millis(50)),
                                Err(mpsc::RecvTimeoutError::Timeout)
                            ),
                            "No clipboard operation should have been sent"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn rejected_selection_worker_reply_blocks_shortcuts_in_the_completion_frame() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let mut before = Document::new_depth(32, 32, depth).unwrap();
            before.layers[0].pixels.set16(6, 7, [12347, 23459, 34571, 65535]);
            {
                let mut engine = app.shared.lock().unwrap();
                engine.resident_state_mut().unwrap().document = before.clone();
                engine.reserve("assistant", "Selection reservation", vec![crate::engine::Scope {
                    target: None, rect: None,
                }]).unwrap();
            }
            app.select_content(&before.layers[0].id);
            app.select_tool(Tool::MagicWand);
            app.clipboard = crate::clipboard::Worker::memory(ctx.clone());
            frame(&mut app, &ctx, vec![], Default::default());
            let shared = app.shared.clone();
            let (ready, wait) = mpsc::channel();
            app.edit_worker.submit(
                edit_job::Request {
                    project: app.project_id.clone(), document: before.id.clone(),
                    revision: before.revision, label: "Select rectangle".into(),
                    selection_edit: true, origin: edit_job::Origin::Action,
                },
                edit_job::Commands::resident(vec![json!({"op":"selection","rect":[4,5,12,13]})]),
                move |request, commands| server::edit_shared(
                    &shared, "human", commands, Some(request.revision), None, &request.label, None, Some(&request.document),
                ),
                move || { let _ = ready.send(()); },
            ).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();

            frame(&mut app, &ctx, vec![egui::Event::Copy, shortcut_press(egui::Key::W)], Default::default());
            assert!(app.selection_commit_failed && !app.edit_worker.pending());
            assert!(app.message.contains("Reserved"), "{}", app.message);
            assert!(app.tool == Tool::MagicWand);
            assert!(matches!(app.clipboard.replies.try_recv(), Err(mpsc::TryRecvError::Empty)));
            {
                let engine = app.shared.lock().unwrap();
                let state = engine.resident_state().unwrap();
                assert!(state.document.selection.is_none());
                assert_eq!(state.document.layers[0].pixels, before.layers[0].pixels);
                assert!(state.undo.is_empty());
            }

            frame(&mut app, &ctx, vec![egui::Event::Copy], Default::default());
            // The worker is serialized. Any copy incorrectly queued in the
            // rejected frame would produce a second terminal reply here.
            assert!(app.clipboard.replies.recv_timeout(Duration::from_secs(5)).unwrap().result.is_ok());
            assert!(matches!(app.clipboard.replies.recv_timeout(Duration::from_millis(50)), Err(mpsc::RecvTimeoutError::Timeout)));
        }
    }
    #[test]
    fn wand_release_before_copy_cut_or_delete_uses_current_selected_pixels() {
        for depth in [8, 16] {
            for action in ["copy", "cut", "delete"] {
                let (mut app, ctx, point, before) = pressed_wand_fixture(depth);
                let event = match action {
                    "copy" => egui::Event::Copy,
                    "cut" => egui::Event::Cut,
                    _ => shortcut_press(egui::Key::Delete),
                };
                frame(
                    &mut app,
                    &ctx,
                    vec![primary_release(point), event],
                    Default::default(),
                );
                if action != "delete" {
                    let reply = app
                        .clipboard
                        .replies
                        .recv_timeout(Duration::from_secs(3))
                        .unwrap();
                    assert!(reply.result.is_ok(), "{:?}", reply.result);
                }
                {
                    let engine = app.shared.lock().unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").document.selection, Some([4, 5, 12, 13]));
                    assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 1, "{depth}-bit {action}");
                    for y in 0..32 {
                        for x in 0..32 {
                            let expected =
                                if action != "copy" && (4..12).contains(&x) && (5..13).contains(&y)
                                {
                                    [0; 4]
                                } else {
                                    before.layers[0].pixels.get16(x, y)
                                };
                            assert_eq!(
                                engine.resident_state().expect("resident fixture").document.layers[0].pixels.get16(x, y),
                                expected,
                                "{depth}-bit {action} at {x},{y}"
                            );
                        }
                    }
                }
                if action != "delete" {
                    let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                    app.clipboard
                        .request(crate::clipboard::Request::Paste {
                            shared: app.shared.clone(),
                            document: doc.id,
                            target: app.selected.clone(),
                            revision: doc.revision,
                        })
                        .unwrap();
                    let reply = app
                        .clipboard
                        .replies
                        .recv_timeout(Duration::from_secs(3))
                        .unwrap();
                    assert!(reply.result.is_ok(), "{:?}", reply.result);
                    let mut engine = app.shared.lock().unwrap();
                    let pasted = engine
                        .resident_state().expect("resident fixture").document
                        .layers
                        .iter()
                        .find(|l| Some(&l.id) == reply.selected.as_ref())
                        .unwrap();
                    assert_eq!([pasted.x, pasted.y], [4, 5]);
                    assert_eq!([pasted.pixels.width, pasted.pixels.height], [8, 8]);
                    assert_eq!(pasted.pixels.depth, depth);
                    assert_eq!(
                        pasted.pixels.get16(0, 0),
                        before.layers[0].pixels.get16(4, 5)
                    );
                    engine.undo("human").unwrap();
                }
                let mut engine = app.shared.lock().unwrap();
                if action != "copy" {
                    engine.undo("human").unwrap();
                }
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                assert_eq!(engine.resident_state().expect("resident fixture").document.selection, Some([4, 5, 12, 13]));
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.selection, None);
            }
        }
    }
    #[test]
    fn selected_clipboard_shortcuts_ignore_alt_chords_and_key_repeats() {
        for depth in [8, 16] {
            let (mut app, ctx, point, _) = pressed_wand_fixture(depth);
            frame(&mut app, &ctx, vec![primary_release(point)], Default::default());
            let selected = app.shared.lock().unwrap().resident_state().unwrap().document.clone();
            assert_eq!(selected.selection, Some([4, 5, 12, 13]));
            app.doc_snapshot = DocumentSnapshot::Resident(Arc::new(selected.clone()));
            let before = app.message.clone();
            let key = |key, alt, repeat| egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat,
                modifiers: egui::Modifiers { alt, ctrl: true, command: true, ..Default::default() },
            };
            let dispatch = |app: &mut PeerBrush, events: Vec<egui::Event>| {
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    ctx.input_mut(|input| input.events.clone_from(&events));
                    app.document_shortcuts(ctx, &selected);
                });
            };
            for (alt, repeat) in [(true, false), (false, true)] {
                let events: Vec<_> = [egui::Key::C, egui::Key::X, egui::Key::V].into_iter()
                    .map(|letter| key(letter, alt, repeat)).collect();
                assert!(events.iter().all(|event| app.shortcuts.selection_dependent_event(event) == !alt));
                dispatch(&mut app, events);
                assert_eq!(app.message, before);
            }
            assert_eq!(app.shared.lock().unwrap().history_lengths(), [1, 0]);
            dispatch(&mut app, vec![key(egui::Key::X, true, false), key(egui::Key::X, false, false)]);
            let cut = app.clipboard.replies.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(cut.result.is_ok(), "{:?}", cut.result);
            let engine = app.shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [2, 0]);
            let doc = &engine.resident_state().unwrap().document;
            assert_eq!(doc.layers[0].pixels.get16(4, 5)[3], 0);
            assert_eq!(doc.layers[0].pixels.get16(3, 5), selected.layers[0].pixels.get16(3, 5));
        }
    }

    #[test]
    fn selected_copy_uses_the_keypress_mode_despite_later_shift_changes() {
        for depth in [8, 16] {
            for merged in [false, true] {
                let (mut app, ctx, point, _) = pressed_wand_fixture(depth);
                frame(&mut app, &ctx, vec![primary_release(point)], Default::default());
                let selected = {
                    let mut engine = app.shared.lock().unwrap();
                    engine
                        .edit(
                            "human",
                            &[json!({"op":"filter.add","kind":"invert"})],
                            None,
                            None,
                            "Invert",
                        )
                        .unwrap();
                    engine.resident_state().unwrap().document.clone()
                };
                assert_eq!(selected.selection, Some([4, 5, 12, 13]));
                let expected = crate::clipboard::Image::copy(&selected, &app.selected, false, merged)
                    .unwrap();
                let other = crate::clipboard::Image::copy(&selected, &app.selected, false, !merged)
                    .unwrap();
                assert_ne!(expected.bytes, other.bytes);
                let mut events = vec![];
                if merged {
                    events.push(egui::Event::Key {
                        key: egui::Key::C,
                        physical_key: Some(egui::Key::C),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers {
                            command: true,
                            shift: true,
                            ..Default::default()
                        },
                    });
                }
                events.push(egui::Event::Copy);
                frame(
                    &mut app,
                    &ctx,
                    events,
                    egui::Modifiers { shift: !merged, ..Default::default() },
                );
                let reply = app.clipboard.replies.recv_timeout(Duration::from_secs(3)).unwrap();
                assert!(reply.result.is_ok(), "{:?}", reply.result);
                assert_eq!(app.shared.lock().unwrap().history_lengths(), [2, 0]);
                app.clipboard
                    .request(crate::clipboard::Request::Paste {
                        shared: app.shared.clone(),
                        document: selected.id,
                        target: app.selected.clone(),
                        revision: selected.revision,
                    })
                    .unwrap();
                let reply = app.clipboard.replies.recv_timeout(Duration::from_secs(3)).unwrap();
                assert!(reply.result.is_ok(), "{:?}", reply.result);
                let engine = app.shared.lock().unwrap();
                let doc = &engine.resident_state().unwrap().document;
                let pasted = doc.layers.iter()
                    .find(|layer| Some(&layer.id) == reply.selected.as_ref())
                    .unwrap();
                assert_eq!(Some([pasted.x, pasted.y]), expected.origin);
                assert_eq!(pasted.pixels.rgba(), expected.bytes);
                assert_eq!(pasted.pixels.depth, depth);
                if let Some(words) = expected.samples16 {
                    assert_eq!(pasted.pixels.rgba16(), words);
                }
                assert_eq!(engine.history_lengths(), [3, 0]);
            }
        }
    }
    #[test]
    fn shortcut_before_wand_release_requires_retry_and_preserves_focus_guards() {
        for depth in [8, 16] {
            let (mut app, ctx, point, _) = pressed_wand_fixture(depth);
            frame(
                &mut app,
                &ctx,
                vec![shortcut_press(egui::Key::W), primary_release(point)],
                Default::default(),
            );
            assert!(app.tool == Tool::MagicWand);
            assert_eq!(
                app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection,
                Some([4, 5, 12, 13])
            );
            assert!(app.message.contains("retry the shortcut"));
            assert!(app.canvas_press_document.is_none());
            let (mut app, ctx, point, _) = pressed_wand_fixture(depth);
            app.shortcuts.open = true;
            frame(
                &mut app,
                &ctx,
                vec![primary_release(point), shortcut_press(egui::Key::W)],
                Default::default(),
            );
            assert!(app.tool == Tool::MagicWand);
        }
    }
    #[test]
    fn a_second_canvas_press_in_the_release_frame_holds_copy_and_transform() {
        for depth in [8, 16] {
            let (mut app, ctx, point, before) = pressed_wand_fixture(depth);
            let canvas = app.view_rect.unwrap();
            let next = canvas.min + Vec2::new(20.5, 21.5) * (canvas.width() / 32.);
            frame(&mut app, &ctx, vec![
                primary_release(point),
                egui::Event::PointerMoved(next),
                button(next, egui::PointerButton::Primary, true, Default::default()),
                egui::Event::Copy,
                shortcut_press(egui::Key::W),
                primary_release(next),
            ], Default::default());
            assert!(app.tool == Tool::MagicWand);
            assert!(matches!(
                app.clipboard.replies.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            let engine = app.shared.lock().unwrap();
            assert_eq!(engine.resident().unwrap().layers[0].pixels, before.layers[0].pixels);
            assert!(engine.history_lengths()[0] <= 1);
        }
    }

    #[test]
    fn native_keyboard_selection_change_keeps_shortcuts_in_input_order() {
        use crate::engine::{
            native::{worker::tests::Fixture, NativeDocument},
            scene::{CapturedScene, LegacyTileCache},
        };
        for depth in [8, 16] {
            let fixture = Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let mut document = Document::new_depth(32, 32, depth).unwrap();
            let layer = document.layers[0].id.clone();
            document.layers[0].pixels.set16(4, 5, [12347, 23459, 34571, 65535]);
            let sample = document.layers[0].pixels.get16(4, 5);
            let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
            let staged = CapturedScene::from_parts(document, "keyboard-selection".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(())).unwrap();
            let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
            let shared = Arc::new(Mutex::new(Engine::from_native(native.clone(), 8).unwrap()));
            let workspace = crate::workspace::attach(&shared);
            workspace.lock().unwrap().source_runtime = Some(runtime);
            let (mut app, ctx) = small_fixture();
            app.project_id = shared.lock().unwrap().project_id.clone();
            app.shared = shared.clone();
            app.workspace_root = shared.clone();
            app.workspace = workspace;
            app.doc_snapshot = native.into();
            app.selected = layer.clone();
            app.selection_layers = [layer].into_iter().collect();
            app.layer_clipboard = true;
            app.clipboard = crate::clipboard::Worker::memory(ctx.clone());
            app.select_tool(Tool::MagicWand);
            frame(&mut app, &ctx, vec![], Default::default());
            let select_all = egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            };
            frame(&mut app, &ctx, vec![
                egui::Event::Copy,
                select_all,
                egui::Event::Cut,
                shortcut_press(egui::Key::W),
            ], Default::default());
            assert!(app.edit_worker.pending());
            assert!(app.tool == Tool::MagicWand);
            assert!(app.message.contains("Press the shortcut again"));
            let copy = app.clipboard.replies.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(copy.result.as_deref(), Ok("Layers copied"));
            assert!(matches!(app.clipboard.replies.try_recv(), Err(mpsc::TryRecvError::Empty)));

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while app.edit_worker.pending() {
                assert!(std::time::Instant::now() < deadline);
                frame(&mut app, &ctx, vec![], Default::default());
                std::thread::yield_now();
            }
            frame(&mut app, &ctx, vec![shortcut_press(egui::Key::W)], Default::default());
            assert!(app.tool == Tool::MagicWand);
            {
                let engine = shared.lock().unwrap();
                let native = engine.native_document().unwrap();
                assert_eq!(native.metadata().selection(), Some([0, 0, 32, 32]));
                assert_eq!(engine.history_lengths(), [1, 0]);
                let raster = native.metadata().layer(0).unwrap().pixels();
                let pixels = native.source(raster.slot()).unwrap().prepare_region(
                    crate::raster::backed::Region::new([4, 5, 5, 6]).unwrap(), &mut || Ok(()),
                ).unwrap();
                assert_eq!(pixels.pixel16(4, 5).unwrap(), sample);
            }
            frame(&mut app, &ctx, vec![egui::Event::Copy], Default::default());
            let copy = app.clipboard.replies.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(copy.result.as_deref(), Ok("Selection copied"));

            let before = shared.lock().unwrap().doc_snapshot();
            app.edit(vec![json!({"op":"selection","kind":"rectangle","rect":[4,5,5,6]})], "Select one pixel");
            app.queue_selection_shortcuts(
                before.view(),
                vec![egui::Event::Cut, shortcut_press(egui::Key::W)],
                Default::default(),
            );
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while app.edit_worker.pending() {
                assert!(std::time::Instant::now() < deadline);
                app.poll_edit_job();
                std::thread::yield_now();
            }
            assert!(app.deferred_selection_shortcuts.as_ref().unwrap().committed.is_some());
            let deselect = egui::Event::Key {
                key: egui::Key::D,
                physical_key: Some(egui::Key::D),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers { shift: true, ..egui::Modifiers::COMMAND },
            };
            frame(&mut app, &ctx, vec![deselect], Default::default());
            assert!(app.deferred_selection_shortcuts.is_none());
            assert!(app.tool == Tool::MagicWand);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while app.edit_worker.pending() {
                assert!(std::time::Instant::now() < deadline);
                frame(&mut app, &ctx, vec![], Default::default());
                std::thread::yield_now();
            }
            let engine = shared.lock().unwrap();
            let native = engine.native_document().unwrap();
            assert!(native.metadata().selection().is_none());
            assert_eq!(engine.history_lengths(), [3, 0]);
            let raster = native.metadata().layer(0).unwrap().pixels();
            let pixels = native.source(raster.slot()).unwrap().prepare_region(
                crate::raster::backed::Region::new([4, 5, 5, 6]).unwrap(), &mut || Ok(()),
            ).unwrap();
            assert_eq!(pixels.pixel16(4, 5).unwrap(), sample);
        }
    }

    #[test]
    fn a_canvas_release_cannot_attach_shortcuts_to_an_earlier_selection_worker() {
        let (mut app, ctx, point, before) = pressed_wand_fixture(16);
        app.tool = Tool::Move;
        let (release_worker, blocked) = mpsc::channel();
        app.edit_worker.submit(
            edit_job::Request {
                project: app.project_id.clone(), document: before.id.clone(), revision: before.revision,
                label: "Earlier selection".into(), selection_edit: true, origin: edit_job::Origin::Action,
            },
            edit_job::Commands::resident(vec![]),
            move |_, _| {
                let _ = blocked.recv_timeout(Duration::from_secs(5));
                Err("Stopped fixture".into())
            },
            || {},
        ).unwrap();
        let raw = egui::RawInput {
            events: vec![primary_release(point), egui::Event::Copy, shortcut_press(egui::Key::W)],
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| {
            assert!(app.hold_canvas_selection_shortcuts(ctx, &before).is_some());
            assert!(app.defer_canvas_shortcuts(ctx, &before, false).is_none());
            assert!(ctx.input(|input| !input.events.iter().any(|event| app.shortcuts.selection_dependent_event(event))));
        });
        release_worker.send(()).unwrap();
    }

    #[test]
    fn held_wand_shortcuts_preserve_sources_until_release_and_explicit_retry() {
        for depth in [8, 16] {
            for action in ["copy", "cut", "paste", "delete", "move", "rotate", "scale"] {
                let (mut app, ctx, point, before) = pressed_wand_fixture(depth);
                let event = match action {
                    "copy" => egui::Event::Copy,
                    "cut" => egui::Event::Cut,
                    "paste" => egui::Event::Paste(String::new()),
                    "delete" => shortcut_press(egui::Key::Delete),
                    "move" => shortcut_press(egui::Key::W),
                    "rotate" => shortcut_press(egui::Key::E),
                    _ => shortcut_press(egui::Key::R),
                };
                frame(&mut app, &ctx, vec![event.clone()], Default::default());
                assert!(app.tool == Tool::MagicWand);
                assert!(app.message.contains("Release the selection gesture"));
                {
                    let engine = app.shared.lock().unwrap();
                    assert_eq!(
                        serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                        serde_json::to_value(&before).unwrap()
                    );
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        before.export_png().unwrap()
                    );
                    assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                }
                if matches!(action, "copy" | "cut" | "paste") {
                    assert!(matches!(
                        app.clipboard
                            .replies
                            .recv_timeout(Duration::from_millis(50)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ));
                }
                let mut release_events = vec![primary_release(point)];
                if let egui::Event::Key {
                    key,
                    physical_key,
                    modifiers,
                    ..
                } = event.clone()
                {
                    release_events.push(egui::Event::Key {
                        key,
                        physical_key,
                        pressed: false,
                        repeat: false,
                        modifiers,
                    });
                }
                frame(&mut app, &ctx, release_events, Default::default());
                assert!(app.tool == Tool::MagicWand);
                assert_eq!(
                    app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection,
                    Some([4, 5, 12, 13])
                );
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
                if matches!(action, "copy" | "cut" | "paste") {
                    assert!(matches!(
                        app.clipboard
                            .replies
                            .recv_timeout(Duration::from_millis(50)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ));
                }
                if action == "paste" {
                    continue;
                }
                frame(&mut app, &ctx, vec![event], Default::default());
                if matches!(action, "copy" | "cut") {
                    assert!(app
                        .clipboard
                        .replies
                        .recv_timeout(Duration::from_secs(3))
                        .unwrap()
                        .result
                        .is_ok());
                }
                match action {
                    "move" => assert!(app.tool == Tool::Move),
                    "rotate" => assert!(app.tool == Tool::Rotate),
                    "scale" => assert!(app.tool == Tool::Scale),
                    _ => {}
                }
                let mut engine = app.shared.lock().unwrap();
                for y in 0..32 {
                    for x in 0..32 {
                        let expected = if matches!(action, "cut" | "delete")
                            && (4..12).contains(&x)
                            && (5..13).contains(&y)
                        {
                            [0; 4]
                        } else {
                            before.layers[0].pixels.get16(x, y)
                        };
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.layers[0].pixels.get16(x, y),
                            expected,
                            "{depth} {action} {x},{y}"
                        );
                    }
                }
                if matches!(action, "cut" | "delete") {
                    engine.undo("human").unwrap();
                }
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.selection, None);
            }
        }
    }
    #[test]
    fn cancelling_a_held_selection_never_retains_a_clipboard_or_transform_shortcut() {
        for depth in [8, 16] {
            for event in [
                egui::Event::Copy,
                egui::Event::Cut,
                shortcut_press(egui::Key::W),
            ] {
                let (mut app, ctx, point, before) = pressed_wand_fixture(depth);
                frame(
                    &mut app,
                    &ctx,
                    vec![event, shortcut_press(egui::Key::Escape)],
                    Default::default(),
                );
                frame(
                    &mut app,
                    &ctx,
                    vec![primary_release(point)],
                    Default::default(),
                );
                assert!(app.tool == Tool::MagicWand);
                let engine = app.shared.lock().unwrap();
                assert_eq!(
                    serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    serde_json::to_value(&before).unwrap()
                );
                assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                drop(engine);
                assert!(matches!(
                    app.clipboard
                        .replies
                        .recv_timeout(Duration::from_millis(50)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ));
            }
        }
    }
    #[test]
    fn unfinished_selection_paths_require_explicit_clipboard_or_transform_retry() {
        for depth in [8, 16] {
            for kind in ["polygon", "magnetic"] {
                for action in ["copy", "cut", "paste", "move", "rotate", "scale"] {
                    let (mut app, ctx, point, before) =
                        pressed_selection_fixture_with_previous(depth, Tool::Selection, kind, true);
                    frame(
                        &mut app,
                        &ctx,
                        vec![primary_release(point)],
                        Default::default(),
                    );
                    app.selection_path = vec![[4., 5.], [12., 5.], [12., 13.], [4., 13.]];
                    let path = app.selection_path.clone();
                    let event = match action {
                        "copy" => egui::Event::Copy,
                        "cut" => egui::Event::Cut,
                        "paste" => egui::Event::Paste(String::new()),
                        "move" => shortcut_press(egui::Key::W),
                        "rotate" => shortcut_press(egui::Key::E),
                        _ => shortcut_press(egui::Key::R),
                    };
                    frame(
                        &mut app,
                        &ctx,
                        vec![egui::Event::PointerMoved(Pos2::new(1., 1.)), event.clone()],
                        Default::default(),
                    );
                    assert!(app.tool == Tool::Selection);
                    assert_eq!(app.selection_path, path);
                    assert!(app.message.contains("Finish or cancel the selection path"));
                    {
                        let engine = app.shared.lock().unwrap();
                        assert_eq!(
                            serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            serde_json::to_value(&before).unwrap()
                        );
                        assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                    }
                    assert!(matches!(
                        app.clipboard
                            .replies
                            .recv_timeout(Duration::from_millis(30)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ));
                    let release_key = |event: &egui::Event| {
                        if let egui::Event::Key {
                            key,
                            physical_key,
                            modifiers,
                            ..
                        } = event
                        {
                            Some(egui::Event::Key {
                                key: *key,
                                physical_key: *physical_key,
                                modifiers: *modifiers,
                                pressed: false,
                                repeat: false,
                            })
                        } else {
                            None
                        }
                    };
                    frame(
                        &mut app,
                        &ctx,
                        release_key(&event).into_iter().collect(),
                        Default::default(),
                    );
                    // Completing the path and requesting work in one frame must
                    // still require a retry; no work is queued behind the commit.
                    frame(
                        &mut app,
                        &ctx,
                        vec![shortcut_press(egui::Key::Enter), event.clone()],
                        Default::default(),
                    );
                    assert!(app.tool == Tool::Selection);
                    assert!(app.selection_path.is_empty());
                    assert!(app.message.contains("Selection path ended"));
                    assert!(matches!(
                        app.clipboard
                            .replies
                            .recv_timeout(Duration::from_millis(30)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ));
                    {
                        let engine = app.shared.lock().unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.selection, Some([4, 5, 12, 13]));
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                            before.layers[0].pixels.rgba16()
                        );
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                    }
                    let mut releases = vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: Some(egui::Key::Enter),
                        modifiers: Default::default(),
                        pressed: false,
                        repeat: false,
                    }];
                    releases.extend(release_key(&event));
                    frame(&mut app, &ctx, releases, Default::default());
                    if action == "paste" {
                        continue;
                    }
                    frame(&mut app, &ctx, vec![event], Default::default());
                    if matches!(action, "copy" | "cut") {
                        let reply = app
                            .clipboard
                            .replies
                            .recv_timeout(Duration::from_secs(3))
                            .unwrap();
                        assert!(reply.result.is_ok(), "{:?}", reply.result);
                    }
                    match action {
                        "move" => assert!(app.tool == Tool::Move),
                        "rotate" => assert!(app.tool == Tool::Rotate),
                        "scale" => assert!(app.tool == Tool::Scale),
                        _ => {}
                    }
                    let mut engine = app.shared.lock().unwrap();
                    for y in 0..32 {
                        for x in 0..32 {
                            assert_eq!(
                                engine.resident_state().expect("resident fixture").document.layers[0].pixels.get16(x, y),
                                if action == "cut" && (4..12).contains(&x) && (5..13).contains(&y) {
                                    [0; 4]
                                } else {
                                    before.layers[0].pixels.get16(x, y)
                                }
                            );
                        }
                    }
                    if action == "cut" {
                        engine.undo("human").unwrap();
                    }
                    engine.undo("human").unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").document.selection, before.selection);
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        before.export_png().unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn unfinished_selections_hold_foreground_fill_and_selected_duplication_until_retry() {
        for depth in [8, 16] {
            for kind in ["wand", "polygon", "magnetic"] {
                for action in ["fill_b", "fill_backspace", "duplicate"] {
                    let (mut app, ctx, point, before) = pressed_selection_fixture(
                        depth,
                        if kind == "wand" {
                            Tool::MagicWand
                        } else {
                            Tool::Selection
                        },
                        kind,
                    );
                    let path = kind != "wand";
                    if path {
                        frame(
                            &mut app,
                            &ctx,
                            vec![primary_release(point)],
                            Default::default(),
                        );
                        app.selection_path = vec![[4., 5.], [12., 5.], [12., 13.], [4., 13.]];
                    }
                    let modifiers = if action == "duplicate" {
                        egui::Modifiers::SHIFT
                    } else {
                        egui::Modifiers {
                            alt: true,
                            ..Default::default()
                        }
                    };
                    let key = match action {
                        "fill_b" => egui::Key::B,
                        "fill_backspace" => egui::Key::Backspace,
                        _ => egui::Key::D,
                    };
                    let event = |pressed| egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed,
                        repeat: false,
                        modifiers,
                    };
                    frame(
                        &mut app,
                        &ctx,
                        if path {
                            vec![egui::Event::PointerMoved(Pos2::new(1., 1.)), event(true)]
                        } else {
                            vec![event(true)]
                        },
                        modifiers,
                    );
                    {
                        let engine = app.shared.lock().unwrap();
                        assert_eq!(
                            serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            serde_json::to_value(&before).unwrap(),
                            "{depth} {kind} {action}"
                        );
                        assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                    }
                    // Release the shortcut before completing the gesture.
                    frame(&mut app, &ctx, vec![event(false)], Default::default());
                    if path {
                        frame(
                            &mut app,
                            &ctx,
                            vec![shortcut_press(egui::Key::Enter)],
                            Default::default(),
                        );
                    } else {
                        frame(
                            &mut app,
                            &ctx,
                            vec![egui::Event::PointerMoved(point), primary_release(point)],
                            Default::default(),
                        );
                    }
                    assert_eq!(
                        app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection,
                        Some([4, 5, 12, 13]),
                        "{depth} {kind} {action}"
                    );
                    assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
                    frame(&mut app, &ctx, vec![event(true)], modifiers);
                    let mut engine = app.shared.lock().unwrap();
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").undo.len(),
                        2,
                        "{depth} {kind} {action}: {}",
                        app.message
                    );
                    if action == "duplicate" {
                        assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 2);
                        let duplicate = engine
                            .resident_state().expect("resident fixture").document
                            .layers
                            .iter()
                            .find(|layer| layer.id != before.layers[0].id)
                            .unwrap();
                        assert_eq!([duplicate.x, duplicate.y], [4, 5]);
                        assert_eq!([duplicate.pixels.width, duplicate.pixels.height], [8, 8]);
                        assert_eq!(duplicate.pixels.depth, depth);
                        for y in 0..8 {
                            for x in 0..8 {
                                assert_eq!(
                                    duplicate.pixels.get16(x, y),
                                    before.layers[0].pixels.get16(x + 4, y + 5)
                                );
                            }
                        }
                    } else {
                        for y in 0..32 {
                            for x in 0..32 {
                                assert_eq!(
                                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.get16(x, y),
                                    if (4..12).contains(&x) && (5..13).contains(&y) {
                                        app.color.map(|channel| u16::from(channel) * 257)
                                    } else {
                                        before.layers[0].pixels.get16(x, y)
                                    }
                                );
                            }
                        }
                    }
                    engine.undo("human").unwrap();
                    engine.undo("human").unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").document.selection, before.selection);
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        before.export_png().unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn selection_draft_commands_reject_changes_after_the_canvas_snapshot() {
        for depth in [8, 16] {
            let (app, _, _, _) = pressed_selection_fixture(depth, Tool::MagicWand, "wand");
            let mut engine = app.shared.lock().unwrap();
            let snapshot = engine.resident_state().expect("resident fixture").document.clone();
            let command = app.select_command(&snapshot, &[[8., 8.]], "replace");
            assert_eq!(command["document_id"], snapshot.id);
            assert_eq!(command["source_revision"], snapshot.revision);
            assert_eq!(command["layer"], app.selected);
            engine
                .edit(
                    "human",
                    &[json!({"op":"selection","kind":"rectangle","rect":[0,0,2,2]})],
                    None,
                    None,
                    "Later coverage",
                )
                .unwrap();
            let before = engine.resident_state().expect("resident fixture").document.clone();
            assert!(engine
                .edit("human", &[command], None, None, "Late wand completion")
                .is_err());
            assert_eq!(
                serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
        }
    }

    #[test]
    fn selection_drafts_preserve_concurrent_coverage_and_require_a_fresh_retry() {
        for depth in [8, 16] {
            for kind in ["wand", "rectangle", "polygon", "magnetic"] {
                let tool = if kind == "wand" {
                    Tool::MagicWand
                } else {
                    Tool::Selection
                };
                let (mut app, ctx, point, _) = pressed_selection_fixture(depth, tool, kind);
                let path_tool = ["polygon", "magnetic"].contains(&kind);
                let end = point + Vec2::splat(app.view_rect.unwrap().width() / 4.);
                if path_tool {
                    frame(
                        &mut app,
                        &ctx,
                        vec![primary_release(point)],
                        Default::default(),
                    );
                    app.selection_path = vec![[4., 5.], [12., 5.], [12., 13.], [4., 13.]];
                } else if kind == "rectangle" {
                    frame(
                        &mut app,
                        &ctx,
                        vec![egui::Event::PointerMoved(end)],
                        Default::default(),
                    );
                    assert!(app.drag_start.is_some());
                }
                assert!(
                    app.selection_gesture_source.is_some(),
                    "{depth} {kind}: initial source not retained"
                );
                let before = {
                    let mut engine = app.shared.lock().unwrap();
                    engine
                        .edit(
                            "human",
                            &[json!({"op":"selection","kind":"rectangle","rect":[0,0,2,2]})],
                            None,
                            None,
                            "Concurrent selection",
                        )
                        .unwrap();
                    engine.resident_state().expect("resident fixture").document.clone()
                };
                let mut events = vec![primary_release(if kind == "rectangle" {
                    end
                } else {
                    point
                })];
                if path_tool {
                    events.push(shortcut_press(egui::Key::Enter));
                }
                events.push(shortcut_press(egui::Key::Delete));
                frame(&mut app, &ctx, events, Default::default());
                assert!(app.selection_gesture_source.is_none());
                assert!(app.selection_path.is_empty());
                assert!(app.points.is_empty());
                assert!(app.drag_start.is_none());
                {
                    let engine = app.shared.lock().unwrap();
                    assert_eq!(
                        serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                        serde_json::to_value(&before).unwrap(),
                        "{depth} {kind}: stale gesture or late Delete changed later work"
                    );
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                }
                // A held rejected edit is not a retry after cancellation.
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::Key {
                        key: egui::Key::Delete,
                        physical_key: None,
                        pressed: true,
                        repeat: true,
                        modifiers: Default::default(),
                    }],
                    Default::default(),
                );
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, before.revision);
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::Key {
                        key: egui::Key::Delete,
                        physical_key: None,
                        pressed: false,
                        repeat: false,
                        modifiers: Default::default(),
                    }],
                    Default::default(),
                );
                frame(
                    &mut app,
                    &ctx,
                    vec![shortcut_press(egui::Key::Delete)],
                    Default::default(),
                );
                let mut engine = app.shared.lock().unwrap();
                assert_ne!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert_eq!(engine.resident_state().expect("resident fixture").document.selection, before.selection);
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 2);
                engine.undo("human").unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert_eq!(engine.resident_state().expect("resident fixture").document.selection, before.selection);
            }
        }
    }

    #[test]
    fn selection_focus_loss_cancels_drafts_without_running_document_shortcuts() {
        for depth in [8, 16] {
            for kind in ["wand", "rectangle", "polygon", "magnetic"] {
                for release in [true, false] {
                    for action in [
                        "none",
                        "copy",
                        "cut",
                        "paste",
                        "delete",
                        "move",
                        "rotate",
                        "scale",
                        "fill",
                        "duplicate",
                    ] {
                        let tool = if kind == "wand" {
                            Tool::MagicWand
                        } else {
                            Tool::Selection
                        };
                        let (mut app, ctx, point, before) =
                            pressed_selection_fixture_with_previous(depth, tool, kind, true);
                        let path_tool = ["polygon", "magnetic"].contains(&kind);
                        if path_tool {
                            frame(
                                &mut app,
                                &ctx,
                                vec![
                                    primary_release(point),
                                    egui::Event::PointerMoved(Pos2::new(1., 1.)),
                                ],
                                Default::default(),
                            );
                            app.selection_path = vec![[4., 5.], [12., 5.], [12., 13.], [4., 13.]];
                            assert_eq!(
                                app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection,
                                before.selection,
                                "{depth} {kind}: initial anchor release changed previous coverage"
                            );
                        } else if kind == "rectangle" {
                            let end = point + Vec2::splat(app.view_rect.unwrap().width() / 4.);
                            frame(
                                &mut app,
                                &ctx,
                                vec![egui::Event::PointerMoved(end)],
                                Default::default(),
                            );
                            assert!(app.drag_start.is_some());
                        }
                        let modifiers = match action {
                            "fill" => egui::Modifiers {
                                alt: true,
                                ..Default::default()
                            },
                            "duplicate" => egui::Modifiers::SHIFT,
                            _ => Default::default(),
                        };
                        let event = match action {
                            "copy" => Some(egui::Event::Copy),
                            "cut" => Some(egui::Event::Cut),
                            "paste" => Some(egui::Event::Paste(String::new())),
                            "delete" => Some(shortcut_press(egui::Key::Delete)),
                            "move" => Some(shortcut_press(egui::Key::W)),
                            "rotate" => Some(shortcut_press(egui::Key::E)),
                            "scale" => Some(shortcut_press(egui::Key::R)),
                            "fill" | "duplicate" => Some(egui::Event::Key {
                                key: if action == "fill" {
                                    egui::Key::B
                                } else {
                                    egui::Key::D
                                },
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers,
                            }),
                            _ => None,
                        };
                        let mut events = vec![egui::Event::WindowFocused(false)];
                        if release {
                            events.push(primary_release(point));
                        }
                        if path_tool {
                            events.push(shortcut_press(egui::Key::Enter));
                        }
                        events.extend(event.clone());
                        // Also cover loss and return in one frame: the final
                        // focus flag alone must not revive an unfinished draft.
                        let refocused_in_frame = depth == 16;
                        if refocused_in_frame {
                            events.push(egui::Event::WindowFocused(true));
                        }
                        let _ = ctx.run(
                            egui::RawInput {
                                screen_rect: Some(Rect::from_min_size(
                                    Pos2::ZERO,
                                    Vec2::new(1360., 900.),
                                )),
                                focused: refocused_in_frame,
                                events,
                                modifiers,
                                ..Default::default()
                            },
                            |ctx| app.draw(ctx),
                        );
                        assert!(
                            app.tool == tool,
                            "{depth} {kind} {release} {action}: tool changed after focus loss"
                        );
                        assert!(
                            app.selection_path.is_empty(),
                            "unfinished path survived focus loss"
                        );
                        assert!(app.points.is_empty());
                        assert!(app.drag_start.is_none());
                        assert!(app.canvas_press_document.is_none());
                        assert!(app.selection_gesture_mode.is_none());
                        {
                            let engine = app.shared.lock().unwrap();
                            assert_eq!(
                                engine.resident_state().expect("resident fixture").document.selection, before.selection,
                                "{depth} {kind} {release} {action}"
                            );
                            assert_eq!(engine.resident_state().expect("resident fixture").document.revision, before.revision);
                            assert!(
                                serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap()
                                    == serde_json::to_value(&before).unwrap(),
                                "document changed after selection focus loss"
                            );
                            assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                        }
                        assert!(
                            matches!(
                                app.clipboard
                                    .replies
                                    .recv_timeout(Duration::from_millis(20)),
                                Err(mpsc::RecvTimeoutError::Timeout)
                            ),
                            "{depth} {kind} {release} {action}: clipboard command ran after focus loss"
                        );
                        // Refocus/release cannot complete the abandoned gesture
                        // or replay a command rejected while focus was absent.
                        let mut events = vec![
                            egui::Event::WindowFocused(true),
                            primary_release(point),
                            egui::Event::PointerMoved(Pos2::new(1., 1.)),
                        ];
                        for key in [egui::Key::Enter]
                            .into_iter()
                            .chain(event.as_ref().and_then(|event| {
                                if let egui::Event::Key { key, .. } = event {
                                    Some(*key)
                                } else {
                                    None
                                }
                            }))
                        {
                            events.push(egui::Event::Key {
                                key,
                                physical_key: Some(key),
                                pressed: false,
                                repeat: false,
                                modifiers: Default::default(),
                            });
                        }
                        frame(&mut app, &ctx, events, Default::default());
                        let engine = app.shared.lock().unwrap();
                        assert!(
                            serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap()
                                == serde_json::to_value(&before).unwrap(),
                            "refocus changed the document"
                        );
                        assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn modified_lasso_anchor_backspace_cannot_turn_into_foreground_fill_on_repeat() {
        for depth in [8, 16] {
            for kind in ["polygon", "magnetic"] {
                let (mut app, ctx, point, original) =
                    pressed_selection_fixture(depth, Tool::Selection, kind);
                frame(
                    &mut app,
                    &ctx,
                    vec![
                        primary_release(point),
                        egui::Event::PointerMoved(Pos2::new(1., 1.)),
                    ],
                    Default::default(),
                );
                app.selection_path = vec![[4., 5.], [12., 5.], [12., 13.], [4., 13.], [3., 9.]];
                let key = |pressed, repeat, modifiers| egui::Event::Key {
                    key: egui::Key::Backspace,
                    physical_key: Some(egui::Key::Backspace),
                    pressed,
                    repeat,
                    modifiers,
                };
                let control = egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                };
                frame(&mut app, &ctx, vec![key(true, false, control)], control);
                assert_eq!(
                    app.selection_path.len(),
                    4,
                    "retain existing modified anchor removal"
                );
                frame(
                    &mut app,
                    &ctx,
                    vec![shortcut_press(egui::Key::Enter)],
                    Default::default(),
                );
                assert!(app.selection_path.is_empty());
                let selected = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                assert!(selected.selection.is_some());
                let alt = egui::Modifiers {
                    alt: true,
                    ..Default::default()
                };
                frame(&mut app, &ctx, vec![key(true, true, alt)], alt);
                {
                    let engine = app.shared.lock().unwrap();
                    assert!(
                        engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16() == original.layers[0].pixels.rgba16(),
                        "{depth} {kind}: modified anchor repeat filled artwork"
                    );
                    assert_eq!(engine.resident_state().expect("resident fixture").document.selection, selected.selection);
                    assert_eq!(engine.resident_state().expect("resident fixture").document.revision, selected.revision);
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                }
                frame(
                    &mut app,
                    &ctx,
                    vec![key(false, false, Default::default())],
                    Default::default(),
                );
                frame(&mut app, &ctx, vec![key(true, false, alt)], alt);
                let mut engine = app.shared.lock().unwrap();
                assert!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16() != original.layers[0].pixels.rgba16(),
                    "fresh foreground fill was rejected"
                );
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.get16(0, 0),
                    original.layers[0].pixels.get16(0, 0)
                );
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 2);
                engine.undo("human").unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    original.layers[0].pixels.rgba16()
                );
                assert_eq!(engine.resident_state().expect("resident fixture").document.selection, selected.selection);
            }
        }
    }
    #[test]
    fn held_lasso_anchor_keys_cannot_become_document_edits_after_completion() {
        for depth in [8, 16] {
            for kind in ["polygon", "magnetic"] {
                for anchor_key in [egui::Key::Delete, egui::Key::Backspace] {
                    let (mut app, ctx, point, original) =
                        pressed_selection_fixture(depth, Tool::Selection, kind);
                    frame(
                        &mut app,
                        &ctx,
                        vec![
                            primary_release(point),
                            egui::Event::PointerMoved(Pos2::new(1., 1.)),
                        ],
                        Default::default(),
                    );
                    app.selection_path = vec![[4., 5.], [12., 5.], [12., 13.], [4., 13.], [3., 9.]];
                    let key = |pressed, repeat, modifiers| egui::Event::Key {
                        key: anchor_key,
                        physical_key: Some(anchor_key),
                        pressed,
                        repeat,
                        modifiers,
                    };
                    frame(
                        &mut app,
                        &ctx,
                        vec![key(true, false, Default::default())],
                        Default::default(),
                    );
                    assert_eq!(app.selection_path.len(), 4);
                    assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                    frame(
                        &mut app,
                        &ctx,
                        vec![shortcut_press(egui::Key::Enter)],
                        Default::default(),
                    );
                    assert!(app.selection_path.is_empty());
                    let completed = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                    assert!(completed.selection.is_some());
                    assert_eq!(
                        completed.layers[0].pixels.rgba16(),
                        original.layers[0].pixels.rgba16()
                    );
                    assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
                    let modifiers = if anchor_key == egui::Key::Backspace {
                        egui::Modifiers {
                            alt: true,
                            ..Default::default()
                        }
                    } else {
                        Default::default()
                    };
                    // A repeat retains the same physical press, including if
                    // modifiers change from anchor removal to foreground fill.
                    frame(&mut app, &ctx, vec![key(true, true, modifiers)], modifiers);
                    {
                        let engine = app.shared.lock().unwrap();
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                            original.layers[0].pixels.rgba16(),
                            "{depth} {kind} {anchor_key:?}: anchor repeat edited artwork"
                        );
                        assert_eq!(engine.resident_state().expect("resident fixture").document.selection, completed.selection);
                        assert_eq!(engine.resident_state().expect("resident fixture").document.revision, completed.revision);
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                    }
                    frame(
                        &mut app,
                        &ctx,
                        vec![key(false, false, Default::default())],
                        Default::default(),
                    );
                    frame(&mut app, &ctx, vec![key(true, false, modifiers)], modifiers);
                    {
                        let mut engine = app.shared.lock().unwrap();
                        assert_ne!(
                            engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                            original.layers[0].pixels.rgba16(),
                            "a fresh document-edit press was lost"
                        );
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.layers[0].pixels.get16(0, 0),
                            original.layers[0].pixels.get16(0, 0)
                        );
                        assert_eq!(engine.resident_state().expect("resident fixture").document.selection, completed.selection);
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 2);
                        engine.undo("human").unwrap();
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                            original.layers[0].pixels.rgba16()
                        );
                        assert_eq!(engine.resident_state().expect("resident fixture").document.selection, completed.selection);
                    }
                }
            }
        }
    }
    #[test]
    fn held_lasso_anchor_keys_preserve_layers_after_removing_the_last_anchor() {
        for depth in [8, 16] {
            for kind in ["polygon", "magnetic"] {
                let (mut app, ctx, point, original) =
                    pressed_selection_fixture(depth, Tool::Selection, kind);
                frame(
                    &mut app,
                    &ctx,
                    vec![
                        primary_release(point),
                        egui::Event::PointerMoved(Pos2::new(1., 1.)),
                    ],
                    Default::default(),
                );
                app.selection_path = vec![[4., 5.], [12., 5.], [12., 13.]];
                let event = |repeat| egui::Event::Key {
                    key: egui::Key::Delete,
                    physical_key: Some(egui::Key::Delete),
                    pressed: true,
                    repeat,
                    modifiers: Default::default(),
                };
                for remaining in (0..3).rev() {
                    frame(
                        &mut app,
                        &ctx,
                        vec![event(remaining != 2)],
                        Default::default(),
                    );
                    assert_eq!(app.selection_path.len(), remaining);
                }
                frame(&mut app, &ctx, vec![event(true)], Default::default());
                let engine = app.shared.lock().unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers.len(),
                    original.layers.len(),
                    "{depth} {kind}: anchor repeat deleted a layer"
                );
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    original.layers[0].pixels.rgba16()
                );
                assert_eq!(engine.resident_state().expect("resident fixture").document.selection, original.selection);
                assert_eq!(engine.resident_state().expect("resident fixture").document.revision, original.revision);
                assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
            }
        }
    }
    #[test]
    fn held_selection_shortcut_repeats_cannot_run_after_completion_without_key_release() {
        for depth in [8, 16] {
            for kind in ["wand", "polygon"] {
                for action in ["move", "fill", "duplicate"] {
                    let (mut app, ctx, point, _) = pressed_selection_fixture(
                        depth,
                        if kind == "wand" {
                            Tool::MagicWand
                        } else {
                            Tool::Selection
                        },
                        kind,
                    );
                    if kind == "polygon" {
                        frame(
                            &mut app,
                            &ctx,
                            vec![primary_release(point)],
                            Default::default(),
                        );
                        app.selection_path = vec![[4., 5.], [12., 5.], [12., 13.], [4., 13.]];
                    }
                    let modifiers = match action {
                        "fill" => egui::Modifiers {
                            alt: true,
                            ..Default::default()
                        },
                        "duplicate" => egui::Modifiers::SHIFT,
                        _ => Default::default(),
                    };
                    let key = match action {
                        "fill" => egui::Key::B,
                        "duplicate" => egui::Key::D,
                        // Exercise different logical/physical identities.
                        _ => egui::Key::W,
                    };
                    let event = |pressed, repeat| egui::Event::Key {
                        key,
                        physical_key: Some(if action == "move" { egui::Key::Z } else { key }),
                        pressed,
                        repeat,
                        modifiers,
                    };
                    let mut events = vec![event(true, false)];
                    if kind == "polygon" {
                        events.insert(0, egui::Event::PointerMoved(Pos2::new(1., 1.)));
                    }
                    frame(&mut app, &ctx, events, modifiers);
                    assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                    frame(
                        &mut app,
                        &ctx,
                        vec![if kind == "polygon" {
                            shortcut_press(egui::Key::Enter)
                        } else {
                            primary_release(point)
                        }],
                        Default::default(),
                    );
                    let selected = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                    assert_eq!(selected.selection, Some([4, 5, 12, 13]));
                    assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
                    frame(&mut app, &ctx, vec![event(true, true)], modifiers);
                    assert!(
                        matches!(app.tool, Tool::Selection | Tool::MagicWand),
                        "{depth} {kind} {action}"
                    );
                    {
                        let engine = app.shared.lock().unwrap();
                        assert_eq!(
                            serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            serde_json::to_value(&selected).unwrap(),
                            "{depth} {kind} {action}"
                        );
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                    }
                    frame(
                        &mut app,
                        &ctx,
                        vec![event(false, false)],
                        Default::default(),
                    );
                    frame(&mut app, &ctx, vec![event(true, false)], modifiers);
                    if action == "move" {
                        assert!(app.tool == Tool::Move);
                        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
                    } else {
                        let mut engine = app.shared.lock().unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 2);
                        if action == "duplicate" {
                            assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 2);
                        }
                        engine.undo("human").unwrap();
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                            selected.layers[0].pixels.rgba16()
                        );
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                            selected.export_png().unwrap()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn selection_path_shortcut_guard_preserves_anchor_delete_and_escape() {
        for depth in [8, 16] {
            for kind in ["polygon", "magnetic"] {
                let (mut app, ctx, point, before) =
                    pressed_selection_fixture(depth, Tool::Selection, kind);
                frame(
                    &mut app,
                    &ctx,
                    vec![primary_release(point), egui::Event::Cut],
                    Default::default(),
                );
                assert_eq!(app.selection_path.len(), 1);
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                app.selection_path.push([12., 5.]);
                frame(
                    &mut app,
                    &ctx,
                    vec![shortcut_press(egui::Key::Delete)],
                    Default::default(),
                );
                assert_eq!(app.selection_path.len(), 1);
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::Copy, shortcut_press(egui::Key::Escape)],
                    Default::default(),
                );
                assert!(app.selection_path.is_empty());
                let engine = app.shared.lock().unwrap();
                assert_eq!(
                    serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    serde_json::to_value(&before).unwrap()
                );
                assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                drop(engine);
                assert!(matches!(
                    app.clipboard
                        .replies
                        .recv_timeout(Duration::from_millis(30)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ));
            }
        }
    }

    #[test]
    fn marquee_uses_the_exact_button_up_sample_before_later_motion() {
        for depth in [8, 16] {
            let (mut app, ctx, _, before) =
                pressed_selection_fixture_with_previous(depth, Tool::Selection, "rectangle", true);
            let canvas = app.view_rect.unwrap();
            let screen = |x, y| canvas.min + Vec2::new(x, y) * (canvas.width() / 32.);
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(screen(12.95, 14.95))], Default::default());
            assert!(app.drag_start.is_some());
            frame(&mut app, &ctx, vec![primary_release(screen(13.2, 15.2)),
                egui::Event::PointerMoved(screen(23., 25.))], Default::default());
            let mut engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            assert_eq!(state.document.selection, Some([6, 7, 14, 16]));
            assert_eq!(state.document.layers[0].pixels.rgba16(), before.layers[0].pixels.rgba16());
            assert_eq!(state.undo.len(), 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().unwrap().document.selection, before.selection);
        }
    }

    #[test]
    fn marquee_can_start_in_workspace_and_commits_only_canvas_coverage() {
        for depth in [8, 16] {
            let (mut app, ctx, before) = deselection_fixture(depth);
            app.select_tool(Tool::Selection);
            app.selection_kind = "rectangle".into();
            app.selection_mode = "replace".into();
            app.view = view::View { angle: 35., mirror_h: true, ..Default::default() };
            frame(&mut app, &ctx, vec![], Default::default());
            let rect = app.view_rect.unwrap();
            let projection = app.view.projection(rect, rect.width() / 32.);
            let start = projection.point([-4., -3.]);
            let inside = projection.point([8.2, 10.2]);
            let end = projection.point([12.2, 15.2]);
            assert!(!projection.contains(start));
            assert!(app.canvas_input_region.unwrap().0.contains(start));
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(start), egui::Event::PointerButton {
                pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default(),
            }], Default::default());
            assert!(app.selection_gesture_source.is_some());
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(inside)], Default::default());
            assert_eq!(app.drag_start, Some([-4., -3.]));
            assert!(app.selection_drag_preview_key().is_some());
            let preview = Engine::preview_edits(before.clone(), &app.transient).unwrap();
            assert_eq!(preview.selection, Some([0, 0, 9, 11]));
            assert_eq!(preview.layers[0].pixels, before.layers[0].pixels);
            {
                let engine = app.shared.lock().unwrap();
                let state = engine.resident_state().unwrap();
                assert_eq!(state.document.selection, before.selection);
                assert!(state.undo.is_empty());
            }
            let workspace = app.canvas_input_region.unwrap().0;
            let across_panel = egui::pos2(workspace.left() - 5., inside.y);
            assert!(!workspace.contains(across_panel));
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(across_panel)], Default::default());
            assert_eq!(app.points.last().copied(), Some(projection.document(across_panel)));
            frame(&mut app, &ctx, vec![primary_release(end), egui::Event::PointerMoved(projection.point([25., 20.]))], Default::default());
            let mut engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            assert_eq!(state.document.selection, Some([0, 0, 13, 16]));
            assert_eq!(state.document.layers[0].pixels, before.layers[0].pixels);
            assert_eq!(state.undo.len(), 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().unwrap().document.selection_coverage, before.selection_coverage);
        }
    }

    #[test]
    fn single_row_and_column_accept_workspace_clicks_aligned_with_the_canvas() {
        for (kind, at, expected) in [
            ("row", [-4., 7.5], [0, 7, 32, 8]),
            ("column", [9.5, -4.], [9, 0, 10, 24]),
        ] {
            let (mut app, ctx, before) = deselection_fixture(16);
            app.select_tool(Tool::Selection);
            app.selection_kind = kind.into();
            app.selection_mode = "replace".into();
            app.selection_feather = 0.;
            app.view = view::View { angle: 35., mirror_h: true, ..Default::default() };
            frame(&mut app, &ctx, vec![], Default::default());
            let rect = app.view_rect.unwrap();
            let projection = app.view.projection(rect, rect.width() / 32.);
            let point = projection.point(at);
            assert!(!projection.contains(point));
            assert!(app.canvas_input_region.unwrap().0.contains(point));
            click(&mut app, &ctx, point);
            let mut engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            assert_eq!(state.document.selection, Some(expected));
            assert_eq!(state.document.layers[0].pixels, before.layers[0].pixels);
            assert_eq!(state.undo.len(), 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().unwrap().document.selection_coverage, before.selection_coverage);
        }
    }

    #[test]
    fn fill_from_workspace_uses_the_same_selection_and_one_undo_step() {
        for depth in [8, 16] {
            let (mut app, ctx, before) = deselection_fixture(depth);
            app.select_tool(Tool::Fill);
            app.color = [201, 7, 18, 255];
            frame(&mut app, &ctx, vec![], Default::default());
            let canvas = app.view_rect.unwrap();
            let position = canvas.left_center() - Vec2::new(12., 0.);
            assert!(app.canvas_input_region.unwrap().0.contains(position));
            assert!(!canvas.contains(position));
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(position),
                button(position, egui::PointerButton::Primary, true, Default::default())], Default::default());
            frame(&mut app, &ctx, vec![egui::Event::WindowFocused(false),
                egui::Event::WindowFocused(true), primary_release(position)], Default::default());
            assert!(app.fill_press.is_none());
            assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
            click(&mut app, &ctx, position);
            let mut engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            let expected = crate::engine::Engine::preview_edits(before.clone(), &[
                json!({"op":"paint.fill","layer":app.selected,"color":app.color}),
            ]).unwrap();
            assert_eq!(state.document.layers[0].pixels, expected.layers[0].pixels);
            assert_eq!(state.document.selection_coverage, before.selection_coverage);
            assert_eq!(state.undo.len(), 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().unwrap().document.layers[0].pixels, before.layers[0].pixels);
        }
    }

    #[test]
    fn lasso_paths_can_anchor_in_workspace_and_commit_clipped_coverage() {
        for depth in [8, 16] {
            for kind in ["polygon", "magnetic"] {
                let (mut app, ctx, before) = deselection_fixture(depth);
                app.select_tool(Tool::Selection);
                app.selection_kind = kind.into();
                app.selection_mode = "replace".into();
                app.selection_feather = 0.;
                app.view = view::View { angle: 35., mirror_h: true, ..Default::default() };
                frame(&mut app, &ctx, vec![], Default::default());
                let row = app.layer_rects[&app.selected].center();
                click(&mut app, &ctx, row);
                assert!(app.selection_path.is_empty());
                let canvas = app.view_rect.unwrap();
                let projection = app.view.projection(canvas, canvas.width() / 32.);
                let points = [[-4., 3.], [14., 3.], [14., 16.], [-4., 16.]];
                for point in points {
                    let position = projection.point(point);
                    assert!(app.canvas_input_region.unwrap().0.contains(position));
                    click(&mut app, &ctx, position);
                }
                assert_eq!(app.selection_path, points);
                {
                    let engine = app.shared.lock().unwrap();
                    let state = engine.resident_state().unwrap();
                    assert_eq!(state.document.selection_coverage, before.selection_coverage);
                    assert!(state.undo.is_empty());
                }
                key(&mut app, &ctx, egui::Key::Enter);
                assert!(app.selection_path.is_empty());
                let mut engine = app.shared.lock().unwrap();
                let state = engine.resident_state().unwrap();
                assert_eq!(state.document.selection, Some([0, 3, 14, 16]));
                assert_eq!(state.document.layers[0].pixels, before.layers[0].pixels);
                assert_eq!(state.undo.len(), 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().unwrap().document.selection_coverage, before.selection_coverage);
            }
        }
    }

    #[test]
    fn workspace_sampling_uses_release_and_refuses_a_changed_source() {
        for tool in [Tool::Picker, Tool::Clone] {
            for (stale, border) in [(false, false), (true, false), (false, true)] {
                let (mut app, ctx, _) = deselection_fixture(16);
                app.select_tool(tool);
                app.retouch_pick = tool == Tool::Clone;
                app.view = view::View { angle: 35., mirror_h: true, ..Default::default() };
                frame(&mut app, &ctx, vec![], Default::default());
                let rect = app.view_rect.unwrap();
                let projection = app.view.projection(rect, rect.width() / 32.);
                let start = projection.point([-4., -3.]);
                let end = projection.point(if border { [32., 15.25] } else { [12.25, 15.25] });
                assert!(!projection.contains(start));
                assert!(app.canvas_input_region.unwrap().0.contains(start));
                frame(&mut app, &ctx, vec![
                    egui::Event::PointerMoved(start),
                    button(start, egui::PointerButton::Primary, true, Default::default()),
                ], Default::default());
                assert!(app.sample_press.is_some());
                if stale {
                    app.shared.lock().unwrap().resident_state_mut().unwrap().document.revision += 1;
                }
                frame(&mut app, &ctx, vec![
                    egui::Event::PointerMoved(end),
                    primary_release(end),
                    egui::Event::PointerMoved(projection.point([27., 20.])),
                ], Default::default());
                assert!(app.sample_press.is_none());
                let accepted = !stale && !border;
                if tool == Tool::Picker {
                    assert_eq!(app.color_pick_runs, usize::from(accepted));
                } else {
                    assert_eq!(app.retouch_pick, !accepted);
                    if accepted {
                        let actual = app.retouch_source.as_ref().unwrap().point;
                        assert!((actual[0] - 12.25).abs() < 0.01 && (actual[1] - 15.25).abs() < 0.01);
                    } else {
                        assert!(app.retouch_source.is_none());
                    }
                }
                assert!(app.shared.lock().unwrap().resident_state().unwrap().undo.is_empty());
            }
        }
    }

    #[test]
    fn rapid_workspace_marquee_uses_the_exact_press_and_release() {
        for depth in [8, 16] {
            for (split, cancel, add) in [
                (false, false, false), (true, false, false), (true, true, false), (true, false, true),
            ] {
                let (mut app, ctx, before) = deselection_fixture(depth);
                app.select_tool(Tool::Selection);
                app.selection_kind = "rectangle".into();
                app.selection_mode = "replace".into();
                app.view = view::View { angle: 35., mirror_h: true, ..Default::default() };
                frame(&mut app, &ctx, vec![], Default::default());
                let rect = app.view_rect.unwrap();
                let projection = app.view.projection(rect, rect.width() / 32.);
                let start = projection.point([-4., -3.]);
                let end = projection.point([12.2, 15.2]);
                assert!(!projection.contains(start));
                assert!(app.canvas_input_region.unwrap().0.contains(start));
                let mut events = Vec::new();
                if !split {
                    // The new marquee owns the release after its press.
                    events.push(primary_release(start));
                }
                events.extend([egui::Event::PointerMoved(start), egui::Event::PointerButton {
                    pos: start, button: egui::PointerButton::Primary, pressed: true,
                    modifiers: egui::Modifiers { shift: add, ..Default::default() },
                }]);
                if split {
                    frame(&mut app, &ctx, std::mem::take(&mut events), Default::default());
                    assert!(app.selection_gesture_source.is_some());
                    assert!(app.drag_start.is_none());
                    let engine = app.shared.lock().unwrap();
                    assert!(engine.resident_state().unwrap().undo.is_empty());
                }
                if add {
                    frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)], Default::default());
                    assert_eq!(app.selection_gesture_mode.as_deref(), Some("add"));
                }
                if cancel {
                    events.extend([
                        egui::Event::Touch {
                            device_id: egui::TouchDeviceId(1), id: egui::TouchId(1),
                            phase: egui::TouchPhase::Cancel, pos: end, force: None,
                        },
                        egui::Event::PointerGone,
                    ]);
                }
                events.extend([
                    primary_release(end),
                    egui::Event::PointerMoved(projection.point([27., 20.])),
                ]);
                frame(&mut app, &ctx, events, Default::default());
                let engine = app.shared.lock().unwrap();
                let state = engine.resident_state().unwrap();
                if add {
                    assert!(crate::selection::contains_pixel(&state.document, 0, 0));
                    assert!(crate::selection::contains_pixel(&state.document, 19, 13));
                } else {
                    assert_eq!(state.document.selection, if cancel { before.selection } else { Some([0, 0, 13, 16]) });
                }
                assert_eq!(state.document.layers[0].pixels, before.layers[0].pixels);
                assert_eq!(state.undo.len(), usize::from(!cancel));
            }
        }
    }

    #[test]
    fn wand_drag_samples_where_it_releases_after_a_canvas_or_workspace_press() {
        for (depth, start_x) in [8, 16].into_iter().flat_map(|depth| [-4., 4.].map(|x| (depth, x))) {
            let (mut app, ctx, before) = deselection_fixture(depth);
            app.select_tool(Tool::MagicWand);
            app.selection_mode = "replace".into();
            app.selection_contiguous = true;
            app.selection_feather = 0.;
            app.view = view::View { angle: 25., mirror_h: true, ..Default::default() };
            frame(&mut app, &ctx, vec![], Default::default());
            let rect = app.view_rect.unwrap();
            let projection = app.view.projection(rect, rect.width() / before.width as f32);
            let start = projection.point([start_x, 12.5]);
            let end = projection.point([12.5, 12.5]);
            assert_eq!(projection.contains(start), start_x >= 0.);
            assert!(app.canvas_input_region.unwrap().0.contains(start));
            frame(&mut app, &ctx, vec![
                egui::Event::PointerMoved(start),
                button(start, egui::PointerButton::Primary, true, egui::Modifiers::ALT),
            ], egui::Modifiers::ALT);
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)], Default::default());
            assert!(app.drag_start.is_none());
            assert!(app.selection_gesture_source.is_some());
            assert!(app.transient.is_empty());
            assert!(app.shared.lock().unwrap().resident_state().unwrap().undo.is_empty());
            frame(&mut app, &ctx, vec![primary_release(end), egui::Event::PointerMoved(start)], Default::default());
            let engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            assert_eq!(state.document.selection, Some([0, 0, 32, 24]));
            assert_eq!(state.document.layers[0].pixels, before.layers[0].pixels);
            assert_eq!(state.undo.len(), 1);
        }
    }

    #[test]
    fn single_line_selection_uses_the_release_axis_after_a_canvas_or_workspace_press() {
        for (kind, start, expected) in [
            ("row", [12.5, -4.], [0, 12, 32, 13]),
            ("row", [12.5, 4.], [0, 12, 32, 13]),
            ("column", [-4., 12.5], [12, 0, 13, 24]),
            ("column", [4., 12.5], [12, 0, 13, 24]),
        ] {
            let (mut app, ctx, before) = deselection_fixture(16);
            app.select_tool(Tool::Selection);
            app.selection_kind = kind.into();
            app.selection_mode = "replace".into();
            app.selection_feather = 0.;
            app.view = view::View { angle: 25., mirror_h: true, ..Default::default() };
            frame(&mut app, &ctx, vec![], Default::default());
            let rect = app.view_rect.unwrap();
            let projection = app.view.projection(rect, rect.width() / before.width as f32);
            let start = projection.point(start);
            let end = projection.point([12.5, 12.5]);
            assert!(app.canvas_input_region.unwrap().0.contains(start));
            frame(&mut app, &ctx, vec![
                egui::Event::PointerMoved(start),
                button(start, egui::PointerButton::Primary, true, Default::default()),
            ], Default::default());
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)], Default::default());
            assert!(app.drag_start.is_none());
            frame(&mut app, &ctx, vec![primary_release(end)], Default::default());
            let engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            assert_eq!(state.document.selection, Some(expected));
            assert_eq!(state.undo.len(), 1);
        }
    }

    #[test]
    fn rapid_workspace_lasso_preserves_the_motion_before_its_release() {
        let (mut app, ctx, before) = deselection_fixture(16);
        app.select_tool(Tool::Selection);
        app.selection_kind = "lasso".into();
        app.selection_mode = "replace".into();
        frame(&mut app, &ctx, vec![], Default::default());
        let rect = app.view_rect.unwrap();
        let projection = app.view.projection(rect, rect.width() / 32.);
        let start = projection.point([-4., -3.]);
        frame(&mut app, &ctx, vec![
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start, button: egui::PointerButton::Primary, pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerMoved(projection.point([16., 2.])),
            egui::Event::PointerMoved(projection.point([16., 12.])),
            egui::Event::PointerMoved(projection.point([0., 12.])),
            primary_release(start),
            egui::Event::PointerMoved(projection.point([28., 20.])),
        ], Default::default());
        let engine = app.shared.lock().unwrap();
        let state = engine.resident_state().unwrap();
        assert_ne!(state.document.selection, before.selection);
        assert!(crate::selection::contains_pixel(&state.document, 6, 6));
        assert!(!crate::selection::contains_pixel(&state.document, 21, 12));
        assert_eq!(state.document.layers[0].pixels, before.layers[0].pixels);
        assert_eq!(state.undo.len(), 1);
    }

    #[test]
    fn workspace_primitives_preview_and_commit_the_exact_release_bounds() {
        for depth in [8, 16] {
            for tool in [Tool::Rectangle, Tool::Ellipse, Tool::Gradient] {
                for held in [false, true] {
                    let (mut app, ctx, before) = deselection_fixture(depth);
                    app.select_tool(tool);
                    app.view = view::View { angle: 35., mirror_h: true, ..Default::default() };
                    frame(&mut app, &ctx, vec![], Default::default());
                    let rect = app.view_rect.unwrap();
                    let projection = app.view.projection(rect, rect.width() / 32.);
                    let start = projection.point([-4.4, -3.4]);
                    let end = projection.point([12.8, 15.8]);
                    assert!(!projection.contains(start));
                    assert!(app.canvas_input_region.unwrap().0.contains(start));
                    let mut events = vec![egui::Event::PointerMoved(start), egui::Event::PointerButton {
                        pos: start, button: egui::PointerButton::Primary, pressed: true,
                        modifiers: Default::default(),
                    }];
                    if held {
                        frame(&mut app, &ctx, std::mem::take(&mut events), Default::default());
                        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(projection.point([8.6, 10.6]))], Default::default());
                        assert_eq!(app.transient.len(), 1);
                        assert_eq!(app.transient[0]["rect"], json!([-4, -3, 8, 10]));
                        let preview = Engine::preview_edits(before.clone(), &app.transient).unwrap();
                        assert_ne!(preview.layers[0].pixels, before.layers[0].pixels);
                        let engine = app.shared.lock().unwrap();
                        let state = engine.resident_state().unwrap();
                        assert_eq!(state.document.layers[0].pixels, before.layers[0].pixels);
                        assert!(state.undo.is_empty());
                    }
                    events.extend([
                        primary_release(end),
                        egui::Event::PointerMoved(projection.point([27., 20.])),
                    ]);
                    frame(&mut app, &ctx, events, Default::default());
                    let expected = Engine::preview_edits(before.clone(), &[json!({
                        "op": if tool == Tool::Gradient { "gradient" } else { "shape" },
                        "kind": if tool == Tool::Ellipse { "ellipse" } else { "rectangle" },
                        "rect": [-4, -3, 12, 15],
                        "layer": before.layers[0].id, "color": app.color,
                    })]).unwrap();
                    let mut engine = app.shared.lock().unwrap();
                    let state = engine.resident_state().unwrap();
                    assert_eq!(state.document.layers[0].pixels, expected.layers[0].pixels);
                    assert_ne!(state.document.layers[0].pixels, before.layers[0].pixels);
                    assert_eq!(state.document.selection, before.selection);
                    assert_eq!(state.undo.len(), 1);
                    engine.undo("human").unwrap();
                    assert_eq!(engine.resident_state().unwrap().document.layers[0].pixels, before.layers[0].pixels);
                }
            }
        }
    }

    #[test]
    fn workspace_primitive_release_over_empty_space_discards_its_preview() {
        let (mut app, ctx, before) = deselection_fixture(16);
        app.select_tool(Tool::Ellipse);
        app.zoom = 0.55;
        frame(&mut app, &ctx, vec![], Default::default());
        let rect = app.view_rect.unwrap();
        let projection = app.view.projection(rect, rect.width() / 32.);
        let start = projection.point([-8., 2.]);
        let inside = projection.point([10., 12.]);
        let outside = projection.point([-3., 12.]);
        assert!(app.canvas_input_region.unwrap().0.contains(start));
        frame(&mut app, &ctx, vec![
            egui::Event::PointerMoved(start),
            button(start, egui::PointerButton::Primary, true, Default::default()),
        ], Default::default());
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(inside)], Default::default());
        assert_eq!(app.transient.len(), 1);
        let epoch = app.preview_epoch.load(std::sync::atomic::Ordering::Relaxed);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(outside)], Default::default());
        assert!(app.transient.is_empty());
        assert_ne!(app.preview_epoch.load(std::sync::atomic::Ordering::Relaxed), epoch);
        frame(&mut app, &ctx, vec![primary_release(outside)], Default::default());
        assert!(app.primitive_drag.is_none());
        let engine = app.shared.lock().unwrap();
        assert_eq!(engine.history_lengths(), [0, 0]);
        assert_eq!(engine.resident().unwrap().layers[0].pixels, before.layers[0].pixels);
    }

    #[test]
    fn workspace_primitive_cannot_commit_after_its_source_revision_changes() {
        let (mut app, ctx, before) = deselection_fixture(16);
        app.select_tool(Tool::Gradient);
        frame(&mut app, &ctx, vec![], Default::default());
        let rect = app.view_rect.unwrap();
        let projection = app.view.projection(rect, rect.width() / 32.);
        let start = projection.point([-4.4, -3.4]);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(start), egui::Event::PointerButton {
            pos: start, button: egui::PointerButton::Primary, pressed: true,
            modifiers: Default::default(),
        }], Default::default());
        assert!(app.primitive_drag.is_some());
        app.shared.lock().unwrap().edit("human", &[json!({
            "op":"layer.update", "layer":before.layers[0].id, "name":"New source",
        })], None, None, "Rename").unwrap();
        frame(&mut app, &ctx, vec![primary_release(projection.point([12.8, 15.8]))], Default::default());
        assert!(app.primitive_drag.is_none());
        let engine = app.shared.lock().unwrap();
        let state = engine.resident_state().unwrap();
        assert_eq!(state.document.layers[0].name, "New source");
        assert_eq!(state.document.layers[0].pixels, before.layers[0].pixels);
        assert_eq!(state.undo.len(), 1);
    }

    #[test]
    fn rectangle_gestures_keep_shortcuts_safe_and_escape_cancels_through_release() {
        for depth in [8, 16] {
            for cancel in [false, true] {
                let (mut app, ctx, point, before) =
                    pressed_selection_fixture(depth, Tool::Selection, "rectangle");
                let scale = app.view_rect.unwrap().width() / 32.;
                let end = point + Vec2::splat(8. * scale);
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(end), shortcut_press(egui::Key::W)],
                    Default::default(),
                );
                assert!(app.tool == Tool::Selection);
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                if cancel {
                    frame(
                        &mut app,
                        &ctx,
                        vec![shortcut_press(egui::Key::Escape)],
                        Default::default(),
                    );
                }
                let mut release_events = vec![primary_release(end)];
                if cancel {
                    release_events.extend([egui::Event::Copy, shortcut_press(egui::Key::W)]);
                }
                frame(
                    &mut app,
                    &ctx,
                    release_events,
                    Default::default(),
                );
                assert!(app.tool == Tool::Selection);
                assert!(app.clipboard.replies.try_recv().is_err());
                let engine = app.shared.lock().unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                if cancel {
                    assert_eq!(engine.resident_state().expect("resident fixture").document.selection, None);
                    assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                } else {
                    assert_eq!(engine.resident_state().expect("resident fixture").document.selection, Some([6, 7, 15, 16]));
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                }
            }
        }
    }
    #[test]
    fn wand_release_without_a_selection_result_requires_fresh_shortcuts() {
        for depth in [8, 16] {
            let (mut app, ctx, point, before) = pressed_selection_fixture_with_previous(
                depth, Tool::MagicWand, "wand", true,
            );
            let outside = app.canvas_input_region.unwrap().0.min - Vec2::splat(4.);
            frame(&mut app, &ctx, vec![
                egui::Event::PointerMoved(outside),
                primary_release(outside),
                egui::Event::Copy,
                shortcut_press(egui::Key::W),
            ], Default::default());
            assert!(app.tool == Tool::MagicWand);
            assert!(app.message.contains("No current selection result"));
            assert!(app.clipboard.replies.try_recv().is_err());
            {
                let engine = app.shared.lock().unwrap();
                let state = engine.resident_state().unwrap();
                assert_eq!(state.document.selection, before.selection);
                assert!(state.undo.is_empty());
            }

            frame(&mut app, &ctx, vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
                primary_release(point),
                shortcut_press(egui::Key::E),
            ], Default::default());
            assert!(app.tool == Tool::Rotate);
            let engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            assert_eq!(state.document.selection, Some([4, 5, 12, 13]));
            assert_eq!(state.undo.len(), 1);
        }
    }
    #[test]
    fn fast_wand_click_and_transform_in_one_frame_commits_selection_first() {
        for depth in [8, 16] {
            let (mut app, ctx, point, _) = pressed_wand_fixture(depth);
            frame(
                &mut app,
                &ctx,
                vec![primary_release(Pos2::ZERO)],
                Default::default(),
            );
            assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection, None);
            frame(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                    primary_release(point),
                    shortcut_press(egui::Key::W),
                ],
                Default::default(),
            );
            assert!(app.tool == Tool::Move);
            let engine = app.shared.lock().unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.selection, Some([4, 5, 12, 13]));
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
        }
    }
    #[test]
    fn late_merged_copy_keeps_the_wand_release_selection_modifiers() {
        for depth in [8, 16] {
            let (mut app, ctx, point, _) =
                pressed_selection_fixture_with_previous(depth, Tool::MagicWand, "wand", true);
            frame(
                &mut app,
                &ctx,
                vec![primary_release(point), egui::Event::Copy],
                egui::Modifiers::SHIFT,
            );
            let reply = app
                .clipboard
                .replies
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            assert!(reply.result.is_ok());
            let mut e = app.shared.lock().unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.selection, Some([4, 5, 12, 13]));
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
            e.undo("human").unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.selection, Some([0, 0, 2, 2]));
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
            e.resident_state_mut().expect("resident fixture").undo.clear();
            e.resident_state().expect("resident fixture").document
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
        let e = app.shared.lock().unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 4);
        assert_eq!(app.selection_layers.len(), 2);
        assert!(app.selection_layers.iter().all(|id| !original.contains(id)));
        assert!(app.selection_layers.contains(&app.selected));
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
        let copies = app.selection_layers.clone();
        let active_copy = app.selected.clone();
        drop(e);
        app.history_action(false);
        assert_eq!(app.selected, original[0]);
        assert_eq!(app.selection_layers, original.iter().cloned().collect::<HashSet<_>>());
        let e = app.shared.lock().unwrap();
        assert_eq!(
            e.resident_state().expect("resident fixture").document
                .layers
                .iter()
                .map(|l| l.id.clone())
                .collect::<Vec<_>>(),
            original
        );
        drop(e);
        app.history_action(true);
        assert_eq!(app.selected, active_copy);
        assert_eq!(app.selection_layers, copies);
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
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        app.rename_edit.as_mut().unwrap().text = "Renamed artwork".into();
        key(&mut app, &ctx, egui::Key::Enter);
        assert!(app.rename_edit.is_none());
        let e = app.shared.lock().unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].name, "Renamed artwork");
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
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
            e.resident_state_mut().expect("resident fixture").undo.clear();
            result["created"][0].as_str().unwrap().to_string()
        };
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.begin_rename(&doc, &folder);
        frame(&mut app, &ctx, vec![], Default::default());
        app.rename_edit.as_mut().unwrap().text = "Discard me".into();
        key(&mut app, &ctx, egui::Key::Escape);
        assert!(app.rename_edit.is_none());
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
            e.resident_state().expect("resident fixture").document.layers.iter().find(|l| l.id == folder).unwrap().name,
            "Assets"
        );
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
    }
    #[test]
    fn connected_add_mask_batches_only_missing_masks_and_undo_restores_color() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture();
            let (before, existing) = {
                let mut engine = app.shared.lock().unwrap();
                engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
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
                for (index, layer) in engine.resident_state_mut().expect("resident fixture").document.layers.iter_mut().enumerate() {
                    layer
                        .pixels
                        .set16(index as i32 + 1, 2, [12347, 33559, 51237, 45679]);
                }
                let existing = engine.resident_state().expect("resident fixture").document.layers[1].id.clone();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"mask.add","layer":existing,"value":129})],
                        None,
                        None,
                        "Existing mask",
                    )
                    .unwrap();
                engine.resident_state_mut().expect("resident fixture").undo.clear();
                (engine.resident_state().expect("resident fixture").document.clone(), existing)
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
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                assert!(engine.resident_state().expect("resident fixture").document.layers.iter().all(|layer| layer.mask.is_some()));
                assert!(engine
                    .resident_state().expect("resident fixture").document
                    .layers
                    .iter()
                    .flat_map(|layer| &layer.mask.as_ref().unwrap().steps)
                    .all(|step| step.pixels.depth == depth));
                let kept = engine
                    .resident_state().expect("resident fixture").document
                    .layers
                    .iter()
                    .find(|layer| layer.id == existing)
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(&kept.mask).unwrap(),
                    serde_json::to_value(&before.layers[1].mask).unwrap()
                );
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                engine.undo("human").unwrap();
                assert_eq!(
                    engine
                        .resident_state().expect("resident fixture").document
                        .layers
                        .iter()
                        .filter(|layer| layer.mask.is_some())
                        .count(),
                    1
                );
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
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
            let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            {
                let mut engine = app.shared.lock().unwrap();
                match failure {
                    "locked" => engine.resident_state_mut().expect("resident fixture").document.layers[0].locked = true,
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
            assert!(engine.resident_state().expect("resident fixture").document.layers[0].mask.is_none());
            assert_eq!(
                engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), usize::from(failure == "stale"));
            if failure == "stale" {
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].name, "New human name");
            }
            assert!(!app.message.is_empty());
        }
    }
    #[test]
    fn connected_add_mask_uses_selection_coverage_and_preserves_existing_selected_masks() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let (before, existing) = {
                let mut e = app.shared.lock().unwrap();
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
                let kept = crate::engine::Layer::new("Existing mask", "paint", 32, 24);
                let existing = kept.id.clone();
                e.resident_state_mut().expect("resident fixture").document.layers.push(kept);
                e.edit(
                    "human",
                    &[json!({"op":"mask.add","layer":existing,"value":123})],
                    None,
                    None,
                    "Existing",
                )
                .unwrap();
                e.edit(
                    "human",
                    &[json!({"op":"selection","kind":"ellipse","rect":[3,3,26,21],"feather":3})],
                    None,
                    None,
                    "Selection",
                )
                .unwrap();
                e.resident_state_mut().expect("resident fixture").undo.clear();
                (e.resident_state().expect("resident fixture").document.clone(), existing)
            };
            app.select_content(&before.layers[0].id);
            app.selection_layers = before.layers.iter().map(|l| l.id.clone()).collect();
            frame(&mut app, &ctx, vec![], Default::default());
            let add = app.mask_tab_rect.unwrap().center();
            click(&mut app, &ctx, add);
            assert!(app.mask);
            let mut e = app.shared.lock().unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
            let current = crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap();
            let mask = e.resident_state().expect("resident fixture").document.layers[0].mask.as_ref().unwrap();
            assert_eq!(mask.steps.len(), 1);
            for y in 0..24 {
                for x in 0..32 {
                    let expected = (current.value(x, y) * 65535.).round() as u16;
                    assert_eq!(mask.steps[0].pixels.get16(x, y)[0], expected);
                    assert_eq!(mask.steps[0].pixels.get16(x, y)[3], 65535);
                }
            }
            let kept = e.resident_state().expect("resident fixture").document.layers.iter().find(|l| l.id == existing).unwrap();
            assert_eq!(
                serde_json::to_value(&kept.mask).unwrap(),
                serde_json::to_value(&before.layers[1].mask).unwrap()
            );
            e.undo("human").unwrap();
            assert!(e.resident_state().expect("resident fixture").document.layers[0].mask.is_none());
            assert_eq!(
                crate::selection::current(&e.resident_state().expect("resident fixture").document).unwrap().mask.rgba(),
                current.mask.rgba()
            );
        }
    }
    #[test]
    fn solo_view_names_its_channel_dims_other_rows_and_renders_actual_pixels_without_edits() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let before = {
                let mut e = app.shared.lock().unwrap();
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
                e.resident_state_mut().expect("resident fixture").document.layers[0].name = "Solo target".into();
                for y in 3..18 {
                    for x in 4..22 {
                        e.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(x, y, [12347, 33559, 51237, 65535]);
                    }
                }
                let mut other = crate::engine::Layer::new("Other layer", "fill", 32, 24);
                other.color = [233, 84, 32, 255];
                e.resident_state_mut().expect("resident fixture").document.layers.push(other);
                let target = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                e.edit(
                    "human",
                    &[json!({"op":"mask.add","layer":target,"value":128})],
                    None,
                    None,
                    "Fixture mask",
                )
                .unwrap();
                e.resident_state_mut().expect("resident fixture").undo.clear();
                e.resident_state().expect("resident fixture").document.clone()
            };
            app.select_content(&before.layers[0].id);
            let saved = crate::psd::encode(&before).unwrap();
            let draw = |app: &mut PeerBrush| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            let label_pos = |out: &egui::FullOutput, name: &str| {
                out.shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Text(t) if t.galley.job.text == name => {
                            Some(t.pos + t.galley.size() * 0.5)
                        }
                        _ => None,
                    })
                    .unwrap()
            };
            for mask in [false, true] {
                if mask {
                    let p = app.mask_tab_rect.unwrap().center();
                    click(&mut app, &ctx, p);
                }
                let out = draw(&mut app);
                let toggle = label_pos(&out, "Solo");
                click(&mut app, &ctx, toggle);
                assert!(app.isolate);
                assert_eq!(app.mask, mask);
                let out = draw(&mut app);
                let caption = format!("Solo {} · Solo target", if mask { "Mask" } else { "Color" });
                assert_eq!(app.solo_caption(&before), Some(caption.clone()));
                assert!(out.shapes.iter().any(
                    |s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text==caption)
                ));
                let opacity = |name: &str| {
                    out.shapes
                        .iter()
                        .find_map(|s| match &s.shape {
                            egui::Shape::Text(t) if t.galley.job.text == name => Some(
                                t.galley.rows[0].visuals.mesh.vertices[0].color.a() as f32 / 255.,
                            ),
                            _ => None,
                        })
                        .unwrap()
                };
                assert_eq!(opacity("Solo target"), 1.);
                assert!((opacity("Other layer") - 0.55).abs() < 0.005);
                assert!(app.eye_rects.contains_key(&before.layers[1].id));
                let expected = before
                    .preview(None, 8192, Some(&before.layers[0].id), mask)
                    .unwrap()
                    .2;
                // Observe the actual canvas accepted from the worker, including its
                // target checks, rather than draining replies behind the UI's back.
                app.last_preview = None;
                app.canvas_pixels = None;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    let _ = draw(&mut app);
                    if !app.pending
                        && app
                            .canvas_pixels
                            .as_ref()
                            .is_some_and(|(_, _, p)| p == &expected)
                    {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Solo canvas did not match shared-engine pixels"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                let exit = label_pos(&out, "Exit solo");
                click(&mut app, &ctx, exit);
                assert!(!app.isolate);
                assert!(app.solo_caption(&before).is_none());
                let out = draw(&mut app);
                assert!(out.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="Other layer" && t.galley.rows[0].visuals.mesh.vertices[0].color.a()==255)));
                let e = app.shared.lock().unwrap();
                assert!(e.resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap(), saved);
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
            }
        }
    }
    #[test]
    fn solo_exits_on_layer_and_channel_clicks_and_restores_actual_composite() {
        for depth in [8, 16] {
            let (mut app, ctx, before) = solo_fixture(depth);
            let target = &before.layers[0].id;
            let other = &before.layers[1].id;
            let saved = crate::psd::encode(&before).unwrap();
            for modifiers in [
                Default::default(),
                egui::Modifiers {
                    shift: true,
                    ..Default::default()
                },
                egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
            ] {
                app.select_content(target);
                app.set_solo(true);
                frame(&mut app, &ctx, vec![], Default::default());
                let row = app.layer_rects[target];
                click(&mut app, &ctx, egui::pos2(row.right() - 6., row.top() + 2.));
                assert!(app.isolate, "Selecting the same Color target keeps solo");
                let row = app.layer_rects[other];
                solo_click(
                    &mut app,
                    &ctx,
                    egui::pos2(row.right() - 6., row.top() + 2.),
                    modifiers,
                );
                assert_eq!(&app.selected, other);
                assert!(!app.isolate);
                assert!(app.solo_target.is_none());
                wait_solo_pixels(&mut app, &ctx, &before, None, false);
            }
            for mask in [false, true] {
                app.select_content(target);
                app.select_channel(mask);
                app.set_solo(true);
                frame(&mut app, &ctx, vec![], Default::default());
                let same = if mask {
                    app.mask_tab_rect
                } else {
                    app.color_tab_rect
                }
                .unwrap()
                .center();
                click(&mut app, &ctx, same);
                assert!(app.isolate, "Selecting the same channel keeps solo");
                let other = if mask {
                    app.color_tab_rect
                } else {
                    app.mask_tab_rect
                }
                .unwrap()
                .center();
                click(&mut app, &ctx, other);
                assert!(!app.isolate);
                assert_eq!(app.mask, !mask);
                wait_solo_pixels(&mut app, &ctx, &before, None, false);
                assert!(app.solo_caption(&before).is_none());
            }
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            );
            for name in ["Solo target", "Other layer"] {
                assert!(out.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text==name && t.galley.rows[0].visuals.mesh.vertices[0].color.a()==255)));
            }
            let e = app.shared.lock().unwrap();
            assert!(e.resident_state().expect("resident fixture").undo.is_empty());
            assert_eq!(crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap(), saved);
        }
    }
    #[test]
    fn solo_mask_thumbnails_preserve_the_same_target_and_alt_explicitly_toggles_new_targets() {
        let (mut app, ctx, before) = solo_fixture(16);
        let target = &before.layers[0].id;
        let other = &before.layers[1].id;
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        frame(&mut app, &ctx, vec![], Default::default());
        let mask_pos = |app: &mut PeerBrush, id: &str| {
            // The caption adds/removes a line above the stack; use current layout.
            frame(app, &ctx, vec![], Default::default());
            app.layer_rects[id].right_center() - Vec2::new(19., 0.)
        };
        let p = mask_pos(&mut app, target);
        solo_click(&mut app, &ctx, p, alt);
        assert!(app.isolate && app.mask);
        assert_eq!(&app.selected, target);
        let p = mask_pos(&mut app, target);
        click(&mut app, &ctx, p);
        assert!(app.isolate && app.mask);
        let p = mask_pos(&mut app, other);
        click(&mut app, &ctx, p);
        assert!(!app.isolate && app.mask);
        assert_eq!(&app.selected, other);
        let p = mask_pos(&mut app, other);
        solo_click(&mut app, &ctx, p, alt);
        assert!(app.isolate && app.mask);
        let p = mask_pos(&mut app, target);
        solo_click(&mut app, &ctx, p, alt);
        assert!(app.isolate && app.mask);
        assert_eq!(&app.selected, target);
        let p = mask_pos(&mut app, target);
        solo_click(&mut app, &ctx, p, alt);
        assert!(!app.isolate && app.mask);
        let e = app.shared.lock().unwrap();
        assert!(e.resident_state().expect("resident fixture").undo.is_empty());
        assert_eq!(
            crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap(),
            crate::psd::encode(&before).unwrap()
        );
    }
    #[test]
    fn solo_exits_when_shared_edits_remove_its_mask_or_layer_and_undo_does_not_reenable_it() {
        for depth in [8, 16] {
            let (mut app, ctx, before) = solo_fixture(depth);
            let target = &before.layers[0].id;
            let other = &before.layers[1].id;
            for op in ["mask.remove", "layer.delete"] {
                app.select_content(target);
                app.select_channel(op == "mask.remove");
                app.set_solo(true);
                app.shared
                    .lock()
                    .unwrap()
                    .edit(
                        "human",
                        &[json!({"op":op,"layer":target})],
                        None,
                        None,
                        "Remove solo target",
                    )
                    .unwrap();
                frame(&mut app, &ctx, vec![], Default::default());
                assert!(!app.isolate && !app.mask);
                let current = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                wait_solo_pixels(&mut app, &ctx, &current, None, false);
                app.shared.lock().unwrap().undo("human").unwrap();
                frame(&mut app, &ctx, vec![], Default::default());
                assert!(!app.isolate);
                assert_eq!(
                    app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
                    before.export_png().unwrap()
                );
            }
            app.select_content(target);
            app.set_solo(true);
            // Covers asynchronous selections that bypass row controls.
            app.selected = other.clone();
            frame(&mut app, &ctx, vec![], Default::default());
            assert!(!app.isolate);
        }
    }
    fn solo_fixture(depth: u16) -> (PeerBrush, egui::Context, Document) {
        let (mut app, ctx) = small_fixture();
        let before = {
            let mut e = app.shared.lock().unwrap();
            e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
            e.resident_state_mut().expect("resident fixture").document.layers[0].name = "Solo target".into();
            for y in 3..18 {
                for x in 4..22 {
                    e.resident_state_mut().expect("resident fixture").document.layers[0]
                        .pixels
                        .set16(x, y, [12347, 33559, 51237, 65535]);
                }
            }
            let mut other = crate::engine::Layer::new("Other layer", "fill", 32, 24);
            other.color = [233, 84, 32, 255];
            let other_id = other.id.clone();
            e.resident_state_mut().expect("resident fixture").document.layers.push(other);
            let target = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
            e.edit(
                "human",
                &[
                    json!({"op":"mask.add","layer":target,"value":128}),
                    json!({"op":"mask.add","layer":other_id,"value":255}),
                ],
                None,
                None,
                "Fixture masks",
            )
            .unwrap();
            e.resident_state_mut().expect("resident fixture").undo.clear();
            e.resident_state().expect("resident fixture").document.clone()
        };
        app.select_content(&before.layers[0].id);
        (app, ctx, before)
    }
    fn solo_click(app: &mut PeerBrush, ctx: &egui::Context, pos: Pos2, mods: egui::Modifiers) {
        frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(pos),
                button(pos, egui::PointerButton::Primary, true, mods),
            ],
            mods,
        );
        frame(
            app,
            ctx,
            vec![button(pos, egui::PointerButton::Primary, false, mods)],
            mods,
        );
    }
    fn wait_solo_pixels(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        doc: &Document,
        layer: Option<&str>,
        mask: bool,
    ) {
        let expected = doc.preview(None, 8192, layer, mask).unwrap().2;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            frame(app, ctx, vec![], Default::default());
            if !app.pending
                && app
                    .canvas_pixels
                    .as_ref()
                    .is_some_and(|(_, _, p)| p == &expected)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Solo target change did not restore rendered pixels"
            );
            std::thread::sleep(Duration::from_millis(5));
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
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.selection = Some([4, 5, 10, 11]);
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
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get(4, 5), app.color);
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get(9, 10), app.color);
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get(3, 5), [0; 4]);
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get(10, 10), [0; 4]);
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].kind, "paint");
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get(4, 5), [0; 4]);
    }
    #[test]
    fn native_control_clicks_and_folder_button_group_native16_sources_once() {
        let (mut app, ctx) = small_fixture();
        let ids = {
            let mut e = app.shared.lock().unwrap();
            e.resident_state_mut().expect("resident fixture").document.bit_depth = 16;
            e.resident_state_mut().expect("resident fixture").document.ensure_depth();
            e.edit(
                "human",
                &[json!({"op":"layer.add","name":"Second"})],
                None,
                None,
                "Setup",
            )
            .unwrap();
            for (index, layer) in e.resident_state_mut().expect("resident fixture").document.layers.iter_mut().enumerate() {
                layer
                    .pixels
                    .set16(index as i32 + 3, 5, [12347, 23459, 34571, 65535]);
            }
            e.resident_state_mut().expect("resident fixture").undo.clear();
            e.resident_state().expect("resident fixture").document
                .layers
                .iter()
                .map(|l| l.id.clone())
                .collect::<Vec<_>>()
        };
        app.select_content(&ids[0]);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![], Default::default());
        }
        let baseline = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
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
        let group = e.resident_state().expect("resident fixture").document.layers.iter().find(|l| l.id == app.selected).unwrap();
        assert_eq!(group.kind, "group");
        assert_eq!(group.pixels.depth, 16);
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
        assert_eq!(
            app.selection_layers,
            [group.id.clone()].into_iter().collect()
        );
        assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), baseline);
        assert!(ids.iter().all(|id| e
            .resident_state().expect("resident fixture").document
            .layers
            .iter()
            .any(|l| &l.id == id && l.parent.as_deref() == Some(group.id.as_str()))));
    }
    #[test]
    fn delete_key_removes_selected_roots_once_and_preserves_native_undo() {
        let (mut app, ctx) = small_fixture();
        let (roots, child, baseline) = {
            let mut e = app.shared.lock().unwrap();
            e.resident_state_mut().expect("resident fixture").document.bit_depth = 16;
            e.resident_state_mut().expect("resident fixture").document.ensure_depth();
            let child = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
            e.resident_state_mut().expect("resident fixture").document.layers[0]
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
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .filter(|l| ["Folder", "Outside"].contains(&l.name.as_str()))
                .map(|l| l.id.clone())
                .collect::<Vec<_>>();
            e.resident_state_mut().expect("resident fixture").undo.clear();
            (roots, child, e.resident_state().expect("resident fixture").document.clone())
        };
        app.select_content(&roots[0]);
        app.selection_layers.extend(roots.iter().cloned());
        app.selection_layers.insert(child);
        frame(&mut app, &ctx, vec![], Default::default());
        key(&mut app, &ctx, egui::Key::Delete);
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].name, "Keep");
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(
            serde_json::to_value(&e.resident_state().expect("resident fixture").document.layers).unwrap(),
            serde_json::to_value(&baseline.layers).unwrap()
        );
        assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), baseline.export_png().unwrap());
    }
    #[test]
    fn delete_key_preserves_typing_and_unfinished_selection_anchors() {
        for typing in [true, false] {
            let (mut app, ctx) = small_fixture();
            frame(&mut app, &ctx, vec![], Default::default());
            let id = app.selected.clone();
            if typing {
                let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                app.begin_rename(&doc, &id);
                frame(&mut app, &ctx, vec![], Default::default());
            } else {
                app.tool = Tool::Selection;
                app.selection_kind = "polygon".into();
                app.selection_path = vec![[2., 2.], [8., 4.]];
            }
            key(&mut app, &ctx, egui::Key::Delete);
            let e = app.shared.lock().unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
            assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].id, id);
            assert!(e.resident_state().expect("resident fixture").undo.is_empty());
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
            e.resident_state_mut().expect("resident fixture").undo.clear();
            e.resident_state().expect("resident fixture").document
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
        assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 3);
        assert_eq!(
            e.resident_state().expect("resident fixture").document
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
            .resident_state().expect("resident fixture").document
            .layers
            .iter()
            .find(|l| l.id == *id)
            .unwrap()
            .parent
            .as_deref()
            == Some(&app.selected)));
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
    }
    #[test]
    fn selection_ants_follow_the_rendered_transform_polygon_and_reject_stale_geometry() {
        let (mut app, _) = small_fixture();
        let mut doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        doc.selection = Some([4, 4, 12, 12]);
        doc.selection_polygon = Some(vec![[8.0, 4.0], [12.0, 8.0], [8.0, 12.0], [4.0, 8.0]]);
        let preview_polygon = vec![[12.0, 5.0], [16.0, 9.0], [12.0, 13.0], [8.0, 9.0]];
        app.canvas_selection = Some(CanvasSelection {
            document: doc.id.clone(),
            revision: doc.revision,
            bounds: Some([8, 5, 16, 13]),
            polygon: Some(preview_polygon.clone()),
            coverage: None,
        });
        assert_eq!(app.selection_display((&doc).into()).1, Some(preview_polygon.as_slice()));
        doc.revision += 1;
        assert_eq!(app.selection_display((&doc).into()).1, doc.selection_polygon.as_deref());
    }
    #[test]
    fn cleared_preview_hides_committed_selection_contours() {
        let (mut app, _) = small_fixture();
        let mut doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        doc.selection = Some([1, 2, 7, 8]);
        doc.selection_coverage = Some(crate::selection::Coverage {
            mask: crate::raster::Raster::new(6, 6),
            bounds: [1, 2, 7, 8],
            origin: [1, 2],
            contours: vec![vec![[1., 2.], [7., 2.], [7., 8.]]],
        });
        assert!(app.selection_display((&doc).into()).2.is_some());
        app.canvas_selection = Some(CanvasSelection {
            document: doc.id.clone(),
            revision: doc.revision,
            bounds: None,
            polygon: None,
            coverage: None,
        });
        assert_eq!(app.selection_display((&doc).into()), (None, None, None));
        app.canvas_selection.as_mut().unwrap().revision += 1;
        assert!(app.selection_display((&doc).into()).2.is_some());
        doc.selection = None;
        assert!(app.selection_display((&doc).into()).2.is_none());
        doc.selection = Some([2, 2, 7, 8]);
        assert!(app.selection_display((&doc).into()).2.is_none());
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
            e.resident_state_mut().expect("resident fixture").document.selection = Some([4, 5, 12, 13]);
            e.resident_state_mut().expect("resident fixture").undo.clear();
            e.resident_state().expect("resident fixture").document
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
            app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty(),
            "Live rotation preview must not commit history"
        );
    }
    #[test]
    fn transform_drafts_cancel_on_source_focus_and_target_changes_without_rebasing_selection() {
        for depth in [8, 16] {
            for tool in [Tool::Move, Tool::Rotate, Tool::Scale] {
                for change in [
                    "selection",
                    "focus",
                    "focus-return",
                    "contact",
                    "target",
                    "channel",
                    "document",
                ] {
                    let (mut app, ctx) = small_fixture();
                    let (layer, second, step) =
                        {
                            let mut engine = app.shared.lock().unwrap();
                            engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                            let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                            for y in 5..13 {
                                for x in 4..12 {
                                    engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
                                        x,
                                        y,
                                        [12347, 23459, 34571, 65535],
                                    );
                                }
                            }
                            engine.resident_state_mut().expect("resident fixture").document.layers[0]
                                .pixels
                                .set16(25, 25, [51239, 12349, 33561, 65535]);
                            // A quarter-turn of a uniform square is invisible.
                            // Retain an asymmetric native sample for retry proof.
                            engine.resident_state_mut().expect("resident fixture").document.layers[0]
                                .pixels
                                .set16(4, 5, [51347, 10359, 22371, 65535]);
                            engine.edit("human", &[
                            json!({"op":"layer.add","name":"Other target"}),
                            json!({"op":"mask.add","layer":layer,"value":255}),
                            json!({"op":"selection","kind":"rectangle","rect":[4,5,12,13]}),
                        ], None, None, "Fixture").unwrap();
                            let second = engine
                                .resident_state_mut().expect("resident fixture").document
                                .layers
                                .iter_mut()
                                .find(|l| l.name == "Other target")
                                .unwrap();
                            second.pixels.set16(7, 8, [43591, 22347, 10539, 65535]);
                            let second = second.id.clone();
                            let step = engine
                                .resident_state().expect("resident fixture").document
                                .layers
                                .iter()
                                .find(|l| l.id == layer)
                                .unwrap()
                                .mask
                                .as_ref()
                                .unwrap()
                                .steps[0]
                                .id
                                .clone();
                            engine.resident_state_mut().expect("resident fixture").undo.clear();
                            (layer, second, step)
                        };
                    app.select_content(&layer);
                    app.select_tool(tool);
                    frame(&mut app, &ctx, vec![], Default::default());
                    let rect = app.view_rect.unwrap();
                    let center = rect.min + Vec2::new(8., 9.) * (rect.width() / 32.);
                    let (start, end) = if tool == Tool::Rotate {
                        (center + Vec2::new(58., 0.), center + Vec2::new(0., 58.))
                    } else {
                        (center, center + Vec2::new(35., 10.))
                    };
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
                    assert_ne!(app.gizmo_handle, 0, "{depth} {} {change}", tool.label());
                    frame(
                        &mut app,
                        &ctx,
                        vec![egui::Event::PointerMoved(end)],
                        Default::default(),
                    );
                    assert_eq!(app.transient.len(), 1);
                    assert_eq!(app.transient[0]["selection_only"], true);
                    assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                    let events = match change {
                        "selection" => {
                            app.shared
                                .lock()
                                .unwrap()
                                .edit(
                                    "other",
                                    &[json!({"op":"selection.clear"})],
                                    None,
                                    None,
                                    "Newer coverage",
                                )
                                .unwrap();
                            vec![egui::Event::PointerMoved(end)]
                        }
                        "focus" => vec![
                            egui::Event::WindowFocused(false),
                            egui::Event::PointerMoved(end),
                        ],
                        "focus-return" => vec![
                            egui::Event::WindowFocused(false),
                            egui::Event::WindowFocused(true),
                            egui::Event::PointerMoved(end),
                        ],
                        "contact" => vec![
                            egui::Event::Touch {
                                device_id: egui::TouchDeviceId(1), id: egui::TouchId(1),
                                phase: egui::TouchPhase::Cancel, pos: end, force: None,
                            },
                            egui::Event::PointerGone,
                            primary_release(end),
                        ],
                        "target" => {
                            app.select_content(&second);
                            vec![egui::Event::PointerMoved(end)]
                        }
                        "channel" => {
                            app.mask = true;
                            app.mask_step = Some(step);
                            vec![egui::Event::PointerMoved(end)]
                        }
                        _ => {
                            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.id = "different-transform-source".into();
                            vec![egui::Event::PointerMoved(end)]
                        }
                    };
                    let later = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                    let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
                    frame(&mut app, &ctx, events, Default::default());
                    assert_eq!(app.gizmo_handle, 0, "{depth} {} {change}", tool.label());
                    assert!(app.gizmo_source.is_none());
                    assert!(app.transient.is_empty());
                    assert!(app.drag_start.is_none());
                    assert!(app.drag_revision.is_none());
                    assert!(app.live_gesture.is_none());
                    // Late release after cancellation cannot use newer coverage,
                    // retarget the transform or commit a whole-layer fallback.
                    frame(
                        &mut app,
                        &ctx,
                        vec![
                            egui::Event::WindowFocused(true),
                            button(end, egui::PointerButton::Primary, false, Default::default()),
                        ],
                        Default::default(),
                    );
                    {
                        let engine = app.shared.lock().unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history);
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                            later.export_png().unwrap()
                        );
                        assert_eq!(
                            serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            serde_json::to_value(&later).unwrap()
                        );
                    }
                    // A fresh press can still transform the current selected
                    // source, preview actual pixels and commit one exact undo.
                    app.select_content(&layer);
                    {
                        let mut engine = app.shared.lock().unwrap();
                        engine
                            .edit(
                                "human",
                                &[json!({"op":"selection","kind":"rectangle","rect":[4,5,12,13]})],
                                None,
                                None,
                                "Retry coverage",
                            )
                            .unwrap();
                        engine.resident_state_mut().expect("resident fixture").undo.clear();
                    }
                    frame(&mut app, &ctx, vec![], Default::default());
                    let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
                    let draft = Engine::preview_edits(before.clone(), &app.transient).unwrap();
                    assert!(
                        draft.export_png().unwrap() != before.export_png().unwrap(),
                        "{depth} {} {change}: retry must render the transformed pixels",
                        tool.label()
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
                    let mut engine = app.shared.lock().unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1, "{}", app.message);
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        draft.export_png().unwrap()
                    );
                    engine.undo("human").unwrap();
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        before.export_png().unwrap()
                    );
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                    assert_eq!(engine.resident_state().expect("resident fixture").document.selection_coverage, before.selection_coverage);
                }
            }
        }
    }

    #[test]
    fn transform_draft_commands_reject_source_races_in_shared_previews_and_transactions() {
        for depth in [8, 16] {
            for tool in [Tool::Move, Tool::Rotate, Tool::Scale] {
                let (mut app, ctx) = small_fixture();
                {
                    let mut engine = app.shared.lock().unwrap();
                    engine.resident_state_mut().expect("resident fixture").document.bit_depth = depth;
                    engine.resident_state_mut().expect("resident fixture").document.ensure_depth();
                    for y in 5..13 {
                        for x in 4..12 {
                            engine.resident_state_mut().expect("resident fixture").document.layers[0]
                                .pixels
                                .set16(x, y, [12347, 23459, 34571, 65535]);
                        }
                    }
                    engine
                        .edit(
                            "human",
                            &[json!({"op":"selection","kind":"rectangle","rect":[4,5,12,13]})],
                            None,
                            None,
                            "Fixture",
                        )
                        .unwrap();
                    engine.resident_state_mut().expect("resident fixture").undo.clear();
                }
                app.select_tool(tool);
                frame(&mut app, &ctx, vec![], Default::default());
                let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                let rect = app.view_rect.unwrap();
                let center = rect.min + Vec2::new(8., 9.) * (rect.width() / 32.);
                let (start, end) = if tool == Tool::Rotate {
                    (center + Vec2::new(58., 0.), center + Vec2::new(0., 58.))
                } else {
                    (center, center + Vec2::new(35., 10.))
                };
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
                let commands = app.transient.clone();
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0]["document_id"], before.id);
                assert_eq!(commands[0]["source_revision"], before.revision);
                assert!(Engine::preview_edits(before.clone(), &commands).is_ok());
                let mut engine = app.shared.lock().unwrap();
                engine
                    .edit(
                        "other",
                        &[json!({"op":"selection.clear"})],
                        None,
                        None,
                        "Later coverage",
                    )
                    .unwrap();
                let later = engine.resident_state().expect("resident fixture").document.clone();
                assert!(Engine::preview_edits(later.clone(), &commands).is_err());
                assert!(engine
                    .edit("human", &commands, None, None, "Reject stale transform")
                    .is_err());
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(
                    serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    serde_json::to_value(&later).unwrap()
                );
                let mut other = before;
                other.id = "different-transform-source".into();
                assert!(Engine::preview_edits(other, &commands).is_err());
            }
        }
    }

    #[test]
    fn folder_gizmo_wer_previews_and_commits_only_the_wand_selection() {
        for depth in [8, 16] {
            for tool in [Tool::Move, Tool::Rotate, Tool::Scale] {
                let (mut app, ctx) = small_fixture();
                let (root, child) = {
                    let mut e = app.shared.lock().unwrap();
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                    let mut folder = crate::engine::Layer::new("Folder", "group", 32, 32);
                    folder.pixels = crate::raster::Raster::new_depth(32, 32, depth);
                    let root = folder.id.clone();
                    let child = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    e.resident_state_mut().expect("resident fixture").document.layers[0].parent = Some(root.clone());
                    for y in 5..11 {
                        for x in 4..12 {
                            e.resident_state_mut().expect("resident fixture").document.layers[0]
                                .pixels
                                .set16(x, y, [12347, 23459, 34571, 65535]);
                        }
                    }
                    e.resident_state_mut().expect("resident fixture").document.layers[0]
                        .pixels
                        .set16(25, 25, [51239, 12349, 33561, 65535]);
                    e.resident_state_mut().expect("resident fixture").document.layers.push(folder);
                    e.edit("human", &[json!({"op":"selection","kind":"wand","layer":child,"point":[6,7],"sample_merged":false,"tolerance":0})], None, None, "Select").unwrap();
                    e.resident_state_mut().expect("resident fixture").undo.clear();
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
                let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
                        egui::Event::PointerMoved(end),
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
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                frame(
                    &mut app,
                    &ctx,
                    vec![primary_release(end), egui::Event::PointerMoved(start), egui::Event::PointerGone],
                    egui::Modifiers { shift: true, ..Default::default() },
                );
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1, "{}: {}", tool.label(), app.message);
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), preview.export_png().unwrap());
                let layer = e.resident_state().expect("resident fixture").document.layers.iter().find(|l| l.id == child).unwrap();
                assert_eq!(
                    layer.pixels.get16(25 - layer.x, 25 - layer.y),
                    before.layers[0].pixels.get16(25, 25)
                );
                assert_ne!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                e.undo("human").unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                assert_eq!(e.resident_state().expect("resident fixture").document.selection, before.selection);
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
            let child = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
            e.resident_state_mut().expect("resident fixture").document.layers[0].parent = Some(root.clone());
            for y in 5..13 {
                for x in 4..12 {
                    e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(x, y, [200, 87, 31, 255]);
                }
            }
            e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(4, 5, [31, 87, 200, 255]);
            e.resident_state_mut().expect("resident fixture").document.layers.push(folder);
            e.resident_state_mut().expect("resident fixture").undo.clear();
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
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let preview = Engine::preview_edits(doc.clone(), &transforms).unwrap();
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
        assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), preview.export_png().unwrap());
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
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
                engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels =
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
                app.stroke_command(&app.points, app.color)
            };
            let document = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
            assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, document.revision);
            assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
                engine.resident_state().expect("resident fixture").document.preview(None, 32, None, false).unwrap().2,
                expected
            );
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
            if tool == Tool::Liquify {
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba(), source);
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].effects[0].kind, "liquify");
            } else {
                assert_ne!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba(), source);
            }
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba(), source);
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
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").undo.clear();
        key(&mut app, &ctx, egui::Key::L);
        app.mask = true;
        let source = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
        assert!(engine.resident_state().expect("resident fixture").document.layers[0].effects.is_empty());
        assert_eq!(
            engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba(),
            source.layers[0].pixels.rgba()
        );
        assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
        assert!(
            app.message.contains("Color"),
            "Explain the unsupported mask channel compactly"
        );
    }
    #[test]
    fn shift_d_duplicates_active_pixels_or_layer_and_preserves_typing() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture();
            {
                let mut engine = app.shared.lock().unwrap();
                engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(16, 16, depth).unwrap();
                for y in 0..16 {
                    for x in 0..16 {
                        engine.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(x, y, [12347, 33559, 51237, 45679]);
                    }
                }
            }
            let id = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].id.clone();
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
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").undo.clear();
            frame(&mut app, &ctx, vec![], Default::default());
            app.layer_clipboard = true;
            modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::COMMAND);
            assert!(app.shared.lock().unwrap().resident_state().unwrap().undo.is_empty());
            let mods = egui::Modifiers::SHIFT;
            modified_key(&mut app, &ctx, egui::Key::D, mods);
            let first = app.selected.clone();
            app.history_action(false);
            assert_eq!(app.selected, id);
            assert_eq!(app.selection_layers, HashSet::from([id.clone()]));
            app.history_action(true);
            assert_eq!(app.selected, first);
            {
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 2);
                assert_eq!(engine.resident_state().expect("resident fixture").document.selection, Some([1, 1, 13, 13]));
                assert_eq!(app.selected, engine.resident_state().expect("resident fixture").document.layers[0].id);
                assert_eq!((engine.resident_state().expect("resident fixture").document.layers[0].x, engine.resident_state().expect("resident fixture").document.layers[0].y), (1, 1));
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.depth, depth);
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
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
                engine.resident_state_mut().expect("resident fixture").undo.clear();
            }
            app.select_content(&id);
            modified_key(&mut app, &ctx, egui::Key::D, mods);
            let first = app.selected.clone();
            app.history_action(false);
            assert_eq!(app.selected, id);
            app.history_action(true);
            assert_eq!(app.selected, first);
            {
                let engine = app.shared.lock().unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 2);
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    engine.resident_state().expect("resident fixture").document.layers[1].pixels.rgba16()
                );
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
            }
            // Shift+D remains ordinary text input when an editor owns focus.
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
            assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers.len(), 2);
            assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        }
    }

    #[test]
    fn shift_d_repeats_the_last_copy_movement_in_one_history_entry() {
        let (mut app, ctx) = small_fixture();
        let original = app.selected.clone();
        modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
        let first = app.selected.clone();
        assert_ne!(first, original);
        app.edit(vec![json!({"op":"move","layer":first,"dx":3,"dy":2,"selection_only":false})], "Move copy");
        let mut copies = vec![first.clone()];
        for (x, y) in [(6, 4), (9, 6)] {
            modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
            copies.push(app.selected.clone());
            let engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            let layer = state.document.layers.iter().find(|layer| layer.id == app.selected).unwrap();
            assert_eq!((layer.x, layer.y), (x, y));
        }
        let mut engine = app.shared.lock().unwrap();
        let state = engine.resident_state().unwrap();
        assert_eq!(state.undo.len(), 4);
        assert_eq!(state.document.layers.len(), 4);
        drop(engine);
        app.history_action(false);
        assert_eq!(app.selected, copies[1]);
        app.history_action(false);
        assert_eq!(app.selected, copies[0]);
        assert_eq!(app.selection_layers, HashSet::from([copies[0].clone()]));
        app.history_action(true);
        assert_eq!(app.selected, copies[1]);
        app.history_action(true);
        assert_eq!(app.selected, copies[2]);
        app.history_action(false);
        assert_eq!(app.selected, copies[1]);
        modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
        {
            let engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            let layer = state.document.layers.iter().find(|layer| layer.id == app.selected).unwrap();
            assert_eq!((layer.x, layer.y), (9, 6));
            assert_eq!(state.undo.len(), 4);
            assert!(state.redo.is_empty());
        }
        let moved = app.selected.clone();
        app.select_tool(Tool::Rotate);
        app.history_action(false);
        assert_eq!(app.selected, copies[1]);
        app.history_action(true);
        assert_eq!(app.selected, moved);
        modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
        let mut engine = app.shared.lock().unwrap();
        let state = engine.resident_state().unwrap();
        let layer = state.document.layers.iter().find(|layer| layer.id == app.selected).unwrap();
        assert_eq!((layer.x, layer.y), (9, 6));
        engine.undo("human").unwrap();
        engine.undo("human").unwrap();
        assert_eq!(engine.resident_state().unwrap().document.layers.len(), 3);
    }

    #[test]
    fn shift_d_repeats_whole_target_movement_after_a_pixel_copy_with_a_marquee() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let mut document = Document::new_depth(32, 32, depth).unwrap();
            document.layers[0].pixels.set16(2, 2, [11223, 22334, 33445, 65535]);
            document.selection = Some([1, 1, 4, 4]);
            let original = document.layers[0].clone();
            let marquee = document.selection;
            app.shared.lock().unwrap().resident_state_mut().unwrap().document = document;
            app.select_content(&original.id);
            modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
            let first = app.selected.clone();
            assert_ne!(first, original.id);
            key(&mut app, &ctx, egui::Key::W);
            app.history_action(false);
            assert_eq!(app.selected, original.id);
            app.history_action(true);
            assert_eq!(app.selected, first);
            frame(&mut app, &ctx, vec![], Default::default());
            let rect = app.view_rect.unwrap();
            let scale = rect.width() / 32.;
            let start = app.view.point([2.5, 2.5], rect, scale);
            let end = app.view.point([5.5, 4.5], rect, scale);
            frame(&mut app, &ctx, vec![
                egui::Event::PointerMoved(start),
                button(start, egui::PointerButton::Primary, true, Default::default()),
            ], Default::default());
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)], Default::default());
            assert_eq!(app.transient[0]["selection_only"], false);
            frame(
                &mut app, &ctx,
                vec![button(end, egui::PointerButton::Primary, false, Default::default())],
                Default::default(),
            );
            app.edit(vec![json!({"op":"selection.clear"})], "Clear marquee");
            app.edit(vec![json!({"op":"selection.reselect"})], "Restore marquee");
            app.edit(vec![json!({"op":"layer.update","layer":first,"name":"Offset sample","opacity":0.5})], "Edit copy properties");
            app.edit(vec![json!({"op":"effect.add","layer":first,"kind":"invert"})], "Add effect to copy");
            let mut copies = vec![first];
            for (index, (x, y)) in [(7, 5), (10, 7)].into_iter().enumerate() {
                modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
                copies.push(app.selected.clone());
                let engine = app.shared.lock().unwrap();
                let state = engine.resident_state().unwrap();
                let layers = &state.document.layers;
                let copy = layers.iter().find(|layer| layer.id == app.selected).unwrap();
                assert_eq!((copy.x, copy.y), (x, y));
                assert_eq!(copy.pixels.depth, depth);
                assert_eq!(copy.opacity, 0.5);
                assert_eq!(copy.effects[0].kind, "invert");
                assert_eq!(copy.pixels.get16(1, 1), original.pixels.get16(2, 2));
                assert_eq!(layers.last().unwrap().pixels, original.pixels);
                assert_eq!(state.document.selection, marquee);
                assert_eq!(state.undo.len(), 7 + index);
            }
            for (redo, copy, selection) in [
                (false, &copies[1], marquee),
                (false, &copies[0], marquee),
                (false, &copies[0], marquee),
                (false, &copies[0], marquee),
                (false, &copies[0], None),
                (false, &copies[0], marquee),
                (false, &copies[0], marquee),
                (false, &original.id, marquee),
                (true, &copies[0], marquee),
                (true, &copies[0], marquee),
                (true, &copies[0], None),
                (true, &copies[0], marquee),
                (true, &copies[0], marquee),
                (true, &copies[0], marquee),
                (true, &copies[1], marquee),
            ] {
                app.history_action(redo);
                assert_eq!(&app.selected, copy);
                assert_eq!(app.shared.lock().unwrap().resident_state().unwrap().document.selection, selection);
            }
            let engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            assert_eq!(state.document.layers.len(), 3);
            assert_eq!(state.document.selection, marquee);
        }
    }

    #[test]
    fn shift_d_copies_a_marquee_before_repeating_a_remembered_layer_transform() {
        let (mut app, ctx) = small_fixture();
        let mut document = Document::new_depth(32, 32, 16).unwrap();
        let color = [11223, 22334, 33445, 65535];
        document.layers[0].pixels.set16(2, 2, color);
        document.layers[0].pixels.set16(20, 20, color);
        let original = document.layers[0].id.clone();
        app.shared.lock().unwrap().resident_state_mut().unwrap().document = document;
        app.select_content(&original);
        app.edit(vec![json!({"op":"move","layer":original,"dx":3,"selection_only":false})], "Move layer");
        app.edit(vec![json!({"op":"selection","kind":"rectangle","rect":[4,1,7,4]})], "Select one mark");
        modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
        let first_copy = app.selected.clone();
        assert_ne!(first_copy, original);
        {
            let engine = app.shared.lock().unwrap();
            let state = engine.resident_state().unwrap();
            let copy = state.document.layers.iter().find(|layer| layer.id == first_copy).unwrap();
            assert_eq!((copy.x, copy.y, copy.pixels.width, copy.pixels.height), (4, 1, 3, 3));
            assert_eq!(copy.pixels.get16(1, 1), color);
            assert_eq!(state.undo.len(), 3);
        }
        app.history_action(false);
        assert_eq!(app.selected, original);
        app.history_action(true);
        assert_eq!(app.selected, first_copy);

        app.edit(vec![json!({"op":"move","layer":first_copy,"dx":6,"selection_only":false})], "Move copy");
        modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
        let engine = app.shared.lock().unwrap();
        let state = engine.resident_state().unwrap();
        let copy = state.document.layers.iter().find(|layer| layer.id == app.selected).unwrap();
        assert_eq!((copy.x, copy.y), (16, 1));
        assert_eq!(copy.pixels.get16(1, 1), color);
        assert_eq!(state.document.selection, Some([4, 1, 7, 4]));
        assert_eq!(state.undo.len(), 5);
    }

    #[test]
    fn shift_d_repeats_an_affine_transform_before_the_first_copy() {
        for depth in [8, 16] {
            let (mut app, ctx) = small_fixture();
            let mut doc = Document::new_depth(32, 32, depth).unwrap();
            doc.layers[0].pixels.set16(10, 17, [11223, 22334, 33445, 65535]);
            let mut expected = doc.layers[0].clone();
            let original = expected.id.clone();
            app.shared.lock().unwrap().resident_state_mut().unwrap().document = doc;
            app.select_content(&original);
            let mut transform = json!({
                "op":"transform", "layer":original, "selection_only":false,
                "angle":90, "scale_x":1.25, "scale_y":0.75, "pivot":[12,18], "dx":3, "dy":-2
            });
            crate::transform::layer(&mut expected, &transform).unwrap();
            let moved_original = expected.clone();
            app.edit(vec![transform.clone()], "Transform original");
            app.history_action(false);
            assert_eq!(app.selected, original);
            app.history_action(true);
            assert_eq!(app.selected, original);
            for index in 1..=2 {
                transform["pivot"] = json!([12 + index * 3, 18 - index * 2]);
                crate::transform::layer(&mut expected, &transform).unwrap();
                modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
                let engine = app.shared.lock().unwrap();
                let state = engine.resident_state().unwrap();
                let layers = &state.document.layers;
                let copied = layers.iter().find(|layer| layer.id == app.selected).unwrap();
                let original = layers.iter().find(|layer| layer.id == original).unwrap();
                assert_ne!(copied.id, original.id);
                assert_eq!((copied.x, copied.y), (expected.x, expected.y));
                assert_eq!(copied.pixels, expected.pixels);
                assert_eq!(original.pixels, moved_original.pixels);
                assert_eq!((original.x, original.y), (moved_original.x, moved_original.y));
                assert_eq!(state.undo.len(), index as usize + 1);
            }
        }
    }

    #[test]
    fn shift_d_transforms_the_copied_roots_of_a_nested_selection_once() {
        use crate::engine::Layer;
        let (mut app, ctx) = small_fixture();
        let folder = Layer::new("Folder", "group", 32, 32);
        let folder_id = folder.id.clone();
        let mut child = Layer::new("Child", "paint", 32, 32);
        child.parent = Some(folder_id.clone());
        child.pixels.set(0, 0, [10, 20, 30, 255]);
        let child_id = child.id.clone();
        let sibling = app.selected.clone();
        {
            let mut engine = app.shared.lock().unwrap();
            let layers = &mut engine.resident_state_mut().unwrap().document.layers;
            layers.splice(0..0, [child, folder]);
        }
        app.select_content(&child_id);
        app.selection_layers = [folder_id.clone(), child_id.clone(), sibling.clone()]
            .into_iter()
            .collect();
        app.edit(
            vec![
                json!({"op":"move", "layer":folder_id, "dx":3, "dy":1, "selection_only":false}),
                json!({"op":"move", "layer":sibling, "dx":-2, "dy":4, "selection_only":false}),
            ],
            "Move selected roots",
        );
        modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
        let engine = app.shared.lock().unwrap();
        let state = engine.resident_state().unwrap();
        let layers = &state.document.layers;
        let copied_folder = layers
            .iter()
            .find(|layer| layer.kind == "group" && layer.id != folder_id)
            .unwrap();
        let copied_child = layers
            .iter()
            .find(|layer| layer.parent.as_deref() == Some(copied_folder.id.as_str()))
            .unwrap();
        let copied_sibling = layers
            .iter()
            .find(|layer| layer.kind == "paint" && layer.parent.is_none() && layer.id != sibling)
            .unwrap();
        let original_child = layers.iter().find(|layer| layer.id == child_id).unwrap();
        assert_eq!((copied_child.x, copied_child.y), (6, 2));
        assert_eq!((copied_sibling.x, copied_sibling.y), (-4, 8));
        assert_eq!((original_child.x, original_child.y), (3, 1));
        assert_eq!(
            app.selection_layers,
            HashSet::from([copied_folder.id.clone(), copied_sibling.id.clone()])
        );
        assert_eq!(app.selected, copied_folder.id);
        let copied_folder_id = copied_folder.id.clone();
        assert_eq!(state.undo.len(), 2);
        drop(engine);
        app.history_action(false);
        assert_eq!(app.selected, child_id);
        app.history_action(true);
        assert_eq!(app.selected, copied_folder_id);

        app.edit(
            vec![json!({"op":"move", "layer":copied_folder_id, "dx":1, "dy":-2, "selection_only":false})],
            "Move one root",
        );
        modified_key(&mut app, &ctx, egui::Key::D, egui::Modifiers::SHIFT);
        let engine = app.shared.lock().unwrap();
        let state = engine.resident_state().unwrap();
        let layers = &state.document.layers;
        assert_ne!(app.selected, copied_folder_id);
        let latest_child = layers.iter()
            .find(|layer| layer.parent.as_deref() == Some(app.selected.as_str()))
            .unwrap();
        let latest_sibling = layers.iter()
            .find(|layer| layer.id != app.selected && app.selection_layers.contains(&layer.id))
            .unwrap();
        assert_eq!((latest_child.x, latest_child.y), (8, -2));
        assert_eq!((latest_sibling.x, latest_sibling.y), (-4, 8));
        assert_eq!(state.undo.len(), 4);
    }

    fn deselection_fixture(depth: u16) -> (PeerBrush, egui::Context, Document) {
        let (mut app, ctx) = fixture();
        {
            let mut e = app.shared.lock().unwrap();
            e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
            for y in 0..24 {
                for x in 0..32 {
                    e.resident_state_mut().expect("resident fixture").document.layers[0]
                        .pixels
                        .set16(x, y, [12347, 33559, 51237, 45679]);
                }
            }
            app.selected = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
            app.selection_layers = [app.selected.clone()].into_iter().collect();
            e.edit(
                "human",
                &[json!({"op":"selection","kind":"ellipse","rect":[4,4,24,20],"feather":1})],
                None,
                None,
                "Area",
            )
            .unwrap();
            e.resident_state_mut().expect("resident fixture").undo.clear();
        }
        app.zoom = 0.55;
        frame(&mut app, &ctx, vec![], Default::default());
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        (app, ctx, before)
    }
    #[test]
    fn deselection_keys_reselect_native_coverage_and_cancel_gestures() {
        for depth in [8, 16] {
            for ctrl in [false, true] {
                let (mut app, ctx, before) = deselection_fixture(depth);
                app.select_tool(Tool::Move);
                app.drag_start = Some([4., 4.]);
                app.points = vec![[4., 4.], [8., 8.]];
                app.transient = vec![json!({"op":"move","layer":app.selected,"dx":4,"dy":4})];
                let mods = egui::Modifiers {
                    command: true,
                    ctrl,
                    shift: true,
                    ..Default::default()
                };
                modified_key(&mut app, &ctx, egui::Key::D, mods);
                assert!(app.tool == Tool::Move);
                assert!(
                    app.points.is_empty() && app.transient.is_empty() && app.drag_start.is_none()
                );
                {
                    let mut e = app.shared.lock().unwrap();
                    assert!(e.resident_state().expect("resident fixture").document.selection.is_none());
                    assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                    assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels, before.layers[0].pixels);
                    e.undo("human").unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").document.selection, before.selection);
                    assert_eq!(e.resident_state().expect("resident fixture").document.selection_coverage, before.selection_coverage);
                    e.redo("human").unwrap();
                }
                modified_key(
                    &mut app,
                    &ctx,
                    egui::Key::D,
                    egui::Modifiers {
                        command: true,
                        ctrl,
                        alt: true,
                        ..Default::default()
                    },
                );
                {
                    let e = app.shared.lock().unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").document.selection_coverage, before.selection_coverage);
                    assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
                    assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 2);
                }
                key(&mut app, &ctx, egui::Key::Q);
                assert!(app.tool == Tool::None);
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection.is_none());
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 3);
                key(&mut app, &ctx, egui::Key::Q);
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 3);
            }
        }
    }
    #[test]
    fn deselection_canvas_clicks_clear_all_modes_and_workspace_only() {
        for depth in [8, 16] {
            for kind in ["rectangle", "ellipse", "lasso", "object"] {
                let (mut app, ctx, before) = deselection_fixture(depth);
                for mode in ["replace", "add", "subtract", "intersect"] {
                    app.select_tool(Tool::Selection);
                    app.selection_kind = kind.into();
                    app.selection_mode = mode.into();
                    frame(&mut app, &ctx, vec![], Default::default());
                    let pos = app.view_rect.unwrap().center();
                    click(&mut app, &ctx, pos);
                    let mut e = app.shared.lock().unwrap();
                    assert!(e.resident_state().expect("resident fixture").document.selection.is_none(), "{kind} {mode}");
                    assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                    assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels, before.layers[0].pixels);
                    e.undo("human").unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").document.selection_coverage, before.selection_coverage);
                }
            }
            for kind in [
                "rectangle",
                "ellipse",
                "row",
                "column",
                "lasso",
                "wand",
                "quick",
                "object",
            ] {
                let (mut app, ctx, before) = deselection_fixture(depth);
                app.select_tool(if kind == "wand" {
                    Tool::MagicWand
                } else {
                    Tool::Selection
                });
                app.selection_kind = kind.into();
                frame(&mut app, &ctx, vec![], Default::default());
                let row = app.layer_rects[&app.selected].center();
                click(&mut app, &ctx, row);
                assert_eq!(
                    app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection_coverage,
                    before.selection_coverage,
                    "Layer control {kind}"
                );
                app.selection_path = vec![[3., 3.], [8., 4.]];
                let outside = app.view_rect.unwrap().left_center() - Vec2::new(12., 0.);
                click(&mut app, &ctx, outside);
                let e = app.shared.lock().unwrap();
                assert!(e.resident_state().expect("resident fixture").document.selection.is_none(), "Workspace {kind}");
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                assert!(app.selection_path.is_empty());
                assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels, before.layers[0].pixels);
            }
            // Polygon clicks still construct a path; wand/row/column clicks still select.
            for kind in ["polygon", "wand", "row", "column", "quick"] {
                let (mut app, ctx, _) = deselection_fixture(depth);
                app.select_tool(Tool::Selection);
                app.selection_kind = kind.into();
                frame(&mut app, &ctx, vec![], Default::default());
                let pos = app.view_rect.unwrap().center();
                click(&mut app, &ctx, pos);
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection.is_some());
                if kind == "polygon" {
                    assert_eq!(app.selection_path.len(), 1);
                    assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                }
            }
        }
    }
    #[test]
    fn deselection_reservations_and_text_focus_preserve_artwork() {
        for depth in [8, 16] {
            let (mut app, ctx, before) = deselection_fixture(depth);
            app.select_tool(Tool::Brush);
            app.shared
                .lock()
                .unwrap()
                .reserve(
                    "other",
                    "Area",
                    vec![crate::engine::Scope::layer("@selection")],
                )
                .unwrap();
            key(&mut app, &ctx, egui::Key::Q);
            assert!(app.tool == Tool::None);
            {
                let e = app.shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.selection_coverage, before.selection_coverage);
                assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels, before.layers[0].pixels);
                assert!(e.resident_state().expect("resident fixture").undo.is_empty());
            }
            assert!(app.message.contains("Reserved by other"));
            app.select_tool(Tool::Brush);
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut String::new())
                            .id(egui::Id::new("deselection typing")),
                    )
                    .request_focus();
                });
            });
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
            assert!(app.tool == Tool::Brush);
            assert_eq!(
                app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection_coverage,
                before.selection_coverage
            );
        }
    }
    fn straight_preview(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        before: &Document,
        mods: egui::Modifiers,
    ) -> Document {
        let c = app.stroke_command(
            &app.points,
            if app.mask {
                [app.mask_value, app.mask_value, app.mask_value, app.color[3]]
            } else {
                app.color
            },
        );
        let preview = Engine::preview_edits(before.clone(), &[c]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            frame(app, ctx, vec![], mods);
            if app.canvas_pixels.as_ref().is_some_and(|(w, h, p)| {
                preview.preview(None, (*w).max(*h), None, false).unwrap().2 == *p
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Straight preview absent"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
            before.export_png().unwrap()
        );
        preview
    }
    #[test]
    fn straight_shift_clicks_preview_native_pixels_join_endpoints_and_commit_once() {
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        for depth in [8, 16] {
            for mask in [false, true] {
                let (mut app, ctx, before) = deselection_fixture(depth);
                app.select_tool(Tool::Brush);
                app.mask = mask;
                app.mask_value = 0;
                if mask {
                    app.shared
                        .lock()
                        .unwrap()
                        .edit(
                            "human",
                            &[json!({"op":"mask.add","layer":app.selected,"value":255})],
                            None,
                            None,
                            "Mask",
                        )
                        .unwrap();
                }
                app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").undo.clear();
                app.brush.tip = crate::brush::TipKind::Grain;
                app.brush.seed = 19;
                app.brush.density = 0.7;
                app.brush.flow = 0.6;
                app.brush.opacity = 0.75;
                app.radius = 2.;
                frame(&mut app, &ctx, vec![], Default::default());
                let r = app.view_rect.unwrap();
                let scale = r.width() / 32.;
                let pos = |p: [f32; 2]| r.min + Vec2::new(p[0], p[1]) * scale;
                let contact = |pos, phase, force| egui::Event::Touch {
                    device_id: egui::TouchDeviceId(1), id: egui::TouchId(1),
                    pos, phase, force: Some(force),
                };
                let first = pos([7., 12.]);
                frame(&mut app, &ctx, vec![
                    contact(first, egui::TouchPhase::Start, 0.35),
                    egui::Event::PointerMoved(first),
                    button(first, egui::PointerButton::Primary, true, Default::default()),
                    contact(first, egui::TouchPhase::End, 0.),
                    primary_release(first),
                    egui::Event::PointerGone,
                ], Default::default());
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
                for (count, point, previous) in
                    [(2, [21., 12.], [7., 12.]), (3, [14., 6.], [21., 12.])]
                {
                    let source = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                    let p = pos(point);
                    frame(
                        &mut app,
                        &ctx,
                        vec![
                            contact(p, egui::TouchPhase::Start, 0.9),
                            egui::Event::PointerMoved(p),
                            button(p, egui::PointerButton::Primary, true, shift),
                        ],
                        shift,
                    );
                    assert!(app.straight.line);
                    assert_eq!(app.points.len(), 2);
                    for axis in 0..2 {
                        assert!((app.points[0][axis] - previous[axis]).abs() < 0.0001);
                        assert!((app.points[1][axis] - point[axis]).abs() < 0.0001);
                    }
                    assert_eq!(app.stroke_pressures.last().copied(), Some(0.9));
                    let preview = straight_preview(&mut app, &ctx, &source, shift);
                    assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), count - 1);
                    frame(
                        &mut app,
                        &ctx,
                        vec![
                            contact(p, egui::TouchPhase::End, 0.),
                            button(p, egui::PointerButton::Primary, false, shift),
                            egui::Event::PointerGone,
                        ],
                        shift,
                    );
                    let e = app.shared.lock().unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").undo.len(), count, "{}", app.message);
                    assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), preview.export_png().unwrap());
                    assert_eq!(
                        e.resident_state().expect("resident fixture").document.layers[0].pixels.get16(0, 0),
                        before.layers[0].pixels.get16(0, 0)
                    );
                    if mask {
                        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels, before.layers[0].pixels);
                    }
                }
                let mut e = app.shared.lock().unwrap();
                let reopened = crate::psd::decode(&crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap()).unwrap();
                assert_eq!(reopened.export_png().unwrap(), e.resident_state().expect("resident fixture").document.export_png().unwrap());
                assert_eq!(reopened.bit_depth, depth);
                for _ in 0..3 {
                    e.undo("human").unwrap();
                }
                assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels, before.layers[0].pixels);
            }
        }
    }
    #[test]
    fn straight_shift_drag_latches_document_axis_and_modifier_changes_keep_one_undo() {
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        for depth in [8, 16] {
            for horizontal in [false, true] {
                let (mut app, ctx, before) = deselection_fixture(depth);
                app.select_tool(Tool::Brush);
                app.radius = 2.;
                frame(&mut app, &ctx, vec![], Default::default());
                let r = app.view_rect.unwrap();
                let scale = r.width() / 32.;
                let pos = |p: [f32; 2]| r.min + Vec2::new(p[0], p[1]) * scale;
                let a = [8., 8.];
                let b = if horizontal { [20., 11.] } else { [11., 20.] };
                frame(
                    &mut app,
                    &ctx,
                    vec![
                        egui::Event::PointerMoved(pos(a)),
                        button(pos(a), egui::PointerButton::Primary, true, shift),
                    ],
                    shift,
                );
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(pos(b))],
                    shift,
                );
                let c = app.stroke_command(&app.points, app.color);
                let (p, _) = crate::brush::points_from_command(&c).unwrap();
                assert_eq!(
                    c["stroke_axis"],
                    if horizontal { "horizontal" } else { "vertical" }
                );
                assert!((p.last().unwrap()[if horizontal { 1 } else { 0 }] - 8.).abs() < 0.0001);
                let swapped = if horizontal { [14., 21.] } else { [24., 14.] };
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(pos(swapped))],
                    shift,
                );
                let c = app.stroke_command(&app.points, app.color);
                assert_eq!(
                    c["stroke_axis"],
                    if horizontal { "horizontal" } else { "vertical" }
                );
                straight_preview(&mut app, &ctx, &before, shift);
                frame(&mut app, &ctx, vec![], Default::default());
                assert!(app.straight.axis.is_none());
                let prefix = app.points.clone();
                let next = [25., 19.];
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(pos(next))],
                    shift,
                );
                let c = app.stroke_command(&app.points, app.color);
                let index = c["stroke_axis_from"].as_u64().unwrap() as usize;
                let (p, _) = crate::brush::points_from_command(&c).unwrap();
                assert_eq!(&p[..index], &prefix[..index]);
                let preview = straight_preview(&mut app, &ctx, &before, shift);
                frame(
                    &mut app,
                    &ctx,
                    vec![button(
                        pos(next),
                        egui::PointerButton::Primary,
                        false,
                        shift,
                    )],
                    shift,
                );
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1, "{}", app.message);
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), preview.export_png().unwrap());
                e.undo("human").unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels, before.layers[0].pixels);
            }
        }
    }
    #[test]
    fn straight_cancelled_shift_click_never_becomes_a_release_dab_after_human_edits_or_escape() {
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        for cancel in ["human", "escape", "alt"] {
            let (mut app, ctx, _) = deselection_fixture(16);
            app.select_tool(Tool::Brush);
            app.radius = 2.;
            frame(&mut app, &ctx, vec![], Default::default());
            let r = app.view_rect.unwrap();
            let scale = r.width() / 32.;
            let a = r.min + Vec2::new(8., 12.) * scale;
            let b = r.min + Vec2::new(20., 12.) * scale;
            click(&mut app, &ctx, a);
            let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
            frame(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(b),
                    button(b, egui::PointerButton::Primary, true, shift),
                ],
                shift,
            );
            assert!(app.straight.line);
            if cancel == "human" {
                app.shared
                    .lock()
                    .unwrap()
                    .edit(
                        "human",
                        &[json!({"op":"layer.update","layer":app.selected,"name":"Human rename"})],
                        None,
                        None,
                        "Rename",
                    )
                    .unwrap();
                frame(&mut app, &ctx, vec![], shift);
            } else if cancel == "alt" {
                let alt = egui::Modifiers {
                    alt: true,
                    ..Default::default()
                };
                frame(&mut app, &ctx, vec![], alt);
                frame(
                    &mut app,
                    &ctx,
                    vec![button(b, egui::PointerButton::Primary, false, alt)],
                    alt,
                );
            } else {
                key(&mut app, &ctx, egui::Key::Escape);
            }
            if cancel != "alt" {
                frame(
                    &mut app,
                    &ctx,
                    vec![button(b, egui::PointerButton::Primary, false, shift)],
                    shift,
                );
            } else {
                frame(&mut app, &ctx, vec![], Default::default());
            }
            let e = app.shared.lock().unwrap();
            assert!(
                e.resident_state().expect("resident fixture").document.export_png().unwrap() == before,
                "Cancelled {cancel} wrote pixels"
            );
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), if cancel == "human" { 2 } else { 1 });
            if cancel == "human" {
                assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].name, "Human rename");
            }
            drop(e);
            if cancel == "alt" {
                app.color = [255, 0, 0, 255];
                click(&mut app, &ctx, b);
                assert_eq!(
                    app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(),
                    2,
                    "Sampling must not leave a stale line gesture"
                );
            }
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
            let source = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
            let target = format!("{};;8192", app.preview_variant());
            app.preview_tx
                .send(Preview {
                    job: app.preview_job,
                    parameter: None,
                    parameter_document: None,
                    parameter_replayed: false,
                    coarse: false,
                    error: None,
                    live: String::new(),
                    doc_id: source.id.clone(),
                    revision: source.revision,
                    w: 1,
                    h: 1,
                    bytes: vec![0, 0, 0, 255],
                    patch: None,
                    selection: None,
                    selection_polygon: None,
                    selection_coverage: None,
                    text: None,
                    target,
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
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.selection = Some([2, 2, 8, 8]);
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.layers[0]
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.selection, Some([2, 2, 8, 8]));
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers.len(), 2);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        assert!(app.tool == Tool::Selection);
    }
    #[test]
    fn refinement_previews_real_cutout_without_history_and_cancels_on_source_changes() {
        let (mut app, ctx) = small_fixture();
        {
            let mut e = app.shared.lock().unwrap();
            for y in 0..16 {
                for x in 0..16 {
                    e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set(x, y, [233, 84, 32, 255]);
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
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let count = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
        app.open_refinement(&doc, None);
        app.refinement.as_mut().unwrap().values[1] = 2.;
        frame(&mut app, &ctx, vec![], Default::default());
        assert_eq!(app.transient.len(), 1);
        let preview = crate::engine::Engine::preview_edits(doc.clone(), &app.transient).unwrap();
        let (w, h, mut pixels, _) = preview.preview(None, 64, None, false).unwrap();
        let original = pixels.clone();
        crate::selection::display::apply(&preview, w, h, &mut pixels, "cutout");
        assert!(pixels != original, "Cutout must alter rendered alpha");
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), count);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, doc.revision);
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
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), count + 1);
    }
}
