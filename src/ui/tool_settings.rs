use super::*;
use crate::tool_settings::{Profile, Snapshot, Store};

pub(super) struct Cache {
    pub tool: String,
    pub snapshot: Snapshot,
}
pub(super) struct Feedback {
    tool: String,
    from: crate::brush::Settings,
    to: crate::brush::Settings,
    started: f64,
}
pub(super) fn key(tool: Tool) -> Option<&'static str> {
    match tool {
        Tool::Brush => Some("brush"),
        Tool::Eraser => Some("eraser"),
        Tool::Smudge => Some("smudge"),
        Tool::Clone => Some("clone"),
        Tool::Heal => Some("heal"),
        Tool::Liquify => Some("liquify"),
        _ => None,
    }
}
impl PeerBrush {
    fn current_tool_profile(&self) -> Profile {
        let mut settings = self.brush;
        settings.radius = self.radius;
        if self.tool == Tool::Liquify {
            Profile {
                settings: crate::brush::Settings {
                    radius: self.radius,
                    opacity: self.liquify.strength,
                    ..Default::default()
                },
                preset: None,
                liquify_mode: Some(self.liquify.mode.key().into()),
            }
        } else {
            Profile {
                settings,
                preset: self.brush_preset.clone(),
                liquify_mode: None,
            }
        }
    }
    fn load_tool_profile(&mut self, tool: &str, snapshot: Snapshot, reset_name: bool) {
        let changed_preset = self.brush_preset != snapshot.profile.preset;
        self.brush = snapshot.profile.settings;
        self.radius = self.brush.radius;
        self.brush_preset = snapshot.profile.preset.clone();
        if tool == "liquify" {
            self.liquify.strength = snapshot.profile.settings.opacity;
            self.liquify.mode = match snapshot.profile.liquify_mode.as_deref() {
                Some("expand") => liquify::Mode::Expand,
                Some("pinch") => liquify::Mode::Pinch,
                Some("restore") => liquify::Mode::Restore,
                _ => liquify::Mode::Push,
            };
        }
        if reset_name || changed_preset {
            let library = self.workspace.lock().unwrap().brush_library.clone();
            let preset = self
                .brush_preset
                .as_deref()
                .and_then(|id| library.lock().unwrap().get(id).ok());
            self.brush_name = preset
                .as_ref()
                .map_or_else(|| "My brush".into(), |p| p.name.clone());
            self.brush_save_category = preset.map_or_else(|| "Paint".into(), |p| p.category);
        }
        self.tool_profile = Cache {
            tool: tool.into(),
            snapshot,
        };
    }
    pub(super) fn store_current_tool_profile(&mut self) {
        let Some(tool) = key(self.tool) else {
            return;
        };
        if self.tool_profile.tool != tool {
            return;
        }
        let profile = self.current_tool_profile();
        if profile == self.tool_profile.snapshot.profile {
            return;
        }
        let result = self.tool_settings_store.lock().unwrap().human_delta(
            tool,
            &self.tool_profile.snapshot.profile,
            &profile,
        );
        match result {
            Ok(snapshot) => {
                self.tool_feedback = None;
                self.load_tool_profile(tool, snapshot, false);
            }
            Err(error) => {
                self.message = error;
                let snapshot = self.tool_settings_store.lock().unwrap().get(tool).unwrap();
                self.load_tool_profile(tool, snapshot, false);
            }
        }
    }
    pub(super) fn restore_tool_profile(&mut self, tool: Tool) {
        self.tool_feedback = None;
        if let Some(key) = key(tool) {
            let snapshot = self.tool_settings_store.lock().unwrap().get(key).unwrap();
            self.load_tool_profile(key, snapshot, true);
        }
    }
    pub(super) fn sync_tool_profile(&mut self, ctx: &egui::Context) {
        self.store_current_tool_profile();
        if !ctx.input(|i| i.pointer.any_down())
            && self.live_gesture.is_none()
            && self.size_drag.is_none()
            && !ctx.wants_keyboard_input()
        {
            if let Err(error) = self.tool_settings_store.lock().unwrap().flush() {
                self.message = format!("Tool settings not saved · {error}");
            }
        }
        let human_input = ctx.input(|i| {
            i.pointer.any_pressed()
                || i.events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Key { pressed: true, .. }))
        });
        if human_input {
            self.tool_feedback = None;
        }
        let Some(tool) = key(self.tool) else {
            return;
        };
        // A running human gesture keeps its frozen brush parameters. Later
        // preferences become visible when that gesture finishes.
        if self.live_gesture.is_some() || self.size_drag.is_some() {
            return;
        }
        let snapshot = self.tool_settings_store.lock().unwrap().get(tool).unwrap();
        if self.tool_profile.tool != tool {
            self.load_tool_profile(tool, snapshot, true);
        } else if snapshot.revision != self.tool_profile.snapshot.revision {
            if snapshot.actor != "human" && !human_input {
                self.tool_feedback = Some(Feedback {
                    tool: tool.into(),
                    from: self.brush,
                    to: snapshot.profile.settings,
                    started: ctx.input(|i| i.time),
                });
            } else {
                self.tool_feedback = None;
            }
            self.load_tool_profile(tool, snapshot, false);
            ctx.request_repaint();
        }
    }
    pub(super) fn tool_preferences_ai(&self, time: f64) -> bool {
        self.tool_feedback
            .as_ref()
            .is_some_and(|f| key(self.tool) == Some(f.tool.as_str()) && time - f.started < 3.)
    }
    pub(super) fn tool_display_number(&self, name: &str, actual: f32, time: f64) -> f32 {
        let Some(feedback) = self
            .tool_feedback
            .as_ref()
            .filter(|_| self.tool_preferences_ai(time))
        else {
            return actual;
        };
        let before = serde_json::to_value(feedback.from).unwrap();
        let after = serde_json::to_value(feedback.to).unwrap();
        let (Some(a), Some(b)) = (before[name].as_f64(), after[name].as_f64()) else {
            return actual;
        };
        let t = ((time - feedback.started) / 0.36).clamp(0., 1.) as f32;
        let t = t * t * (3. - 2. * t);
        a as f32 + (b - a) as f32 * t
    }
    pub(super) fn tool_display_values(&self, time: f64) -> Value {
        let profile = self.current_tool_profile();
        let mut value = serde_json::to_value(profile.settings).unwrap();
        for (name, number) in value.as_object_mut().unwrap() {
            if name != "seed" {
                if let Some(actual) = number.as_f64() {
                    *number = json!(self.tool_display_number(name, actual as f32, time));
                }
            }
        }
        value
    }
}

pub(super) fn number(
    ui: &mut egui::Ui,
    id: &str,
    actual: &mut f32,
    display: f32,
    limits: std::ops::RangeInclusive<f32>,
    width: f32,
    suffix: &str,
    decimals: usize,
    log: bool,
    ai: bool,
) -> egui::Response {
    let mut value = display;
    let response = controls::range_with_color(
        ui,
        id,
        &mut value,
        limits,
        width,
        suffix,
        decimals,
        log,
        if ai {
            AI_BLUE
        } else {
            Color32::from_rgb(190, 70, 29)
        },
    );
    if response.changed() {
        *actual = value;
    }
    response
}

pub(super) fn initial(store: &Arc<std::sync::Mutex<Store>>) -> Cache {
    Cache {
        tool: "brush".into(),
        snapshot: store.lock().unwrap().get("brush").unwrap(),
    }
}
