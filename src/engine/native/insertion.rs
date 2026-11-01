//! Structural publication for a foreground-fill child, paired with sealed pixels.
use super::*;

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
    /// The fill producer supplies a complete zero-baseline canvas result derived
    /// from this exact command. Parent slots authorize the insertion only; their
    /// pixels are never replaced. Even a transparent fill creates a real child.
    pub(crate) fn insert_fill_child(
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
                .ok_or("Fill insertion requires a normalized command")?;
            let command = descriptor
                .commands()
                .next()
                .ok_or("Missing fill insertion command")?;
            if descriptor.commands().len() != 1
                || command.token() != token
                || command.method() != Method::Fill
                || command.parameters()["mask"] == true
            {
                return Err("Fill insertion differs from its captured command".into());
            }
            let id = command.parameters()["layer"]
                .as_str()
                .ok_or("Choose a folder to fill")?;
            let metadata = &self.base.core.metadata;
            let index = metadata
                .layers
                .iter()
                .position(|layer| layer.id == id)
                .ok_or("Fill folder no longer exists")?;
            if metadata.layers[index].kind != "group" {
                return Err("Fill insertion requires a folder".into());
            }
            super::patch::unlocked(metadata, id)?;
            let mut area = command
                .parameters()
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
            if area[0].max(0) >= area[2].min(metadata.width as i32)
                || area[1].max(0) >= area[3].min(metadata.height as i32)
            {
                return Err("Empty foreground fill does not insert a child".into());
            }
            if metadata.layers.len() >= 100 {
                return Err("Initial version supports up to 100 layers".into());
            }
            let color = command.parameters()["color"]
                .as_array()
                .ok_or("Fill color must be RGBA")?;
            if color.len() != 4 || color.iter().any(|v| v.as_u64().is_none_or(|v| v > 255)) {
                return Err("Fill color must contain four byte values".into());
            }
            let surface = output.surface();
            if surface.width() != metadata.width
                || surface.height() != metadata.height
                || surface.depth() != metadata.bit_depth
                || !surface.uses_budget(self.base.budget())
                || !surface.uses_disk_budget(self.base.disk_budget())
            {
                return Err(
                    "Fill child pixels must match the document canvas, precision and admission"
                        .into(),
                );
            }
            let intent = self.planned_intent(Intent::layers(
                IntentKind::Pixels,
                std::slice::from_ref(&metadata.layers[index].id),
                self.base.budget(),
            )?)?;
            let locations = self.base.roster().len();
            let ordinal =
                u32::try_from(locations + 1).map_err(|_| "Native insertion ordinal overflow")?;
            let bytes = (metadata.layers.len() + 1)
                .checked_mul(std::mem::size_of::<Layer36>())
                .and_then(|n| n.checked_add(id.len() + 4096))
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
                    index + 1,
                    Layer36 {
                        id: crate::engine::id(),
                        name: "Foreground fill".into(),
                        kind: "paint".into(),
                        parent: Some(id.to_owned()),
                        clip_to: None,
                        visible: true,
                        locked: false,
                        opacity: 1.,
                        blend: "normal".into(),
                        x: 0,
                        y: 0,
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
                        source: None,
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
                .ok_or("Missing fill insertion admission")?
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
            Ok((NativeDocument { core }, intent, admission, outputs))
        })();
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
