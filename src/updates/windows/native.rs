//! Native package trust and dormant exact local deployment. Activation stays gated.
use super::*;
use ::windows::{
    core::{w, Interface, PCWSTR, PWSTR},
    ApplicationModel::{AppInstallerInfo, IPackage6, Package, PackageSignatureKind},
    Foundation::{AsyncOperationProgressHandler, AsyncStatus, IAsyncOperationWithProgress, Uri},
    Management::Deployment::{
        AddPackageOptions, DeploymentOptions, DeploymentProgress, DeploymentResult, PackageManager,
        StagePackageOptions,
    },
    Win32::{
        Foundation::HANDLE,
        Security::{
            Cryptography::{
                CertNameToStrW, CERT_NAME_STR_REVERSE_FLAG, CERT_STRING_TYPE, CERT_X500_NAME_STR,
                X509_ASN_ENCODING,
            },
            WinTrust::*,
        },
        Storage::Packaging::Appx::*,
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IStream,
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, STGM_READ, STGM_SHARE_DENY_WRITE,
            STREAM_SEEK_SET,
        },
        UI::Shell::SHCreateStreamOnFileEx,
    },
};
use std::{
    collections::BTreeSet,
    mem::size_of,
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
};

fn native<T>(result: ::windows::core::Result<T>) -> Result<T> {
    result.map_err(|_| {
        "Native Windows package verification could not complete; retained state needs review."
            .into()
    })
}
pub(super) fn session<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    if !ENABLED {
        return Err(DISABLED.into());
    }
    unsafe {
        native(CoInitializeEx(None, COINIT_MULTITHREADED).ok())?;
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    let _apartment = Apartment;
    work()
}

pub(super) struct Verifier;
// Distinguish native successful absence from a failed association query.
fn association(package: &Package) -> Result<Option<AppInstallerInfo>> {
    let interface: IPackage6 = native(package.cast())?;
    let mut pointer = std::ptr::null_mut();
    native(unsafe {
        (Interface::vtable(&interface).GetAppInstallerInfo)(
            Interface::as_raw(&interface),
            &mut pointer,
        )
        .ok()
    })?;
    Ok(if pointer.is_null() {
        None
    } else {
        Some(unsafe { AppInstallerInfo::from_raw(pointer) })
    })
}
impl super::Verifier for Verifier {
    fn source(&self, policy: &Policy) -> Result<Source> {
        if !ENABLED {
            return Err(DISABLED.into());
        }
        if !cfg!(target_arch = "x86_64") {
            return Err(
                "Signed Windows preparation is limited to the reviewed x64 package route.".into(),
            );
        }
        policy.validate()?;
        let executing = native(Package::Current())?;
        let executing_name = native(native(executing.Id())?.FullName())?;
        let current = native(
            native(PackageManager::new())?.FindPackageByUserSecurityIdPackageFullName(
                &::windows::core::HSTRING::new(),
                &executing_name,
            ),
        )?;
        let kind = native(current.SignatureKind())?;
        if (kind != PackageSignatureKind::Developer && kind != PackageSignatureKind::Enterprise)
            || native(current.IsFramework())?
            || native(current.IsResourcePackage())?
            || native(current.IsOptional())?
            || native(current.IsDevelopmentMode())?
        {
            return Err("Signed Windows preparation requires this exact non-Store App Installer main package.".into());
        }
        let id = native(current.Id())?;
        let info = association(&current)?;
        let version = native(id.Version())?;
        let build: serde_json::Value = serde_json::from_str(crate::build_info::json())
            .map_err(|_| "Cannot identify this Windows build.")?;
        let commit = build["commit"].as_str().unwrap_or_default();
        if native(id.FullName())? != executing_name
            || build["tree"] != "clean"
            || commit.len() != 40
            || !commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || native(id.Name())?.to_string() != policy.name
            || native(id.Publisher())?.to_string() != policy.publisher
            || native(id.FamilyName())?.to_string() != policy.family
        {
            return Err(
                "Installed Windows source, identity or approved association differs.".into(),
            );
        }
        let version = [
            version.Major,
            version.Minor,
            version.Build,
            version.Revision,
        ];
        let (association, route) = if let Some(info) = info {
            (
                native(native(info.Uri())?.AbsoluteUri())?.to_string(),
                Route::AppInstaller,
            )
        } else {
            (
                format!(
                    "{}PeerBrush-{}-Windows-x64.appinstaller",
                    policy.archive_root,
                    version_text(version)
                ),
                Route::VerifiedLocal,
            )
        };
        let source = Source {
            full_name: native(id.FullName())?.to_string(),
            family: policy.family.clone(),
            version,
            association,
            build: commit.into(),
            route,
        };
        policy.source_descriptor(&source)?;
        Ok(source)
    }
    fn media(
        &self,
        file: &Held,
        policy: &Policy,
        selected: &Selection,
        runtime: bool,
        control: &Control,
    ) -> Result<Option<Target>> {
        if !ENABLED {
            return Err(DISABLED.into());
        }
        control.check()?;
        file.check(control)?;
        let target = unsafe { verify(file, policy, selected, runtime, control)? };
        file.check(control)?;
        Ok(target)
    }
    fn current_media(
        &self,
        file: &Held,
        policy: &Policy,
        current: &Source,
        control: &Control,
    ) -> Result<Target> {
        if !ENABLED {
            return Err(DISABLED.into());
        }
        control.check()?;
        if &self.source(policy)? != current {
            return Err(STAGE_HOLD.into());
        }
        let target = self.retained_current_media(file, policy, current, control)?;
        if &self.source(policy)? != current {
            return Err(STAGE_HOLD.into());
        }
        Ok(target)
    }
    fn retained_current_media(
        &self,
        file: &Held,
        policy: &Policy,
        current: &Source,
        control: &Control,
    ) -> Result<Target> {
        if !ENABLED {
            return Err(DISABLED.into());
        }
        control.check()?;
        policy.source_descriptor(current)?;
        file.check(control)?;
        let target = unsafe { verify_version(file, policy, current.version, false, control)? }
            .ok_or(STAGE_HOLD)?;
        if !target.matches(current, policy) {
            return Err(STAGE_HOLD.into());
        }
        file.check(control)?;
        control.check()?;
        Ok(target)
    }
    fn running_target(
        &self,
        policy: &Policy,
        target: &Target,
        control: &Control,
    ) -> Result<RunningTarget> {
        if !ENABLED {
            return Err(DISABLED.into());
        }
        let current = self.source(policy)?;
        if !target.matches(&current, policy) {
            return Err(STAGE_HOLD.into());
        }
        let executing = std::env::current_exe().map_err(|_| STAGE_HOLD)?;
        let root = PathBuf::from(native(native(Package::Current())?.InstalledPath())?.to_string());
        let running = RunningTarget::open(&root, &executing, current, target, control)?;
        if self.source(policy)? != running.current {
            return Err(STAGE_HOLD.into());
        }
        Ok(running)
    }
}

