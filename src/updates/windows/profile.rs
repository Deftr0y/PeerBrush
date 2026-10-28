//! Initial settings preservation. This is not the final no-later-writer checkpoint.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

const FILE_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    source: Source,
    current_sha256: String,
    prepared_sha256: String,
    files: BTreeMap<PathBuf, Option<Media>>,
}

pub(super) struct Copy {
    directories: Vec<Directory>,
    marker: Held,
    files: Vec<Held>,
    absent: Vec<PathBuf>,
}
impl Copy {
    #[cfg(windows)]
    pub(super) fn matches_materialized(&self, destination: &Path, control: &Control) -> Result<()> {
        self.check(control)?;
        let directory = Directory::open(destination)?;
        let record: Record =
            serde_json::from_slice(&small(&self.marker, control)?).map_err(|_| STAGE_HOLD)?;
        for (relative, media) in record.files {
            let path = destination.join(relative);
            if let Some(media) = media {
                let actual = Held::open(&path, FILE_LIMIT, control)?;
                if actual.media != media {
                    return Err("The health process changed the profile; compatible recovery is not established.".into());
                }
                actual.check(control)?;
            } else if !fs::symlink_metadata(path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                return Err(STAGE_HOLD.into());
            }
        }
        directory.check()?;
        self.check(control)
    }
    #[cfg(windows)]
    pub(super) fn materialize(&self, destination: &Path, control: &Control) -> Result<()> {
        self.check(control)?;
        let source = self.marker.path.parent().ok_or(STAGE_HOLD)?;
        fs::create_dir(destination).map_err(|_| STAGE_HOLD)?;
        let directory = Directory::open(destination)?;
        fs::create_dir(destination.join("migration-backup")).map_err(|_| STAGE_HOLD)?;
        for file in &self.files {
            let relative = file.path.strip_prefix(source).map_err(|_| STAGE_HOLD)?;
            if !crate::settings::update_files().contains(&relative.to_owned()) {
                return Err(STAGE_HOLD.into());
            }
            let path = destination.join(relative);
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(|_| STAGE_HOLD)?;
            let identity = crate::file_safety::identity(&output)?;
            output
                .write_all(&bounded_bytes(file, FILE_LIMIT, control)?)
                .map_err(|_| STAGE_HOLD)?;
            output.sync_all().map_err(|_| STAGE_HOLD)?;
            drop(output);
            let copied = Held::open(&path, FILE_LIMIT, control)?;
            if copied.media != file.media || copied.identity != identity {
                return Err(STAGE_HOLD.into());
            }
        }
        directory.check()?;
        self.check(control)
    }
    pub(super) fn digest(&self) -> &str {
        &self.marker.media.sha256
    }
    pub(super) fn check(&self, control: &Control) -> Result<()> {
        for directory in &self.directories {
            directory.check()?;
        }
        self.marker.check(control)?;
        for file in &self.files {
            file.check(control)?;
        }
        for path in &self.absent {
            control.check()?;
            if !fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                return Err(STAGE_HOLD.into());
            }
        }
        Ok(())
    }
}

pub(super) fn capture(
    retained: &RetainedSelection,
    current: &CurrentArchive<'_>,
    profile: &crate::settings::UpdateProfile,
    policy: &Policy,
    verifier: &dyn Verifier,
    control: &Control,
) -> Result<Copy> {
    capture_at(retained, current, profile, policy, verifier, control, false)
}

pub(super) fn capture_final(
    retained: &RetainedSelection,
    current: &CurrentArchive<'_>,
    profile: &crate::settings::UpdateProfile,
    policy: &Policy,
    verifier: &dyn Verifier,
    control: &Control,
) -> Result<Copy> {
    capture_at(retained, current, profile, policy, verifier, control, true)
}

