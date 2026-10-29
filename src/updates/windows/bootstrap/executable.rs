//! Executable adapter; no runtime offer paths or URLs.
use super::*;

#[cfg(windows)]
struct Platform<'a> {
    permit: Option<&'a Permit>,
}
#[cfg(windows)]
impl Deployment for Platform<'_> {
    fn absent(&self, policy: &Policy, control: &Control) -> Result<bool> {
        if crate::packaged::package_family()?.is_some() {
            return Err(HOLD.into());
        }
        native::setup_absent(policy, control)
    }
    fn submit(
        &self,
        files: &Files,
        offer: &Offer,
        control: &Control,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<Box<dyn StageOperation>> {
        native::submit_setup(
            self.permit.ok_or(HOLD)?,
            &files.package,
            &files.runtime,
            &offer.policy,
            &offer.selection,
            &offer.target,
            control,
            guard,
        )
    }
    fn installed(&self, offer: &Offer, control: &Control) -> Result<bool> {
        match native::observe_registered(&offer.policy, control)? {
            Some(package) => {
                package.prove(&offer.target, control)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}
#[cfg(windows)]
struct SignedMedia;
#[cfg(windows)]
impl Verifier for SignedMedia {
    fn source(&self, _: &Policy) -> Result<Source> {
        Err(HOLD.into())
    }
    fn media(
        &self,
        file: &Held,
        policy: &Policy,
        selection: &Selection,
        runtime: bool,
        control: &Control,
    ) -> Result<Option<Target>> {
        native::inspect_media(file, policy, selection, runtime, control)
    }
}

// Compiled only into the feature's test executable. The ignored caller checks
// the disposable hosted account, explicit fixture approval, exact offer digest
// and clean stable source before entering this path. No production flag changes.
#[cfg(all(test, windows))]
pub(super) fn qualify_fixture(offer: &Offer, root: &Path, control: &Control) -> Result<bool> {
    let build: serde_json::Value =
        serde_json::from_str(crate::build_info::json()).map_err(|_| HOLD)?;
    if !offer.matches_build(&build) {
        return Err(HOLD.into());
    }
    let permit = Permit { _private: () };
    native::inspection_session(|| {
        let platform = Platform {
            permit: Some(&permit),
        };
        if !platform.absent(&offer.policy, control)? {
            return Err(HOLD.into());
        }
        let mut files = Files::prepare(root, offer, &Http::new(), control)?;
        install(&mut files, offer, &SignedMedia, &platform, control)
    })
}

pub(super) fn entry(embedded: Option<&str>) -> i32 {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let build: serde_json::Value = match serde_json::from_str(crate::build_info::json()) {
        Ok(value) => value,
        Err(_) => return 1,
    };
    let parsed = embedded.map(|s| Offer::parse(s.as_bytes())).transpose();
    let run = || -> Result<()> {
        let offer = parsed?;
        if args.len() == 1 && args[0] == "--setup-info" {
            let value = serde_json::json!({"schema":1,"format":"PeerBrushWindowsSetupInfoV1","build":build,
                "offer_sha256":offer.as_ref().map(Offer::digest).transpose()?,"installation_enabled":INSTALL_ENABLED,
                "installed_acceptance":"not-established","update_eligible":false,"ready":false});
            println!("{}", serde_json::to_string(&value).map_err(|_| HOLD)?);
            return Ok(());
        }
        let offer = offer.ok_or(DISABLED)?;
        if !offer.matches_build(&build) {
            return Err("Setup source and fixed offer differ.".into());
        }
        #[cfg(windows)]
        {
            if !cfg!(target_arch = "x86_64") {
                return Err("Setup requires the reviewed Windows x64 route.".into());
            }
            let control = Control {
                cancelled: Arc::new(AtomicBool::new(false)),
                deadline: Instant::now() + TIMEOUT,
                view: Arc::new(Mutex::new(View::default())),
            };
            if args.len() == 1 && args[0] == "--verify-installed" {
                return native::inspection_session(|| {
                    println!(
                        "{}",
                        serde_json::to_string(&observe(
                            &offer,
                            &Platform { permit: None },
                            &control
                        )?)
                        .map_err(|_| HOLD)?
                    );
                    Ok(())
                });
            }
            if !args.is_empty() || !INSTALL_ENABLED {
                return Err(DISABLED.into());
            }
            let consent = rfd::MessageDialog::new().set_title("Install PeerBrush")
                .set_description(format!("Install PeerBrush {} for this Windows account?\n\nPublisher: {}\nDownload: {} MiB\n\nThis registers Start menu and Open with entries. Default editors remain your choice. No project will be opened.", offer.version, offer.policy.publisher_display, (offer.package.bytes + offer.runtime.bytes).div_ceil(1024 * 1024)))
                .set_buttons(rfd::MessageButtons::OkCancel).show();
            if consent != rfd::MessageDialogResult::Ok {
                return Ok(());
            }
            let permit = Permit { _private: () };
            native::inspection_session(|| {
                let platform = Platform {
                    permit: Some(&permit),
                };
                if !platform.absent(&offer.policy, &control)? {
                    return Err(
                        "An installation already exists. Use its own update channel.".into(),
                    );
                }
                let root = crate::settings::Locations::user()?
                    .cache
                    .join("windows-setup-v1");
                eprintln!("Downloading and verifying the selected PeerBrush installation.");
                let mut files = Files::prepare(&root, &offer, &Http::new(), &control)?;
                eprintln!("Installing PeerBrush. Keep this setup process open until it finishes.");
                if !install(&mut files, &offer, &SignedMedia, &platform, &control)? {
                    return Err(HOLD.into());
                }
                rfd::MessageDialog::new()
                    .set_title("PeerBrush installed")
                    .set_description(
                        "Open PeerBrush from Start. Your default editor has not changed.",
                    )
                    .show();
                Ok(())
            })
        }
        #[cfg(not(windows))]
        {
            let _ = offer;
            Err("This setup executable requires Windows x64.".into())
        }
    };
    match run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("PeerBrush setup: {error}");
            #[cfg(windows)]
            if args.is_empty() {
                rfd::MessageDialog::new()
                    .set_title("PeerBrush setup did not complete")
                    .set_description(&error)
                    .set_level(rfd::MessageLevel::Error)
                    .show();
            }
            1
        }
    }
}