fn staging<T>(value: ::windows::core::Result<T>) -> Result<T> {
    value.map_err(|_| STAGE_HOLD.into())
}
fn local_uri(path: &Path) -> Result<Uri> {
    let value = url::Url::from_file_path(path).map_err(|_| STAGE_HOLD)?;
    if !path.is_absolute()
        || value.host_str().is_some()
        || value.to_file_path().map_err(|_| STAGE_HOLD)? != path
    {
        return Err(STAGE_HOLD.into());
    }
    staging(Uri::CreateUri(&::windows::core::HSTRING::from(
        value.as_str(),
    )))
}
pub(super) struct Stager;
struct Stage {
    operation: IAsyncOperationWithProgress<DeploymentResult, DeploymentProgress>,
    _progress: AsyncOperationProgressHandler<DeploymentResult, DeploymentProgress>,
    _manager: PackageManager,
    progress: Arc<StageProgress>,
}
impl super::Stager for Stager {
    fn submit(
        &self,
        selected: &RetainedSelection,
        view: Arc<Mutex<View>>,
    ) -> Result<Box<dyn StageOperation>> {
        if !ENABLED {
            return Err(DISABLED.into());
        }
        let main = local_uri(&selected.package.path)?;
        let runtime = local_uri(&selected.runtime.path)?;
        let manager = staging(PackageManager::new())?;
        let options = staging(StagePackageOptions::new())?;
        staging(options.SetAllowUnsigned(false))?;
        staging(options.SetDeveloperMode(false))?;
        staging(options.SetForceUpdateFromAnyVersion(false))?;
        staging(options.SetStageInPlace(false))?;
        staging(staging(options.DependencyPackageUris())?.Append(&runtime))?;
        let state = Arc::new(StageProgress::new(view));
        let callback_state = state.clone();
        let progress =
            AsyncOperationProgressHandler::new(move |_, progress: &DeploymentProgress| {
                callback_state.percentage(progress.percentage);
                Ok(())
            });
        // All other fallible setup precedes submission. A callback-registration
        // failure afterwards returns an owned operation to quarantine/poll.
        let operation = staging(manager.StagePackageByUriAsync(&main, &options))?;
        if operation.SetProgress(&progress).is_err() {
            state.invalidate();
        }
        Ok(Box::new(Stage {
            operation,
            _progress: progress,
            _manager: manager,
            progress: state,
        }))
    }
}
impl StageOperation for Stage {
    fn poll(&mut self) -> StageObservation {
        self.progress.poll(|| match self.operation.Status() {
            Ok(AsyncStatus::Started) => StageObservation::Pending,
            Ok(AsyncStatus::Completed) => {
                let terminal = match self.operation.GetResults() {
                    Ok(result) => StageTerminal {
                        hresult: result.ExtendedErrorCode().ok().map(|v| v.0),
                        registered: result.IsRegistered().ok(),
                        canceled: false,
                        invalid_progress: false,
                    },
                    Err(error) => StageTerminal {
                        hresult: Some(error.code().0),
                        registered: None,
                        canceled: false,
                        invalid_progress: false,
                    },
                };
                StageObservation::Terminal(terminal)
            }
            Ok(status) if status == AsyncStatus::Canceled || status == AsyncStatus::Error => {
                StageObservation::Terminal(StageTerminal {
                    hresult: self.operation.ErrorCode().ok().map(|value| value.0),
                    registered: None,
                    canceled: status == AsyncStatus::Canceled,
                    invalid_progress: false,
                })
            }
            _ => StageObservation::Uncertain,
        })
    }
    fn cancel(&mut self) {
        let _ = self.operation.Cancel();
    }
}

