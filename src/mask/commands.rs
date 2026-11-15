//! Shared mask-step parameter validation; pixel ownership stays with the caller.
use serde_json::Value;
use std::borrow::Cow;

pub(crate) fn validate_settings(kind: &str, settings: &Value) -> Result<(), String> {
    if crate::effects::has_channel_curves(kind, settings) {
        return Err("Mask Curves use a single curve".into());
    }
    if crate::effects::curves::uses_photoshop(kind, settings) {
        return Err("Photoshop spline Curves require a color target".into());
    }
    crate::effects::validate(if kind == "gaussian" { "blur" } else { kind }, settings)
}

pub(crate) struct NewStep<'a> {
    pub kind: &'a str,
    pub weight: f32,
    pub value: f32,
    pub settings: Cow<'a, Value>,
}
impl<'a> NewStep<'a> {
    pub(crate) fn parse(command: &'a Value) -> Result<Self, String> {
        let kind = command["kind"].as_str().unwrap_or("invert");
        if !crate::effects::catalog::get(kind).is_some_and(|entry| entry.supports(true)) {
            return Err("Unsupported mask effect; inspect the shared effect catalog".into());
        }
        let value = crate::engine::mask_parameter(
            kind,
            command["value"].as_f64().unwrap_or(match kind {
                "levels" => 1.,
                "blur" => 8.,
                _ => 255.,
            }),
        )?;
        let settings = if ["curves", "adjust", "gaussian"].contains(&kind) {
            let effect = if kind == "gaussian" { "blur" } else { kind };
            let settings = command
                .get("settings")
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Owned(crate::effects::defaults(effect)));
            validate_settings(kind, &settings)?;
            settings
        } else {
            Cow::Owned(Value::Null)
        };
        Ok(Self {
            kind,
            weight: crate::effects::command_weight(command)?,
            value,
            settings,
        })
    }
}

pub(crate) struct StepUpdate<'a> {
    pub enabled: Option<bool>,
    pub weight: Option<f32>,
    pub value: Option<f32>,
    pub settings: Option<&'a Value>,
}
impl<'a> StepUpdate<'a> {
    pub(crate) fn parse(kind: &str, command: &'a Value) -> Result<Self, String> {
        let settings = command.get("settings");
        if let Some(settings) = settings {
            validate_settings(kind, settings)?;
        }
        Ok(Self {
            enabled: command.get("enabled").and_then(Value::as_bool),
            weight: command
                .get("weight")
                .map(|_| crate::effects::command_weight(command))
                .transpose()?,
            value: command
                .get("value")
                .map(|value| crate::engine::mask_parameter(kind, value.as_f64().unwrap_or(1.0)))
                .transpose()?,
            settings,
        })
    }
}
