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
];
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
        let Some(key) = self.key() else {
            return false;
        };
        let mods = key_modifiers(input, key);
        !mods.command && !mods.ctrl && !mods.alt && mods.shift == self.shift &&
            (input.key_pressed(key) || input.events.iter().any(|e| matches!(e, egui::Event::Key {physical_key:Some(k),pressed:true,..} if *k==key)))
    }
    fn held(&self, input: &egui::InputState) -> bool {
        !input.modifiers.command
            && !input.modifiers.ctrl
            && !input.modifiers.alt
            && input.modifiers.shift == self.shift
            && self.key().is_some_and(|key| input.key_down(key))
    }
    fn label(&self) -> String {
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
            version: 1,
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
        };
        if let Some(path) = &this.path {
            let result = (|| {
                this.original = crate::tool_settings::read_owned(path)?;
                if let Some(bytes) = &this.original {
                    let prefs: Preferences = serde_json::from_slice(bytes)
                        .map_err(|e| format!("Unreadable shortcuts: {e}"))?;
                    Self::validate(&prefs)?;
                    if crate::tool_settings::read_owned(path)? != this.original {
                        return Err("Shortcuts changed while loading".into());
                    }
                    this.preferences = prefs;
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
    fn validate(prefs: &Preferences) -> Result<(), String> {
        let defaults = Preferences::default();
        if prefs.version != 1 || prefs.bindings.keys().ne(defaults.bindings.keys()) {
            return Err("Unsupported shortcut preferences".into());
        }
        let mut used = BTreeMap::new();
        for (action, binding) in &prefs.bindings {
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
            let _lease = crate::file_safety::Lease::acquire(path)?;
            if crate::tool_settings::read_owned(path)? != self.original {
                return Err("Shortcuts changed on disk; restart before saving".into());
            }
            let bytes = serde_json::to_vec_pretty(&prefs).map_err(|e| e.to_string())?;
            server::atomic_write_checked(path, &bytes, |_| {
                if crate::tool_settings::read_owned(path)? == self.original {
                    Ok(())
                } else {
                    Err("Shortcuts changed while saving".into())
                }
            })?;
            self.original = Some(bytes);
        }
        self.preferences = prefs;
        Ok(())
    }
    fn pressed(&self, action: &str, input: &egui::InputState) -> bool {
        if self.preferences.bindings[action].pressed(input) {
            return true;
        }
        let (key, text) = match action {
            "Smaller size" => (egui::Key::OpenBracket, "{"),
            "Larger size" => (egui::Key::CloseBracket, "}"),
            _ => return false,
        };
        let mods = key_modifiers(input, key);
        self.preferences.bindings[action].key == key.symbol_or_name()
            && !self.preferences.bindings[action].shift
            && !mods.command
            && !mods.ctrl
            && !mods.alt
            && (input.key_pressed(key)
                || input
                    .events
                    .iter()
                    .any(|e| matches!(e,egui::Event::Text(t) if t==text)))
    }
    pub fn size_held(&self, input: &egui::InputState) -> bool {
        self.preferences.bindings["Size / hardness gesture"].held(input)
    }
    pub(super) fn selection_dependent_event(&self, event: &egui::Event) -> bool {
        match event {
            egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) => true,
            egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                modifiers,
                ..
            } => {
                let is_key = |wanted| *key == wanted || *physical_key == Some(wanted);
                if (modifiers.command || modifiers.ctrl)
                    && !modifiers.alt
                    && [egui::Key::C, egui::Key::X, egui::Key::V]
                        .into_iter()
                        .any(is_key)
                {
                    return true;
                }
                if is_key(egui::Key::Delete) && !modifiers.any() {
                    return true;
                }
                if modifiers.alt
                    && !modifiers.command
                    && [egui::Key::B, egui::Key::Backspace].into_iter().any(is_key)
                    || (modifiers.command || modifiers.ctrl)
                        && !modifiers.alt
                        && !modifiers.shift
                        && is_key(egui::Key::D)
                {
                    return true;
                }
                ["Move", "Rotate artwork", "Scale artwork"]
                    .into_iter()
                    .any(|action| {
                        let binding = &self.preferences.bindings[action];
                        !modifiers.command
                            && !modifiers.ctrl
                            && !modifiers.alt
                            && modifiers.shift == binding.shift
                            && binding.key().is_some_and(is_key)
                    })
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
    pub(super) fn configured_shortcuts(&mut self, input: &egui::InputState, doc: &Document) {
        if self.shortcuts.open || self.shortcuts.temporary.is_some() {
            return;
        }
        for (action, _, tool) in TOOLS {
            if self.shortcuts.pressed(action, input) {
                if *tool == Tool::None {
                    self.deselect_area(doc, true);
                } else {
                    self.select_tool(*tool);
                }
            }
        }
        if self.shortcuts.pressed("Frame canvas", input) {
            self.frame_pending = true;
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
                self.message = format!("Brush size {} px", (self.radius * 2.).round());
            }
        }
        if self.points.is_empty()
            && self.drag_start.is_none()
            && self.gizmo_handle == 0
            && self.live_gesture.is_none()
            && !self.pen.in_gesture()
        {
            for (action, delta) in [("Rotate view left", -15.), ("Rotate view right", 15.)] {
                if self.shortcuts.pressed(action, input) {
                    self.view.angle = (self.view.angle + delta + 180.).rem_euclid(360.) - 180.;
                }
            }
            if self.shortcuts.pressed("Mirror view horizontally", input) {
                self.view.mirror_h = !self.view.mirror_h;
            }
            if self.shortcuts.pressed("Mirror view vertically", input) {
                self.view.mirror_v = !self.view.mirror_v;
            }
            if self.shortcuts.pressed("Reset view", input) {
                self.reset_canvas_view();
            }
        }
    }
    pub(super) fn shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.shortcuts.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Keyboard and pen shortcuts").open(&mut open).resizable(false).default_width(420.).show(ctx,|ui| {
            ui.label("Choose a letter, digit or bracket, with optional Shift.");
            ui.label(RichText::new("Ctrl/Command, Alt, Space and editing keys retain their standard actions. Shortcuts pause during text entry.").small().color(MUTED));
            let mut candidate=self.shortcuts.preferences.clone();
            egui::ScrollArea::vertical().max_height(420.).show(ui,|ui| {
                for (action,binding) in &mut candidate.bindings {
                    ui.horizontal(|ui| {
                        ui.add_sized(Vec2::new(200.,20.),egui::Label::new(action));
                        egui::ComboBox::from_id_salt(action).width(50.).selected_text(&binding.key).show_ui(ui,|ui| {
                            for key in egui::Key::ALL.iter().copied().filter(|key|Shortcuts::allowed(*key)) {ui.selectable_value(&mut binding.key,key.symbol_or_name().into(),key.symbol_or_name());}
                        });
                        ui.checkbox(&mut binding.shift,"Shift");
                    });
                }
            });
            for index in 0..2 {
                ui.horizontal(|ui| {
                    ui.label(format!("Pen extra button {}",index+1));
                    egui::ComboBox::from_id_salt(("pen extra",index)).selected_text(&candidate.buttons[index]).show_ui(ui,|ui| {
                        for role in ["None","Eraser","Pan"] {ui.selectable_value(&mut candidate.buttons[index],role.into(),role);}
                    });
                });
            }
            ui.label(RichText::new("Configure the tablet driver to send extra mouse buttons. Tool restoration waits for a stroke to finish; focus loss restores it immediately. Eraser-end identity is not supplied by this input backend.").small().color(MUTED));
            if ui.button("Reset shortcuts and buttons").clicked() {candidate=Preferences::default();}
            if candidate!=self.shortcuts.preferences {
                self.shortcuts.message=match self.shortcuts.apply(candidate) {Ok(())=>"Preferences saved".into(),Err(error)=>error};
            }
            if !self.shortcuts.message.is_empty() {ui.label(&self.shortcuts.message);}
        });
        self.shortcuts.open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn frame(app: &mut PeerBrush, ctx: &egui::Context, events: Vec<egui::Event>, focused: bool) {
        let time = ctx.input(|i| i.time) + 0.1;
        let _ = ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                events,
                focused,
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        );
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
    fn remapped_keys_size_hardness_and_temporary_pen_buttons_keep_history_clean() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        app.shortcuts = Shortcuts::load(Path::new(""));
        let before = app.shared.lock().unwrap().doc.export_png().unwrap();
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
        assert!(app.shared.lock().unwrap().undo.is_empty());
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
        assert_eq!(app.shared.lock().unwrap().doc.export_png().unwrap(), before);
        assert!(app.shared.lock().unwrap().undo.is_empty());
    }
    #[test]
    fn actual_inline_text_entry_and_configuration_pause_shortcuts() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        app.shortcuts = Shortcuts::load(Path::new(""));
        app.select_tool(Tool::Picker);
        frame(&mut app, &ctx, vec![], true);
        let doc = app.shared.lock().unwrap().doc.clone();
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
            app.shared.lock().unwrap().doc.export_png().unwrap(),
            doc.export_png().unwrap()
        );
        assert!(app.shared.lock().unwrap().undo.is_empty());
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
                    e.doc = Document::new_depth(64, 48, depth).unwrap();
                    for y in 0..48 {
                        for x in 0..64 {
                            e.doc.layers[0]
                                .pixels
                                .set16(x, y, [12001, 23003, 34007, 65535]);
                        }
                    }
                    let layer = e.doc.layers[0].id.clone();
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
                    e.undo.clear();
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
                assert!(app.shared.lock().unwrap().undo.is_empty());
                app.radius = 6.;
                let before = app.shared.lock().unwrap().doc.clone();
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
                assert!(app.shared.lock().unwrap().undo.is_empty());
                let color = if mask {
                    [app.mask_value, app.mask_value, app.mask_value, app.color[3]]
                } else {
                    app.color
                };
                let preview = crate::engine::Engine::preview_edits(
                    before.clone(),
                    &[app.stroke_command(app.points.clone(), color)],
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
                assert_eq!(e.undo.len(), 1, "{}", app.message);
                assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
                assert_ne!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            }
        }
    }
}
