//! Actual profile/instance admission for persistent source namespaces.
use super::store::{open_directory, validate_ancestors};
use crate::{server, settings};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(crate) struct Directory {
    path: PathBuf,
    file: fs::File,
    identity: (u64, u64),
}
impl Directory {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        validate_ancestors(path).map_err(|e| e.to_string())?;
        let file = open_directory(path).map_err(|e| e.to_string())?;
        let identity = crate::file_safety::identity(&file)?;
        let held = Self {
            path: path.to_owned(),
            file,
            identity,
        };
        held.check()?;
        Ok(held)
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn identity(&self) -> (u64, u64) {
        self.identity
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        validate_ancestors(&self.path).map_err(|e| e.to_string())?;
        let named = open_directory(&self.path).map_err(|e| e.to_string())?;
        if crate::file_safety::identity(&named)? != self.identity
            || crate::file_safety::identity(&self.file)? != self.identity
        {
            return Err("Persistent source directory ownership changed; work retained".into());
        }
        Ok(())
    }
    pub(crate) fn child(&self, name: &str) -> Result<Self, String> {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err("Invalid internal source directory name".into());
        }
        self.check()?;
        let path = self.path.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                private_dir(&path)?;
                server::sync_parent(&path)?;
            }
            Err(_) => return Err("Cannot inspect persistent source directory".into()),
        }
        let child = Self::open(&path)?;
        self.check()?;
        Ok(child)
    }
    /// Only an independently verified, never-published, now-empty child may be
    /// retired. This does not recursively clean or adopt any directory contents.
    pub(crate) fn retire_empty(
        self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        self.check()?;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options
                .access_mode(0x100A0)
                .share_mode(7)
                .custom_flags(0x02000000 | 0x00200000);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        }
        let retiring = options
            .open(&self.path)
            .map_err(|_| "Cannot retain owned empty directory for retirement")?;
        if crate::file_safety::identity(&retiring)? != self.identity {
            return Err("Empty source directory ownership changed".into());
        }
        if fs::read_dir(&self.path)
            .map_err(|_| "Cannot inspect owned empty source directory")?
            .next()
            .is_some()
        {
            return Err("Source directory contains retained entries; cleanup refused".into());
        }
        check()?;
        self.check()?;
        if fs::read_dir(&self.path)
            .map_err(|_| "Cannot recheck owned empty source directory")?
            .next()
            .is_some()
        {
            return Err("Source directory changed before retirement; entries retained".into());
        }
        #[cfg(windows)]
        super::linear::unlink_owned(&retiring, &self.path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        fs::remove_dir(&self.path).map_err(|_| "Owned empty directory retirement failed")?;
        server::sync_parent(&self.path)?;
        let Self { path, file, .. } = self;
        drop(retiring);
        drop(file);
        if !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            return Err("Empty source retirement outcome uncertain; inventory retained".into());
        }
        Ok(())
    }
}

fn private_dir(path: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|_| "Cannot create private persistent source directory".into())
}

fn open_or_create_base(path: &Path) -> Result<Directory, String> {
    if !path.is_absolute() {
        return Err("Persistent source base must be absolute".into());
    }
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if missing.len() == 64 {
                    return Err("Persistent source directory depth exceeded".into());
                }
                missing.push(existing.to_owned());
                existing = existing
                    .parent()
                    .ok_or("Persistent source parent is missing")?;
            }
            Err(_) => return Err("Cannot inspect persistent source parent".into()),
        }
    }
    let mut held = Directory::open(existing)?;
    for next in missing.into_iter().rev() {
        held.check()?;
        private_dir(&next)?;
        server::sync_parent(&next)?;
        let child = Directory::open(&next)?;
        held.check()?;
        held = child;
    }
    Ok(held)
}

pub(crate) struct PersistentRootContext {
    admission: server::SourceAdmission,
    connection: Directory,
    instance_identity: (u64, u64),
    base: PathBuf,
    initialized: Mutex<Option<Arc<Directory>>>,
    profile: Option<settings::SourceProfile>,
}
impl PersistentRootContext {
    /// Reads admission only. No source directory or ledger is created here.
    pub(crate) fn from_profile(
        connection: &server::Connection,
        profile: settings::SourceProfile,
    ) -> Result<Arc<Self>, String> {
        profile.validate()?;
        let base = profile.locations().data.clone();
        Self::new(connection, Some(profile), base)
    }
    /// Caller selects this only for an actual explicit --state-dir session.
    pub(crate) fn from_explicit_state(
        connection: &server::Connection,
    ) -> Result<Arc<Self>, String> {
        let base = fs::canonicalize(&connection.state_dir)
            .map_err(|_| "Explicit source state directory unavailable")?;
        Self::new(connection, None, base)
    }
    fn new(
        connection: &server::Connection,
        profile: Option<settings::SourceProfile>,
        base: PathBuf,
    ) -> Result<Arc<Self>, String> {
        if !connection.is_available() || !base.is_absolute() {
            return Err("Persistent source session is unavailable".into());
        }
        let canonical = fs::canonicalize(&connection.state_dir)
            .map_err(|_| "Source connection directory unavailable")?;
        let directory = Directory::open(&canonical)?;
        let (admission, instance_identity) = connection
            .instance_lock
            .as_ref()
            .ok_or("Actual source instance owner is unavailable")?
            .source_admission_for(&canonical)?;
        Ok(Arc::new(Self {
            admission,
            connection: directory,
            instance_identity,
            profile,
            base,
            initialized: Mutex::new(None),
        }))
    }
    fn validate(&self) -> Result<(), String> {
        self.connection.check()?;
        let path = self.connection.path().join("instance.lock");
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| "Source instance name is unavailable")?;
        let named = crate::file_safety::open_for_identity(&path)
            .map_err(|_| "Source instance name is unavailable")?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || crate::file_safety::identity(&named)? != self.instance_identity
            || crate::file_safety::link_count(&named)? != 1
        {
            return Err("Source instance ownership changed; work retained".into());
        }
        if let Some(profile) = &self.profile {
            profile.validate()?;
        }
        Ok(())
    }
    /// Begin only when nonzero source I/O is required, outside application locks.
    pub(crate) fn begin(self: &Arc<Self>) -> Result<RootOperation, String> {
        let operation = self.admission.begin()?;
        operation.check()?;
        self.validate()?;
        let mut initialized = self
            .initialized
            .try_lock()
            .map_err(|_| "Persistent source initialization is busy")?;
        let base = match &*initialized {
            Some(base) => base.clone(),
            None => {
                let base = Arc::new(open_or_create_base(&self.base)?);
                self.validate()?;
                *initialized = Some(base.clone());
                base
            }
        };
        drop(initialized);
        let held = RootOperation {
            context: self.clone(),
            base,
            _operation: operation,
        };
        held.check()?;
        Ok(held)
    }
}

pub(crate) struct RootOperation {
    context: Arc<PersistentRootContext>,
    base: Arc<Directory>,
    _operation: server::SourceOperation,
}
impl RootOperation {
    pub(crate) fn check(&self) -> Result<(), String> {
        self._operation.check()?;
        self.context.validate()?;
        self.base.check()
    }
    pub(crate) fn source_directory(&self) -> Result<Directory, String> {
        self.check()?;
        let directory = self.base.child("native-sources-v1")?;
        self.check()?;
        Ok(directory)
    }
}