/// Called only by the surviving owner after its final-seal/kernel-exit guards.
/// The descriptor is evidence, never an installer input. No remote URI enters
/// this API, and all submitted package bytes remain held until native terminal.
pub(super) fn submit_local(
    retained: &RetainedSelection,
    role: transaction::Role,
    control: &Control,
) -> Result<Box<dyn StageOperation>> {
    if !ENABLED {
        return Err(DISABLED.into());
    }
    if retained.prepared.source.route != Route::VerifiedLocal {
        return Err(
            "Legacy App Installer association requires separate migration qualification.".into(),
        );
    }
    retained.check_media(control)?;
    let verified = super::Verifier::media(
        &Verifier,
        &retained.package,
        &retained.prepared.policy,
        &retained.prepared.selection,
        false,
        control,
    )?;
    if verified != retained.target || verified.is_none() {
        return Err(STAGE_HOLD.into());
    }
    let main = local_uri(&retained.package.path)?;
    let mut dependencies = vec![];
    if role == transaction::Role::Selected {
        super::Verifier::media(
            &Verifier,
            &retained.runtime,
            &retained.prepared.policy,
            &retained.prepared.selection,
            true,
            control,
        )?;
        dependencies.push(Some(local_uri(&retained.runtime.path)?));
    }
    let dependencies: ::windows::Foundation::Collections::IIterable<Uri> =
        staging(dependencies.try_into())?;
    let manager = staging(PackageManager::new())?;
    let mut state = StageProgress::new(control.view.clone());
    state.phase = if role == transaction::Role::Selected {
        "Applying verified local Windows package"
    } else {
        "Restoring verified previous Windows package"
    };
    let state = Arc::new(state);
    let callback = state.clone();
    let progress = AsyncOperationProgressHandler::new(move |_, progress: &DeploymentProgress| {
        callback.percentage(progress.percentage);
        Ok(())
    });

    // Never force-shut down applications, defer registration, allow unsigned
    // packages or downgrade a shared dependency. Rollback submits only the
    // retained previous main package; its runtime was required by the old app.
    retained.check_media(control)?;
    let operation = if role == transaction::Role::Previous {
        // The older AddPackageAsync overload does not document this flag.
        // This 19041 API admits it explicitly; its URI is still only our held
        // local MSIX, never HTTP or an App Installer descriptor.
        let options = staging(AddPackageOptions::new())?;
        staging(options.SetAllowUnsigned(false))?;
        staging(options.SetDeveloperMode(false))?;
        staging(options.SetForceAppShutdown(false))?;
        staging(options.SetForceTargetAppShutdown(false))?;
        staging(options.SetDeferRegistrationWhenPackagesAreInUse(false))?;
        staging(options.SetForceUpdateFromAnyVersion(true))?;
        retained.check_media(control)?;
        staging(manager.AddPackageByUriAsync(&main, &options))?
    } else {
        staging(manager.AddPackageAsync(&main, &dependencies, DeploymentOptions::None))?
    };
    if operation.SetProgress(&progress).is_err() {
        state.invalidate();
    }
    Ok(Box::new(Stage {
        operation,
        _progress: progress,
        _manager: manager,
        progress: state,
    }))
}

pub(super) struct Registered {
    pub(super) full_name: String,
    pub(super) root: PathBuf,
    version: [u16; 4],
}
impl Registered {
    pub(super) fn prove(&self, target: &Target, control: &Control) -> Result<()> {
        if self.full_name != target.full_name || self.version != target.version {
            return Err(STAGE_HOLD.into());
        }
        let directory = Directory::open(&self.root)?;
        let executable = Held::open(&self.root.join("peerbrush.exe"), PACKAGE, control)?;
        let source = Held::open(&self.root.join(&target.source_name), PACKAGE, control)?;
        if executable.media != target.executable || source.media != target.source {
            return Err(STAGE_HOLD.into());
        }
        directory.check()?;
        executable.check(control)?;
        source.check(control)
    }
}
pub(super) fn registered(policy: &Policy, control: &Control) -> Result<Option<Registered>> {
    if !ENABLED {
        return Err(DISABLED.into());
    }
    policy.validate()?;
    control.check()?;
    let manager = native(PackageManager::new())?;
    let packages = native(manager.FindPackagesByUserSecurityIdPackageFamilyName(
        &::windows::core::HSTRING::new(),
        &::windows::core::HSTRING::from(&policy.family),
    ))?;
    let iterator = native(packages.First())?;
    let mut result = None;
    let mut count = 0;
    while native(iterator.HasCurrent())? {
        count += 1;
        if count > 16 {
            return Err(STAGE_HOLD.into());
        }
        let package = native(iterator.Current())?;
        if !native(package.IsFramework())?
            && !native(package.IsResourcePackage())?
            && !native(package.IsOptional())?
        {
            let id = native(package.Id())?;
            let kind = native(package.SignatureKind())?;
            if result.is_some()
                || native(package.IsDevelopmentMode())?
                || (kind != PackageSignatureKind::Developer
                    && kind != PackageSignatureKind::Enterprise)
                || association(&package)?.is_some()
                || !native(native(package.Status())?.VerifyIsOK())?
                || native(id.Name())?.to_string() != policy.name
                || native(id.Publisher())?.to_string() != policy.publisher
                || native(id.FamilyName())?.to_string() != policy.family
            {
                return Err(STAGE_HOLD.into());
            }
            let version = native(id.Version())?;
            result = Some(Registered {
                full_name: native(id.FullName())?.to_string(),
                root: PathBuf::from(native(package.InstalledPath())?.to_string()),
                version: [
                    version.Major,
                    version.Minor,
                    version.Build,
                    version.Revision,
                ],
            });
        }
        native(iterator.MoveNext())?;
    }
    control.check()?;
    Ok(result)
}