fn capture_at(
    retained: &RetainedSelection,
    current: &CurrentArchive<'_>,
    profile: &crate::settings::UpdateProfile,
    policy: &Policy,
    verifier: &dyn Verifier,
    control: &Control,
    final_copy: bool,
) -> Result<Copy> {
    profile.validate()?;
    current.check(policy, verifier, control)?;
    retained.check(verifier, control)?;
    let source = profile.locations.profile();
    if retained.owned.path.parent() != Some(source.join("windows-prepared").as_path())
        || current.retained.prepared.source != retained.prepared.source
    {
        return Err(STAGE_HOLD.into());
    }
    let source_directory = Directory::open(&source)?;
    let previous = source.join("migration-backup");
    let previous_directory = match fs::symlink_metadata(&previous) {
        Ok(_) => Some(Directory::open(&previous)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(STAGE_HOLD.into()),
    };
    let destination = retained.owned.path.join(if final_copy {
        "profile-final"
    } else {
        "profile-initial"
    });
    // Exclusive creation preserves any interrupted or earlier backup.
    fs::create_dir(&destination).map_err(|_| STAGE_HOLD)?;
    let mut directories = vec![Directory::open(&destination)?];
    fs::create_dir(destination.join("migration-backup")).map_err(|_| STAGE_HOLD)?;
    directories.push(Directory::open(&destination.join("migration-backup"))?);
    let mut originals = Vec::new();
    let mut absent = Vec::new();
    let mut files = Vec::new();
    let mut absent_copies = Vec::new();
    let mut inventory = BTreeMap::new();
    for relative in crate::settings::update_files() {
        control.check()?;
        let path = source.join(&relative);
        let original = match fs::symlink_metadata(&path) {
            Ok(_) => Held::open(&path, FILE_LIMIT, control)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                absent.push(path);
                absent_copies.push(destination.join(&relative));
                inventory.insert(relative, None);
                continue;
            }
            Err(_) => return Err(STAGE_HOLD.into()),
        };
        let bytes = bounded_bytes(&original, FILE_LIMIT, control)?;
        let target = destination.join(&relative);
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)
            .map_err(|_| STAGE_HOLD)?;
        output.write_all(&bytes).map_err(|_| STAGE_HOLD)?;
        output.sync_all().map_err(|_| STAGE_HOLD)?;
        drop(output);
        crate::file_safety::preserve_permissions(&path, &target)?;
        let copied = Held::open(&target, FILE_LIMIT, control)?;
        if copied.media != original.media {
            return Err(STAGE_HOLD.into());
        }
        inventory.insert(relative, Some(copied.media.clone()));
        files.push(copied);
        originals.push(original);
    }
    for original in &originals {
        original.check(control)?;
    }
    for path in absent {
        if !fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            return Err(STAGE_HOLD.into());
        }
    }
    source_directory.check()?;
    if let Some(directory) = &previous_directory {
        directory.check()?;
    } else if !fs::symlink_metadata(previous)
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        return Err(STAGE_HOLD.into());
    }
    profile.validate()?;
    current.check(policy, verifier, control)?;
    retained.check(verifier, control)?;
    crate::server::sync_parent(&destination.join("migration-backup/entry"))?;
    let marker = stage_record(
        &destination.join(if final_copy {
            "final.json"
        } else {
            "initial.json"
        }),
        &Record {
            schema: 1,
            source: retained.prepared.source.clone(),
            current_sha256: current.retained.journal.media.sha256.clone(),
            prepared_sha256: retained.journal.media.sha256.clone(),
            files: inventory,
        },
        control,
    )?;
    let copy = Copy {
        directories,
        marker,
        files,
        absent: absent_copies,
    };
    copy.check(control)?;
    Ok(copy)
}

pub(super) fn reopen(
    retained: &RetainedSelection,
    expected: &str,
    current: &RetainedSelection,
    control: &Control,
) -> Result<Copy> {
    reopen_at(retained, expected, current, control, false)
}

pub(super) fn reopen_final(
    retained: &RetainedSelection,
    expected: &str,
    current: &RetainedSelection,
    control: &Control,
) -> Result<Copy> {
    reopen_at(retained, expected, current, control, true)
}

