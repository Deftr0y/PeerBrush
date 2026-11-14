//! Runtime-only replay ownership. Neither commands nor baselines enter PSD data.
use super::*;
use serde_json::Value;
#[cfg(test)]
#[path = "wand_tests.rs"]
mod tests;

pub(crate) struct Replay {
    id: String,
    command: Admitted<Value>,
    // A single baseline with no nested replay; all artwork sources are shared.
    baseline: NativeDocument,
    captured: DocumentKey,
    token: CommandToken,
}
impl Replay {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(crate) fn command(&self) -> &Value {
        &self.command
    }
    pub(crate) fn baseline(&self) -> &NativeDocument {
        &self.baseline
    }

    pub(crate) fn prepare(
        edit: &CapturedEdit,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Admitted<Self>> {
        edit.cancellation.check()?;
        check()?;
        let descriptor = edit
            .descriptor()
            .ok_or("Wand replay requires normalized capture")?;
        let command = descriptor.commands().next().ok_or("Wand command missing")?;
        let parameters = command.parameters();
        if descriptor.commands().len() != 1
            || command.method() != Method::Selection
            || !(parameters["op"] == "selection.wand_update"
                || parameters["op"] == "selection" && parameters["kind"] == "wand")
        {
            return Err("Wand replay differs from its captured command".into());
        }
        let base = edit.document();
        let charge =
            MetadataCharge::reserve(base.budget(), std::mem::size_of::<Self>() + 2048, check)?;
        let (id, normalized, baseline) = if parameters["op"] == "selection.wand_update" {
            let old = base
                .wand()
                .ok_or("Click with the Magic Wand again after changing the artwork or selection")?;
            let copied: Admitted<Value> = copy_admitted(old.command(), base.budget(), true, check)?;
            let normalized = copied.map_unique(|mut value, _| {
                crate::selection::wand::update(&mut value, parameters)?;
                Ok(value)
            })?;
            (old.id.clone(), normalized, old.baseline.clone())
        } else {
            (
                crate::engine::id(),
                copy_admitted(
                    &crate::selection::wand::Command(parameters),
                    base.budget(),
                    true,
                    check,
                )?,
                restamp(base, base.metadata().revision(), None, check)?,
            )
        };
        edit.cancellation.check()?;
        check()?;
        Ok(Admitted::from_parts(
            Arc::new(Self {
                id,
                command: normalized,
                baseline,
                captured: base.key(),
                token: command.token(),
            }),
            charge,
        ))
    }
}

impl NativeDocument {
    pub(crate) fn wand(&self) -> Option<&Replay> {
        self.core.wand.as_deref()
    }

    /// Runtime replay sources participate in provisional reachability, separately
    /// from the persisted raster roster and its standard-channel/source hashes.
    pub(super) fn owned_sources(&self) -> impl Iterator<Item = &Surface> {
        self.roster().iter().map(|(_, source)| source).chain(
            self.core
                .wand
                .iter()
                .flat_map(|wand| wand.baseline.roster().iter().map(|(_, source)| source)),
        )
    }
}

impl PreparedDelta {
    pub(crate) fn with_wand(
        mut self,
        replay: Admitted<Replay>,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<Self, RefusedDelta> {
        let prepared = (|| {
            self.cancellation.check()?;
            check()?;
            let descriptor = self
                .admission
                .as_ref()
                .and_then(|a| a.descriptor())
                .ok_or("Wand result requires normalized capture")?;
            let command = descriptor
                .commands()
                .next()
                .ok_or("Wand result command missing")?;
            if descriptor.commands().len() != 1
                || replay.token != command.token()
                || replay.captured != self.base
                || command.method() != Method::Selection
                || !(command.parameters()["op"] == "selection.wand_update"
                    || command.parameters()["op"] == "selection"
                        && command.parameters()["kind"] == "wand")
            {
                return Err("Wand result belongs to another captured command".into());
            }
            restamp(
                &self.candidate,
                self.candidate.metadata().revision(),
                Some(replay),
                check,
            )
        })();
        match prepared {
            Ok(candidate) => {
                self.candidate = candidate;
                Ok(self)
            }
            Err(reason) => Err(RefusedDelta {
                reason,
                prepared: self,
            }),
        }
    }
}

pub(super) fn restamp(
    source: &NativeDocument,
    revision: u64,
    replay: Option<Admitted<Replay>>,
    check: &dyn Fn() -> Result<()>,
) -> Result<NativeDocument> {
    restamp_named(source, revision, source.metadata().name(), replay, check)
}
pub(super) fn restamp_named(
    source: &NativeDocument,
    revision: u64,
    name: &str,
    replay: Option<Admitted<Replay>>,
    check: &dyn Fn() -> Result<()>,
) -> Result<NativeDocument> {
    check()?;
    if let Some(replay) = &replay {
        if !replay.owner().uses_budget(source.budget())
            || !replay.baseline.budget().same_pool(source.budget())
            || !replay
                .baseline
                .disk_budget()
                .same_pool(source.disk_budget())
            || replay.baseline.metadata().id() != source.metadata().id()
            || replay.baseline.core.wand.is_some()
        {
            return Err("Wand baseline differs from its native document admission".into());
        }
    }
    let different = name != source.metadata().name();
    let _name_charge = different
        .then(|| {
            MetadataCharge::reserve(
                source.budget(),
                name.len()
                    .checked_add(128)
                    .ok_or("Saved name admission overflow")?,
                check,
            )
        })
        .transpose()?;
    let mut copied_name = String::new();
    if different {
        copied_name
            .try_reserve_exact(name.len())
            .map_err(|_| "Saved name allocation refused")?;
        if copied_name.capacity() > name.len() {
            return Err("Saved name exceeds its admission".into());
        }
        copied_name.push_str(name);
    }
    let metadata = copy_metadata(&source.core.metadata, source.budget(), check)?;
    let roster = source.core.roster.clone();
    let resident = source.budget().clone();
    let disk = source.disk_budget().clone();
    let core = metadata.rehome_unique(|mut metadata, _| {
        metadata.revision = revision;
        if different {
            metadata.name = copied_name;
        }
        let (mut core, charge) = Core::prepare(metadata, roster, resident, disk, check)?;
        core.wand = replay;
        check()?;
        Ok((core, charge))
    })?;
    Ok(NativeDocument { core })
}