pub(super) struct HelperFiles {
    pub(super) inventory: std::collections::BTreeMap<String, Media>,
    pub(super) held: Vec<Held>,
}
fn helper_name(name: &str, runtime: bool) -> Result<Option<String>> {
    if name.contains(['/', '\\']) {
        return Ok(None);
    }
    let name = name.to_ascii_lowercase();
    if name != "peerbrush.exe" && !name.ends_with(".dll") || runtime && name == "peerbrush.exe" {
        return Ok(None);
    }
    let stem = name.split('.').next().unwrap_or_default();
    if name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || name.starts_with('.')
        || ["con", "prn", "aux", "nul"].contains(&stem)
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(STAGE_HOLD.into());
    }
    Ok(Some(name))
}
/// The copied owner cannot depend on the old package graph surviving replacement.
/// Its executable and local DLLs come only from the held signed CURRENT/runtime.
/// No optional payload, arbitrary path or caller-provided DLL is accepted.
pub(super) fn helper_files(
    current: &RetainedSelection,
    destination: Option<&Path>,
    control: &Control,
) -> Result<HelperFiles> {
    if !ENABLED {
        return Err(DISABLED.into());
    }
    current.check_media(control)?;
    let target = super::Verifier::retained_current_media(
        &Verifier,
        &current.package,
        &current.prepared.policy,
        &current.prepared.source,
        control,
    )?;
    if current.target.as_ref() != Some(&target)
        || current.prepared.source.route != Route::VerifiedLocal
    {
        return Err(STAGE_HOLD.into());
    }
    super::Verifier::media(
        &Verifier,
        &current.runtime,
        &current.prepared.policy,
        &current.prepared.selection,
        true,
        control,
    )?;
    let mut inventory = std::collections::BTreeMap::new();
    let mut held_files = vec![];
    let mut total = 0u64;
    for (file, runtime) in [(&current.package, false), (&current.runtime, true)] {
        let path: Vec<u16> = file.path.as_os_str().encode_wide().chain(Some(0)).collect();
        let stream = unsafe {
            native(SHCreateStreamOnFileEx(
                PCWSTR(path.as_ptr()),
                (STGM_READ | STGM_SHARE_DENY_WRITE).0,
                0,
                false,
                None,
            ))?
        };
        if unsafe { stream_hash(&stream, control)? } != file.media {
            return Err(STAGE_HOLD.into());
        }
        unsafe {
            native(stream.Seek(0, STREAM_SEEK_SET, None))?;
        }
        let factory: IAppxFactory =
            unsafe { native(CoCreateInstance(&AppxFactory, None, CLSCTX_INPROC_SERVER))? };
        let reader = unsafe { native(factory.CreatePackageReader(&stream))? };
        let files = unsafe { native(reader.GetPayloadFiles())? };
        while unsafe { native(files.GetHasCurrent())?.as_bool() } {
            let payload = unsafe { native(files.GetCurrent())? };
            let name = unsafe { string(payload.GetName())? };
            if let Some(name) = helper_name(&name, runtime)? {
                let data = unsafe { native(payload.GetStream())? };
                let media = unsafe { stream_hash(&data, control)? };
                if media.bytes == 0 || media.bytes > 64 * 1024 * 1024 {
                    return Err(STAGE_HOLD.into());
                }
                if let Some(old) = inventory.get(&name) {
                    if old != &media {
                        return Err(STAGE_HOLD.into());
                    }
                } else {
                    total = total.checked_add(media.bytes).ok_or(STAGE_HOLD)?;
                    if total > 256 * 1024 * 1024 || inventory.len() >= 64 {
                        return Err(STAGE_HOLD.into());
                    }
                    if let Some(root) = destination {
                        let path = root.join(&name);
                        let mut output = OpenOptions::new()
                            .create_new(true)
                            .write(true)
                            .share_mode(1)
                            .open(&path)
                            .map_err(|_| STAGE_HOLD)?;
                        let identity = crate::file_safety::identity(&output)?;
                        unsafe {
                            native(data.Seek(0, STREAM_SEEK_SET, None))?;
                        }
                        let mut bytes = 0u64;
                        let mut block = [0; 64 * 1024];
                        loop {
                            control.check()?;
                            let mut count = 0;
                            unsafe {
                                native(
                                    data.Read(
                                        block.as_mut_ptr() as _,
                                        block.len() as u32,
                                        Some(&mut count),
                                    )
                                    .ok(),
                                )?;
                            }
                            if count as usize > block.len()
                                || bytes + u64::from(count) > media.bytes
                            {
                                return Err(STAGE_HOLD.into());
                            }
                            if count == 0 {
                                break;
                            }
                            output
                                .write_all(&block[..count as usize])
                                .map_err(|_| STAGE_HOLD)?;
                            bytes += u64::from(count);
                        }
                        if bytes != media.bytes {
                            return Err(STAGE_HOLD.into());
                        }
                        output.sync_all().map_err(|_| STAGE_HOLD)?;
                        drop(output);
                        let copied = Held::open(&path, 64 * 1024 * 1024, control)?;
                        if copied.identity != identity || copied.media != media {
                            return Err(STAGE_HOLD.into());
                        }
                        held_files.push(copied);
                    }
                    inventory.insert(name, media);
                }
            }
            unsafe {
                native(files.MoveNext())?;
            }
        }
        if unsafe { stream_hash(&stream, control)? } != file.media {
            return Err(STAGE_HOLD.into());
        }
    }
    if inventory.get("peerbrush.exe") != Some(&target.executable) {
        return Err(STAGE_HOLD.into());
    }
    current.check_media(control)?;
    Ok(HelperFiles {
        inventory,
        held: held_files,
    })
}

