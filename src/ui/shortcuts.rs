use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const TOOLS: &[(&str, &str, Tool)] = &[
    ("Brush", "B", Tool::Brush),
    ("Pen", "P", Tool::Pen),
    ("Smudge", "U", Tool::Smudge),
    ("Clone", "C", Tool::Clone),
    ("Heal", "J", Tool::Heal),
    ("Liquify", "L", Tool::Liquify),
    ("Eraser", "D", Tool::Eraser),
    ("Deselect", "Q", Tool::None),
    ("Move", "W", Tool::Move),
    ("Rotate artwork", "E", Tool::Rotate),
    ("Scale artwork", "R", Tool::Scale),
    ("Fill", "G", Tool::Fill),
    ("Picker", "I", Tool::Picker),
    ("Pan", "H", Tool::Pan),
    ("Selection", "M", Tool::Selection),
    ("Magic wand", "K", Tool::MagicWand),
    ("Text", "T", Tool::Text),
];
const TEXT_UNASSIGNED: &str =
    "Text shortcut is unassigned. Choose a key in Keyboard and pen shortcuts.";
pub(super) const DUPLICATE_TRANSFORM: &str = "Duplicate with Transform";
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    key: String,
    shift: bool,
}
impl Binding {
    fn key(&self) -> Option<egui::Key> {
        egui::Key::from_name(&self.key)
    }
    fn pressed(&self, input: &egui::InputState) -> bool {
        // egui-winit resolves logical keys, with one physical fallback for
        // non-Latin layouts. Matching both here would dispatch two actions.
        input.events.iter().any(|event| self.matches(event))
    }
    fn matches(&self, event: &egui::Event) -> bool {
        matches!(event,
            egui::Event::Key { key, pressed: true, modifiers, .. }
                if !modifiers.command && !modifiers.ctrl && !modifiers.alt
                    && modifiers.shift == self.shift && self.key() == Some(*key))
    }
    fn fresh_press(&self, event: &egui::Event) -> bool {
        !matches!(event, egui::Event::Key { repeat: true, .. }) && self.matches(event)
    }
    fn held(&self, input: &egui::InputState) -> bool {
        !input.modifiers.command
            && !input.modifiers.ctrl
            && !input.modifiers.alt
            && input.modifiers.shift == self.shift
            && self.key().is_some_and(|key| input.key_down(key))
    }
    fn label(&self) -> String {
        if self.key.is_empty() {
            return "Unassigned".into();
        }
        format!("{}{}", if self.shift { "Shift+" } else { "" }, self.key)
    }
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preferences {
    version: u32,
    bindings: BTreeMap<String, Binding>,
    buttons: [String; 2],
}
impl Default for Preferences {
    fn default() -> Self {
        let mut bindings = BTreeMap::new();
        for (action, key, _) in TOOLS {
            bindings.insert(
                (*action).into(),
                Binding {
                    key: (*key).into(),
                    shift: false,
                },
            );
        }
        for (action, key, shift) in [
            (DUPLICATE_TRANSFORM, "D", true),
            ("Frame canvas", "F", false),
            ("Swap colors", "X", false),
            ("Smaller size", "[", false),
            ("Larger size", "]", false),
            ("Size / hardness gesture", "S", false),
            ("Rotate view left", "E", true),
            ("Rotate view right", "R", true),
            ("Mirror view horizontally", "V", true),
            ("Mirror view vertically", "N", true),
            ("Reset view", "F", true),
        ] {
            bindings.insert(
                action.into(),
                Binding {
                    key: key.into(),
                    shift,
                },
            );
        }
        Self {
            version: 3,
            bindings,
            buttons: ["Eraser".into(), "Pan".into()],
        }
    }
}
pub(super) struct Shortcuts {
    preferences: Preferences,
    path: Option<PathBuf>,
    original: Option<Vec<u8>>,
    protected: bool,
    pub open: bool,
    pub message: String,
    temporary: Option<Tool>,
    recording: Option<Recording>,
}

struct Recording {
    action: String,
    button: egui::Id,
}
pub(crate) fn validate_file(path: &Path) -> Result<(), String> {
    if let Some(bytes) = crate::tool_settings::read_owned(path)? {
        Shortcuts::decode(&bytes)?;
    }
    Ok(())
}
impl Shortcuts {
    pub fn load(state: &Path) -> Self {
        let mut this = Self {
            preferences: Preferences::default(),
            path: (!state.as_os_str().is_empty()).then(|| state.join("shortcuts.json")),
            original: None,
            protected: false,
            open: false,
            message: String::new(),
            temporary: None,
            recording: None,
        };
        if let Some(path) = &this.path {
            let result = (|| {
                this.original = crate::tool_settings::read_owned(path)?;
                if let Some(bytes) = &this.original {
                    let prefs = Self::decode(bytes)?;
                    if crate::tool_settings::read_owned(path)? != this.original {
                        return Err("Shortcuts changed while loading".into());
                    }
                    this.preferences = prefs;
                    if this.preferences.bindings["Text"].key.is_empty() {
                        this.message = TEXT_UNASSIGNED.into();
                    }
                }
                Ok::<_, String>(())
            })();
            if let Err(error) = result {
                this.protected = true;
                this.message = format!("Protected shortcut file · {error}");
            }
        }
        this
    }
    pub(super) fn allowed(key: egui::Key) -> bool {
        matches!(
            key,
            egui::Key::A
                | egui::Key::B
                | egui::Key::C
                | egui::Key::D
                | egui::Key::E
                | egui::Key::F
                | egui::Key::G
                | egui::Key::H
                | egui::Key::I
                | egui::Key::J
                | egui::Key::K
                | egui::Key::L
                | egui::Key::M
                | egui::Key::N
                | egui::Key::O
                | egui::Key::P
                | egui::Key::Q
                | egui::Key::R
                | egui::Key::S
                | egui::Key::T
                | egui::Key::U
                | egui::Key::V
                | egui::Key::W
                | egui::Key::X
                | egui::Key::Y
                | egui::Key::Z
                | egui::Key::Num0
                | egui::Key::Num1
                | egui::Key::Num2
                | egui::Key::Num3
                | egui::Key::Num4
                | egui::Key::Num5
                | egui::Key::Num6
                | egui::Key::Num7
                | egui::Key::Num8
                | egui::Key::Num9
                | egui::Key::OpenBracket
                | egui::Key::CloseBracket
        )
    }
    fn decode(bytes: &[u8]) -> Result<Preferences, String> {
        let mut prefs: Preferences =
            serde_json::from_slice(bytes).map_err(|e| format!("Unreadable shortcuts: {e}"))?;
        if matches!(prefs.version, 1 | 2) {
            let defaults = Preferences::default();
            let added = |action: &str| action == DUPLICATE_TRANSFORM || (prefs.version == 1 && action == "Text");
            if prefs.bindings.keys().ne(defaults.bindings.keys().filter(|action| !added(action))) {
                return Err("Unsupported shortcut preferences".into());
            }
            for action in ["Text", DUPLICATE_TRANSFORM].into_iter().filter(|action| added(action)) {
                let mut binding = defaults.bindings[action].clone();
                if prefs.bindings.values().any(|existing| *existing == binding) {
                    binding.key.clear();
                    binding.shift = false;
                }
                prefs.bindings.insert(action.into(), binding);
            }
            prefs.version = defaults.version;
        }
        Self::validate(&prefs)?;
        Ok(prefs)
    }
    fn validate(prefs: &Preferences) -> Result<(), String> {
        let defaults = Preferences::default();
        if prefs.version != defaults.version || prefs.bindings.keys().ne(defaults.bindings.keys()) {
            return Err("Unsupported shortcut preferences".into());
        }
        let mut used = BTreeMap::new();
        for (action, binding) in &prefs.bindings {
            if matches!(action.as_str(), "Text" | DUPLICATE_TRANSFORM) && binding.key.is_empty() && !binding.shift {
                continue;
            }
            if !binding
                .key()
                .is_some_and(|key| Self::allowed(key) && binding.key == key.symbol_or_name())
            {
                return Err(format!("Unsupported shortcut for {action}"));
            }
            if let Some(other) = used.insert(binding.label(), action) {
                return Err(format!(
                    "{} is already assigned to {other}",
                    binding.label()
                ));
            }
        }
        for (action, key) in [("Smaller size", "["), ("Larger size", "]")] {
            if prefs.bindings[action] == defaults.bindings[action]
                && prefs
                    .bindings
                    .iter()
                    .any(|(other, binding)| other != action && binding.shift && binding.key == key)
            {
                return Err(format!(
                    "Shift+{key} is the default size alias; reassign {action} first"
                ));
            }
        }
        if prefs
            .buttons
            .iter()
            .any(|v| !matches!(v.as_str(), "None" | "Eraser" | "Pan"))
        {
            return Err("Unsupported pen button action".into());
        }
        Ok(())
    }
    fn apply(&mut self, prefs: Preferences) -> Result<(), String> {
        Self::validate(&prefs)?;
        if self.protected {
            return Err(
                "Shortcut file remains protected; repair or restore it before saving".into(),
            );
        }
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec_pretty(&prefs).map_err(|e| e.to_string())?;
            crate::tool_settings::write_owned(
                path,
                &bytes,
                self.original.as_deref(),
                "Shortcuts changed on disk; restart before saving",
            )?;
            self.original = Some(bytes);
        }
        self.preferences = prefs;
        Ok(())
    }
    fn save_preferences(&mut self, candidate: Preferences) {
        match self.apply(candidate) {
            Ok(()) => {
                self.recording = None;
                self.message = "Preferences saved".into();
            }
            Err(error) => self.message = error,
        }
    }
    pub(super) fn record_key(&mut self, ctx: &egui::Context) {
        let Some(recording) = &self.recording else { return };
        let escape = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        if !self.open
            || ctx.input(|input| {
                !input.focused || input.pointer.any_pressed() || input.key_pressed(egui::Key::Tab)
                    || input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))
            })
            || (!escape && !ctx.memory(|memory| memory.has_focus(recording.button)))
        {
            self.recording = None;
            return;
        }
        // Route the recording before canvas commands or text drafts see these events.
        let pressed = ctx.input_mut(|input| {
            let mut pressed = None;
            input.events.retain(|event| match event {
                egui::Event::Key { key, pressed: true, modifiers, repeat, .. } => {
                    if !repeat {
                        pressed.get_or_insert((*key, *modifiers));
                    }
                    false
                }
                egui::Event::Text(_)
                | egui::Event::Ime(_)
                | egui::Event::Copy
                | egui::Event::Cut
                | egui::Event::Paste(_) => false,
                _ => true,
            });
            pressed
        });
        if escape {
            self.recording = None;
            self.message.clear();
        } else if let Some((key, modifiers)) = pressed {
            if !Self::allowed(key) || modifiers.ctrl || modifiers.command || modifiers.alt {
                self.message = "Use a letter, digit or bracket, with optional Shift".into();
                return;
            }
            let mut candidate = self.preferences.clone();
            candidate.bindings.insert(recording.action.clone(), Binding {
                key: key.symbol_or_name().into(),
                shift: modifiers.shift,
            });
            self.save_preferences(candidate);
        }
    }
    pub(super) fn hint(&self, action: &str, label: &str) -> String {
        match self.preferences.bindings.get(action) {
            Some(binding) => format!("{label} · {}", binding.label()),
            None => label.to_owned(),
        }
    }

    pub(super) fn pressed(&self, action: &str, input: &egui::InputState) -> bool {
        if self.preferences.bindings[action].pressed(input) {
            return true;
        }
        let (key, text) = match action {
            "Smaller size" => (egui::Key::OpenBracket, "{"),
            "Larger size" => (egui::Key::CloseBracket, "}"),
            _ => return false,
        };
        let pressed_key = input.events.iter().any(|event| {
            matches!(event,
            egui::Event::Key { key: k, pressed: true, .. } if *k == key)
        });
        self.preferences.bindings[action].key == key.symbol_or_name()
            && !self.preferences.bindings[action].shift
            && input.events.iter().any(|event| match event {
                egui::Event::Key {
                    key: k,
                    pressed: true,
                    modifiers,
                    ..
                } => *k == key && !modifiers.command && !modifiers.ctrl && !modifiers.alt,
                egui::Event::Text(t) => {
                    !pressed_key
                        && t == text
                        && !input.modifiers.command
                        && !input.modifiers.ctrl
                        && !input.modifiers.alt
                }
                _ => false,
            })
    }
    pub(super) fn pressed_once(&self, action: &str, input: &egui::InputState) -> bool {
        input.events.iter().any(|event| self.preferences.bindings[action].fresh_press(event))
    }
    pub fn size_held(&self, input: &egui::InputState) -> bool {
        self.preferences.bindings["Size / hardness gesture"].held(input)
    }
    pub(super) fn duplicate_transform_shortcut(&self) -> String {
        let binding = &self.preferences.bindings[DUPLICATE_TRANSFORM];
        binding.key().map_or_else(String::new, |_| binding.label())
    }
    pub(super) fn duplicate_transform_event(&self, event: &egui::Event) -> bool {
        self.preferences.bindings[DUPLICATE_TRANSFORM].fresh_press(event)
    }
    pub(super) fn selection_change_event(&self, event: &egui::Event) -> bool {
        if let egui::Event::Key { key, pressed: true, modifiers, .. } = event {
            if modifiers.command || modifiers.ctrl {
                return match key {
                    egui::Key::A | egui::Key::Z => true,
                    egui::Key::D => modifiers.shift != modifiers.alt,
                    egui::Key::I => modifiers.shift && !modifiers.alt,
                    _ => false,
                };
            }
        }
        self.temporary.is_none() && self.preferences.bindings["Deselect"].fresh_press(event)
    }
    pub(super) fn selection_dependent_event(&self, event: &egui::Event) -> bool {
        if clipboard_shortcut(event).is_some() || self.duplicate_transform_event(event) {
            return true;
        }
        match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let is_key = |wanted| *key == wanted;
                if (modifiers.command || modifiers.ctrl) && !modifiers.alt && !modifiers.shift
                    && is_key(egui::Key::T)
                {
                    return true;
                }
                if is_key(egui::Key::Delete) && !modifiers.any() {
                    return true;
                }
                if modifiers.alt
                    && !modifiers.command
                    && [egui::Key::B, egui::Key::Backspace].into_iter().any(is_key)
                {
                    return true;
                }
                ["Move", "Rotate artwork", "Scale artwork"]
                    .into_iter()
                    .any(|action| self.preferences.bindings[action].fresh_press(event))
            }
            _ => false,
        }
    }
    pub fn tool_label(&self, tool: Tool) -> String {
        TOOLS
            .iter()
            .find(|(_, _, t)| *t == tool)
            .map(|(action, _, _)| self.preferences.bindings[*action].label())
            .unwrap_or_default()
    }
}
impl PeerBrush {
    pub(super) fn tool_caption(&self, tool: Tool) -> String {
        if tool == Tool::Eraser {
            let command = if cfg!(target_os = "macos") {
                "Cmd"
            } else {
                "Ctrl"
            };
            return format!("Eraser · {command}+E / {}", self.shortcuts.tool_label(tool));
        }
        let action = TOOLS
            .iter()
            .find(|(_, _, t)| *t == tool)
            .map(|(action, key, _)| (*action, *key));
        if action.is_none_or(|(action, key)| {
            let binding = &self.shortcuts.preferences.bindings[action];
            binding.key == key && !binding.shift
        }) {
            return tool.label().into();
        }
        format!(
            "{} · {}",
            tool.label().split(" · ").next().unwrap(),
            self.shortcuts.tool_label(tool)
        )
    }
    pub(super) fn temporary_pen_tool(&mut self, ctx: &egui::Context) {
        let wanted = ctx.input(|i| {
            if !i.focused {
                return None;
            }
            [
                (egui::PointerButton::Extra1, 0),
                (egui::PointerButton::Extra2, 1),
            ]
            .into_iter()
            .find_map(|(button, index)| {
                if !i.pointer.button_down(button) {
                    return None;
                }
                match self.shortcuts.preferences.buttons[index].as_str() {
                    "Eraser" => Some(Tool::Eraser),
                    "Pan" => Some(Tool::Pan),
                    _ => None,
                }
            })
        });
        if let Some(tool) = wanted {
            if self.shortcuts.temporary.is_none()
                && self.points.is_empty()
                && self.drag_start.is_none()
                && self.gizmo_handle == 0
                && !ctx.wants_keyboard_input()
            {
                self.shortcuts.temporary = Some(self.tool);
                self.select_tool(tool);
            }
        } else if let Some(prior) = self.shortcuts.temporary {
            if self.points.is_empty()
                && self.drag_start.is_none()
                && self.gizmo_handle == 0
                && !ctx.input(|i| i.pointer.primary_down())
                || !ctx.input(|i| i.focused)
            {
                self.shortcuts.temporary = None;
                self.select_tool(prior);
            }
        }
    }
    pub(super) fn configured_shortcuts<'a>(
        &mut self,
        input: &egui::InputState,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        if self.shortcuts.open || self.shortcuts.temporary.is_some() {
            return;
        }
        if self.tool != Tool::Eraser && input.events.iter().any(|event| matches!(event,
            egui::Event::Key { key: egui::Key::E, pressed: true, repeat: false, modifiers, .. }
                if (modifiers.command || modifiers.ctrl) && !modifiers.shift && !modifiers.alt))
        {
            self.select_tool(Tool::Eraser);
        }
        for (action, _, tool) in TOOLS {
            if *tool == Tool::Eraser && self.tool == Tool::Eraser {
                continue;
            }
            if self.shortcuts.pressed_once(action, input) {
                if *tool == Tool::None {
                    self.deselect_area(doc, true);
                } else {
                    self.select_tool(*tool);
                }
            }
        }
        if self.shortcuts.pressed("Swap colors", input) {
            self.swap_colors();
        }
        if matches!(
            self.tool,
            Tool::Brush | Tool::Eraser | Tool::Smudge | Tool::Clone | Tool::Heal | Tool::Liquify
        ) {
            let smaller = self.shortcuts.pressed("Smaller size", input);
            let larger = self.shortcuts.pressed("Larger size", input);
            if smaller || larger {
                self.radius = (self.radius * if larger { 1.1 } else { 1. / 1.1 }).clamp(0.5, 512.);
                self.message = format!("Brush size {} px", (self.radius * 2.).round()).into();
            }
        }
        if !self.canvas_view_gesture_active() {
            if self.shortcuts.pressed("Frame canvas", input) {
                self.frame_pending = true;
            }
            let mut orientation = self.view;
            for (action, delta) in [("Rotate view left", -15.), ("Rotate view right", 15.)] {
                if self.shortcuts.pressed(action, input) {
                    orientation.angle = (orientation.angle + delta + 180.).rem_euclid(360.) - 180.;
                }
            }
            if self.shortcuts.pressed("Mirror view horizontally", input) {
                orientation.mirror_h = !orientation.mirror_h;
            }
            if self.shortcuts.pressed("Mirror view vertically", input) {
                orientation.mirror_v = !orientation.mirror_v;
            }
            self.change_canvas_orientation(doc, orientation);
            if self.shortcuts.pressed("Reset view", input) {
                self.reset_canvas_view();
            }
        }
    }
    pub(super) fn shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.shortcuts.open {
            return;
        }
        let shortcuts = &mut self.shortcuts;
        let mut open = true;
        egui::Window::new("Keyboard and pen shortcuts")
            .open(&mut open)
            .resizable(false)
            .default_width(420.)
            .show(ctx, |ui| {
                ui.label("Click a shortcut and press its key, with optional Shift. Escape cancels.");
                ui.label(RichText::new("Use a letter, digit or bracket. Ctrl/Command, Alt, Space and editing keys retain their standard actions.").small().color(MUTED));
                let mut candidate = shortcuts.preferences.clone();
                egui::ScrollArea::vertical().max_height(420.).show(ui, |ui| {
                    for (action, binding) in &mut candidate.bindings {
                        ui.push_id(action, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_sized(Vec2::new(200., 20.), egui::Label::new(action));
                                let active = shortcuts.recording.as_ref().is_some_and(|r| r.action == *action);
                                let label = if active { "Press shortcut…".into() } else { binding.label() };
                                let response = ui.add_sized(
                                    [116., 22.],
                                    egui::Button::new(&label).selected(active),
                                ).on_hover_text(format!("Record shortcut for {action}"));
                                response.widget_info(|| egui::WidgetInfo::selected(
                                    egui::WidgetType::Button, ui.is_enabled(), active,
                                    format!("{action} shortcut, {label}"),
                                ));
                                if response.clicked() {
                                    response.request_focus();
                                    shortcuts.recording = Some(Recording {
                                        action: action.clone(),
                                        button: response.id,
                                    });
                                    shortcuts.message.clear();
                                }
                                shortcut_key_choices(ui, action, binding);
                            });
                        });
                    }
                });
                for (index, button) in candidate.buttons.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("Pen extra button {}", index + 1));
                        if ui.selectable_label(button == "None", "None").clicked() {
                            *button = "None".into();
                        }
                        for (role, icon) in [("Eraser", Icon::Eraser), ("Pan", Icon::Pan)] {
                            let label = format!("Pen extra button {} · Temporary {role}", index + 1);
                            if icons::selectable(ui, icon, button == role, &label).clicked() {
                                *button = role.into();
                            }
                        }
                    });
                }
                ui.label(RichText::new("Configure the tablet driver to send extra mouse buttons. Tool restoration waits for a stroke to finish. Focus loss restores it immediately.").small().color(MUTED));
                let reset = ui.button("Reset shortcuts and buttons").clicked();
                if reset {
                    candidate = Preferences::default();
                }
                if candidate != shortcuts.preferences {
                    shortcuts.save_preferences(candidate);
                }
                ui.label("Ctrl/Cmd+E selects Eraser. Ctrl/Cmd+Shift+G merges layers.");
                if shortcuts.preferences.bindings["Text"].key.is_empty() {
                    ui.label(TEXT_UNASSIGNED);
                }
                if !shortcuts.message.is_empty() && shortcuts.message != TEXT_UNASSIGNED {
                    ui.label(&shortcuts.message);
                }
            });
        shortcuts.open = open;
        if !open {
            shortcuts.recording = None;
        }
    }
}

