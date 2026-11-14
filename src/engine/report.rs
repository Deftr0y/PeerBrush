//! Capture under Engine; build admitted native reports after releasing its lock.
pub(crate) mod header;
pub(crate) mod metadata;
use super::{scene::SceneSnapshot, RuntimeValue};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

type Result<T> = std::result::Result<T, String>;

#[must_use = "Project the captured state after releasing the Engine lock"]
pub struct StateCapture(Body);
enum Body {
    Resident(Value),
    Native {
        scene: Arc<SceneSnapshot>,
        header: RuntimeValue<Value>,
        saving: Option<crate::saving::Control>,
        loading: Option<crate::loading::Control>,
    },
}
impl StateCapture {
    pub(super) fn resident(value: Value) -> Self {
        Self(Body::Resident(value))
    }
    pub(super) fn native(
        scene: Arc<SceneSnapshot>,
        header: RuntimeValue<Value>,
        saving: Option<crate::saving::Control>,
        loading: Option<crate::loading::Control>,
    ) -> Self {
        Self(Body::Native {
            scene,
            header,
            saving,
            loading,
        })
    }
    /// This may prepare display-profile metadata, and must run off the Engine lock.
    pub fn project(self) -> Result<RuntimeValue<Value>> {
        let (scene, header, saving, loading) = match self.0 {
            Body::Resident(value) => return Ok(value.into()),
            Body::Native {
                scene,
                header,
                saving,
                loading,
            } => (scene, header, saving, loading),
        };
        let pool = scene.budget();
        let saving: Option<RuntimeValue<Value>> = saving
            .as_ref()
            .map(|control| {
                control.read_status(|status| RuntimeValue::project(status, pool, &|| Ok(())))
            })
            .transpose()?;
        #[derive(Serialize)]
        struct Loading<'a> {
            stage: &'a str,
            completed: usize,
            total: usize,
        }
        let loading: Option<RuntimeValue<Value>> = loading
            .as_ref()
            .map(|control| {
                control.read_status(|status| {
                    RuntimeValue::project(
                        &Loading {
                            stage: &status.stage,
                            completed: status.completed,
                            total: status.total,
                        },
                        pool,
                        &|| Ok(()),
                    )
                })
            })
            .transpose()?;
        use crate::color_profile::prepared::{DisplayPlan, DisplayStatus};
        let plan = DisplayPlan::prepare(scene.metadata().profile(), false, pool, &mut || Ok(()))?;
        let profile = match plan.status() {
            DisplayStatus::Unprofiled => {
                json!({"source":"sRGB","preview":"sRGB","conversion_available":false})
            }
            DisplayStatus::Converted => {
                json!({"source":"embedded RGB ICC","preview":"sRGB","conversion_available":true})
            }
            DisplayStatus::Unsupported => {
                json!({"source":"embedded ICC","preview":"unmanaged","conversion_available":false,"reason":plan.warning().unwrap()})
            }
            DisplayStatus::Mask => return Err("State report cannot use mask display policy".into()),
        };
        #[derive(Serialize)]
        struct State<'a> {
            #[serde(flatten)]
            header: &'a Value,
            saving: Option<&'a Value>,
            loading: Option<&'a Value>,
            document: metadata::Document<'a>,
            layers: metadata::Layers<'a>,
        }
        RuntimeValue::project(
            &State {
                header: &header,
                saving: saving.as_deref(),
                loading: loading.as_deref(),
                document: metadata::Document {
                    scene: &scene,
                    profile: &profile,
                },
                layers: metadata::Layers(&scene),
            },
            pool,
            &|| Ok(()),
        )
    }
}