// Provider state owns all certificate/signer pointers until STATEACTION_CLOSE.
struct Trust(WINTRUST_DATA);
impl Drop for Trust {
    fn drop(&mut self) {
        self.0.dwStateAction = WTD_STATEACTION_CLOSE;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        unsafe {
            WinVerifyTrust(
                ::windows::Win32::Foundation::HWND(-1isize as _),
                &mut action,
                &mut self.0 as *mut _ as _,
            );
        }
    }
}
unsafe fn signature(file: &Held, certificate: &str) -> Result<String> {
    let path: Vec<u16> = file.path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut input = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(path.as_ptr()),
        hFile: HANDLE(file.file.as_raw_handle()),
        ..Default::default()
    };
    let mut settings = WINTRUST_SIGNATURE_SETTINGS {
        cbStruct: size_of::<WINTRUST_SIGNATURE_SETTINGS>() as u32,
        dwFlags: WINTRUST_SIGNATURE_SETTINGS_FLAGS(
            WSS_VERIFY_SPECIFIC.0 | WSS_GET_SECONDARY_SIG_COUNT.0,
        ),
        ..Default::default()
    };
    let mut state = Trust(WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut input },
        dwStateAction: WTD_STATEACTION_VERIFY,
        pSignatureSettings: &mut settings,
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT | WTD_DISABLE_MD2_MD4 | WTD_MOTW,
        dwUIContext: WTD_UICONTEXT_INSTALL,
        ..Default::default()
    });
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // Microsoft explicitly supports GENERIC_VERIFY_V2 for Appx/MSIX packages.
    // WTD_UI_NONE; revocation is not weakened/offline-assumed. This synchronous
    // provider may contact the network: the owner retains one cancel quarantine.
    if WinVerifyTrust(
        ::windows::Win32::Foundation::HWND(-1isize as _),
        &mut action,
        &mut state.0 as *mut _ as _,
    ) != 0
        || settings.cSecondarySigs != 0
        || settings.dwVerifiedSigIndex != 0
    {
        return Err("Windows signature trust or exact embedded signature selection failed.".into());
    }
    let data = WTHelperProvDataFromStateData(state.0.hWVTStateData);
    if data.is_null()
        || (*data).cbStruct as usize != size_of::<CRYPT_PROVIDER_DATA>()
        || (*data).csSigners != 1
        || (*data).dwError != 0
        || (*data).dwFinalError != 0
    {
        return Err("Unsupported or ambiguous native signature provider result.".into());
    }
    let signer = WTHelperGetProvSignerFromChain(data, 0, false, 0);
    if signer.is_null()
        || (*signer).cbStruct as usize != size_of::<CRYPT_PROVIDER_SGNR>()
        || (*signer).dwError != 0
        || (*signer).csCertChain == 0
        || (*signer).csCertChain > 16
        || (*signer).csCounterSigners != 1
        || (*signer).psSigner.is_null()
        || !sha256_oid((*(*signer).psSigner).HashAlgorithm.pszObjId.0)
    {
        return Err("The signed package lacks one native verified signer and timestamp.".into());
    }
    let counter = WTHelperGetProvSignerFromChain(data, 0, true, 0);
    if counter.is_null()
        || (*counter).cbStruct as usize != size_of::<CRYPT_PROVIDER_SGNR>()
        || (*counter).dwError != 0
        || (*counter).dwSignerType != SGNR_TYPE_TIMESTAMP
        || (*counter).csCertChain == 0
        || (*counter).csCertChain > 16
        || (*counter).csCounterSigners != 0
        || (*counter).pChainContext.is_null()
        || (*counter).psSigner.is_null()
        || !sha256_oid((*(*counter).psSigner).HashAlgorithm.pszObjId.0)
        || (*counter).sftVerifyAsOf.dwLowDateTime == 0
            && (*counter).sftVerifyAsOf.dwHighDateTime == 0
        || (*(*counter).pChainContext).TrustStatus.dwErrorStatus != 0
    {
        return Err("A native trusted timestamp was not established.".into());
    }
    let leaf = WTHelperGetProvCertFromChain(signer, 0);
    if leaf.is_null() || (*leaf).pCert.is_null() {
        return Err("No verified native signing certificate.".into());
    }
    let cert = &*(*leaf).pCert;
    if cert.pbCertEncoded.is_null()
        || cert.cbCertEncoded == 0
        || cert.cbCertEncoded > 64 * 1024
        || hash(std::slice::from_raw_parts(
            cert.pbCertEncoded,
            cert.cbCertEncoded as usize,
        )) != certificate
    {
        return Err("Native signing certificate differs from the reviewed public policy.".into());
    }
    if cert.pCertInfo.is_null() {
        return Err("Verified certificate has no encoded subject.".into());
    }
    let flags = CERT_STRING_TYPE(CERT_X500_NAME_STR.0 | CERT_NAME_STR_REVERSE_FLAG);
    let count = CertNameToStrW(X509_ASN_ENCODING, &(*cert.pCertInfo).Subject, flags, None);
    if count < 2 || count > 4096 {
        return Err("Verified certificate subject exceeds its bound.".into());
    }
    let mut subject = vec![0u16; count as usize];
    if CertNameToStrW(
        X509_ASN_ENCODING,
        &(*cert.pCertInfo).Subject,
        flags,
        Some(&mut subject),
    ) != count
        || subject.last() != Some(&0)
        || subject[..subject.len() - 1].contains(&0)
    {
        return Err("Verified certificate subject could not be decoded exactly.".into());
    }
    String::from_utf16(&subject[..subject.len() - 1])
        .map_err(|_| "Verified certificate subject has invalid text.".into())
}
unsafe fn sha256_oid(value: *const u8) -> bool {
    let expected = b"2.16.840.1.101.3.4.2.1\0";
    !value.is_null()
        && expected
            .iter()
            .enumerate()
            .all(|(i, b)| *value.add(i) == *b)
}

