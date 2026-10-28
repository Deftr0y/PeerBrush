//! Live owner context. Disk records preserve evidence; only authenticated process
//! participants and a final writer seal can cross the deployment boundary.
use super::*;
use std::collections::BTreeSet;

pub(super) struct Context {
    pub(super) selected: RetainedSelection,
    pub(super) previous: RetainedSelection,
    roots: Vec<Directory>,
    leases: Vec<crate::file_safety::Lease>,
    inventory: [Vec<PathBuf>; 2],
    stage: Vec<Held>,
    initial: profile::Copy,
    pub(super) intent: Option<Held>,
    pub(super) final_profile: Option<profile::Copy>,
    pub(super) seal: Option<Held>,
    records: Vec<Held>,
    record_directory: Option<Directory>,
}
impl Context {
    pub(super) fn load(
        root: &Path,
        policy: &Policy,
        verifier: &dyn Verifier,
        control: &Control,
        take_leases: bool,
    ) -> Result<Self> {
        let current = current_root(root)?;
        let mut roots = vec![Directory::open(&current)?, Directory::open(root)?];
        roots.push(Directory::open(root.parent().ok_or(STAGE_HOLD)?)?);
        roots.push(Directory::open(&current.join(".peerbrush-versions"))?);
        roots.push(Directory::open(&root.join(".peerbrush-versions"))?);
        let mut leases = vec![];
        if take_leases {
            leases.push(crate::file_safety::Lease::acquire(
                &current.join("acquisition.json"),
            )?);
        }
        let (mut old_inventory, previous) = attempts_for(&current, MediaRole::Current)?;
        let previous = previous.ok_or(STAGE_HOLD)?;
        roots.push(Directory::open(&previous.join(".peerbrush-versions"))?);
        if take_leases {
            leases.push(crate::file_safety::Lease::acquire(
                &previous.join("current.json"),
            )?);
        }
        if take_leases {
            leases.push(crate::file_safety::Lease::acquire(
                &root.join("acquisition.json"),
            )?);
        }
        let (mut new_inventory, selected) = attempt_inventory(root)?;
        let selected = selected.ok_or(STAGE_HOLD)?;
        roots.push(Directory::open(&selected.join(".peerbrush-versions"))?);
        if take_leases {
            leases.push(crate::file_safety::Lease::acquire(
                &selected.join("prepared.json"),
            )?);
        }
        let selected = authenticate_media_for(
            read_prepared_for(&selected, policy, control, MediaRole::New)?,
            policy,
            verifier,
            None,
            control,
            MediaRole::New,
        )?;
        if selected.prepared.source.route != Route::VerifiedLocal || selected.target.is_none() {
            return Err(STAGE_HOLD.into());
        }
        let input = read_prepared_for(&previous, policy, control, MediaRole::Current)?;
        if input.2.source != selected.prepared.source {
            return Err(STAGE_HOLD.into());
        }
        let previous = authenticate_media_for(
            input,
            policy,
            verifier,
            Some(&selected.prepared.source),
            control,
            MediaRole::Current,
        )?;
        if !previous
            .target
            .as_ref()
            .is_some_and(|target| target.matches(&selected.prepared.source, policy))
        {
            return Err(STAGE_HOLD.into());
        }
        let intent = Held::open(
            &selected.owned.path.join("stage-intent.json"),
            SMALL,
            control,
        )?;
        let recorded: RecordedIntent =
            serde_json::from_slice(&small(&intent, control)?).map_err(|_| STAGE_HOLD)?;
        if recorded.schema != 1
            || recorded.prepared != selected.prepared
            || recorded.prepared_sha256 != selected.journal.media.sha256
        {
            return Err(STAGE_HOLD.into());
        }
        let initial = profile::reopen(
            &selected,
            recorded
                .initial_profile_sha256
                .as_deref()
                .ok_or(STAGE_HOLD)?,
            &previous,
            control,
        )?;
        let outcome = Held::open(
            &selected.owned.path.join("stage-outcome.json"),
            SMALL,
            control,
        )?;
        let recorded: RecordedOutcome =
            serde_json::from_slice(&small(&outcome, control)?).map_err(|_| STAGE_HOLD)?;
        if recorded.schema != 1
            || recorded.intent_sha256 != intent.media.sha256
            || recorded.rejected
            || recorded.terminal.hresult != Some(0)
            || recorded.terminal.registered != Some(false)
            || recorded.terminal.canceled
            || recorded.terminal.invalid_progress
        {
            return Err(STAGE_HOLD.into());
        }
        old_inventory.sort();
        new_inventory.sort();
        let mut context = Self {
            selected,
            previous,
            roots,
            leases,
            inventory: [old_inventory, new_inventory],
            stage: vec![intent, outcome],
            initial,
            intent: None,
            final_profile: None,
            seal: None,
            records: vec![],
            record_directory: None,
        };
        let path = context.selected.owned.path.clone();
        if path
            .join("owner-intent.json")
            .try_exists()
            .map_err(|_| STAGE_HOLD)?
        {
            context.intent = Some(Held::open(&path.join("owner-intent.json"), SMALL, control)?);
        }
        if path
            .join("final-seal.json")
            .try_exists()
            .map_err(|_| STAGE_HOLD)?
        {
            let held = Held::open(&path.join("final-seal.json"), SMALL, control)?;
            let seal: Seal =
                serde_json::from_slice(&small(&held, control)?).map_err(|_| STAGE_HOLD)?;
            context.final_profile = Some(profile::reopen_final(
                &context.selected,
                &seal.final_profile_sha256,
                &context.previous,
                control,
            )?);
            context.seal = Some(held);
        }
        if path
            .join("apply-records")
            .try_exists()
            .map_err(|_| STAGE_HOLD)?
        {
            let directory = Directory::open(&path.join("apply-records"))?;
            let count = Self::names(&directory.path)?
                .into_iter()
                .filter(|n| n != ".peerbrush-versions")
                .count();
            if count > 16 {
                return Err(STAGE_HOLD.into());
            }
            for index in 1..=count {
                context.records.push(Held::open(
                    &directory.path.join(format!("{index:03}.json")),
                    SMALL,
                    control,
                )?);
            }
            context.record_directory = Some(directory);
        }
        context.check(control)?;
        Ok(context)
    }
    pub(super) fn check(&self, control: &Control) -> Result<()> {
        self.selected.check_media(control)?;
        self.previous.check_media(control)?;
        self.initial.check(control)?;
        for root in &self.roots {
            root.check()?;
        }
        for lease in &self.leases {
            lease.validate()?;
        }
        for record in &self.stage {
            record.check(control)?;
        }
        let (mut old, previous) = attempts_for(&self.roots[0].path, MediaRole::Current)?;
        let (mut new, selected) = attempt_inventory(&self.roots[1].path)?;
        old.sort();
        new.sort();
        if [old, new] != self.inventory
            || previous.as_ref() != Some(&self.previous.owned.path)
            || selected.as_ref() != Some(&self.selected.owned.path)
        {
            return Err(STAGE_HOLD.into());
        }
        let previous_names: BTreeSet<String> = [
            "current.json",
            "selected.appinstaller",
            "main.msix",
            "runtime.appx",
            ".peerbrush-versions",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if Self::names(&self.previous.owned.path)? != previous_names {
            return Err(STAGE_HOLD.into());
        }
        let mut expected: BTreeSet<String> = [
            "prepared.json",
            "selected.appinstaller",
            "discovery.appinstaller",
            "main.msix",
            "runtime.appx",
            ".peerbrush-versions",
            "stage-intent.json",
            "stage-outcome.json",
            "profile-initial",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        for (present, name) in [
            (self.intent.is_some(), "owner-intent.json"),
            (self.final_profile.is_some(), "profile-final"),
            (self.seal.is_some(), "final-seal.json"),
            (self.record_directory.is_some(), "apply-records"),
        ] {
            if present {
                expected.insert(name.into());
            }
        }
        if Self::names(&self.selected.owned.path)? != expected {
            return Err(STAGE_HOLD.into());
        }
        if let Some(record) = &self.intent {
            record.check(control)?;
        }
        if let Some(record) = &self.seal {
            record.check(control)?;
        }
        if let Some(profile) = &self.final_profile {
            profile.check(control)?;
        }
        if let Some(directory) = &self.record_directory {
            directory.check()?;
            let mut expected: BTreeSet<String> = self
                .records
                .iter()
                .map(|r| r.path.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            if !self.records.is_empty() {
                expected.insert(".peerbrush-versions".into());
            }
            if Self::names(&directory.path)? != expected {
                return Err(STAGE_HOLD.into());
            }
        }
        for record in &self.records {
            record.check(control)?;
        }
        Ok(())
    }
    fn names(root: &Path) -> Result<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        for entry in fs::read_dir(root).map_err(|_| STAGE_HOLD)? {
            let entry = entry.map_err(|_| STAGE_HOLD)?;
            let name = entry.file_name().into_string().map_err(|_| STAGE_HOLD)?;
            if names.len() >= 32 || !names.insert(name) {
                return Err(STAGE_HOLD.into());
            }
        }
        Ok(names)
    }
    pub(super) fn start_records(&mut self, control: &Control) -> Result<()> {
        self.check(control)?;
        if self.record_directory.is_some() {
            return Err(STAGE_HOLD.into());
        }
        let path = self.selected.owned.path.join("apply-records");
        fs::create_dir(&path).map_err(|_| STAGE_HOLD)?;
        self.record_directory = Some(Directory::open(&path)?);
        self.check(control)
    }
    pub(super) fn record(&mut self, event: transaction::Event, control: &Control) -> Result<()> {
        self.check(control)?;
        if self.records.len() >= 16 {
            return Err(STAGE_HOLD.into());
        }
        let directory = self.record_directory.as_ref().ok_or(STAGE_HOLD)?;
        let bytes = serde_json::json!({"schema":1,"event":event,
            "previous_record_sha256":self.records.last().map(|r|r.media.sha256.as_str()),
            "current_sha256":self.previous.journal.media.sha256,"prepared_sha256":self.selected.journal.media.sha256,
            "owner_intent_sha256":self.intent.as_ref().ok_or(STAGE_HOLD)?.media.sha256,
            "final_seal_sha256":self.seal.as_ref().ok_or(STAGE_HOLD)?.media.sha256,
            "final_profile_sha256":self.final_profile.as_ref().ok_or(STAGE_HOLD)?.digest()});
        let file = stage_record(
            &directory
                .path
                .join(format!("{:03}.json", self.records.len() + 1)),
            &bytes,
            control,
        )?;
        self.records.push(file);
        self.check(control)
    }
}

use super::{pipe::Pipe, process::Process};
use crate::settings::{Locations, UpdateProfile};
use std::{
    collections::BTreeMap,
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
};

fn control() -> Control {
    Control {
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline: Instant::now() + TIMEOUT,
        view: Arc::new(Mutex::new(View::default())),
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Namespace {
    user_profile: PathBuf,
    logical: Locations,
    physical: Locations,
    // Directory identity, including the actual settings profile and main lock.
    config: (u64, u64),
    profile: (u64, u64),
    lock: (u64, u64),
}
impl Namespace {
    fn capture(profile: &UpdateProfile) -> Result<Self> {
        profile.ensure_update_locations()?;
        let logical = profile.locations.clone();
        let canonical = |p: &Path| fs::canonicalize(p).map_err(|_| STAGE_HOLD.to_string());
        let physical = Locations {
            config: canonical(&logical.config)?,
            data: canonical(&logical.data)?,
            cache: canonical(&logical.cache)?,
        };
        let user_profile = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or(STAGE_HOLD)?;
        let result = Self {
            user_profile,
            config: Directory::open(&logical.config)?.identity,
            profile: Directory::open(&logical.profile())?.identity,
            lock: crate::file_safety::identity(
                &crate::file_safety::open_for_identity(&logical.config.join("settings.lock"))
                    .map_err(|_| STAGE_HOLD)?,
            )?,
            logical,
            physical,
        };
        result.check(false)?;
        result.check(true)?;
        Ok(result)
    }
    fn check(&self, logical: bool) -> Result<()> {
        let locations = if logical {
            &self.logical
        } else {
            &self.physical
        };
        if !self.user_profile.is_absolute()
            || ![&locations.config, &locations.data, &locations.cache]
                .iter()
                .all(|p| p.is_absolute())
            || Directory::open(&locations.config)?.identity != self.config
            || Directory::open(&locations.profile())?.identity != self.profile
            || crate::file_safety::identity(
                &crate::file_safety::open_for_identity(&locations.config.join("settings.lock"))
                    .map_err(|_| STAGE_HOLD)?,
            )? != self.lock
        {
            return Err(
                "Windows package profile mapping changed; preserve the original profile.".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    schema: u32,
    nonce: String,
    parent_created: u64,
    namespace: Namespace,
    current_sha256: String,
    prepared_sha256: String,
    helper: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    schema: u32,
    nonce: String,
    owner_created: u64,
    intent_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seal {
    schema: u32,
    nonce: String,
    parent_created: u64,
    owner_created: u64,
    intent_sha256: String,
    current_sha256: String,
    prepared_sha256: String,
    final_profile_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    nonce: String,
    final_seal_sha256: String,
}

struct Helper {
    directory: Directory,
    files: Vec<Held>,
    inventory: BTreeMap<String, Media>,
    temporary: Directory,
}
impl Helper {
    fn check(&self, c: &Control) -> Result<()> {
        self.directory.check()?;
        self.temporary.check()?;
        let mut names: BTreeSet<_> = self.inventory.keys().cloned().collect();
        names.insert("tmp".into());
        if Context::names(&self.directory.path)? != names {
            return Err(STAGE_HOLD.into());
        }
        for file in &self.files {
            file.check(c)?;
        }
        Ok(())
    }
    fn open(path: &Path, context: &Context, c: &Control) -> Result<Self> {
        let expected = native::helper_files(&context.previous, None, c)?.inventory;
        let directory = Directory::open(path)?;
        let mut files = vec![];
        for (name, media) in &expected {
            let file = Held::open(&path.join(name), 64 * 1024 * 1024, c)?;
            if &file.media != media {
                return Err(STAGE_HOLD.into());
            }
            files.push(file);
        }
        let result = Self {
            directory,
            files,
            inventory: expected,
            temporary: Directory::open(&path.join("tmp"))?,
        };
        result.check(c)?;
        Ok(result)
    }
    fn create(context: &Context, namespace: &Namespace, c: &Control) -> Result<Self> {
        let root = namespace.physical.cache.join("windows-update-owners");
        match fs::create_dir(&root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(STAGE_HOLD.into()),
        }
        let parent = Directory::open(&root)?;
        if fs::read_dir(&root).map_err(|_| STAGE_HOLD)?.count() >= 8 {
            return Err("Retained Windows owner capacity requires review; no historical archives were deleted.".into());
        }
        let path = root.join(format!("owner-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).map_err(|_| STAGE_HOLD)?;
        let directory = Directory::open(&path)?;
        let files = native::helper_files(&context.previous, Some(&path), c)?;
        fs::create_dir(path.join("tmp")).map_err(|_| STAGE_HOLD)?;
        let result = Self {
            directory,
            files: files.held,
            inventory: files.inventory,
            temporary: Directory::open(&path.join("tmp"))?,
        };
        parent.check()?;
        result.check(c)?;
        Ok(result)
    }
}

/// Prepared on a background thread; final publication occurs on the owning UI
/// thread under the shared Store/workspace writer transaction.
pub(crate) struct Parent {
    context: Context,
    namespace: Namespace,
    helper: Helper,
    child: Child,
    owner: Process,
    parent: Process,
    pipe: Pipe,
    hello: Hello,
    ready: Ready,
    c: Control,
}
impl Parent {
    pub(crate) fn prepare(profile: &UpdateProfile) -> Result<Self> {
        if !ENABLED {
            return Err(DISABLED.into());
        }
        let policy = configured().ok_or(DISABLED)?;
        native::session(|| {
            let c = control();
            profile.validate()?;
            let namespace = Namespace::capture(profile)?;
            let context = Context::load(
                &profile.locations.profile().join("windows-prepared"),
                &policy,
                &native::Verifier,
                &c,
                false,
            )?;
            if context.intent.is_some()
                || context.seal.is_some()
                || context.record_directory.is_some()
            {
                return Err(STAGE_HOLD.into());
            }
            let source = native::Verifier.source(&policy)?;
            if source != context.selected.prepared.source {
                return Err(STAGE_HOLD.into());
            }
            current_archive(&context.previous, &source, &policy, &native::Verifier, &c)?;
            let helper = Helper::create(&context, &namespace, &c)?;
            let mut pipe = Pipe::server()?;
            let parent = Process::open(std::process::id())?;
            let mut command = Command::new(helper.directory.path.join("peerbrush.exe"));
            process::environment(
                &mut command,
                &namespace.logical,
                &helper.temporary.path,
                &helper.directory.path,
            )?;
            command.env("USERPROFILE", &namespace.user_profile);
            let child = command
                .args(["--windows-update-owner", &pipe.name])
                .creation_flags(0x08000000 | 0x01000000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| STAGE_HOLD)?;
            let owner = Process::child(&child)?;
            pipe.accept(&owner, c.deadline)?;
            authenticate_helper(&owner, &helper, &c)?;
            let hello = Hello {
                schema: 1,
                nonce: pipe.name.clone(),
                parent_created: parent.created,
                namespace: namespace.clone(),
                current_sha256: context.previous.journal.media.sha256.clone(),
                prepared_sha256: context.selected.journal.media.sha256.clone(),
                helper: helper.directory.path.clone(),
            };
            pipe.send(&hello, &owner, c.deadline)?;
            let ready: Ready = pipe.receive(&owner, c.deadline)?;
            if ready.schema != 1
                || ready.nonce != hello.nonce
                || ready.owner_created != owner.created
            {
                return Err(STAGE_HOLD.into());
            }
            let mut result = Self {
                context,
                namespace,
                helper,
                child,
                owner,
                parent,
                pipe,
                hello,
                ready,
                c,
            };
            result.context.intent = Some(Held::open(
                &result.context.selected.owned.path.join("owner-intent.json"),
                SMALL,
                &result.c,
            )?);
            if result.context.intent.as_ref().unwrap().media.sha256 != result.ready.intent_sha256 {
                return Err(STAGE_HOLD.into());
            }
            result.check()?;
            Ok(result)
        })
    }
    fn check(&self) -> Result<()> {
        self.context.check(&self.c)?;
        self.namespace.check(true)?;
        self.helper.check(&self.c)?;
        authenticate_helper(&self.owner, &self.helper, &self.c)?;
        self.parent.authenticate(
            &self.parent.image()?,
            self.context.previous.target.as_ref().ok_or(STAGE_HOLD)?,
            &self.c,
        )
    }
    pub(crate) fn seal(
        &mut self,
        profile: &UpdateProfile,
        tools: &crate::tool_settings::Store,
    ) -> Result<()> {
        self.check()?;
        tools.authenticate_update_profile(&profile.locations.profile())?;
        if profile.locations != self.namespace.logical {
            return Err(STAGE_HOLD.into());
        }
        // All package/signature COM work completed before acquiring writer locks.
        // This source capability only exposes the already authenticated live parent.
        let verifier = ParentIdentity {
            process: &self.parent,
            target: self.context.previous.target.as_ref().ok_or(STAGE_HOLD)?,
            source: &self.context.previous.prepared.source,
        };
        let running = RunningTarget::open(
            self.parent.image()?.parent().ok_or(STAGE_HOLD)?,
            &self.parent.image()?,
            (*verifier.source).clone(),
            verifier.target,
            &self.c,
        )?;
        let current = CurrentArchive {
            retained: &self.context.previous,
            running,
        };
        let copy = profile::capture_final(
            &self.context.selected,
            &current,
            profile,
            &self.context.selected.prepared.policy,
            &verifier,
            &self.c,
        )?;
        self.context.final_profile = Some(copy);
        let seal = Seal {
            schema: 1,
            nonce: self.hello.nonce.clone(),
            parent_created: self.parent.created,
            owner_created: self.owner.created,
            intent_sha256: self.ready.intent_sha256.clone(),
            current_sha256: self.hello.current_sha256.clone(),
            prepared_sha256: self.hello.prepared_sha256.clone(),
            final_profile_sha256: self.context.final_profile.as_ref().unwrap().digest().into(),
        };
        self.context.seal = Some(stage_record(
            &self.context.selected.owned.path.join("final-seal.json"),
            &seal,
            &self.c,
        )?);
        self.check()
    }
    pub(crate) fn commit(&mut self) -> Result<()> {
        self.check()?;
        self.pipe.send(
            &Commit {
                nonce: self.hello.nonce.clone(),
                final_seal_sha256: self
                    .context
                    .seal
                    .as_ref()
                    .ok_or(STAGE_HOLD)?
                    .media
                    .sha256
                    .clone(),
            },
            &self.owner,
            self.c.deadline,
        )
    }
}
impl Drop for Parent {
    fn drop(&mut self) {
        // Never terminate a surviving deployment owner. A lost parent/pipe is a
        // hold, and durable admission prevents normal profile migration.
        let _ = self.child.try_wait();
    }
}
struct ParentIdentity<'a> {
    process: &'a Process,
    target: &'a Target,
    source: &'a Source,
}
impl Verifier for ParentIdentity<'_> {
    fn source(&self, policy: &Policy) -> Result<Source> {
        if !self.target.matches(self.source, policy) {
            return Err(STAGE_HOLD.into());
        }
        self.process
            .authenticate(&self.process.image()?, self.target, &control())?;
        Ok(self.source.clone())
    }
    fn media(
        &self,
        _: &Held,
        _: &Policy,
        _: &Selection,
        _: bool,
        _: &Control,
    ) -> Result<Option<Target>> {
        Err(STAGE_HOLD.into())
    }
}
fn authenticate_helper(process: &Process, helper: &Helper, c: &Control) -> Result<()> {
    process.independent_owner()?;
    helper.check(c)?;
    if fs::canonicalize(process.image()?).map_err(|_| STAGE_HOLD)?
        != fs::canonicalize(helper.directory.path.join("peerbrush.exe")).map_err(|_| STAGE_HOLD)?
    {
        return Err(STAGE_HOLD.into());
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema: u32,
    hello: Hello,
    owner_created: u64,
}

pub(crate) fn startup_guard(locations: &Locations) -> Result<()> {
    let profile = locations.profile();
    for name in ["windows-update-active.json", "windows-next-current"] {
        match fs::symlink_metadata(profile.join(name)) {
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},
            _=>return Err("Windows update recovery is required before profile access; retained state was preserved.".into()),
        }
    }
    let root = profile.join("windows-prepared");
    match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(STAGE_HOLD.into()),
        Ok(_) => directory(&root)?,
    }
    for entry in fs::read_dir(root).map_err(|_| STAGE_HOLD)? {
        let path = entry.map_err(|_| STAGE_HOLD)?.path();
        if path
            .file_name()
            .and_then(|v| v.to_str())
            .is_some_and(|s| s.starts_with("attempt-"))
        {
            directory(&path)?;
            for name in [
                "owner-intent.json",
                "profile-final",
                "final-seal.json",
                "apply-records",
            ] {
                match fs::symlink_metadata(path.join(name)) {
                    Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},
                    _=>return Err("Interrupted Windows owner state requires recovery before profile access; no migration or overwrite was attempted.".into()),
                }
            }
        }
    }
    Ok(())
}

pub(super) fn owner_main(name: &str) -> Result<()> {
    let policy = configured().ok_or(DISABLED)?;
    native::session(|| {
        let c = control();
        let (mut pipe, parent) = Pipe::connect(name, c.deadline)?;
        let hello: Hello = pipe.receive(&parent, c.deadline)?;
        if hello.schema != 1 || hello.nonce != name || hello.parent_created != parent.created {
            return Err(STAGE_HOLD.into());
        }
        let root = hello.namespace.physical.profile().join("windows-prepared");
        let readonly = Context::load(&root, &policy, &native::Verifier, &c, false)?;
        if readonly.intent.is_some()
            || readonly.seal.is_some()
            || readonly.record_directory.is_some()
            || readonly.previous.journal.media.sha256 != hello.current_sha256
            || readonly.selected.journal.media.sha256 != hello.prepared_sha256
        {
            return Err(STAGE_HOLD.into());
        }
        parent.authenticate(
            &parent.image()?,
            readonly.previous.target.as_ref().ok_or(STAGE_HOLD)?,
            &c,
        )?;
        // Only the live signed parent may identify its redirected physical profile.
        hello.namespace.check(false)?;
        let helper = Helper::open(&hello.helper, &readonly, &c)?;
        let owner = Process::open(std::process::id())?;
        authenticate_helper(&owner, &helper, &c)?;
        let gate = gate::Gate::own(&hello.namespace.physical.config)?;
        let mut context = Context::load(&root, &policy, &native::Verifier, &c, true)?;
        context.intent = Some(stage_record(
            &context.selected.owned.path.join("owner-intent.json"),
            &Intent {
                schema: 1,
                hello: hello.clone(),
                owner_created: owner.created,
            },
            &c,
        )?);
        context.check(&c)?;
        let active = stage_record(
            &hello
                .namespace
                .physical
                .profile()
                .join("windows-update-active.json"),
            &serde_json::json!({"schema":1,"nonce":name,"intent_sha256":context.intent.as_ref().unwrap().media.sha256}),
            &c,
        )?;
        pipe.send(
            &Ready {
                schema: 1,
                nonce: name.into(),
                owner_created: owner.created,
                intent_sha256: context.intent.as_ref().unwrap().media.sha256.clone(),
            },
            &parent,
            c.deadline,
        )?;
        let commit: Commit = pipe.receive(&parent, c.deadline)?;
        if commit.nonce != name {
            return Err(STAGE_HOLD.into());
        }
        let held = Held::open(
            &context.selected.owned.path.join("final-seal.json"),
            SMALL,
            &c,
        )?;
        let seal: Seal = serde_json::from_slice(&small(&held, &c)?).map_err(|_| STAGE_HOLD)?;
        if held.media.sha256 != commit.final_seal_sha256
            || seal.schema != 1
            || seal.nonce != name
            || seal.parent_created != parent.created
            || seal.owner_created != owner.created
            || seal.intent_sha256 != context.intent.as_ref().unwrap().media.sha256
            || seal.current_sha256 != hello.current_sha256
            || seal.prepared_sha256 != hello.prepared_sha256
        {
            return Err(STAGE_HOLD.into());
        }
        context.final_profile = Some(profile::reopen_final(
            &context.selected,
            &seal.final_profile_sha256,
            &context.previous,
            &c,
        )?);
        context.seal = Some(held);
        context.check(&c)?;
        while !parent.exited()? {
            c.check()?;
            gate.check()?;
            active.check(&c)?;
            helper.check(&c)?;
            std::thread::sleep(Duration::from_millis(25));
        }
        let profile = crate::settings::Profile::lock_for_windows_owner(
            hello.namespace.physical.clone(),
            &gate,
        )?;
        let originals = context
            .final_profile
            .as_ref()
            .unwrap()
            .protect_originals(&profile.update_profile(), &c)?;
        context.start_records(&c)?;
        let mut host = Live {
            context,
            namespace: hello.namespace,
            helper,
            gate,
            parent,
            profile,
            originals,
            active,
            c,
            next: None,
        };
        let outcome = transaction::execute(&mut host)?;
        host.retire(outcome)
    })
}

struct Live {
    context: Context,
    namespace: Namespace,
    helper: Helper,
    gate: gate::Gate,
    parent: Process,
    profile: crate::settings::Profile,
    originals: profile::Originals,
    active: Held,
    c: Control,
    next: Option<Directory>,
}
impl Live {
    fn retained(&self, role: transaction::Role) -> &RetainedSelection {
        match role {
            transaction::Role::Previous => &self.context.previous,
            transaction::Role::Selected => &self.context.selected,
        }
    }
}
impl transaction::Host for Live {
    fn guard(&mut self) -> Result<()> {
        self.context.check(&self.c)?;
        self.namespace.check(false)?;
        self.helper.check(&self.c)?;
        self.gate.validate_owner(&self.namespace.physical.config)?;
        self.profile.update_profile().validate()?;
        self.originals.check(&self.c)?;
        self.active.check(&self.c)?;
        if !self.parent.exited()? {
            return Err(STAGE_HOLD.into());
        }
        Ok(())
    }
    fn installed(&mut self) -> Result<transaction::Installed> {
        self.guard()?;
        let registered = native::registered(&self.context.selected.prepared.policy, &self.c)?;
        let Some(registered) = registered else {
            return Ok(transaction::Installed::Missing);
        };
        for (role, result) in [
            (
                transaction::Role::Previous,
                transaction::Installed::Previous,
            ),
            (
                transaction::Role::Selected,
                transaction::Installed::Selected,
            ),
        ] {
            let target = self.retained(role).target.as_ref().ok_or(STAGE_HOLD)?;
            if registered.full_name == target.full_name {
                registered.prove(target, &self.c)?;
                return Ok(result);
            }
        }
        Ok(transaction::Installed::Other)
    }
    fn record(&mut self, event: transaction::Event) -> Result<()> {
        self.guard()?;
        self.context.record(event, &self.c)
    }
    fn deploy(&mut self, role: transaction::Role) -> Result<bool> {
        self.guard()?;
        let mut operation = native::submit_local(self.retained(role), role, &self.c)?;
        let mut cancel = false;
        loop {
            match operation.poll() {
                StageObservation::Terminal(terminal) => {
                    // Drain precedes interpreting failure, timeout or cancellation.
                    // A fresh readback control cannot invent native terminal state.
                    self.c = control();
                    self.guard()?;
                    return Ok(terminal.hresult == Some(0)
                        && !terminal.canceled
                        && !terminal.invalid_progress);
                }
                StageObservation::Pending | StageObservation::Uncertain => {
                    if self.c.check().is_err() && !cancel {
                        operation.cancel();
                        cancel = true;
                    }
                    // Retain operation and gate until the provider reports terminal.
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    fn health(&mut self, role: transaction::Role) -> Result<bool> {
        self.guard()?;
        self.probe(role)
    }
    fn finish(&mut self, outcome: transaction::Outcome) -> Result<()> {
        self.guard()?;
        if outcome == transaction::Outcome::Healthy {
            self.next = Some(self.promote()?);
        }
        self.guard()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Probe {
    schema: u32,
    nonce: String,
    owner_created: u64,
    namespace: Namespace,
    locations: Locations,
    directories: Vec<(PathBuf, (u64, u64))>,
    helper: PathBuf,
    current_sha256: String,
    prepared_sha256: String,
    final_profile_sha256: String,
    role: transaction::Role,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Health {
    nonce: String,
    full_name: String,
    build: String,
    final_profile_sha256: String,
    frames: u32,
}
impl Live {
    fn probe(&mut self, role: transaction::Role) -> Result<bool> {
        use transaction::Host;
        let target = self
            .retained(role)
            .target
            .as_ref()
            .ok_or(STAGE_HOLD)?
            .clone();
        let registered = native::registered(&self.context.selected.prepared.policy, &self.c)?
            .ok_or(STAGE_HOLD)?;
        registered.prove(&target, &self.c)?;
        let root = self
            .helper
            .temporary
            .path
            .join(format!("health-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).map_err(|_| STAGE_HOLD)?;
        let locations = Locations {
            config: root.join("roaming/PeerBrush"),
            data: root.join("local/PeerBrush/Data"),
            cache: root.join("local/PeerBrush/Cache"),
        };
        for path in [
            &locations.config,
            &locations.data,
            &locations.cache,
            &root.join("state"),
            &root.join("tmp"),
        ] {
            fs::create_dir_all(path).map_err(|_| STAGE_HOLD)?;
        }
        self.context
            .final_profile
            .as_ref()
            .ok_or(STAGE_HOLD)?
            .materialize(&locations.profile(), &self.c)?;
        let directories = [
            &locations.config,
            &locations.data,
            &locations.cache,
            &locations.profile(),
        ]
        .into_iter()
        .map(|p| Ok((p.clone(), Directory::open(p)?.identity)))
        .collect::<Result<Vec<_>>>()?;
        let mut pipe = Pipe::server()?;
        let mut command = Command::new(registered.root.join("peerbrush.exe"));
        process::environment(
            &mut command,
            &locations,
            &root.join("tmp"),
            &registered.root,
        )?;
        command.env("USERPROFILE", &root);
        let mut child = command
            .args(["--windows-update-health", &pipe.name])
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| STAGE_HOLD)?;
        let peer = Process::child(&child)?;
        // The only terminable process is this exact newly created isolated child.
        // Its held kernel object prevents PID reuse even if initial launch fails.
        let deadline = Instant::now() + Duration::from_secs(90);
        let observed: Result<bool> = (|| {
            pipe.accept(&peer, deadline)?;
            peer.authenticate(&registered.root.join("peerbrush.exe"), &target, &self.c)?;
            let probe = Probe {
                schema: 1,
                nonce: pipe.name.clone(),
                owner_created: Process::open(std::process::id())?.created,
                namespace: self.namespace.clone(),
                locations,
                directories,
                helper: self.helper.directory.path.clone(),
                current_sha256: self.context.previous.journal.media.sha256.clone(),
                prepared_sha256: self.context.selected.journal.media.sha256.clone(),
                final_profile_sha256: self.context.final_profile.as_ref().unwrap().digest().into(),
                role,
            };
            pipe.send(&probe, &peer, deadline)?;
            let health: Health = pipe.receive(&peer, deadline)?;
            self.guard()?;
            peer.authenticate(&registered.root.join("peerbrush.exe"), &target, &self.c)?;
            if health.nonce != pipe.name
                || health.full_name != target.full_name
                || health.build != target.build
                || health.frames < 3
                || health.final_profile_sha256 != probe.final_profile_sha256
            {
                return Err(STAGE_HOLD.into());
            }
            pipe.send(&true, &peer, deadline)?;
            while !peer.exited()? {
                if Instant::now() >= deadline {
                    return Err(STAGE_HOLD.into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(child
                .try_wait()
                .map_err(|_| STAGE_HOLD)?
                .is_some_and(|status| status.success()))
        })();
        match observed {
            Ok(true) => {
                self.guard()?;
                Ok(true)
            }
            _ => {
                // No rollback under a possibly live child. Kill through the
                // owned Child handle only, then require actual kernel exit.
                if !peer.exited()? {
                    child.kill().map_err(|_| STAGE_HOLD)?;
                }
                let end = Instant::now() + Duration::from_secs(10);
                while !peer.exited()? {
                    if Instant::now() >= end {
                        return Err(STAGE_HOLD.into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                child.wait().map_err(|_| STAGE_HOLD)?;
                self.guard()?;
                Ok(false)
            }
        }
    }
    fn promote(&self) -> Result<Directory> {
        let selected = &self.context.selected;
        let target = selected.target.as_ref().ok_or(STAGE_HOLD)?;
        let mut prepared = selected.prepared.clone();
        prepared.source = Source {
            full_name: target.full_name.clone(),
            family: prepared.policy.family.clone(),
            version: target.version,
            association: prepared.selection.descriptor.clone(),
            build: target.build.clone(),
            route: Route::VerifiedLocal,
        };
        if !target.matches(&prepared.source, &prepared.policy) {
            return Err(STAGE_HOLD.into());
        }
        let root = self
            .namespace
            .physical
            .profile()
            .join("windows-next-current");
        fs::create_dir(&root).map_err(|_| STAGE_HOLD)?;
        let directory = Directory::open(&root)?;
        let lease = crate::file_safety::Lease::acquire(&root.join("acquisition.json"))?;
        let attempt = root.join(format!("attempt-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&attempt).map_err(|_| STAGE_HOLD)?;
        for (file, name) in [
            (&selected.descriptor, "selected.appinstaller"),
            (&selected.package, "main.msix"),
            (&selected.runtime, "runtime.appx"),
        ] {
            let path = attempt.join(name);
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(|_| STAGE_HOLD)?;
            let identity = crate::file_safety::identity(&output)?;
            let mut input = file.file.try_clone().map_err(|_| STAGE_HOLD)?;
            use std::io::{Seek, SeekFrom};
            input.seek(SeekFrom::Start(0)).map_err(|_| STAGE_HOLD)?;
            let count = std::io::copy(&mut input.take(file.media.bytes + 1), &mut output)
                .map_err(|_| STAGE_HOLD)?;
            output.sync_all().map_err(|_| STAGE_HOLD)?;
            drop(output);
            let copied = Held::open(&path, PACKAGE, &self.c)?;
            if count != file.media.bytes
                || copied.identity != identity
                || copied.media != file.media
            {
                return Err(STAGE_HOLD.into());
            }
            file.check(&self.c)?;
        }
        let journal = stage_record(&attempt.join("current.json"), &prepared, &self.c)?;
        let authenticated = authenticate_media_for(
            read_prepared_for(&attempt, &prepared.policy, &self.c, MediaRole::Current)?,
            &prepared.policy,
            &native::Verifier,
            Some(&prepared.source),
            &self.c,
            MediaRole::Current,
        )?;
        authenticated.check_media(&self.c)?;
        journal.check(&self.c)?;
        lease.validate()?;
        directory.check()?;
        Ok(directory)
    }
    fn retire(self, outcome: transaction::Outcome) -> Result<()> {
        use transaction::Host;
        let mut owned = self;
        owned.guard()?;
        let root = owned.namespace.physical.profile();
        let history = root.join("windows-update-history");
        match fs::create_dir(&history) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(STAGE_HOLD.into()),
        }
        let history_directory = Directory::open(&history)?;
        if fs::read_dir(&history).map_err(|_| STAGE_HOLD)?.count() >= 8 {
            return Err(STAGE_HOLD.into());
        }
        let archive = history.join(format!("transition-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&archive).map_err(|_| STAGE_HOLD)?;
        let archive_directory = Directory::open(&archive)?;
        let prepared_id = owned.context.roots[1].identity;
        let current_id = owned.context.roots[0].identity;
        let next_identity = owned.next.as_ref().map(|p| p.identity);
        let active_id = owned.active.identity;
        let target = match outcome {
            transaction::Outcome::Healthy => owned.context.selected.target.as_ref(),
            transaction::Outcome::Recovered => owned.context.previous.target.as_ref(),
        }
        .ok_or(STAGE_HOLD)?
        .clone();
        let registered = native::registered(&owned.context.selected.prepared.policy, &owned.c)?
            .ok_or(STAGE_HOLD)?;
        registered.prove(&target, &owned.c)?;
        owned.guard()?;
        let locations = owned.namespace.logical.clone();
        let user_profile = owned.namespace.user_profile.clone();
        let helper_path = owned.helper.directory.path.clone();
        // Keep admission, original-file protection and the independently owned
        // profile lock through retirement. Release only cache handles which
        // intentionally denied rename while evidence was being verified.
        let Live {
            context,
            next,
            active,
            gate,
            profile,
            originals,
            helper,
            ..
        } = owned;
        drop(context);
        drop(next);
        drop(active);
        if outcome == transaction::Outcome::Healthy {
            retirement::rename(
                &root.join("windows-current"),
                current_id,
                &archive.join("previous-current"),
            )?;
            retirement::rename(
                &root.join("windows-next-current"),
                next_identity.ok_or(STAGE_HOLD)?,
                &root.join("windows-current"),
            )?;
        }
        retirement::rename(
            &root.join("windows-prepared"),
            prepared_id,
            &archive.join("completed-prepared"),
        )?;
        history_directory.check()?;
        archive_directory.check()?;
        originals.check(&control())?;
        retirement::rename_file(
            &root.join("windows-update-active.json"),
            active_id,
            &archive.join("completed-owner.json"),
        )?;
        drop(originals);
        drop(profile);
        drop(gate);
        let mut command = Command::new(registered.root.join("peerbrush.exe"));
        process::environment(
            &mut command,
            &locations,
            &helper.temporary.path,
            &helper_path,
        )?;
        command.env("USERPROFILE", user_profile);
        let child = command
            .creation_flags(0x08000000 | 0x01000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| STAGE_HOLD)?;
        Process::child(&child)?.authenticate(
            &registered.root.join("peerbrush.exe"),
            &target,
            &control(),
        )?;
        Ok(())
    }
}

pub(super) fn health_main(name: &str) -> Result<()> {
    let policy = configured().ok_or(DISABLED)?;
    let (pipe, owner, probe, profile, target, source) = native::session(|| {
        let c = control();
        let (mut pipe, owner) = Pipe::connect(name, c.deadline)?;
        let probe: Probe = pipe.receive(&owner, c.deadline)?;
        if probe.schema != 1
            || probe.nonce != name
            || probe.owner_created != owner.created
            || probe.directories.len() != 4
        {
            return Err(STAGE_HOLD.into());
        }
        let context = Context::load(
            &probe.namespace.physical.profile().join("windows-prepared"),
            &policy,
            &native::Verifier,
            &c,
            false,
        )?;
        if context.previous.journal.media.sha256 != probe.current_sha256
            || context.selected.journal.media.sha256 != probe.prepared_sha256
            || context.final_profile.as_ref().ok_or(STAGE_HOLD)?.digest()
                != probe.final_profile_sha256
        {
            return Err(STAGE_HOLD.into());
        }
        let helper = Helper::open(&probe.helper, &context, &c)?;
        authenticate_helper(&owner, &helper, &c)?;
        probe.namespace.check(true)?;
        if Locations::user()? != probe.locations {
            return Err(STAGE_HOLD.into());
        }
        for (path, identity) in &probe.directories {
            if Directory::open(path)?.identity != *identity {
                return Err(STAGE_HOLD.into());
            }
        }
        let target = match probe.role {
            transaction::Role::Previous => context.previous.target.as_ref(),
            transaction::Role::Selected => context.selected.target.as_ref(),
        }
        .ok_or(STAGE_HOLD)?;
        let process = Process::open(std::process::id())?;
        process.authenticate(
            &std::env::current_exe().map_err(|_| STAGE_HOLD)?,
            target,
            &c,
        )?;
        let source = native::Verifier.source(&policy)?;
        if !target.matches(&source, &policy) {
            return Err(STAGE_HOLD.into());
        }
        let profile = crate::settings::Profile::open(
            probe.locations.clone(),
            &helper.temporary.path.join("no-legacy"),
        )?;
        profile.update_profile().validate()?;
        Ok((pipe, owner, probe, profile, target.clone(), source))
    })?;
    let preferences = profile.locations.profile();
    let state = probe.locations.cache.join("health-state");
    let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
    let connection = crate::server::start_with_preferences(
        shared.clone(),
        state,
        &preferences,
        &probe.locations.cache,
    )?;
    struct App {
        app: crate::ui::PeerBrush,
        frames: u32,
        pipe: Option<Pipe>,
        owner: Process,
        probe: Probe,
        failed: Arc<AtomicBool>,
        target: Target,
        source: Source,
    }
    impl eframe::App for App {
        fn update(&mut self, ctx: &eframe::egui::Context, frame: &mut eframe::Frame) {
            eframe::App::update(&mut self.app, ctx, frame);
            self.frames += 1;
            ctx.request_repaint();
            if self.frames >= 3 {
                if let Some(mut pipe) = self.pipe.take() {
                    let result: Result<()> = (|| {
                        let c = control();
                        Process::open(std::process::id())?.authenticate(
                            &std::env::current_exe().map_err(|_| STAGE_HOLD)?,
                            &self.target,
                            &c,
                        )?;
                        let source = &self.source;
                        self.probe.namespace.check(true)?;
                        pipe.send(
                            &Health {
                                nonce: self.probe.nonce.clone(),
                                full_name: source.full_name.clone(),
                                build: source.build.clone(),
                                final_profile_sha256: self.probe.final_profile_sha256.clone(),
                                frames: self.frames,
                            },
                            &self.owner,
                            c.deadline,
                        )?;
                        if !pipe.receive::<bool>(&self.owner, c.deadline)? {
                            return Err(STAGE_HOLD.into());
                        }
                        Ok(())
                    })();
                    if result.is_err() {
                        self.failed.store(true, Ordering::Release);
                    }
                    ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
                }
            }
        }
        fn on_exit(&mut self) {
            eframe::App::on_exit(&mut self.app);
        }
    }
    let failed = Arc::new(AtomicBool::new(false));
    let observed = failed.clone();
    eframe::run_native(
        "PeerBrush update health",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default().with_inner_size([980., 640.]),
            ..Default::default()
        },
        Box::new(move |cc| {
            let mut app =
                crate::ui::PeerBrush::new_with_preferences(cc, shared, connection, &preferences);
            app.suspend_updates_for_health();
            Ok(Box::new(App {
                app,
                frames: 0,
                pipe: Some(pipe),
                owner,
                probe,
                failed: observed,
                target,
                source,
            }))
        }),
    )
    .map_err(|_| STAGE_HOLD)?;
    profile.update_profile().validate()?;
    if failed.load(Ordering::Acquire) {
        return Err(STAGE_HOLD.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logical_and_physical_profile_mapping_requires_actual_directory_and_lock_identity() {
        let root =
            std::env::temp_dir().join(format!("peerbrush-namespace-{}", uuid::Uuid::new_v4()));
        let locations = Locations {
            config: root.join("config"),
            data: root.join("data"),
            cache: root.join("cache"),
        };
        fs::create_dir_all(root.join("legacy")).unwrap();
        let profile =
            crate::settings::Profile::open(locations.clone(), &root.join("legacy")).unwrap();
        let namespace = Namespace::capture(&profile.update_profile()).unwrap();
        namespace.check(true).unwrap();
        namespace.check(false).unwrap();
        let foreign = root.join("foreign");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("keep"), b"foreign").unwrap();
        let mut changed = namespace.clone();
        changed.logical.config = foreign.clone();
        assert!(changed.check(true).is_err());
        assert_eq!(fs::read(foreign.join("keep")).unwrap(), b"foreign");
        let mut changed = namespace.clone();
        changed.lock.1 ^= 1;
        assert!(changed.check(false).is_err());
        drop(profile);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interrupted_owner_markers_hold_before_any_profile_migration() {
        let root =
            std::env::temp_dir().join(format!("peerbrush-startup-hold-{}", uuid::Uuid::new_v4()));
        let locations = Locations {
            config: root.join("config"),
            data: root.join("data"),
            cache: root.join("cache"),
        };
        fs::create_dir_all(locations.profile()).unwrap();
        startup_guard(&locations).unwrap();
        for marker in ["windows-update-active.json", "windows-next-current"] {
            let path = locations.profile().join(marker);
            fs::write(&path, b"foreign").unwrap();
            assert!(startup_guard(&locations).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"foreign");
            fs::remove_file(path).unwrap();
        }
        let attempt = locations.profile().join("windows-prepared/attempt-test");
        fs::create_dir_all(&attempt).unwrap();
        for marker in [
            "owner-intent.json",
            "profile-final",
            "final-seal.json",
            "apply-records",
        ] {
            let path = attempt.join(marker);
            fs::write(&path, b"interrupted").unwrap();
            assert!(startup_guard(&locations).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"interrupted");
            fs::remove_file(path).unwrap();
        }
        startup_guard(&locations).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
