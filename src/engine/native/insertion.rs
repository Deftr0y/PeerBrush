//! Structural publication for rendered layers, paired with sealed native pixels.
use super::*;
use crate::engine::scene::{Content36, Mask36, MaskStep36, Source36};

pub(super) fn mask_target<'a>(
    base: &'a NativeDocument,
    parameters: &serde_json::Value,
) -> Result<LayerMetadata<'a>> {
    let id = parameters["layer"]
        .as_str()
        .ok_or("Choose a target layer")?;
    super::patch::unlocked(&base.core.metadata, id)?;
    let layer = base
        .metadata()
        .layers()
        .find(|layer| layer.id() == id)
        .ok_or("Unknown native layer")?;
    let replacing_color = parameters["op"] == "mask.from_color"
        && parameters["mode"].as_str().unwrap_or("replace") == "replace";
    if !replacing_color && layer.mask().is_some_and(|mask| mask.steps().len() >= 32) {
        return Err("Mask stack limit: 32 steps".into());
    }
    Ok(layer)
}

pub(super) struct SourcePlacement<'a> {
    parent: Option<&'a str>,
    name: &'a str,
    origin: [i32; 2],
    pub(super) frame: [u32; 2],
    pub(super) matrix: [f32; 6],
}

pub(super) fn source_placement<'a>(
    base: &NativeDocument,
    parameters: &'a serde_json::Value,
    source: &Source36,
) -> Result<SourcePlacement<'a>> {
    let metadata = &base.core.metadata;
    if metadata.layers.len() >= 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let parent = parameters["parent"].as_str();
    if let Some(parent) = parent {
        if !metadata
            .layers
            .iter()
            .any(|layer| layer.id == parent && layer.kind == "group")
        {
            return Err("Parent must be an existing group".into());
        }
        super::patch::unlocked(metadata, parent)?;
    }
    let mut origin = if parameters["op"] == "path.create" {
        let frame = crate::path::creation_frame([metadata.width, metadata.height], parameters)?;
        if [source.width, source.height]
            != [(frame[2] - frame[0]) as u32, (frame[3] - frame[1]) as u32]
            || !matches!(source.definition(), Content36::Bezier { .. })
        {
            return Err("Path source differs from its captured frame".into());
        }
        [frame[0], frame[1]]
    } else {
        let mut origin = [0; 2];
        for (key, coordinate) in ["x", "y"].into_iter().zip(&mut origin) {
            if let Some(value) = parameters.get(key) {
                *coordinate = value
                    .as_i64()
                    .filter(|v| (-100000..=100000).contains(v))
                    .ok_or("Source origin must be an integer within coordinate limits")?
                    as i32;
            }
        }
        origin
    };
    if origin.iter().any(|v| !(-100000..=100000).contains(v)) {
        return Err("Source origin must be an integer within coordinate limits".into());
    }
    let label = match source.definition() {
        Content36::Bezier { .. } => "Pen path",
        Content36::Text { .. } => "Text",
        Content36::Shape { .. } => "Vector shape",
        Content36::Svg { .. } => "SVG",
    };
    let mut frame = [source.width, source.height];
    let mut matrix = source.matrix;
    if crate::source::fit_text_requested(parameters)? {
        if !matches!(source.definition(), Content36::Text { .. }) {
            return Err("Only editable text can fit its transformed field".into());
        }
        // The admitted output uses the adjusted matrix. Recompute its frame
        // from the immutable input at both preparation and insertion.
        let original = match parameters["source"].get("matrix") {
            Some(value) => serde_json::from_value::<[f32; 6]>(value.clone())
                .map_err(|_| "Invalid editable-source transform")?,
            None => [1., 0., 0., 1., 0., 0.],
        };
        (frame, origin, matrix) = crate::source::projected_text_frame(original, frame, None, origin)?;
        crate::raster::CanvasLimits::Native.check(frame[0], frame[1])?;
    }
    Ok(SourcePlacement {
        parent,
        name: parameters["name"].as_str().unwrap_or(label),
        origin,
        frame,
        matrix,
    })
}