unsafe fn string(value: ::windows::core::Result<PWSTR>) -> Result<String> {
    let value = native(value)?;
    if value.is_null() {
        return Err("Missing native package text.".into());
    }
    struct Owned(PWSTR);
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe {
                CoTaskMemFree(Some(self.0 .0 as _));
            }
        }
    }
    let owned = Owned(value);
    let mut length = 0;
    while length <= 4096 && *owned.0 .0.add(length) != 0 {
        length += 1;
    }
    if length > 4096 {
        return Err("Native package text exceeds its bound.".into());
    }
    String::from_utf16(std::slice::from_raw_parts(owned.0 .0, length))
        .map_err(|_| "Invalid native package text.".into())
}
unsafe fn stream_bytes(stream: &IStream, limit: u64, control: &Control) -> Result<Vec<u8>> {
    native(stream.Seek(0, STREAM_SEEK_SET, None))?;
    let mut bytes = Vec::new();
    let mut block = [0; 64 * 1024];
    loop {
        control.check()?;
        let mut count = 0u32;
        native(
            stream
                .Read(
                    block.as_mut_ptr() as _,
                    block.len() as u32,
                    Some(&mut count),
                )
                .ok(),
        )?;
        if count as usize > block.len() || bytes.len() as u64 + count as u64 > limit {
            return Err("Native package metadata exceeds its bound.".into());
        }
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&block[..count as usize]);
    }
    Ok(bytes)
}
unsafe fn stream_hash(stream: &IStream, control: &Control) -> Result<Media> {
    native(stream.Seek(0, STREAM_SEEK_SET, None))?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut bytes = 0u64;
    let mut block = [0; 64 * 1024];
    loop {
        control.check()?;
        let mut count = 0u32;
        native(
            stream
                .Read(
                    block.as_mut_ptr() as _,
                    block.len() as u32,
                    Some(&mut count),
                )
                .ok(),
        )?;
        if count as usize > block.len() {
            return Err("Invalid native stream length.".into());
        }
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .filter(|n| *n <= PACKAGE)
            .ok_or("Native package exceeds its bound.")?;
        digest.update(&block[..count as usize]);
    }
    Ok(Media {
        bytes,
        sha256: hex(digest.finish().as_ref()),
    })
}

fn canonical(xml: &str) -> Result<Vec<String>> {
    let document = roxmltree::Document::parse(xml).map_err(|_| "Invalid package manifest XML.")?;
    let mut result = Vec::new();
    for node in document.descendants() {
        if node.is_element() {
            let mut attributes = node
                .attributes()
                .map(|a| format!("{:?}:{}={}", a.namespace(), a.name(), a.value()))
                .collect::<Vec<_>>();
            attributes.sort();
            result.push(format!(
                "{}:{:?}:{}:{:?}",
                node.ancestors().count(),
                node.tag_name().namespace(),
                node.tag_name().name(),
                attributes
            ));
        } else if node.is_text() && !node.text().unwrap_or_default().trim().is_empty() {
            result.push(format!(
                "{}:{}",
                node.ancestors().count(),
                node.text().unwrap_or_default().trim()
            ));
        }
    }
    Ok(result)
}
fn manifest(bytes: &[u8], policy: &Policy, expected_version: [u16; 4]) -> Result<()> {
    let actual = std::str::from_utf8(bytes).map_err(|_| "Invalid native package manifest text.")?;
    let expected = include_str!("../../../packaging/msix/AppxManifest.xml.in")
        .replace("{{IDENTITY_NAME}}", &policy.name)
        .replace("{{PUBLISHER}}", &policy.publisher)
        .replace("{{PUBLISHER_DISPLAY_NAME}}", &policy.publisher_display)
        .replace("{{VERSION}}", &version_text(expected_version))
        .replace(
            "{{FILE_TYPES}}",
            "<uap:FileType>.psd</uap:FileType><uap:FileType>.psb</uap:FileType>",
        );
    if canonical(actual)? != canonical(&expected)? {
        return Err("The signed manifest changes the reviewed capabilities, associations or native runtime contract.".into());
    }
    Ok(())
}

