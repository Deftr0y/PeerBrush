//! Extended tone settings require readers that understand their mapping,
//! even when the effect is disabled or currently produces identical pixels.
use super::{Effect36, EffectClip36, MetadataPayload};
use serde::{ser::SerializeStruct, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

impl Serialize for Effect36 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let version = crate::effects::levels::source_version(&self.kind, &self.settings);
        let channel_curves = crate::effects::curves::channel_version(&self.kind, &self.settings);
        let posterize = crate::effects::posterize::source_version(&self.kind, &self.settings);
        let mut wire = serializer.serialize_struct(
            "Effect36", 6 + usize::from(version.is_some()) + usize::from(channel_curves.is_some()) + usize::from(posterize.is_some()),
        )?;
        wire.serialize_field("id", &self.id)?;
        wire.serialize_field("kind", &self.kind)?;
        wire.serialize_field("enabled", &self.enabled)?;
        wire.serialize_field("weight", &self.weight)?;
        wire.serialize_field("settings", &self.settings)?;
        wire.serialize_field("clips", &self.clips)?;
        if let Some(version) = version {
            wire.serialize_field("levels_output_version", &version)?;
        }
        if let Some(version) = channel_curves {
            wire.serialize_field("curves_channel_version", &version)?;
        }
        if let Some(version) = posterize {
            wire.serialize_field("posterize_mapping_version", &version)?;
        }
        wire.end()
    }
}

impl<'de> Deserialize<'de> for Effect36 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            id: String,
            kind: String,
            enabled: bool,
            weight: f32,
            settings: MetadataPayload<Value>,
            clips: Vec<EffectClip36>,
            #[serde(default)]
            levels_output_version: Option<u8>,
            #[serde(default)]
            curves_channel_version: Option<u8>,
            #[serde(default)]
            posterize_mapping_version: Option<u8>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let version = crate::effects::levels::source_version(&wire.kind, &wire.settings);
        if wire.levels_output_version != version {
            return Err(serde::de::Error::custom("Levels output source version differs"));
        }
        let channel_version = crate::effects::curves::channel_version(&wire.kind, &wire.settings);
        if wire.curves_channel_version != channel_version {
            return Err(serde::de::Error::custom("Curves channel source version differs"));
        }
        let posterize = crate::effects::posterize::source_version(&wire.kind, &wire.settings);
        if wire.posterize_mapping_version != posterize {
            return Err(serde::de::Error::custom("Posterize mapping source version differs"));
        }
        Ok(Self {
            id: wire.id,
            kind: wire.kind,
            enabled: wire.enabled,
            weight: wire.weight,
            settings: wire.settings,
            clips: wire.clips,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn disabled_photoshop_posterize_requires_its_native_source_marker() {
        for (mapping, version) in [("photoshop", 1), ("photoshop_native", 2)] {
            let mut wire = json!({"id":"posterize","kind":"posterize","enabled":false,"weight":0.,
                "settings":{"levels":3,"mapping":mapping},"clips":[]});
            assert!(serde_json::from_value::<Effect36>(wire.clone()).is_err());
            wire["posterize_mapping_version"] = json!(version);
            let effect = serde_json::from_value::<Effect36>(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(effect).unwrap(), wire);
            wire["posterize_mapping_version"] = json!(3 - version);
            assert!(serde_json::from_value::<Effect36>(wire).is_err());
        }
    }

    #[test]
    fn disabled_channel_curves_require_the_native_source_marker() {
        let identity = json!({"interpolation":"linear","points":[[0.,0.],[1.,1.]]});
        let mut settings = identity.clone();
        settings["channels"] = json!([identity, identity, identity]);
        let mut wire = json!({"id":"curves","kind":"curves","enabled":false,"weight":0.,
            "settings":settings,"clips":[]});
        assert!(serde_json::from_value::<Effect36>(wire.clone()).is_err());
        wire["curves_channel_version"] = json!(1);
        let effect = serde_json::from_value::<Effect36>(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(effect).unwrap(), wire);
        wire["curves_channel_version"] = json!(2);
        assert!(serde_json::from_value::<Effect36>(wire.clone()).is_err());
        wire["settings"]["composition"] = json!(crate::effects::curves::PHOTOSHOP_BYTE);
        let effect = serde_json::from_value::<Effect36>(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(effect).unwrap(), wire);
        wire["curves_channel_version"] = json!(1);
        assert!(serde_json::from_value::<Effect36>(wire).is_err());
    }

    #[test]
    fn levels_output_marker_is_derived_and_required_for_disabled_native_sources() {
        let legacy = json!({"id":"levels","kind":"levels","enabled":false,"weight":0.,
            "settings":{"gamma":1.2},"clips":[]});
        let effect: Effect36 = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(effect).unwrap(), legacy);
        for (low, high) in [(0.2, 0.8), (0.8, 0.2), (0.4, 0.4)] {
            let mut wire = legacy.clone();
            wire["settings"]["output_black"] = json!(low);
            wire["settings"]["output_white"] = json!(high);
            assert!(serde_json::from_value::<Effect36>(wire.clone()).is_err());
            wire["levels_output_version"] = json!(1);
            let effect: Effect36 = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(effect).unwrap(), wire);
            wire["levels_output_version"] = json!(2);
            assert!(serde_json::from_value::<Effect36>(wire).is_err());
        }
        let mut effect: Effect36 = serde_json::from_value(legacy.clone()).unwrap();
        for settings in [json!({"input_mapping":"exact"}), json!({"gamma":9.99}),
            json!({"input_mapping":"exact","output_black":0.25})] {
            effect.settings = settings.into();
            let wire = serde_json::to_value(&effect).unwrap();
            assert_eq!(wire["levels_output_version"], 2);
            assert!(serde_json::from_value::<Effect36>(wire.clone()).is_ok());
            for version in [json!(null), json!(1), json!(3)] {
                let mut invalid = wire.clone();
                invalid["levels_output_version"] = version;
                assert!(serde_json::from_value::<Effect36>(invalid).is_err());
            }
        }
        effect.settings = json!({"channels":[{}, {}, {}]}).into();
        let wire = serde_json::to_value(&effect).unwrap();
        assert_eq!(wire["levels_output_version"], 3);
        assert!(serde_json::from_value::<Effect36>(wire.clone()).is_ok());
        for version in [json!(null), json!(1), json!(2), json!(4)] {
            let mut invalid = wire.clone();
            invalid["levels_output_version"] = version;
            assert!(serde_json::from_value::<Effect36>(invalid).is_err());
        }
        effect.settings = json!({"input_mapping":"exact","composition":"photoshop_byte","channels":[{}, {}, {}]}).into();
        let mut wire = serde_json::to_value(&effect).unwrap();
        assert_eq!(wire["levels_output_version"], 4);
        assert!(serde_json::from_value::<Effect36>(wire.clone()).is_ok());
        wire["levels_output_version"] = json!(3);
        assert!(serde_json::from_value::<Effect36>(wire).is_err());
        effect.settings = json!({"output_black":0.25}).into();
        assert_eq!(serde_json::to_value(&effect).unwrap()["levels_output_version"], 1);
        effect.settings = json!({"output_black":0.,"output_white":1.}).into();
        assert!(serde_json::to_value(&effect).unwrap().get("levels_output_version").is_none());
        let mut forged = legacy;
        forged["levels_output_version"] = json!(1);
        assert!(serde_json::from_value::<Effect36>(forged).is_err());
    }
}