fn reopen_at(
    retained: &RetainedSelection,
    expected: &str,
    current: &RetainedSelection,
    control: &Control,
    final_copy: bool,
) -> Result<Copy> {
    let destination = retained.owned.path.join(if final_copy {
        "profile-final"
    } else {
        "profile-initial"
    });
    let directories = vec![
        Directory::open(&destination)?,
        Directory::open(&destination.join("migration-backup"))?,
    ];
    let marker = Held::open(
        &destination.join(if final_copy {
            "final.json"
        } else {
            "initial.json"
        }),
        SMALL,
        control,
    )?;
    if marker.media.sha256 != expected {
        return Err(STAGE_HOLD.into());
    }
    let record: Record =
        serde_json::from_slice(&small(&marker, control)?).map_err(|_| STAGE_HOLD)?;
    if record.schema != 1
        || record.source != retained.prepared.source
        || record.prepared_sha256 != retained.journal.media.sha256
        || record.current_sha256 != current.journal.media.sha256
        || current.prepared.source != record.source
        || !record
            .files
            .get(Path::new("profile.json"))
            .is_some_and(Option::is_some)
        || record.files.keys().cloned().collect::<BTreeSet<_>>()
            != crate::settings::update_files()
                .into_iter()
                .collect::<BTreeSet<_>>()
    {
        return Err(STAGE_HOLD.into());
    }
    let mut files = Vec::new();
    let mut absent = Vec::new();
    for (relative, expected) in record.files {
        let path = destination.join(relative);
        if let Some(expected) = expected {
            let file = Held::open(&path, FILE_LIMIT, control)?;
            if file.media != expected {
                return Err(STAGE_HOLD.into());
            }
            files.push(file);
        } else if !fs::symlink_metadata(&path)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            return Err(STAGE_HOLD.into());
        } else {
            absent.push(path);
        }
    }
    let copy = Copy {
        directories,
        marker,
        files,
        absent,
    };
    copy.check(control)?;
    Ok(copy)
}

/// Held only after the real parent's kernel exit and independent profile-lock
/// acquisition. A health probe may mutate its private copy, never these files.
pub(super) struct Originals {
    directories: Vec<Directory>,
    files: Vec<Held>,
    absent: Vec<PathBuf>,
}
impl Originals {
    pub(super) fn check(&self, control: &Control) -> Result<()> {
        for directory in &self.directories {
            directory.check()?;
        }
        for file in &self.files {
            file.check(control)?;
        }
        for path in &self.absent {
            control.check()?;
            if !fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                return Err(STAGE_HOLD.into());
            }
        }
        Ok(())
    }
}
impl Copy {
    pub(super) fn protect_originals(
        &self,
        profile: &crate::settings::UpdateProfile,
        control: &Control,
    ) -> Result<Originals> {
        self.check(control)?;
        profile.validate()?;
        let root = profile.locations.profile();
        let final_directory = self.marker.path.parent().ok_or(STAGE_HOLD)?;
        if self.marker.path.file_name().and_then(|v| v.to_str()) != Some("final.json")
            || final_directory.file_name().and_then(|v| v.to_str()) != Some("profile-final")
            || final_directory.parent().and_then(Path::parent)
                != Some(root.join("windows-prepared").as_path())
        {
            return Err(STAGE_HOLD.into());
        }
        let record: Record =
            serde_json::from_slice(&small(&self.marker, control)?).map_err(|_| STAGE_HOLD)?;
        let mut directories = vec![Directory::open(&root)?];
        let mut absent = vec![];
        match fs::symlink_metadata(root.join("migration-backup")) {
            Ok(_) => directories.push(Directory::open(&root.join("migration-backup"))?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                absent.push(root.join("migration-backup"))
            }
            Err(_) => return Err(STAGE_HOLD.into()),
        }
        let mut files = vec![];
        for (relative, media) in record.files {
            let path = root.join(relative);
            if let Some(media) = media {
                let held = Held::open(&path, FILE_LIMIT, control)?;
                if held.media != media {
                    return Err(STAGE_HOLD.into());
                }
                files.push(held);
            } else {
                absent.push(path);
            }
        }
        let result = Originals {
            directories,
            files,
            absent,
        };
        result.check(control)?;
        self.check(control)?;
        Ok(result)
    }
}
