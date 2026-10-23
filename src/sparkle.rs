//! Dormant package/native preparation. No application path starts an updater.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

pub const BUNDLED_LAYOUT: u32 = 1;

pub const PROPOSED_FEED_URL: &str = "https://peerbrush.com/updates/macos/stable/appcast.xml";

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Configuration {
    pub enabled: bool,
    pub automatic_installation: bool,
    pub feed_url: Option<String>,
    pub public_ed_key: Option<String>,
}

impl Configuration {
    /// Validating public configuration never activates it or proves a feed exists.
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled || self.automatic_installation {
            return Err(
                "Sparkle activation and installation remain disabled in this preparation build"
                    .into(),
            );
        }
        match (&self.feed_url, &self.public_ed_key) {
            (None, None) => Ok(()),
            (Some(feed), Some(key)) if feed == PROPOSED_FEED_URL => {
                let bytes = STANDARD.decode(key).map_err(|_| "Expected a canonical public Ed25519 key")?;
                if bytes.len() != 32 || bytes.iter().all(|byte| *byte == 0) || STANDARD.encode(&bytes) != *key {
                    return Err("Expected a canonical public Ed25519 key".into());
                }
                Ok(())
            }
            _ => Err("Prepare both the proposed HTTPS feed and its public Ed25519 key; neither is configured by default".into()),
        }
    }
}

pub fn require_supported_macos(major: isize) -> Result<(), String> {
    if major < 12 {
        return Err("Sparkle 2.10.0 preparation requires macOS 12+; the disabled core app still supports macOS 11".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_is_disabled_and_never_accepts_an_activation_switch() {
        let mut config = Configuration::default();
        config.validate().unwrap();
        assert!(!config.enabled && !config.automatic_installation);
        config.enabled = true;
        assert!(config.validate().is_err());
        config.enabled = false;
        config.automatic_installation = true;
        assert!(config.validate().is_err());
        assert!(
            serde_json::from_str::<Configuration>(r#"{"private_key":"not-supported"}"#).is_err()
        );
    }

    #[test]
    fn public_configuration_requires_the_exact_proposal_and_canonical_public_key() {
        let mut config = Configuration {
            feed_url: Some(PROPOSED_FEED_URL.into()),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        config.public_ed_key = Some(STANDARD.encode([1_u8; 32])); // Synthetic public fixture, not a configured key.
        config.validate().unwrap();
        for key in [
            STANDARD.encode([0_u8; 32]),
            STANDARD.encode([1_u8; 31]),
            "invalid".into(),
        ] {
            config.public_ed_key = Some(key);
            assert!(config.validate().is_err());
        }
        config.public_ed_key = Some(STANDARD.encode([1_u8; 32]));
        for url in [
            "http://peerbrush.com/updates/macos/stable/appcast.xml",
            "https://example.invalid/appcast.xml",
            "https://peerbrush.com/updates/macos/stable/appcast.xml?key=private",
        ] {
            config.feed_url = Some(url.into());
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn preparation_does_not_raise_the_core_apps_minimum_os() {
        assert!(require_supported_macos(11).is_err());
        require_supported_macos(12).unwrap();
        require_supported_macos(26).unwrap();
    }
}
