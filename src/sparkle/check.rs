//! Manual, information-only Sparkle cycle; this has no installation operation.
use super::Configuration;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) const NOTES_LIMIT: usize = 64 * 1024;
pub(crate) const SEALS: [&str; 4] = [
    "Versions/B/XPCServices/Installer.xpc/Contents/_CodeSignature/CodeResources",
    "Versions/B/XPCServices/Downloader.xpc/Contents/_CodeSignature/CodeResources",
    "Versions/B/Updater.app/Contents/_CodeSignature/CodeResources",
    "Versions/B/_CodeSignature/CodeResources",
];
pub(crate) const OBJECTS: [(&str, &str); 5] = [
    (
        "Versions/B/XPCServices/Installer.xpc",
        "Versions/B/XPCServices/Installer.xpc/Contents/MacOS/Installer",
    ),
    (
        "Versions/B/XPCServices/Downloader.xpc",
        "Versions/B/XPCServices/Downloader.xpc/Contents/MacOS/Downloader",
    ),
    ("Versions/B/Autoupdate", "Versions/B/Autoupdate"),
    (
        "Versions/B/Updater.app",
        "Versions/B/Updater.app/Contents/MacOS/Updater",
    ),
    (".", "Versions/B/Sparkle"),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicPins {
    schema: u32,
    feed_url: Option<String>,
    public_ed_key: Option<String>,
    publisher_team_id: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObjectPolicy {
    pub identifier: String,
    pub unsigned_sha256: String,
    pub entitlements: BTreeMap<String, Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePolicy {
    schema: u32,
    archive_sha256: String,
    objects: Option<BTreeMap<String, ObjectPolicy>>,
}
#[derive(Clone)]
pub(crate) struct ApprovedCheck {
    pub feed_url: String,
    pub public_ed_key: String,
    pub publisher_team_id: String,
    pub objects: BTreeMap<String, ObjectPolicy>,
}

pub(crate) fn compiled() -> Result<Option<ApprovedCheck>, String> {
    configuration(
        include_bytes!("../../packaging/sparkle/check-configuration.json"),
        include_bytes!("../../packaging/sparkle/native-policy.json"),
        env!("CARGO_PKG_VERSION"),
    )
}

fn configuration(
    pins: &[u8],
    policy: &[u8],
    version: &str,
) -> Result<Option<ApprovedCheck>, String> {
    let pins: PublicPins =
        serde_json::from_slice(pins).map_err(|_| "Invalid compiled Sparkle check pins")?;
    if pins.schema != 1 {
        return Err("Unsupported compiled Sparkle check pins".into());
    }
    let (feed_url, public_ed_key, publisher_team_id) =
        match (pins.feed_url, pins.public_ed_key, pins.publisher_team_id) {
            (None, None, None) => return Ok(None),
            (Some(feed), Some(key), Some(team)) => (feed, key, team),
            _ => return Err("Complete reviewed public Sparkle pins are required".into()),
        };
    Configuration {
        feed_url: Some(feed_url.clone()),
        public_ed_key: Some(public_ed_key.clone()),
        ..Default::default()
    }
    .validate()?;
    let version = semver::Version::parse(version).map_err(|_| "Invalid application version")?;
    if !version.pre.is_empty()
        || !version.build.is_empty()
        || publisher_team_id.len() != 10
        || !publisher_team_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Err(
            "Manual Sparkle checks require a stable app and reviewed publisher Team ID".into(),
        );
    }
    let policy: NativePolicy =
        serde_json::from_slice(policy).map_err(|_| "Invalid compiled native Sparkle policy")?;
    let pin: Value = serde_json::from_slice(include_bytes!(
        "../../packaging/sparkle/sparkle-2.10.0.json"
    ))
    .map_err(|_| "Invalid Sparkle pin")?;
    if policy.schema != 1 || Some(policy.archive_sha256.as_str()) != pin["archive_sha256"].as_str()
    {
        return Err("Native Sparkle policy differs from the exact upstream pin".into());
    }
    let objects = policy
        .objects
        .ok_or("Native Sparkle policy awaits independent review")?;
    if objects.len() != OBJECTS.len() {
        return Err("Native Sparkle policy needs exactly five objects".into());
    }
    for (name, _) in OBJECTS {
        let object = objects
            .get(name)
            .ok_or("Native Sparkle object is missing")?;
        if object.identifier.is_empty()
            || object.identifier.len() > 160
            || !object
                .identifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            || object.unsigned_sha256.len() != 64
            || !object
                .unsigned_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || object
                .entitlements
                .contains_key("com.apple.security.get-task-allow")
            || (name != OBJECTS[1].0 && !object.entitlements.is_empty())
        {
            return Err(
                "Unreviewed native Sparkle identifier, unsigned hash or entitlements".into(),
            );
        }
    }
    Ok(Some(ApprovedCheck {
        feed_url,
        public_ed_key,
        publisher_team_id,
        objects,
    }))
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Idle,
    Verifying,
    Checking,
    Found,
    NoUpdate,
    Cancelled,
    Failed,
    Quarantined,
}
#[derive(Default)]
pub(crate) struct Session {
    pub phase: Phase,
    pub version: Option<String>,
    pub notes: String,
    pub notice: &'static str,
    pub busy: bool,
    pub cancelled: bool,
    pub quarantine: bool,
    permit: bool,
}
impl Session {
    pub fn begin(&mut self) -> Result<(), String> {
        if self.busy || self.quarantine {
            return Err("The current app update check must finish first".into());
        }
        *self = Self {
            phase: Phase::Verifying,
            busy: true,
            ..Self::default()
        };
        Ok(())
    }
    pub fn checking(&mut self) {
        if !self.cancelled && !self.quarantine {
            self.phase = Phase::Checking;
            self.permit = true;
        }
    }
    pub fn may_check(&mut self, kind: isize) -> bool {
        kind == 0
            && self.phase == Phase::Checking
            && !self.cancelled
            && !self.quarantine
            && std::mem::take(&mut self.permit)
    }
    pub fn found(&mut self, version: String, notes: String) -> bool {
        if !self.busy
            || self.cancelled
            || self.quarantine
            || self.phase != Phase::Checking
            || version.is_empty()
            || version.len() > 128
            || version.chars().any(char::is_control)
            || notes.len() > NOTES_LIMIT
        {
            return false;
        }
        self.phase = Phase::Found;
        self.version = Some(version);
        self.notes = notes;
        true
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.permit = false;
        if !self.quarantine {
            self.phase = Phase::Cancelled;
        }
    }
    pub fn fail(&mut self) {
        self.permit = false;
        if self.busy && !self.cancelled && !self.quarantine {
            self.phase = Phase::Failed;
        }
    }
    pub fn not_found(&mut self) {
        if self.busy && self.phase == Phase::Checking && !self.cancelled && !self.quarantine {
            self.phase = Phase::NoUpdate;
        }
    }
    pub fn finished(&mut self) {
        self.busy = false;
        self.permit = false;
    }
    pub fn quarantine(&mut self) {
        self.quarantine = true;
        self.permit = false;
        self.phase = Phase::Quarantined;
        self.notice =
            "App update checking stopped safely. Restart PeerBrush before checking again.";
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparkle::PROPOSED_FEED_URL;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde_json::json;
    #[test]
    fn current_compiled_configuration_is_missing_without_native_policy_or_activation() {
        assert!(compiled().unwrap().is_none());
        assert!(configuration(
            br#"{"schema":1,"feed_url":null,"public_ed_key":null,"publisher_team_id":null}"#,
            b"not-read",
            "development"
        )
        .unwrap()
        .is_none());
        let pins = json!({"schema":1,"feed_url":PROPOSED_FEED_URL,"public_ed_key":STANDARD.encode([1;32]),"publisher_team_id":"ABCD123456"});
        assert!(configuration(
            &serde_json::to_vec(&pins).unwrap(),
            include_bytes!("../../packaging/sparkle/native-policy.json"),
            "0.30.0"
        )
        .is_err());
        assert!(configuration(&serde_json::to_vec(&pins).unwrap(), b"{}", "0.30.0-dev.1").is_err());
    }
    #[test]
    fn only_an_owned_manual_check_gets_one_permission_and_cancel_discards_late_results() {
        let mut state = Session::default();
        assert!(!state.may_check(0));
        state.begin().unwrap();
        assert!(state.begin().is_err());
        state.checking();
        assert!(!state.may_check(1));
        assert!(!state.may_check(2));
        assert!(state.may_check(0));
        assert!(!state.may_check(0));
        state.cancel();
        state.not_found();
        assert_eq!(state.phase, Phase::Cancelled);
        assert!(!state.found("0.30.1".into(), "late".into()));
        state.finished();
        state.begin().unwrap();
        state.checking();
        assert!(state.found("0.30.1".into(), "Native16 notes".into()));
        assert_eq!(state.version.as_deref(), Some("0.30.1"));
        assert_eq!(state.notes, "Native16 notes");
        state.finished();
        assert_eq!(state.phase, Phase::Found);
        state.begin().unwrap();
        state.checking();
        state.not_found();
        assert_eq!(state.phase, Phase::NoUpdate);
        state.finished();
        state.begin().unwrap();
        state.fail();
        state.finished();
        assert_eq!(state.phase, Phase::Failed);
        state.begin().unwrap();
        state.quarantine();
        assert!(!state.notice.is_empty());
        state.finished();
        assert!(state.begin().is_err());
    }
    #[test]
    fn complete_public_pins_require_exact_reviewed_objects_and_stable_version() {
        let pins = json!({"schema":1,"feed_url":PROPOSED_FEED_URL,"public_ed_key":STANDARD.encode([1;32]),"publisher_team_id":"ABCD123456"});
        let objects: BTreeMap<_, _> = OBJECTS.into_iter().enumerate().map(|(index,(name,_))| (name,json!({"identifier":format!("org.synthetic.{index}"),"unsigned_sha256":"a".repeat(64),"entitlements":{}}))).collect();
        let mut policy = json!({"schema":1,"archive_sha256":serde_json::from_slice::<Value>(include_bytes!("../../packaging/sparkle/sparkle-2.10.0.json")).unwrap()["archive_sha256"],"objects":objects});
        let approved = configuration(
            &serde_json::to_vec(&pins).unwrap(),
            &serde_json::to_vec(&policy).unwrap(),
            "0.30.0",
        )
        .unwrap()
        .unwrap();
        assert_eq!(approved.feed_url, PROPOSED_FEED_URL);
        assert_eq!(approved.public_ed_key, STANDARD.encode([1; 32]));
        assert_eq!(approved.publisher_team_id, "ABCD123456");
        assert_eq!(approved.objects.len(), 5);
        assert_eq!(approved.objects[OBJECTS[0].0].identifier, "org.synthetic.0");
        assert_eq!(
            approved.objects[OBJECTS[0].0].unsigned_sha256,
            "a".repeat(64)
        );
        assert!(approved.objects[OBJECTS[0].0].entitlements.is_empty());
        assert_eq!(SEALS.len(), 4);
        policy["objects"][OBJECTS[0].0]["entitlements"] = json!({"unexpected":true});
        assert!(configuration(
            &serde_json::to_vec(&pins).unwrap(),
            &serde_json::to_vec(&policy).unwrap(),
            "0.30.0"
        )
        .is_err());
    }
    #[test]
    fn release_notes_are_bounded_plain_text() {
        let mut state = Session::default();
        state.begin().unwrap();
        state.checking();
        assert!(!state.found("0.30.1".into(), "x".repeat(NOTES_LIMIT + 1)));
    }
}