fn shortcut_key_choices(ui: &mut egui::Ui, action: &str, binding: &mut Binding) {
    let response = egui::ComboBox::from_id_salt("key list")
        .width(60.)
        .height(235.)
        .selected_text("Keys")
        .show_ui(ui, |ui| {
            if matches!(action, "Text" | DUPLICATE_TRANSFORM)
                && ui.selectable_label(binding.key.is_empty(), "Unassigned").clicked()
            {
                binding.key.clear();
                binding.shift = false;
            }
            for key in egui::Key::ALL.iter().copied().filter(|&key| Shortcuts::allowed(key)) {
                ui.horizontal(|ui| {
                    for shift in [false, true] {
                        let option = Binding { key: key.symbol_or_name().into(), shift };
                        let label = option.label();
                        ui.selectable_value(binding, option, label);
                    }
                });
            }
        }).response.on_hover_text(format!("Choose a shortcut for {action}"));
    response.widget_info(|| egui::WidgetInfo::labeled(
        egui::WidgetType::ComboBox, ui.is_enabled(), format!("Choose a shortcut for {action}"),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn legacy_preferences() -> Preferences {
        let mut prefs = Preferences::default();
        prefs.version = 1;
        prefs.bindings.remove("Text");
        prefs.bindings.remove(DUPLICATE_TRANSFORM);
        prefs
    }
    #[test]
    fn legacy_shortcuts_migrate_in_memory_with_custom_bindings_buttons_and_original_bytes() {
        let root =
            std::env::temp_dir().join(format!("peerbrush-shortcuts-{}", crate::engine::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("shortcuts.json");
        let mut legacy = legacy_preferences();
        legacy.bindings.get_mut("Brush").unwrap().key = "O".into();
        legacy.bindings.get_mut("Brush").unwrap().shift = true;
        legacy.bindings.get_mut("Smudge").unwrap().key = "Y".into();
        legacy.buttons = ["None".into(), "Eraser".into()];
        let bytes = format!("{}\n  ", serde_json::to_string(&legacy).unwrap()).into_bytes();
        std::fs::write(&path, &bytes).unwrap();
        validate_file(&path).unwrap();
        let mut shortcuts = Shortcuts::load(&root);
        assert!(!shortcuts.protected);
        assert_eq!(shortcuts.preferences.version, 3);
        assert_eq!(shortcuts.preferences.bindings["Text"].label(), "T");
        assert_eq!(shortcuts.duplicate_transform_shortcut(), "Shift+D");
        for (action, binding) in &legacy.bindings {
            assert!(shortcuts.preferences.bindings[action] == *binding);
        }
        assert_eq!(shortcuts.preferences.buttons, legacy.buttons);
        assert_eq!(shortcuts.original.as_deref(), Some(bytes.as_slice()));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let locations = crate::settings::Locations {
            config: root.join("per-user/config"),
            data: root.join("per-user/data"),
            cache: root.join("per-user/cache"),
        };
        let profile = crate::settings::Profile::migrate(locations, &root).unwrap();
        let migrated = profile.locations.profile();
        assert_eq!(
            std::fs::read(migrated.join("shortcuts.json")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read(migrated.join("migration-backup/shortcuts.json")).unwrap(),
            bytes
        );
        assert!(Shortcuts::load(&migrated).preferences == shortcuts.preferences);
        drop(profile);
        let mut edited = shortcuts.preferences.clone();
        edited.buttons[0] = "Pan".into();
        shortcuts.apply(edited.clone()).unwrap();
        let stored = std::fs::read(&path).unwrap();
        assert_ne!(stored, bytes);
        assert_eq!(
            serde_json::from_slice::<Value>(&stored).unwrap()["version"],
            3
        );
        assert!(Shortcuts::load(&root).preferences == edited);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn previous_shortcuts_keep_shift_d_conflicts_and_allow_a_new_duplicate_binding() {
        let mut previous = Preferences::default();
        previous.version = 2;
        previous.bindings.remove(DUPLICATE_TRANSFORM);
        let brush = previous.bindings.get_mut("Brush").unwrap();
        *brush = Binding { key: "D".into(), shift: true };
        let mut shortcuts = Shortcuts::load(Path::new(""));
        shortcuts.preferences = Shortcuts::decode(&serde_json::to_vec(&previous).unwrap()).unwrap();
        assert!(shortcuts.preferences.bindings["Brush"] == previous.bindings["Brush"]);
        assert_eq!(shortcuts.duplicate_transform_shortcut(), "");
        let press = |key| egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT,
        };
        assert!(!shortcuts.duplicate_transform_event(&press(egui::Key::D)));
        let mut candidate = shortcuts.preferences.clone();
        candidate.bindings.insert(DUPLICATE_TRANSFORM.into(), Binding { key: "Y".into(), shift: true });
        shortcuts.apply(candidate).unwrap();
        assert_eq!(shortcuts.duplicate_transform_shortcut(), "Shift+Y");
        assert!(shortcuts.selection_dependent_event(&press(egui::Key::Y)));
        assert!(!shortcuts.duplicate_transform_event(&press(egui::Key::D)));
    }
    #[test]
    fn legacy_t_conflicts_leave_text_unassigned_without_changing_existing_chords() {
        let root =
            std::env::temp_dir().join(format!("peerbrush-shortcuts-{}", crate::engine::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("shortcuts.json");
        for shift in [false, true] {
            let mut legacy = legacy_preferences();
            legacy.bindings.get_mut("Brush").unwrap().key = "T".into();
            legacy.bindings.get_mut("Brush").unwrap().shift = shift;
            let bytes = serde_json::to_vec(&legacy).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            validate_file(&path).unwrap();
            let mut shortcuts = Shortcuts::load(&root);
            assert!(!shortcuts.protected);
            assert!(shortcuts.preferences.bindings["Brush"] == legacy.bindings["Brush"]);
            let text = &shortcuts.preferences.bindings["Text"];
            assert_eq!(text.label(), if shift { "T" } else { "Unassigned" });
            assert!(!text.shift);
            assert_eq!(text.key(), shift.then_some(egui::Key::T));
            if !shift {
                assert_eq!(shortcuts.message, TEXT_UNASSIGNED);
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            let mut edited = shortcuts.preferences.clone();
            edited.bindings.get_mut("Text").unwrap().key = "O".into();
            shortcuts.apply(edited).unwrap();
            let reloaded = Shortcuts::load(&root);
            assert_eq!(reloaded.preferences.bindings["Text"].label(), "O");
            assert!(reloaded.preferences.bindings["Brush"] == legacy.bindings["Brush"]);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn invalid_legacy_or_current_shortcuts_remain_protected_and_byte_exact() {
        let cases: &[(&str, fn(&mut Preferences))] = &[
            ("unknown version", |p| p.version = 4),
            ("zero version", |p| p.version = 0),
            ("legacy with Text", |p| {
                p.bindings.insert(
                    "Text".into(),
                    Preferences::default().bindings["Text"].clone(),
                );
            }),
            ("version 2 missing Text", |p| p.version = 2),
            ("current missing Duplicate with Transform", |p| {
                *p = Preferences::default();
                p.bindings.remove(DUPLICATE_TRANSFORM);
            }),
            ("missing old action", |p| {
                p.bindings.remove("Brush");
            }),
            ("unknown action", |p| {
                p.bindings.insert(
                    "Unknown".into(),
                    Binding {
                        key: "O".into(),
                        shift: false,
                    },
                );
            }),
            ("duplicate chord", |p| {
                p.bindings.get_mut("Eraser").unwrap().key = "B".into()
            }),
            ("noncanonical key", |p| {
                p.bindings.get_mut("Brush").unwrap().key = "b".into()
            }),
            ("empty old action", |p| {
                p.bindings.get_mut("Brush").unwrap().key.clear()
            }),
            ("reserved key", |p| {
                p.bindings.get_mut("Brush").unwrap().key = "Space".into()
            }),
            ("invalid button", |p| p.buttons[0] = "Text".into()),
            ("size alias conflict", |p| {
                let binding = p.bindings.get_mut("Brush").unwrap();
                binding.key = "[".into();
                binding.shift = true;
            }),
            ("shifted unassigned Text", |p| {
                *p = Preferences::default();
                let binding = p.bindings.get_mut("Text").unwrap();
                binding.key.clear();
                binding.shift = true;
            }),
            ("empty current non-Text action", |p| {
                *p = Preferences::default();
                p.bindings.get_mut("Brush").unwrap().key.clear();
            }),
        ];
        let root =
            std::env::temp_dir().join(format!("peerbrush-shortcuts-{}", crate::engine::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("shortcuts.json");
        for (name, change) in cases {
            let mut prefs = legacy_preferences();
            change(&mut prefs);
            let bytes = serde_json::to_vec(&prefs).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(validate_file(&path).is_err(), "{name}");
            let mut shortcuts = Shortcuts::load(&root);
            assert!(shortcuts.protected, "{name}");
            assert!(shortcuts.apply(Preferences::default()).is_err(), "{name}");
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn migrated_shortcuts_preserve_external_bytes_and_existing_save_leases() {
        let root =
            std::env::temp_dir().join(format!("peerbrush-shortcuts-{}", crate::engine::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("shortcuts.json");
        let bytes = serde_json::to_vec(&legacy_preferences()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let mut shortcuts = Shortcuts::load(&root);
        let before = shortcuts.preferences.clone();
        let mut edited = before.clone();
        edited.buttons[0] = "None".into();
        let lease = crate::file_safety::Lease::acquire(&path).unwrap();
        assert!(shortcuts.apply(edited.clone()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(shortcuts.preferences == before);
        drop(lease);
        let external = serde_json::to_vec_pretty(&before).unwrap();
        std::fs::write(&path, &external).unwrap();
        validate_file(&path).unwrap();
        assert!(shortcuts
            .apply(edited)
            .unwrap_err()
            .contains("changed on disk"));
        assert_eq!(std::fs::read(&path).unwrap(), external);
        assert!(shortcuts.preferences == before);
        assert_eq!(shortcuts.original.as_deref(), Some(bytes.as_slice()));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn conflicts_reset_protected_files_and_shortcut_persistence_are_atomic() {
        let root =
            std::env::temp_dir().join(format!("peerbrush-shortcuts-{}", crate::engine::id()));
        std::fs::create_dir(&root).unwrap();
        let mut shortcuts = Shortcuts::load(&root);
        assert!(!shortcuts.protected);
        let mut remapped = shortcuts.preferences.clone();
        remapped.bindings.get_mut("Brush").unwrap().key = "O".into();
        shortcuts.apply(remapped.clone()).unwrap();
        let bytes = std::fs::read(root.join("shortcuts.json")).unwrap();
        assert!(Shortcuts::load(&root).preferences == remapped);
        let user = root.join("per-user");
        let locations = crate::settings::Locations {
            config: user.join("config"),
            data: user.join("data"),
            cache: user.join("cache"),
        };
        let profile = crate::settings::Profile::migrate(locations, &root).unwrap();
        assert!(Shortcuts::load(&profile.locations.profile()).preferences == remapped);
        assert_eq!(
            std::fs::read(
                profile
                    .locations
                    .profile()
                    .join("migration-backup/shortcuts.json")
            )
            .unwrap(),
            bytes
        );
        drop(profile);
        let mut conflict = remapped.clone();
        conflict.bindings.get_mut("Brush").unwrap().key = "D".into();
        assert!(shortcuts.apply(conflict).is_err());
        assert_eq!(std::fs::read(root.join("shortcuts.json")).unwrap(), bytes);
        assert!(shortcuts.preferences == remapped);
        shortcuts.apply(Preferences::default()).unwrap();
        assert!(Shortcuts::load(&root).preferences == Preferences::default());
        let external = b"{\"unsupported\":true}";
        std::fs::write(root.join("shortcuts.json"), external).unwrap();
        assert!(shortcuts.apply(remapped).is_err());
        assert_eq!(
            std::fs::read(root.join("shortcuts.json")).unwrap(),
            external
        );
        let mut protected = Shortcuts::load(&root);
        assert!(protected.protected);
        assert!(protected.apply(Preferences::default()).is_err());
        assert_eq!(
            std::fs::read(root.join("shortcuts.json")).unwrap(),
            external
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    fn frame(
        app: &mut PeerBrush, ctx: &egui::Context, events: Vec<egui::Event>, focused: bool,
    ) -> egui::FullOutput {
        let time = ctx.input(|i| i.time) + 0.1;
        ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                events,
                focused,
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        )
    }
    fn key(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: Default::default(),
        }
    }
    #[test]
    fn shortcut_button_records_one_chord_and_preserves_the_binding_on_conflict_or_cancel() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        app.shortcuts = Shortcuts::load(Path::new(""));
        app.shortcuts.open = true;
        app.select_tool(Tool::Picker);
        let click = |app: &mut PeerBrush, chord: &str| {
            frame(app, &ctx, vec![], true);
            let output = frame(app, &ctx, vec![], true);
            let labels: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text.galley.text(), text.pos + text.galley.size() * 0.5)),
                _ => None,
            }).collect();
            let row = labels.iter().find(|(label, _)| *label == "Brush").unwrap().1;
            let pos = labels.iter().find(|(label, pos)| {
                *label == chord && (row.y - pos.y).abs() < 9. && pos.x > row.x
            }).unwrap().1;
            for pressed in [true, false] {
                frame(app, &ctx, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                    pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
                }], true);
            }
        };
        click(&mut app, "B");
        assert_eq!(app.shortcuts.recording.as_ref().unwrap().action, "Brush");
        let before = super::super::workspace_tests::source(&app);
        frame(&mut app, &ctx, vec![key(egui::Key::D, true)], true);
        assert!(app.shortcuts.message.contains("already assigned"));
        assert_eq!(app.shortcuts.preferences.bindings["Brush"].label(), "B");
        let shifted = egui::Event::Key {
            key: egui::Key::O, physical_key: Some(egui::Key::B), pressed: true, repeat: false,
            modifiers: egui::Modifiers { shift: true, ..Default::default() },
        };
        frame(&mut app, &ctx, vec![key(egui::Key::D, false), shifted, egui::Event::Text("O".into())], true);
        assert_eq!(app.shortcuts.preferences.bindings["Brush"].label(), "Shift+O");
        assert!(app.shortcuts.recording.is_none());
        click(&mut app, "Shift+O");
        frame(&mut app, &ctx, vec![key(egui::Key::Escape, true)], true);
        assert!(app.shortcuts.recording.is_none());
        assert_eq!(app.shortcuts.preferences.bindings["Brush"].label(), "Shift+O");
        assert!(app.tool == Tool::Picker);
        assert_eq!(super::super::workspace_tests::source(&app), before);
    }

    #[test]
    fn eraser_command_alias_preserves_custom_tool_binding() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.shortcuts = Shortcuts::load(Path::new(""));
        let mut custom = app.shortcuts.preferences.clone();
        custom.bindings.get_mut("Eraser").unwrap().key = "O".into();
        app.shortcuts.apply(custom.clone()).unwrap();
        app.select_tool(Tool::Brush);
        frame(&mut app, &ctx, vec![key(egui::Key::O, true)], true);
        assert!(app.tool == Tool::Eraser);
        frame(&mut app, &ctx, vec![key(egui::Key::O, false)], true);
        app.select_tool(Tool::Brush);
        let mut command = key(egui::Key::E, true);
        if let egui::Event::Key { modifiers, .. } = &mut command {
            modifiers.ctrl = true;
            modifiers.command = true;
        }
        frame(&mut app, &ctx, vec![command], true);
        assert!(app.tool == Tool::Eraser);
        assert!(app.shortcuts.preferences == custom);
        assert!(app.tool_caption(Tool::Eraser).ends_with("+E / O"));
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
    }
    #[test]
    fn selecting_the_active_eraser_leaves_its_draft_intact() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.shortcuts = Shortcuts::load(Path::new(""));
        let mut custom = app.shortcuts.preferences.clone();
        custom.bindings.get_mut("Eraser").unwrap().key = "O".into();
        app.shortcuts.apply(custom).unwrap();
        let doc = app.shared.lock().unwrap().resident_state().unwrap().document.clone();
        for (letter, command) in [(egui::Key::E, true), (egui::Key::O, false)] {
            app.select_tool(Tool::Brush);
            let send = |app: &mut PeerBrush, pressed, repeat| {
                let _ = ctx.run(egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: letter, physical_key: Some(letter), pressed, repeat,
                        modifiers: egui::Modifiers { ctrl: command, command, ..Default::default() },
                    }],
                    ..Default::default()
                }, |ctx| ctx.input(|input| app.configured_shortcuts(input, &doc)));
            };
            send(&mut app, true, false);
            assert!(app.tool == Tool::Eraser);
            let points = vec![[2., 3.], [4., 5.]];
            app.points = points.clone();
            app.live_gesture = Some("eraser-draft".into());
            send(&mut app, true, true);
            assert_eq!(app.points, points);
            assert_eq!(app.live_gesture.as_deref(), Some("eraser-draft"));
            send(&mut app, false, false);
            send(&mut app, true, false);
            assert_eq!(app.points, points);
            assert_eq!(app.live_gesture.as_deref(), Some("eraser-draft"));
            send(&mut app, false, false);
        }
        assert!(app.shared.lock().unwrap().resident_state().unwrap().undo.is_empty());
    }

    #[test]
    fn resolved_layout_keys_dispatch_one_binding_with_their_own_modifiers() {
        let shortcuts = Shortcuts::load(Path::new(""));
        for (logical, physical, shift, expected) in [
            (egui::Key::W, egui::Key::Q, false, "Move"),
            (egui::Key::X, egui::Key::B, false, "Swap colors"),
            (egui::Key::R, egui::Key::E, true, "Rotate view right"),
            (egui::Key::T, egui::Key::P, false, "Text"),
            // Non-Latin native input has already selected its physical fallback.
            (egui::Key::W, egui::Key::W, false, "Move"),
        ] {
            let ctx = egui::Context::default();
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: logical,
                        physical_key: Some(physical),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers {
                            shift,
                            ..Default::default()
                        },
                    }],
                    ..Default::default()
                },
                |ctx| {
                    ctx.input(|input| {
                        let active = shortcuts
                            .preferences
                            .bindings
                            .iter()
                            .filter_map(|(name, binding)| {
                                binding.pressed(input).then_some(name.as_str())
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(active, vec![expected]);
                    })
                },
            );
        }
        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                events: vec![
                    egui::Event::Key {
                        key: egui::Key::W,
                        physical_key: Some(egui::Key::Q),
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    },
                    egui::Event::Key {
                        key: egui::Key::Q,
                        physical_key: Some(egui::Key::W),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::COMMAND,
                    },
                ],
                ..Default::default()
            },
            |ctx| {
                ctx.input(|input| {
                    assert!(shortcuts.preferences.bindings["Move"].pressed(input));
                    assert!(!shortcuts.preferences.bindings["Deselect"].pressed(input));
                    assert!(command_key(input, egui::Key::Q));
                    assert!(!command_key(input, egui::Key::W));
                    assert!(!shortcuts.selection_dependent_event(&input.events[1]));
                })
            },
        );
        for reverse in [false, true] {
            let ctx = egui::Context::default();
            let modified = egui::Modifiers {
                shift: true,
                ..egui::Modifiers::COMMAND
            };
            let mut events = vec![
                egui::Event::Key {
                    key: egui::Key::Z,
                    physical_key: Some(egui::Key::W),
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                },
                egui::Event::Key {
                    key: egui::Key::Z,
                    physical_key: Some(egui::Key::W),
                    pressed: true,
                    repeat: false,
                    modifiers: modified,
                },
            ];
            if reverse {
                events.reverse();
            }
            let _ = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    ctx.input(|input| {
                        assert_eq!(command_modifiers(input, egui::Key::Z), Some(modified));
                    })
                },
            );
        }
        let mut remapped = shortcuts.preferences.clone();
        remapped.bindings.get_mut("Brush").unwrap().key = "O".into();
        let mut shortcuts = shortcuts;
        shortcuts.apply(remapped).unwrap();
        for (logical, physical, expected) in [
            (egui::Key::O, egui::Key::B, true),
            (egui::Key::B, egui::Key::O, false),
        ] {
            let ctx = egui::Context::default();
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: logical,
                        physical_key: Some(physical),
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }],
                    ..Default::default()
                },
                |ctx| {
                    ctx.input(|input| {
                        assert_eq!(shortcuts.pressed("Brush", input), expected);
                        assert!(!shortcuts.size_held(input));
                    })
                },
            );
        }
    }
    #[test]
    fn alternate_latin_layouts_preserve_selection_colors_and_native_artwork() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
            app.shortcuts = Shortcuts::load(Path::new(""));
            app.select_tool(Tool::Picker);
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.selection = Some([1, 1, 8, 8]);
            frame(&mut app, &ctx, vec![], true);
            let before = super::super::workspace_tests::source(&app);
            let foreground = app.color;
            let background = app.background;
            for (logical, physical) in [(egui::Key::W, egui::Key::Q), (egui::Key::X, egui::Key::B)]
            {
                for pressed in [true, false] {
                    frame(
                        &mut app,
                        &ctx,
                        vec![egui::Event::Key {
                            key: logical,
                            physical_key: Some(physical),
                            pressed,
                            repeat: false,
                            modifiers: Default::default(),
                        }],
                        true,
                    );
                }
                assert!(
                    app.tool == Tool::Move,
                    "Physical position must not dispatch another tool"
                );
                assert_eq!(super::super::workspace_tests::source(&app), before);
            }
            assert_eq!((app.color, app.background), (background, foreground));
        }
    }
    #[test]
    fn default_brace_aliases_pair_modifiers_and_keep_text_only_fallback_bounded() {
        let mut shortcuts = Shortcuts::load(Path::new(""));
        for (action, key, text) in [
            ("Smaller size", egui::Key::OpenBracket, "{"),
            ("Larger size", egui::Key::CloseBracket, "}"),
        ] {
            let pressed = |modifiers| egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            };
            for reverse in [false, true] {
                let mut events = vec![
                    pressed(egui::Modifiers::COMMAND),
                    pressed(egui::Modifiers::SHIFT),
                ];
                if reverse {
                    events.reverse();
                }
                let ctx = egui::Context::default();
                let _ = ctx.run(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        ctx.input(|input| {
                            assert!(shortcuts.pressed(action, input));
                        })
                    },
                );
            }
            for (events, modifiers, expected) in [
                (
                    vec![egui::Event::Text(text.into())],
                    egui::Modifiers::SHIFT,
                    true,
                ),
                (
                    vec![egui::Event::Text(text.into())],
                    egui::Modifiers::COMMAND,
                    false,
                ),
                (
                    vec![
                        pressed(egui::Modifiers::COMMAND),
                        egui::Event::Text(text.into()),
                    ],
                    egui::Modifiers::NONE,
                    false,
                ),
            ] {
                let ctx = egui::Context::default();
                let _ = ctx.run(
                    egui::RawInput {
                        events,
                        modifiers,
                        ..Default::default()
                    },
                    |ctx| {
                        ctx.input(|input| {
                            assert_eq!(shortcuts.pressed(action, input), expected);
                        })
                    },
                );
            }
            let mut remapped = shortcuts.preferences.clone();
            remapped.bindings.get_mut(action).unwrap().key =
                if action == "Smaller size" { "1" } else { "2" }.into();
            shortcuts.apply(remapped).unwrap();
            let ctx = egui::Context::default();
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![
                        pressed(egui::Modifiers::SHIFT),
                        egui::Event::Text(text.into()),
                    ],
                    ..Default::default()
                },
                |ctx| ctx.input(|input| assert!(!shortcuts.pressed(action, input))),
            );
        }
    }
    #[test]
    fn editing_shortcuts_use_event_modifiers_before_later_modifier_changes() {
        for depth in [8, 16] {
            for reverse in [false, true] {
                let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
                app.shortcuts = Shortcuts::load(Path::new(""));
                app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.selection = Some([1, 1, 8, 8]);
                frame(&mut app, &ctx, vec![], true);
                let before = super::super::workspace_tests::source(&app);
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        time: Some(ctx.input(|i| i.time) + 0.1),
                        modifiers: egui::Modifiers::ALT,
                        events: vec![key(egui::Key::B, true)],
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                );
                assert!(app.tool == Tool::Brush);
                assert_eq!(
                    super::super::workspace_tests::source(&app),
                    before,
                    "Alt pressed after B must not retroactively fill the artwork"
                );
                frame(&mut app, &ctx, vec![key(egui::Key::B, false)], true);
                let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                let prior_undo = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
                let expected = crate::engine::Engine::preview_edits(
                    doc.clone(),
                    &[json!({
                        "op":"paint.clear_selection", "layer":app.selected, "mask":false
                    })],
                )
                .unwrap();
                let mut events = vec![
                    egui::Event::Key {
                        key: egui::Key::Delete,
                        physical_key: Some(egui::Key::Delete),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::COMMAND,
                    },
                    key(egui::Key::Delete, true),
                ];
                if reverse {
                    events.reverse();
                }
                frame(&mut app, &ctx, events, true);
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    expected.export_png().unwrap()
                );
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), prior_undo + 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), prior_undo);
                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), doc.export_png().unwrap());
            }
        }
    }
    #[test]
    fn text_shortcut_respects_text_focus_and_unassigned_conflicts_at_native_depths() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
            app.shortcuts = Shortcuts::load(Path::new(""));
            app.select_tool(Tool::Picker);
            frame(&mut app, &ctx, vec![], true);
            let before = super::super::workspace_tests::source(&app);
            frame(&mut app, &ctx, vec![key(egui::Key::T, true)], true);
            assert!(app.tool == Tool::Text);
            frame(&mut app, &ctx, vec![key(egui::Key::T, false)], true);
            assert_eq!(super::super::workspace_tests::source(&app), before);
            app.select_tool(Tool::Picker);
            let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            let layer = doc
                .layers
                .iter()
                .find(|layer| layer.id == app.selected)
                .unwrap();
            app.rename_edit = Some(RenameEdit {
                document: doc.id.clone(),
                layer: layer.id.clone(),
                original: layer.name.clone(),
                text: layer.name.clone(),
                revision: doc.revision,
                focus: true,
            });
            frame(&mut app, &ctx, vec![], true);
            assert!(ctx
                .memory(|memory| memory.focused())
                .is_some_and(|id| egui::TextEdit::load_state(&ctx, id).is_some()));
            let original_text = app.rename_edit.as_ref().unwrap().text.clone();
            frame(
                &mut app,
                &ctx,
                vec![key(egui::Key::T, true), egui::Event::Text("t".into())],
                true,
            );
            assert!(app.tool == Tool::Picker);
            assert_ne!(app.rename_edit.as_ref().unwrap().text, original_text);
            assert!(app.rename_edit.as_ref().unwrap().text.contains('t'));
            frame(
                &mut app,
                &ctx,
                vec![key(egui::Key::T, false), key(egui::Key::Escape, true)],
                true,
            );
            frame(&mut app, &ctx, vec![key(egui::Key::Escape, false)], true);
            assert!(app.rename_edit.is_none());
            let mut conflict = app.shortcuts.preferences.clone();
            conflict.bindings.get_mut("Brush").unwrap().key = "T".into();
            conflict.bindings.get_mut("Text").unwrap().key.clear();
            app.shortcuts.apply(conflict).unwrap();
            assert!(app.tool_caption(Tool::Text).ends_with(" · Unassigned"));
            frame(&mut app, &ctx, vec![key(egui::Key::T, true)], true);
            assert!(app.tool == Tool::Brush);
            frame(&mut app, &ctx, vec![key(egui::Key::T, false)], true);
            assert_eq!(super::super::workspace_tests::source(&app), before);
        }
    }
    #[test]
    fn remapped_keys_size_hardness_and_temporary_pen_buttons_keep_history_clean() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        app.shortcuts = Shortcuts::load(Path::new(""));
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
        let mut remapped = app.shortcuts.preferences.clone();
        remapped.bindings.get_mut("Brush").unwrap().key = "O".into();
        app.shortcuts.apply(remapped).unwrap();
        app.select_tool(Tool::Picker);
        frame(&mut app, &ctx, vec![key(egui::Key::B, true)], true);
        assert!(app.tool == Tool::Picker);
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::B, false), key(egui::Key::O, true)],
            true,
        );
        assert!(app.tool == Tool::Brush);
        frame(&mut app, &ctx, vec![key(egui::Key::O, false)], true);
        let at = app.view_rect.unwrap().center();
        let radius = app.radius;
        let hardness = app.brush.hardness;
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(at), key(egui::Key::S, true)],
            true,
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(at + Vec2::new(24., 20.))],
            true,
        );
        assert!(app.radius > radius);
        assert!(app.brush.hardness < hardness);
        assert!(app.points.is_empty());
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        frame(&mut app, &ctx, vec![key(egui::Key::S, false)], true);
        let button = |button, pressed| egui::Event::PointerButton {
            pos: at,
            button,
            pressed,
            modifiers: Default::default(),
        };
        frame(
            &mut app,
            &ctx,
            vec![button(egui::PointerButton::Extra1, true)],
            true,
        );
        assert!(app.tool == Tool::Eraser);
        frame(
            &mut app,
            &ctx,
            vec![button(egui::PointerButton::Extra1, false)],
            true,
        );
        assert!(app.tool == Tool::Brush);
        assert_eq!(app.radius, radius * (24_f32 * 0.012).exp());
        frame(
            &mut app,
            &ctx,
            vec![button(egui::PointerButton::Extra2, true)],
            true,
        );
        assert!(app.tool == Tool::Pan);
        frame(&mut app, &ctx, vec![], false);
        assert!(app.tool == Tool::Brush);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(), before);
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
    }
    #[test]
    fn actual_inline_text_entry_and_configuration_pause_shortcuts() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        app.shortcuts = Shortcuts::load(Path::new(""));
        app.select_tool(Tool::Picker);
        frame(&mut app, &ctx, vec![], true);
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let layer = doc.layers.iter().find(|l| l.id == app.selected).unwrap();
        app.rename_edit = Some(RenameEdit {
            document: doc.id.clone(),
            layer: layer.id.clone(),
            original: layer.name.clone(),
            text: layer.name.clone(),
            revision: doc.revision,
            focus: true,
        });
        frame(&mut app, &ctx, vec![], true);
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::B, true), egui::Event::Text("b".into())],
            true,
        );
        assert!(app.tool == Tool::Picker);
        assert!(app.rename_edit.as_ref().unwrap().text.contains('b'));
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::B, false), key(egui::Key::Escape, true)],
            true,
        );
        app.shortcuts.open = true;
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::Escape, false), key(egui::Key::B, true)],
            true,
        );
        assert!(app.tool == Tool::Picker);
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
            doc.export_png().unwrap()
        );
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
    }
    #[test]
    fn releasing_a_temporary_eraser_during_a_native_stroke_waits_for_one_commit() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let (mut app, ctx) = super::super::tests::fixture();
                app.recovery.open = false;
                app.shortcuts = Shortcuts::load(Path::new(""));
                {
                    let mut e = app.shared.lock().unwrap();
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(64, 48, depth).unwrap();
                    for y in 0..48 {
                        for x in 0..64 {
                            e.resident_state_mut().expect("resident fixture").document.layers[0]
                                .pixels
                                .set16(x, y, [12001, 23003, 34007, 65535]);
                        }
                    }
                    let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    e.edit(
                        "human",
                        &[json!({"op":"mask.add","layer":layer,"value":183})],
                        None,
                        None,
                        "Mask",
                    )
                    .unwrap();
                    if mask {
                        e.edit("human",&[json!({"op":"paint","layer":layer,"mask":true,"points":[[16,24],[48,24]],"radius":10,"hardness":1,"color":[30,30,30,255]})],None,None,"Painted mask source").unwrap();
                    }
                    e.resident_state_mut().expect("resident fixture").undo.clear();
                }
                frame(&mut app, &ctx, vec![], true);
                app.mask = mask;
                app.view = view::View {
                    angle: 37.,
                    mirror_h: true,
                    mirror_v: false,
                };
                app.frame_pending = true;
                frame(&mut app, &ctx, vec![], true);
                let rect = app.view_rect.unwrap();
                let scale = rect.width() / 64.;
                let start = app.view.point([16., 24.], rect, scale);
                let end = app.view.point([48., 24.], rect, scale);
                let button = |pos, button, pressed| egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers: Default::default(),
                };
                frame(
                    &mut app,
                    &ctx,
                    vec![
                        egui::Event::PointerMoved(start),
                        button(start, egui::PointerButton::Extra1, true),
                    ],
                    true,
                );
                assert!(app.tool == Tool::Eraser);
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(start + Vec2::new(10., 0.))],
                    true,
                );
                assert!(app.points.is_empty());
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                app.radius = 6.;
                let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                frame(
                    &mut app,
                    &ctx,
                    vec![button(start, egui::PointerButton::Primary, true)],
                    true,
                );
                frame(
                    &mut app,
                    &ctx,
                    vec![
                        egui::Event::PointerMoved(end),
                        button(end, egui::PointerButton::Extra1, false),
                    ],
                    true,
                );
                assert!(app.tool == Tool::Eraser);
                assert!(!app.points.is_empty());
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                let color = if mask {
                    [app.mask_value, app.mask_value, app.mask_value, app.color[3]]
                } else {
                    app.color
                };
                let preview = crate::engine::Engine::preview_edits(
                    before.clone(),
                    &[app.stroke_command(&app.points, color)],
                )
                .unwrap();
                frame(
                    &mut app,
                    &ctx,
                    vec![button(end, egui::PointerButton::Primary, false)],
                    true,
                );
                frame(&mut app, &ctx, vec![], true);
                assert!(app.tool == Tool::Brush);
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1, "{}", app.message);
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), preview.export_png().unwrap());
                assert_ne!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                e.undo("human").unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
            }
        }
    }
}