#[must_use]
pub(crate) struct RefusedInsertion {
    reason: String,
    output: StagedOutput,
}
impl std::fmt::Debug for RefusedInsertion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RefusedInsertion")
            .field(&self.reason)
            .finish()
    }
}
impl RefusedInsertion {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn into_output(self) -> StagedOutput {
        self.output
    }
    pub fn abort(
        self,
        session: &impl super::super::scene::StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        self.output.abort(session, cleanup)
    }
}
impl CapturedEdit {
    pub(crate) fn append_mask(
        self,
        token: CommandToken,
        output: StagedOutput,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedDelta, RefusedInsertion> {
        let prepared = (|| {
            self.cancellation.check()?;
            check()?;
            let descriptor = self
                .descriptor()
                .ok_or("Mask insertion requires a normalized command")?;
            let command = descriptor
                .commands()
                .next()
                .ok_or("Missing mask insertion command")?;
            if descriptor.commands().len() != 1
                || command.token() != token
                || !((command.method() == Method::Mask
                    && matches!(
                        command.parameters()["op"].as_str(),
                        Some(
                            "mask.add" | "mask.from_selection" | "mask.from_color" | "mask.refine"
                        )
                    ))
                    || (command.method() == Method::Source
                        && command.parameters()["op"] == "path.to_mask"))
            {
                return Err("Mask insertion differs from its captured command".into());
            }
            let layer = mask_target(&self.base, command.parameters())?;
            let color = command.parameters()["op"] == "mask.from_color";
            let replace = if color {
                let parameters = crate::smart_mask::Parameters::new(
                    command.parameters(),
                    layer.origin(),
                    [layer.pixels().width(), layer.pixels().height()],
                )?;
                parameters.mode == "replace" || layer.mask().is_none()
            } else {
                false
            };
            if command.parameters()["op"] == "mask.add" {
                if layer.mask().is_some() {
                    return Err("Layer already has a mask".into());
                }
                if self.base.metadata().selection().is_none() {
                    return Err("Selected mask insertion requires a current selection".into());
                }
            }
            let surface = output.surface();
            if surface.width() != layer.pixels().width()
                || surface.height() != layer.pixels().height()
                || surface.depth() != layer.pixels().depth()
                || !surface.uses_budget(self.base.budget())
                || !surface.uses_disk_budget(self.base.disk_budget())
            {
                return Err(
                    "Inserted mask must match its layer frame, precision and admission".into(),
                );
            }
            let intent = self.planned_intent(Intent::from_scopes(
                IntentKind::Pixels,
                command.write_scopes(),
                self.base.budget(),
            )?)?;
            let locations = self.base.roster().len();
            let ordinal =
                u32::try_from(locations + 1).map_err(|_| "Native mask ordinal overflow")?;
            let count = layer.mask().map_or(0, |mask| mask.steps().len());
            let bytes = (count + 2) * std::mem::size_of::<MaskStep36>() + 4096;
            let extra = MetadataCharge::reserve(self.base.budget(), bytes, check)?;
            let copied = copy_metadata(&self.base.core.metadata, self.base.budget(), check)?
                .retain_owner(&extra, check)?;
            let resident = self.base.budget().clone();
            let disk = self.base.disk_budget().clone();
            let zero = if replace {
                Some(
                    crate::raster::surface::Surface::zero(
                        surface.width(),
                        surface.height(),
                        surface.depth(),
                    )
                    .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            let core = copied.rehome_unique(|mut metadata, _| {
                let target = metadata
                    .layers
                    .iter_mut()
                    .find(|target| target.id == layer.id())
                    .ok_or("Mask target disappeared")?;
                if replace {
                    target.mask = None;
                }
                let mask = target.mask.get_or_insert_with(|| Mask36 {
                    enabled: true,
                    linked: true,
                    steps: Vec::new(),
                });
                mask.steps
                    .try_reserve_exact(if replace { 2 } else { 1 })
                    .map_err(|_| "Mask step allocation refused")?;
                if color {
                    mask.enabled = true;
                }
                if replace {
                    mask.steps.push(MaskStep36 {
                        id: crate::engine::id(),
                        kind: "fill".into(),
                        enabled: true,
                        weight: 1.,
                        value: 0.,
                        pixels: RasterRef36 {
                            id: ordinal + 1,
                            width: surface.width(),
                            height: surface.height(),
                            depth: surface.depth(),
                            retained: None,
                        },
                        settings: serde_json::Value::Null.into(),
                    });
                }
                mask.steps.push(MaskStep36 {
                    id: crate::engine::id(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 1.,
                    value: 0.,
                    pixels: RasterRef36 {
                        id: ordinal,
                        width: surface.width(),
                        height: surface.height(),
                        depth: surface.depth(),
                        retained: None,
                    },
                    settings: serde_json::Value::Null.into(),
                });
                let roster = rebuild_from(
                    &mut metadata,
                    locations + if replace { 2 } else { 1 },
                    &resident,
                    &disk,
                    check,
                    |reference| {
                        if reference.id == ordinal {
                            Ok(surface.clone())
                        } else if replace && reference.id == ordinal + 1 {
                            Ok(zero.as_ref().ok_or("Color mask baseline missing")?.clone())
                        } else {
                            self.base.roster().get(reference).cloned()
                        }
                    },
                )?;
                metadata.revision = metadata
                    .revision
                    .checked_add(1)
                    .ok_or("Native revision overflow")?;
                self.cancellation.check()?;
                check()?;
                Core::prepare(metadata, roster, resident, disk, check)
            })?;
            Ok((NativeDocument { core }, intent))
        })();
        self.complete_insertion(prepared, output, check)
    }
    /// The fill producer supplies a complete zero-baseline canvas result derived
    /// from this exact command. Parent slots authorize the insertion only; their
    /// pixels are never replaced. Even a transparent fill creates a real child.
    pub(crate) fn insert_fill_child(
        self,
        token: CommandToken,
        output: StagedOutput,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedDelta, RefusedInsertion> {
        self.insert_rendered(token, output, None, check)
    }
    pub(crate) fn insert_source(
        self,
        token: CommandToken,
        output: StagedOutput,
        source: Admitted<Source36>,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedDelta, RefusedInsertion> {
        self.insert_rendered(token, output, Some(source), check)
    }
    fn insert_rendered(
        self,
        token: CommandToken,
        output: StagedOutput,
        source: Option<Admitted<Source36>>,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedDelta, RefusedInsertion> {
        let prepared = (|| {
            self.cancellation.check()?;
            check()?;
            let descriptor = self
                .descriptor()
                .ok_or("Layer insertion requires a normalized command")?;
            let command = descriptor
                .commands()
                .next()
                .ok_or("Missing layer insertion command")?;
            if descriptor.commands().len() != 1 || command.token() != token {
                return Err("Layer insertion differs from its captured command".into());
            }
            let parameters = command.parameters();
            let metadata = &self.base.core.metadata;
            if metadata.layers.len() >= 100 {
                return Err("Initial version supports up to 100 layers".into());
            }
            let (index, parent, name, origin, frame) = if let Some(source) = &source {
                if command.method() != Method::Source
                    || !matches!(
                        parameters["op"].as_str(),
                        Some("source.add" | "path.create")
                    )
                    || !source.owner().uses_budget(self.base.budget())
                {
                    return Err(
                        "Editable insertion differs from its captured command or admission".into(),
                    );
                }
                crate::source::prepared::validate_definition(
                    source,
                    self.base.budget(),
                    &mut || check(),
                )?;
                let placement = source_placement(&self.base, parameters, source)?;
                if source.matrix != placement.matrix {
                    return Err("Editable insertion differs from its projected source".into());
                }
                (0, placement.parent, placement.name, placement.origin, placement.frame)
            } else {
                let legacy = command.method() == Method::Paint && parameters["op"] == "fill";
                if !(command.method() == Method::Fill || legacy) || parameters["mask"] == true {
                    return Err("Fill insertion differs from its captured command".into());
                }
                let id = parameters["layer"]
                    .as_str()
                    .ok_or("Choose a folder to fill")?;
                let index = metadata
                    .layers
                    .iter()
                    .position(|layer| layer.id == id)
                    .ok_or("Fill folder no longer exists")?;
                if metadata.layers[index].kind != "group" {
                    return Err("Fill insertion requires a folder".into());
                }
                super::patch::unlocked(metadata, id)?;
                let mut area = parameters
                    .get("rect")
                    .and_then(crate::engine::rect)
                    .or(metadata.selection)
                    .unwrap_or([0, 0, metadata.width as i32, metadata.height as i32]);
                if let Some(selection) = metadata.selection {
                    area = [
                        area[0].max(selection[0]),
                        area[1].max(selection[1]),
                        area[2].min(selection[2]),
                        area[3].min(selection[3]),
                    ];
                }
                if !legacy
                    && (area[0].max(0) >= area[2].min(metadata.width as i32)
                        || area[1].max(0) >= area[3].min(metadata.height as i32))
                {
                    return Err("Empty foreground fill does not insert a child".into());
                }
                if !legacy {
                    let color = parameters["color"]
                        .as_array()
                        .ok_or("Fill color must be RGBA")?;
                    if color.len() != 4 || color.iter().any(|v| v.as_u64().is_none_or(|v| v > 255))
                    {
                        return Err("Fill color must contain four byte values".into());
                    }
                }
                (
                    index + 1,
                    Some(id),
                    "Foreground fill",
                    [0, 0],
                    [metadata.width, metadata.height],
                )
            };
            let surface = output.surface();
            if surface.width() != frame[0]
                || surface.height() != frame[1]
                || surface.depth() != metadata.bit_depth
                || !surface.uses_budget(self.base.budget())
                || !surface.uses_disk_budget(self.base.disk_budget())
            {
                return Err(
                    "Inserted pixels must match their frame, native precision and admission".into(),
                );
            }
            let intent = self.planned_intent(if source.is_some() {
                Intent::from_scopes(
                    IntentKind::Pixels,
                    command.write_scopes(),
                    self.base.budget(),
                )?
            } else {
                Intent::layers(
                    IntentKind::Pixels,
                    std::slice::from_ref(&metadata.layers[index - 1].id),
                    self.base.budget(),
                )?
            })?;
            let locations = self.base.roster().len();
            let ordinal =
                u32::try_from(locations + 1).map_err(|_| "Native insertion ordinal overflow")?;
            let bytes = (metadata.layers.len() + 1)
                .checked_mul(std::mem::size_of::<Layer36>())
                .and_then(|n| n.checked_add(parent.map_or(0, str::len) + name.len() + 4096))
                .ok_or("Native insertion metadata footprint overflow")?;
            // Charge the new layer array, strings and structural metadata before
            // allocation; this owner moves into the fresh header, never an old one.
            let extra = MetadataCharge::reserve(self.base.budget(), bytes, check)?;
            let copied =
                copy_metadata(metadata, self.base.budget(), check)?.retain_owner(&extra, check)?;
            let resident = self.base.budget().clone();
            let disk = self.base.disk_budget().clone();
            let core = copied.rehome_unique(|mut metadata, _| {
                metadata
                    .layers
                    .try_reserve_exact(1)
                    .map_err(|_| "Native child layer allocation refused")?;
                metadata.layers.insert(
                    index,
                    Layer36 {
                        id: crate::engine::id(),
                        name: name.into(),
                        kind: "paint".into(),
                        parent: parent.map(str::to_owned),
                        clip_to: None,
                        visible: true,
                        locked: false,
                        opacity: 1.,
                        blend: "normal".into(),
                        x: origin[0],
                        y: origin[1],
                        pixels: RasterRef36 {
                            id: ordinal,
                            width: surface.width(),
                            height: surface.height(),
                            depth: surface.depth(),
                            retained: None,
                        },
                        color: [233, 84, 32, 255],
                        mask: None,
                        effects: vec![],
                        psd_metadata: vec![].into(),
                        source: source
                            .as_ref()
                            .map(|source| source.copy_runtime(&resident, check))
                            .transpose()?,
                    },
                );
                let roster = rebuild_from(
                    &mut metadata,
                    locations + 1,
                    &resident,
                    &disk,
                    check,
                    |reference| {
                        if reference.id == ordinal {
                            Ok(surface.clone())
                        } else {
                            self.base.roster().get(reference).cloned()
                        }
                    },
                )?;
                metadata.revision = metadata
                    .revision
                    .checked_add(1)
                    .ok_or("Native revision overflow")?;
                check()?;
                self.cancellation.check()?;
                Core::prepare(metadata, roster, resident, disk, check)
            })?;
            Ok((NativeDocument { core }, intent))
        })();
        self.complete_insertion(prepared, output, check)
    }
    fn complete_insertion(
        self,
        prepared: Result<(NativeDocument, Intent)>,
        output: StagedOutput,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedDelta, RefusedInsertion> {
        let prepared = prepared.and_then(|(candidate, intent)| {
            let descriptor = self.descriptor().ok_or("Missing insertion descriptor")?;
            // The output vector's allocation must survive with its admission
            // until final publication/abort, independently of the worker stack.
            let bytes = std::mem::size_of::<StagedOutput>() + 128;
            let output_charge = Arc::new(
                self.base
                    .budget()
                    .reserve_bytes(bytes, bytes)
                    .map_err(|e| e.to_string())?,
            );
            let admission = self
                .admission
                .as_ref()
                .ok_or("Missing layer insertion admission")?
                .with_scopes(
                    &self.base,
                    descriptor.shared_scopes(),
                    output_charge,
                    self.admission
                        .as_ref()
                        .is_some_and(|a| a.is_transaction_step()),
                )?;
            let mut outputs = Vec::new();
            outputs
                .try_reserve_exact(1)
                .map_err(|_| "Native insertion output allocation refused")?;
            if outputs.capacity() != 1 {
                return Err("Native insertion output allocation exceeds admission".into());
            }
            check()?;
            self.cancellation.check()?;
            Ok((candidate, intent, admission, outputs))
        });
        match prepared {
            Err(reason) => Err(RefusedInsertion { reason, output }),
            Ok((candidate, intent, admission, mut outputs)) => {
                outputs.push(output);
                Ok(PreparedDelta {
                    base: self.base.key(),
                    candidate,
                    cancellation: self.cancellation,
                    intent,
                    outputs,
                    admission: Some(admission),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::output::tests::Session;
    use super::*;
    use serde_json::json;
    fn group(pool: &ResidentBudget) -> NativeDocument {
        let base = super::super::tests::document(pool);
        let copied = copy_metadata(&base.core.metadata, pool, &|| Ok(())).unwrap();
        let disk = base.disk_budget().clone();
        let resident = pool.clone();
        let roster = RasterRoster::new(vec![Surface::zero(8, 8, 8).unwrap()]).unwrap();
        let bytes = roster.descriptor_bytes().unwrap();
        let charge = pool.reserve_bytes(bytes, bytes).unwrap();
        let core = copied
            .map_unique(|mut metadata, _| {
                metadata.layers[0].kind = "group".into();
                metadata.layers[0].pixels.depth = 8;
                Ok(Core {
                    metadata,
                    roster: Arc::new(OwnedRoster {
                        roster,
                        _charge: charge,
                    }),
                    resident,
                    disk,
                    key: key(),
                    render_key: OnceLock::new(),
                    wand: None,
                    protected_structure: None,
                })
            })
            .unwrap();
        NativeDocument { core }
    }
    fn capture(base: &NativeDocument, command: &serde_json::Value) -> CapturedEdit {
        let descriptor = CommandDescriptor::from_normalized(
            base,
            base.metadata().revision(),
            std::slice::from_ref(command),
            &|| Ok(()),
        )
        .unwrap();
        base.capture_descriptor(descriptor, "worker", &[], 1, false, true)
            .unwrap()
    }
    fn output(base: &NativeDocument, depth: u16) -> StagedOutput {
        OutputDraft::new(Surface::zero(8, 8, depth).unwrap(), vec![])
            .seal(base, &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()))
    }
    #[test]
    fn transparent_child_uses_document_precision_and_one_atomic_history_entry() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut base = group(&pool);
        let id = base.metadata().layer(0).unwrap().id().to_owned();
        assert_eq!(base.metadata().layer(0).unwrap().pixels().depth(), 8);
        let command = json!({"op":"paint.fill","layer":id,"color":[0,0,0,0]});
        let captured = capture(&base, &command);
        let token = captured
            .descriptor()
            .unwrap()
            .commands()
            .next()
            .unwrap()
            .token();
        let delta = captured
            .insert_fill_child(token, output(&base, 16), &|| Ok(()))
            .unwrap();
        assert_eq!(base.metadata().layers().len(), 1);
        let child = delta.preview().metadata().layer(1).unwrap();
        assert_eq!(child.parent(), Some(id.as_str()));
        assert_eq!(child.name(), "Foreground fill");
        assert_eq!(child.kind(), "paint");
        assert_eq!(child.origin(), [0, 0]);
        assert_eq!(child.pixels().depth(), 16);
        assert_ne!(child.id(), id);
        assert!(delta
            .preview()
            .source(child.pixels().slot())
            .unwrap()
            .is_known_zero());
        let mut history = NativeHistory::new(&pool, 20).unwrap();
        let commit = history
            .snapshot()
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "worker",
                    label: "Fill child",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        drop(
            history
                .commit(
                    &mut base,
                    Publication {
                        actor: "worker",
                        leases: &[],
                        now: 1,
                        closed: false,
                        write_available: true,
                    },
                    commit,
                )
                .unwrap(),
        );
        assert_eq!(base.metadata().revision(), 1);
        assert_eq!(history.undo_len(), 1);
        let undo = history
            .snapshot()
            .prepare_move(&base, Direction::Undo, "worker", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(
                    &mut base,
                    Publication {
                        actor: "worker",
                        leases: &[],
                        now: 1,
                        closed: false,
                        write_available: true,
                    },
                    undo,
                )
                .unwrap(),
        );
        assert_eq!(base.metadata().layers().len(), 1);
        drop(base);
        drop(history);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn foreign_token_wrong_precision_empty_area_and_cancel_return_owned_output() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = group(&pool);
        let id = base.metadata().layer(0).unwrap().id();
        let command = json!({"op":"paint.fill","layer":id,"color":[1,2,3,4]});
        let foreign = capture(&base, &command)
            .descriptor()
            .unwrap()
            .commands()
            .next()
            .unwrap()
            .token();
        let captured = capture(&base, &command);
        captured
            .insert_fill_child(foreign, output(&base, 16), &|| Ok(()))
            .err()
            .unwrap()
            .abort(&Session, &|| Ok(()))
            .unwrap();
        let captured = capture(&base, &command);
        let token = captured
            .descriptor()
            .unwrap()
            .commands()
            .next()
            .unwrap()
            .token();
        captured
            .insert_fill_child(token, output(&base, 8), &|| Ok(()))
            .err()
            .unwrap()
            .abort(&Session, &|| Ok(()))
            .unwrap();
        let empty = json!({"op":"paint.fill","layer":id,"color":[1,2,3,4],"rect":[9,9,10,10]});
        let captured = capture(&base, &empty);
        let token = captured
            .descriptor()
            .unwrap()
            .commands()
            .next()
            .unwrap()
            .token();
        captured
            .insert_fill_child(token, output(&base, 16), &|| Ok(()))
            .err()
            .unwrap()
            .abort(&Session, &|| Ok(()))
            .unwrap();
        let captured = capture(&base, &command);
        let token = captured
            .descriptor()
            .unwrap()
            .commands()
            .next()
            .unwrap()
            .token();
        captured.cancellation().cancel();
        captured
            .insert_fill_child(token, output(&base, 16), &|| Ok(()))
            .err()
            .unwrap()
            .abort(&Session, &|| Ok(()))
            .unwrap();
        assert_eq!(base.metadata().layers().len(), 1);
        drop(base);
        assert_eq!(pool.used(), 0);
    }
}