unsafe fn verify(
    file: &Held,
    policy: &Policy,
    selected: &Selection,
    runtime: bool,
    control: &Control,
) -> Result<Option<Target>> {
    verify_version(
        file,
        policy,
        if runtime {
            selected.runtime_version
        } else {
            selected.version
        },
        runtime,
        control,
    )
}
unsafe fn verify_version(
    file: &Held,
    policy: &Policy,
    expected_version: [u16; 4],
    runtime: bool,
    control: &Control,
) -> Result<Option<Target>> {
    use std::io::{Seek, SeekFrom};
    policy.validate()?;
    let mut held = file
        .file
        .try_clone()
        .map_err(|_| "Cannot protect native signature input.")?;
    held.seek(SeekFrom::Start(0))
        .map_err(|_| "Cannot seek native signature input.")?;
    let signer_subject = signature(
        file,
        if runtime {
            &policy.runtime_certificate
        } else {
            &policy.certificate
        },
    )?;
    control.check()?;
    let path: Vec<u16> = file.path.as_os_str().encode_wide().chain(Some(0)).collect();
    let stream = native(SHCreateStreamOnFileEx(
        PCWSTR(path.as_ptr()),
        (STGM_READ | STGM_SHARE_DENY_WRITE).0,
        0,
        false,
        None,
    ))?;
    // A separately opened IStream must contain the same complete immutable bytes
    // as the held signature input; path/mtime equality is never authentication.
    if stream_hash(&stream, control)? != file.media {
        return Err("Native package reader input differs from the signed bytes.".into());
    }
    native(stream.Seek(0, STREAM_SEEK_SET, None))?;
    let factory: IAppxFactory = native(CoCreateInstance(&AppxFactory, None, CLSCTX_INPROC_SERVER))?;
    let reader = native(factory.CreatePackageReader(&stream))?;
    let metadata = native(reader.GetManifest())?;
    let id = native(metadata.GetPackageId())?;
    if !native(id.ComparePublisher(&::windows::core::HSTRING::from(signer_subject)))?.as_bool() {
        return Err(
            "Verified signing certificate subject differs from the native package Publisher."
                .into(),
        );
    }
    let (name, publisher, version) = if runtime {
        (
            &policy.runtime_name,
            &policy.runtime_publisher,
            expected_version,
        )
    } else {
        (&policy.name, &policy.publisher, expected_version)
    };
    let packed = version.into_iter().fold(0u64, |v, n| v << 16 | n as u64);
    if string(id.GetName())? != *name
        || string(id.GetPublisher())? != *publisher
        || native(id.GetArchitecture())? != APPX_PACKAGE_ARCHITECTURE_X64
        || native(id.GetVersion())? != packed
        || !string(id.GetResourceId())?.is_empty()
        || !runtime && string(id.GetPackageFamilyName())? != policy.family
    {
        return Err("Native signed package identity, architecture or version differs.".into());
    }
    let manifest_stream = native(metadata.GetStream())?;
    let manifest_bytes = stream_bytes(&manifest_stream, 4 * 1024 * 1024, control)?;
    if runtime {
        let props = native(metadata.GetProperties())?;
        if !native(props.GetBoolValue(w!("Framework")))?.as_bool() {
            return Err("Selected Windows runtime is not a signed framework package.".into());
        }
        if native(native(metadata.GetApplications())?.GetHasCurrent())?.as_bool() {
            return Err("The selected framework unexpectedly contains applications.".into());
        }
        if native(native(metadata.GetPackageDependencies())?.GetHasCurrent())?.as_bool() {
            return Err("The selected framework adds unreviewed package dependencies.".into());
        }
    } else {
        manifest(&manifest_bytes, policy, expected_version)?;
    }
    let blockmap = native(reader.GetBlockMap())?;
    if native(native(blockmap.GetHashMethod())?.GetAbsoluteUri())?.to_string()
        != "http://www.w3.org/2001/04/xmlenc#sha256"
    {
        return Err("The signed package blockmap does not use the reviewed SHA256 method.".into());
    }
    for kind in [
        APPX_FOOTPRINT_FILE_TYPE_MANIFEST,
        APPX_FOOTPRINT_FILE_TYPE_BLOCKMAP,
        APPX_FOOTPRINT_FILE_TYPE_SIGNATURE,
    ] {
        if native(native(reader.GetFootprintFile(kind))?.GetSize())? > 4 * 1024 * 1024 {
            return Err("Native package footprint exceeds its bound.".into());
        }
    }
    let block_manifest = native(blockmap.GetFile(w!("AppxManifest.xml")))?;
    native(manifest_stream.Seek(0, STREAM_SEEK_SET, None))?;
    if native(block_manifest.GetUncompressedSize())? != manifest_bytes.len() as u64
        || !native(block_manifest.ValidateFileHash(&manifest_stream))?.as_bool()
    {
        return Err("Signed manifest block hashes do not match.".into());
    }
    let files = native(reader.GetPayloadFiles())?;
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    while native(files.GetHasCurrent())?.as_bool() {
        control.check()?;
        let payload = native(files.GetCurrent())?;
        let name = string(payload.GetName())?;
        if name.is_empty()
            || name.len() > 1024
            || !name.is_ascii()
            || name.bytes().any(|b| b < 32 || b == 127)
            || name.contains('/')
            || name.contains(':')
            || name.split('\\').any(|s| {
                s.is_empty() || s == "." || s == ".." || s.ends_with('.') || s.ends_with(' ')
            })
            || !names.insert(name.to_ascii_lowercase())
            || names.len() > 4096
        {
            return Err("Unexpected or aliased native package payload path.".into());
        }
        let size = native(payload.GetSize())?;
        total = total
            .checked_add(size)
            .filter(|n| *n <= PACKAGE)
            .ok_or("Native package payload exceeds its aggregate bound.")?;
        let payload_stream = native(payload.GetStream())?;
        let map_file = native(blockmap.GetFile(&::windows::core::HSTRING::from(&name)))?;
        if native(map_file.GetUncompressedSize())? != size
            || !native(map_file.ValidateFileHash(&payload_stream))?.as_bool()
        {
            return Err(
                "Native package payload block hashes differ from the signed blockmap.".into(),
            );
        }
        native(files.MoveNext())?;
    }
    if names.is_empty() || !runtime && !names.contains("peerbrush.exe") {
        return Err("The signed native package has no expected application payload.".into());
    }
    let target = if !runtime {
        let release = native(reader.GetPayloadFile(w!("release.json")))?;
        if native(release.GetSize())? > SMALL {
            return Err("Signed release metadata exceeds its bound.".into());
        }
        let bytes = stream_bytes(&native(release.GetStream())?, SMALL, control)?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid signed release metadata.")?;
        let commit = value["commit"].as_str().unwrap_or_default();
        let application = if expected_version[0] == 1 && expected_version[3] == 0 {
            format!("0.{}.{}", expected_version[1], expected_version[2])
        } else {
            return Err("Unsupported signed application/package version mapping.".into());
        };
        if value["schema"] != 1
            || value["product"] != "PeerBrush"
            || value["version"] != application
            || value["platform"] != "windows"
            || value["architecture"] != "x64"
            || value["license"] != "GPL-3.0-only"
            || commit.len() != 40
            || !commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || value["binary_source_commit"] != commit
        {
            return Err(
                "Signed release metadata does not bind the selected native version/source/license."
                    .into(),
            );
        }
        let archive = value["source"].as_str().unwrap_or_default();
        if archive != format!("PeerBrush-{application}-source.zip")
            || !names.contains(&archive.to_ascii_lowercase())
        {
            return Err("Signed package omits its matching corresponding-source archive.".into());
        }
        let source_hash = value["source_sha256"].as_str().unwrap_or_default();
        let source_file = native(reader.GetPayloadFile(&::windows::core::HSTRING::from(archive)))?;
        let source = stream_hash(&native(source_file.GetStream())?, control)?;
        if !sha(source_hash) || source.sha256 != source_hash {
            return Err(
                "Signed corresponding-source archive differs from its declared digest.".into(),
            );
        }
        let executable = native(reader.GetPayloadFile(w!("peerbrush.exe")))?;
        let executable = stream_hash(&native(executable.GetStream())?, control)?;
        if value["executable"] != "peerbrush.exe"
            || value["executable_sha256"].as_str() != Some(executable.sha256.as_str())
        {
            return Err("Signed executable payload differs from its declared identity.".into());
        }
        Some(Target {
            full_name: string(id.GetPackageFullName())?,
            version: expected_version,
            build: commit.into(),
            executable,
            source_name: archive.into(),
            source,
        })
    } else {
        None
    };
    if stream_hash(&stream, control)? != file.media {
        return Err("Native reader bytes changed during verification.".into());
    }
    control.check()?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    #[test]
    fn copied_helper_inventory_rejects_aliases_and_unreviewed_paths() {
        use super::*;
        assert_eq!(
            helper_name("peerbrush.exe", false).unwrap(),
            Some("peerbrush.exe".into())
        );
        assert_eq!(
            helper_name("VCRUNTIME140.dll", true).unwrap(),
            Some("vcruntime140.dll".into())
        );
        for name in [
            "com1.dll",
            "LPT9.dll",
            "CON.dll",
            ".foreign.dll",
            "bad name.dll",
            "bad:.dll",
            "com\u{b9}.dll",
        ] {
            assert!(helper_name(name, false).is_err(), "{name}");
        }
        for name in [
            "nested/runtime.dll",
            "nested\\runtime.dll",
            "other.exe",
            "unrelated.txt",
        ] {
            assert!(helper_name(name, false).unwrap().is_none());
        }
        assert!(helper_name("peerbrush.exe", true).unwrap().is_none());
    }
    use super::*;
    #[test]
    fn exact_native_manifest_gate_preserves_capabilities_and_file_association_contract() {
        let policy = super::super::tests::policy();
        let selected = Selection {
            descriptor:
                "https://example.invalid/windows/PeerBrush-1.30.1.0-Windows-x64.appinstaller".into(),
            version: [1, 30, 1, 0],
            package: "https://example.invalid/windows/PeerBrush-1.30.1.0.msix".into(),
            runtime_version: [14, 0, 30704, 0],
        };
        let xml = include_str!("../../../packaging/msix/AppxManifest.xml.in")
            .replace("{{IDENTITY_NAME}}", &policy.name)
            .replace("{{PUBLISHER}}", &policy.publisher)
            .replace("{{PUBLISHER_DISPLAY_NAME}}", &policy.publisher_display)
            .replace("{{VERSION}}", "1.30.1.0")
            .replace(
                "{{FILE_TYPES}}",
                "<uap:FileType>.psd</uap:FileType><uap:FileType>.psb</uap:FileType>",
            );
        assert!(manifest(xml.as_bytes(), &policy, selected.version).is_ok());
        let current = xml.replace("1.30.1.0", "1.30.0.0");
        assert!(manifest(current.as_bytes(), &policy, [1, 30, 0, 0]).is_ok());
        assert!(manifest(current.as_bytes(), &policy, selected.version).is_err());
        for bad in [
            current.replace("runFullTrust", "packageManagement"),
            current.replace(".psb", ".png"),
            current.replace(&policy.publisher, "CN=Foreign"),
        ] {
            assert!(manifest(bad.as_bytes(), &policy, [1, 30, 0, 0]).is_err());
        }
        for bad in [
            xml.replace("runFullTrust", "packageManagement"),
            xml.replace("defer", "default"),
            xml.replace("1.30.1.0", "1.30.0.0"),
            xml.replace(".psb", ".png"),
            xml.replace("MinVersion=\"10.0.19041.0\"", "MinVersion=\"10.0.26100.0\""),
        ] {
            assert!(manifest(bad.as_bytes(), &policy, selected.version).is_err());
        }
        unsafe {
            assert!(sha256_oid(b"2.16.840.1.101.3.4.2.1\0".as_ptr()));
            assert!(!sha256_oid(b"1.3.14.3.2.26\0".as_ptr()));
            assert!(!sha256_oid(std::ptr::null()));
        }
    }
}
