//! Shared native Save staging and publication. Source I/O never holds Engine.
//! Ordinary adoption remains closed pending recovery and remaining native routes.
use super::*;
use crate::{
    engine::{authority::NativeSeed, view::DocumentSnapshot},
    file_safety::NativeLease,
    psd::{
        bulk36::BulkLimits, canonical36::Limits, memory36::MetadataCharge, native_rows36,
        save36::SaveLimits, Format,
    },
    saving::Control,
};
mod publication;
#[cfg(test)]
mod tests;

struct Capture {
    seed: NativeSeed,
    scene: crate::engine::scene::SceneSnapshot,
    saved_revision: u64,
    history: crate::engine::native::HistorySnapshot,
}
impl Capture {
    fn validate(&self, engine: &Engine, control: &Control) -> Result<(), String> {
        engine.ensure_open()?;
        self.seed.validate(engine)?;
        if engine.saved_revision != self.saved_revision
            || !engine
                .saving
                .as_ref()
                .is_some_and(|save| save.same(control))
        {
            return Err("Native save source or request changed; current work preserved".into());
        }
        Ok(())
    }
}

pub(super) fn save(
    shared: &Shared,
    path: &Path,
    request: &Value,
    control: &Control,
    before_commit: impl FnOnce(),
) -> Result<(), String> {
    let format = match path.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("psd") => Format::Psd,
        Some(e) if e.eq_ignore_ascii_case("psb") => Format::Psb,
        _ => return Err("Working documents must use .psd or .psb".into()),
    };
    let captured = {
        let engine = shared.lock().unwrap();
        crate::workspace::validate_target(&engine, request)?;
        engine.ensure_open()?;
        let seed = engine
            .native_seed()?
            .ok_or("Native save authority changed")?;
        control.read_status(|status| {
            if status.document_id != seed.document().metadata().id()
                || status.revision != seed.document().metadata().revision()
                || status.finished
            {
                Err("Native save differs from the begun request")
            } else {
                Ok(())
            }
        })?;
        let captured = Capture {
            history: engine.capture_native_history(&seed)?,
            scene: engine.capture_scene(None)?.into_native()?,
            seed,
            saved_revision: engine.saved_revision,
        };
        captured.validate(&engine, control)?;
        captured
    };
    let source = DocumentSnapshot::Native(captured.seed.document().clone());
    crate::compatibility::authorize_snapshot(&source, request)?;
    drop(source);
    let session = crate::workspace::native_dispatch::begin_edit_scene(shared, &captured.scene)?;
    let current = || captured.validate(&shared.lock().unwrap(), control);
    let active = || {
        control.check()?;
        session.check()?;
        current()
    };
    active()?;
    control.progress("Checking native destination", 0, 0)?;
    let lease = NativeLease::acquire_admitted(session.admit_destination(path, &active)?, active)?;
    let extension = lease.path().extension().and_then(|e| e.to_str());
    if !matches!((format, extension), (Format::Psd, Some(e)) if e.eq_ignore_ascii_case("psd"))
        && !matches!((format, extension), (Format::Psb, Some(e)) if e.eq_ignore_ascii_case("psb"))
    {
        return Err("Save alias must preserve its PSD/PSB destination format".into());
    }
    let same_destination = captured
        .seed
        .source_path()
        .map(|source| lease.same_destination(source, &active))
        .transpose()?
        .unwrap_or(false);
    if same_destination {
        if lease.expected() != captured.seed.file_version() {
            return Err(
                "Photoshop file changed outside PeerBrush. Use Save As to preserve both versions."
                    .into(),
            );
        }
    } else if lease.expected().is_some() && request["overwrite"] != true {
        return Err(
            "Destination exists. Confirm overwrite=true or choose a new Save As path.".into(),
        );
    }
    let pool = captured.scene.budget();
    if lease.expected().is_some() {
        crate::file_safety::NativeExistingDestination::preflight(&lease, pool, &active)?;
    }
    // Retained runtime path/ownership headers need admission after this session ends.
    let headers = lease
        .path()
        .as_os_str()
        .len()
        .checked_mul(16)
        .and_then(|n| n.checked_add(64 * 1024))
        .ok_or("Native saved-file header admission overflow")?;
    let file_charge = MetadataCharge::reserve(pool, headers, &active)?;
    let destination = lease.path().to_owned();
    let name = destination
        .file_name()
        .ok_or("Native saved filename missing")?
        .to_string_lossy();
    let mut prepared_name = Some(captured.history.prepare_save_name(
        captured.seed.document(),
        &name,
        &active,
    )?);
    let ownership = crate::file_ownership::Ownership::save_admitted(
        &destination,
        captured.seed.project_id(),
        file_charge.clone(),
    )?;
    let mut inventory = lease.freeze_inventory(pool, &active)?;
    control.progress("Preparing native Photoshop layers", 0, 0)?;
    let candidate = native_rows36::with_styles(
        &captured.scene,
        &session,
        &active,
        |rows| {
            psd::save36::prepare(
                &captured.scene,
                format,
                &session,
                rows,
                SaveLimits {
                    metadata: Limits {
                        bytes: 64 * 1024 * 1024,
                        nodes: 8_388_608,
                        depth: 128,
                    },
                    bulk: BulkLimits {
                        rasters: 65536,
                        tiles: 1_048_576,
                        references: 1_048_576,
                        chunks: 16384,
                        encoded_bytes: 64 * 1024 * 1024 * 1024,
                    },
                    file_bytes: crate::file_safety::NATIVE_FILE_BYTES,
                },
                &|stage, completed, total| control.progress(stage, completed, total),
                &|| {
                    control.check()?;
                    current()
                },
                &|| Ok(()),
            )
        },
        |candidate| candidate.retire(&|| session.cleanup_check()),
    )?;
    let published = (|| {
        control.progress("Preparing verified Photoshop file for publication", 0, 0)?;
        let observed = inventory.observe_external(&active)?;
        let report = |stage: &str, completed, total| control.progress(stage, completed, total);
        let mut candidate_progress = psd::mib_progress(
            &report,
            "Checking verified Photoshop file before staging",
            candidate.bytes(),
        );
        let intent =
            session.prepare_publication(&candidate, &observed, &active, &mut candidate_progress)?;
        drop(observed);
        control.progress("Preparing Photoshop destination", 0, 0)?;
        let mut writer = publication::Writer::create(
            &session,
            &candidate,
            &mut inventory,
            &intent,
            lease.expected().is_some(),
            &active,
        )?;
        let mut copy_progress = psd::mib_progress(
            &report,
            "Copying verified Photoshop file to destination staging",
            candidate.bytes(),
        );
        let replay = writer
            .replay(&candidate, &active, &mut |copied| {
                if copied == candidate.bytes() {
                    report("Verifying Photoshop destination staging", 0, 0)
                } else {
                    copy_progress(copied)
                }
            })
            .and_then(|_| writer.ready_to_arm(&active))
            .and_then(|_| {
                ownership.prepare(
                    &destination
                        .parent()
                        .ok_or("Native destination parent missing")?
                        .join(intent.temporary_name()),
                )
            });
        if let Err(reason) = replay {
            let cleanup = writer
                .abort(&|| session.cleanup_check())
                .and_then(|receipt| intent.abort(receipt.proof(), &|| session.cleanup_check()));
            return Err(with_cleanup(reason, cleanup.err()));
        }
        let armed = intent.prearm(&candidate, control, &active)?;
        before_commit();
        // All filesystem work and allocations precede this memory-only fence.
        let mut armed = Some(armed);
        let mut refused = None;
        let commit = {
            let engine = shared.lock().unwrap();
            captured
                .validate(&engine, control)
                .and_then(|_| {
                    engine
                        .validate_native_save_name(&captured.seed, prepared_name.as_ref().unwrap())
                })
                .and_then(|_| {
                    armed.take().unwrap().commit(control).map_err(|failure| {
                        let reason = failure.reason().to_owned();
                        refused = Some(failure);
                        reason
                    })
                })
        };
        let committed = match commit {
            Ok(committed) => committed,
            Err(reason) => {
                let cleanup = writer
                    .abort(&|| session.cleanup_check())
                    .and_then(|receipt| {
                        if let Some(refused) = refused {
                            return refused.abort(receipt.proof(), &|| session.cleanup_check());
                        }
                        armed.map_or_else(
                            || Err("Native publication intent retained after fence refusal".into()),
                            |armed| armed.abort(receipt.proof(), &|| session.cleanup_check()),
                        )
                    });
                return Err(with_cleanup(reason, cleanup.err()));
            }
        };
        control.progress("Publishing verified native Photoshop file", 0, 0)?;
        let receipt = writer.publish(&committed, &|| committed.check())?;
        let version = receipt.version();
        ownership.published(version.identity);
        committed.finish(receipt.proof(), &|| session.cleanup_check())?;
        drop(receipt);
        let retired_result = {
            let mut engine = shared.lock().unwrap();
            captured.validate(&engine, control)?;
            match engine.commit_native_save_name(
                &captured.seed,
                control,
                prepared_name.take().unwrap(),
            ) {
                Err(refused) => Err(refused),
                Ok(document) => {
                    let retired = (
                        engine.path.take(),
                        engine.file_ownership.take(),
                        engine.file_state_charge.take(),
                        document,
                    );
                    engine.path = Some(destination);
                    engine.file_version = Some(version);
                    engine.saved_revision = captured.scene.identity().revision;
                    engine.file_ownership = Some(ownership);
                    engine.file_state_charge = Some(file_charge);
                    Ok(retired)
                }
            }
        };
        let retired = retired_result.map_err(|refused| refused.reason().to_owned())?;
        if let Some(owner) = &retired.1 {
            owner.release();
        }
        drop(retired);
        Ok(())
    })();
    // Every candidate/source owner is retired after the final Engine guard drops.
    let cleanup = candidate.retire(&|| session.cleanup_check());
    match published {
        Err(reason) => Err(with_cleanup(reason, cleanup.err())),
        Ok(()) => cleanup.map_err(|reason| {
            format!("Native file saved; staging cleanup remains pending: {reason}")
        }),
    }
}

fn with_cleanup(reason: String, cleanup: Option<String>) -> String {
    match cleanup {
        Some(cleanup) => format!("{reason}; native publication cleanup retained: {cleanup}"),
        None => reason,
    }
}
