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
    let destination = retained.owned.path.join("profile-initial");
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
        &destination.join("initial.json"),
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
    control: &Control,
) -> Result<Copy> {
    let destination = retained.owned.path.join("profile-initial");
    let directories = vec![
        Directory::open(&destination)?,
        Directory::open(&destination.join("migration-backup"))?,
    ];
    let marker = Held::open(&destination.join("initial.json"), SMALL, control)?;
    if marker.media.sha256 != expected {
        return Err(STAGE_HOLD.into());
    }
    let record: Record =
        serde_json::from_slice(&small(&marker, control)?).map_err(|_| STAGE_HOLD)?;
    if record.schema != 1
        || record.source != retained.prepared.source
        || record.prepared_sha256 != retained.journal.media.sha256
        || !sha(&record.current_sha256)
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
