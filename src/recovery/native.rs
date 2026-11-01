//! Live Native recovery uses only the configured source owner and typed v3 proof.
use super::*;
use crate::{
    engine::scene::SceneSnapshot,
    large_document::{BlockRef, Snapshot as SourceSnapshot, Store},
    psd::{canonical36, memory36::MetadataCharge},
    workspace::source_runtime::SourceSession,
};
pub(super) fn limits() -> backed::Limits {
    backed::Limits {
        metadata: canonical36::Limits {
            bytes: 64 * 1024 * 1024,
            nodes: 8_388_608,
            depth: 128,
        },
        refs: 1_048_576,
        stores: 4096,
    }
}
// DISPLAY ONLY: never replaces the name in sealed recovery metadata.
fn display_name(name: &str) -> (String, bool) {
    if let Some((end, _)) = name.char_indices().nth(256) {
        (format!("{}\u{2026}", &name[..end]), true)
    } else {
        (name.to_owned(), false)
    }
}
fn hex(bytes: [u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    use std::fmt::Write;
    for byte in bytes {
        write!(&mut text, "{byte:02x}").expect("writing to String");
    }
    text
}
/// Seal all physical owners, including masks, retained originals and typed clips.
/// A foreign CMIT never substitutes for the Store owning the padded native bytes.
fn seal_sources(
    scene: &SceneSnapshot,
    session: &SourceSession,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(Vec<SourceSnapshot>, Arc<MetadataCharge>), String> {
    let count = scene.rasters().iter().try_fold(0usize, |n, (_, surface)| {
        n.checked_add(surface.physical_tiles().count())
            .filter(|n| *n <= limits().refs)
            .ok_or("Native recovery physical reference count exceeded")
    })?;
    let bytes = count
        .checked_mul(
            2 * std::mem::size_of::<(Store, BlockRef)>() + 2 * std::mem::size_of::<BlockRef>(),
        )
        .and_then(|n| n.checked_add(limits().stores * 2 * std::mem::size_of::<SourceSnapshot>()))
        .ok_or("Native recovery source admission overflow")?;
    let owner = MetadataCharge::reserve(&session.resident(), bytes.max(1), check)?;
    let mut physical = Vec::new();
    physical
        .try_reserve_exact(count)
        .map_err(|_| "Native recovery source allocation refused")?;
    for (_, surface) in scene.rasters().iter() {
        for tile in surface.physical_tiles() {
            check()?;
            if physical.len() == count {
                return Err("Native recovery source roster changed".into());
            }
            physical.push((tile.owner().clone(), tile.reference()));
        }
    }
    if physical.len() != count {
        return Err("Native recovery source roster is incomplete".into());
    }
    physical.sort_unstable_by_key(|(store, reference)| (store.identity(), reference.fingerprint()));
    physical.dedup_by(|a, b| a.0.same_owner(&b.0) && a.1.fingerprint() == b.1.fingerprint());
    let mut refs = Vec::new();
    refs.try_reserve_exact(count)
        .map_err(|_| "Native recovery seal allocation refused")?;
    let mut seals = Vec::new();
    seals
        .try_reserve_exact(limits().stores)
        .map_err(|_| "Native recovery prefix allocation refused")?;
    let capacity = physical
        .capacity()
        .checked_mul(std::mem::size_of::<(Store, BlockRef)>())
        .and_then(|n| n.checked_add(refs.capacity() * std::mem::size_of::<BlockRef>()))
        .and_then(|n| n.checked_add(seals.capacity() * std::mem::size_of::<SourceSnapshot>()))
        .ok_or("Native recovery source capacity overflow")?;
    if capacity > owner.bytes() {
        return Err("Native recovery source capacity exceeds admission".into());
    }
    let binding = scene.render_key()?;
    let mut at = 0;
    while at < physical.len() {
        check()?;
        session.check()?;
        let store = &physical[at].0;
        refs.clear();
        let mut end = at;
        while end < physical.len() && physical[end].0.same_owner(store) {
            refs.push(physical[end].1);
            end += 1;
        }
        if seals.len() == limits().stores {
            return Err("Native recovery Store admission exceeded".into());
        }
        seals.push(session.seal_store(store, &refs, &binding)?);
        at = end;
    }
    check()?;
    Ok((seals, owner))
}
pub(super) fn checkpoint(
    workspace: &workspace::Registry,
    shared: &server::Shared,
    scene: SceneSnapshot,
    path: Option<PathBuf>,
) -> Result<Snapshot, String> {
    let session = workspace::native_dispatch::begin_scene(shared, &scene)?;
    let check = || {
        session.check()?;
        let e = shared
            .try_lock()
            .map_err(|_| "Project is busy during native recovery")?;
        e.ensure_open()?;
        if e.project_id != scene.identity().project_id
            || e.path != path
            || e.file_version != scene.file_version()
            || !e
                .native_document()
                .is_some_and(|doc| doc.key() == scene.key())
            || e.saved_revision == scene.identity().revision
        {
            return Err("Native recovery source changed; checkpoint was not selected".into());
        }
        drop(e);
        if workspace
            .try_lock()
            .map_err(|_| "Workspace is busy during native recovery")?
            .exiting
        {
            return Err("Workspace is exiting; native checkpoint was not selected".into());
        }
        Ok(())
    };
    let (seals, _owner) = seal_sources(&scene, &session, &check)?;
    let capture = backed::Capture::new(&scene, &seals, &session, limits(), &check)?;
    let id = *uuid::Uuid::new_v4().as_bytes();
    let (bytes, sha256, timestamp_ms) = capture.publish_info(id, &session, &check)?;
    let snapshot = uuid::Uuid::from_bytes(id).to_string();
    let (name, name_is_excerpt) = display_name(scene.metadata().name());
    Ok(Snapshot {
        version: 3,
        file: format!("recovery-v3-{snapshot}.pbr"),
        snapshot,
        project_id: scene.identity().project_id.clone(),
        document_id: scene.identity().document_id.clone(),
        revision: scene.identity().revision,
        name,
        name_is_excerpt,
        original_path: None,
        bit_depth: scene.metadata().depth(),
        timestamp_ms,
        bytes,
        checksum: hex(sha256),
    })
}
pub(super) fn catalog(
    workspace: &workspace::Registry,
    mut entries: Vec<Snapshot>,
) -> Result<RecoveryCatalog, String> {
    let runtime = workspace.lock().unwrap().source_runtime.clone();
    let Some(runtime) = runtime else {
        return Ok(RecoveryCatalog {
            entries,
            _native_admission: None,
        });
    };
    let Some(session) = runtime.begin_existing()? else {
        return Ok(RecoveryCatalog {
            entries,
            _native_admission: None,
        });
    };
    let ids = session.recovery_marker_ids()?;
    let count = ids.iter().count();
    let amount = count
        .checked_mul(2 * std::mem::size_of::<Snapshot>() + 2304)
        .and_then(|n| n.checked_add(entries.capacity() * std::mem::size_of::<Snapshot>()))
        .ok_or("Recovery listing admission overflow")?;
    let owner = MetadataCharge::reserve(&session.resident(), amount.max(1), &|| Ok(()))?;
    entries
        .try_reserve_exact(count)
        .map_err(|_| "Recovery listing allocation refused")?;
    if entries
        .capacity()
        .checked_mul(std::mem::size_of::<Snapshot>())
        .is_none_or(|n| n > owner.bytes())
    {
        return Err("Recovery listing capacity exceeds admission".into());
    }
    for id in ids.iter() {
        let decoded = backed::load(&session, id, limits(), &|| Ok(()))?;
        let metadata = decoded.pending.metadata();
        let (name, name_is_excerpt) = display_name(&metadata.name);
        let snapshot = uuid::Uuid::from_bytes(id).to_string();
        entries.push(Snapshot {
            version: 3,
            snapshot: snapshot.clone(),
            file: format!("recovery-v3-{snapshot}.pbr"),
            project_id: uuid::Uuid::from_bytes(decoded.project).to_string(),
            document_id: metadata.id.clone(),
            revision: metadata.revision,
            name,
            name_is_excerpt,
            original_path: None,
            bit_depth: metadata.bit_depth,
            timestamp_ms: decoded.timestamp,
            bytes: decoded.marker.len(),
            checksum: hex(decoded.sha256),
        });
        decoded.marker.check()?;
    }
    entries.sort_by(|a, b| {
        b.timestamp_ms
            .cmp(&a.timestamp_ms)
            .then(b.revision.cmp(&a.revision))
            .then(a.snapshot.cmp(&b.snapshot))
    });
    session.check()?;
    Ok(RecoveryCatalog {
        entries,
        _native_admission: Some(owner),
    })
}
pub(super) fn restore(
    workspace: &workspace::Registry,
    id: [u8; 16],
    control: &crate::loading::Control,
    initiating: Option<&str>,
) -> Result<server::Shared, String> {
    let runtime = workspace
        .lock()
        .unwrap()
        .source_runtime
        .clone()
        .ok_or("Native recovery source service is unavailable")?;
    let session = runtime
        .begin_existing()?
        .ok_or("Native recovery source storage is absent; checkpoint retained")?;
    let decoded = backed::load(&session, id, limits(), &|| control.check())?;
    let document = crate::engine::native::NativeDocument::from_pending(decoded.pending)?;
    decoded.marker.check()?;
    control.check()?;
    let mut engine = engine::Engine::new_native(document)?;
    engine.saved_revision = u64::MAX;
    engine.status = "Recovered into an unsaved tab - choose Save As - recovery retained".into();
    // Registration prepares the fresh identity off engine/registry locks. The
    // actual marker, prefixes and source admission survive until it returns.
    let shared = workspace::register_in(workspace, engine, initiating)?;
    drop(decoded.seals);
    drop(decoded.marker);
    Ok(shared)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn labels_are_bounded_unicode_display_only_and_preserve_the_original() {
        let original = "\u{5f69}\u{1f58c}".repeat(180);
        let (label, excerpt) = display_name(&original);
        assert!(excerpt);
        assert_eq!(label.chars().count(), 257);
        assert!(label.ends_with('\u{2026}'));
        assert_eq!(original.chars().count(), 360);
        assert_eq!(
            display_name("native source"),
            ("native source".into(), false)
        );
    }
}
