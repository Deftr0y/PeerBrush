//! Immutable normalized command binding. Typed handlers validate their parameters
//! before this factory; this layer derives scope policy and grants no source I/O.
use super::*;
use crate::{engine::Scope, large_document::Reservation};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Metadata,
    Paint,
    Fill,
    Clear,
    Smudge,
    Retouch,
    Geometry,
    Selection,
    Mask,
    Effects,
    Bake,
    Import,
    Source,
    Document,
}
impl Method {
    fn from_operation(operation: &str) -> Result<Self> {
        Ok(match operation {
            "paint" | "shape" | "gradient" | "fill" | "adjust" | "pixels.replace"
            | "image.patch" => Self::Paint,
            "paint.fill" => Self::Fill,
            "paint.clear_selection" => Self::Clear,
            "smudge" => Self::Smudge,
            "clone" | "heal" => Self::Retouch,
            "move" | "transform" | "crop" | "resize" | "canvas.resize" | "image.resize" => {
                Self::Geometry
            }
            "selection"
            | "selection.refine"
            | "selection.clear"
            | "selection.reselect"
            | "selection.invert"
            | "selection.modify"
            | "selection.wand_update"
            | "selection.quick_update"
            | "selection.import" => Self::Selection,
            "layer.update"
            | "layer.add"
            | "layer.delete"
            | "layer.reorder"
            | "layer.clip"
            | "layer.parent"
            | "group.create_selected"
            | "layer.duplicate"
            | "mask.remove"
            | "mask.toggle"
            | "mask.link"
            | "mask.step.update"
            | "mask.step.delete"
            | "mask.step.reorder" => Self::Metadata,
            "mask.add"
            | "mask.from_selection"
            | "mask.from_color"
            | "mask.step.add"
            | "mask.refine" => Self::Mask,
            "effect.add" | "effect.update" | "effect.delete" | "effect.reorder" | "filter.add"
            | "filter.update" | "filter.delete" | "filter.reorder" | "adjustment.add"
            | "liquify.stroke" => Self::Effects,
            "layer.merge" | "effect.bake" | "mask.step.bake" => Self::Bake,
            "image.import"
            | "image.place"
            | "image.paste"
            | "layer.paste"
            | "layer.duplicate_selection" => Self::Import,
            "source.add" | "source.update" | "source.rasterize" | "path.create" | "path.update"
            | "path.delete" | "path.to_selection" | "path.to_mask" | "path.node.add"
            | "path.node.split" | "path.node.delete" | "path.node.move" | "path.handle.move"
            | "path.node.convert" => Self::Source,
            "new" | "document.settings" | "document.global_light" => Self::Document,
            _ => return Err("Unsupported normalized native command".into()),
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CommandToken {
    descriptor: DocumentKey,
    index: u32,
}
struct CommandPlan {
    method: Method,
    preserve_selection: bool,
    transforms_new_copy: bool,
}
pub(crate) struct CommandDescriptor {
    generation: DocumentKey,
    revision: u64,
    identity: DocumentKey,
    offset: u32,
    commands: Admitted<Vec<Value>>,
    plans: Vec<CommandPlan>,
    scope_ranges: Vec<std::ops::Range<usize>>,
    scopes: Arc<Intent>,
    _charge: Reservation,
}
#[derive(Clone, Copy)]
pub(crate) struct Command<'a> {
    descriptor: DocumentKey,
    generation: DocumentKey,
    index: u32,
    method: Method,
    preserve_selection: bool,
    transforms_new_copy: bool,
    parameters: &'a Value,
    scopes: &'a [Scope],
}
impl<'a> Command<'a> {
    pub fn token(&self) -> CommandToken {
        CommandToken {
            descriptor: self.descriptor,
            index: self.index,
        }
    }
    pub fn method(&self) -> Method {
        self.method
    }
    pub(crate) fn preserves_selection(&self) -> bool {
        self.preserve_selection
    }
    pub fn parameters(&self) -> &'a Value {
        self.parameters
    }
    pub fn write_scopes(&self) -> &'a [Scope] {
        self.scopes
    }
    /// The producer supplies its actual baseline->proposed logical write box;
    /// read halos are prepared separately and are not writes.
    pub fn check_write(
        &self,
        base: &NativeDocument,
        slot: RasterSlot,
        bounds: [i32; 4],
    ) -> Result<()> {
        if self.generation != base.key() {
            return Err("Native command write belongs to another generation".into());
        }
        base.source(slot)?;
        if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
            return Err("Native changed-pixel write box is empty".into());
        }
        let layer = base
            .core
            .metadata
            .layers
            .iter()
            .find(|layer| super::patch::layer_contains(layer, slot.ordinal));
        let target = layer.map_or("@selection", |layer| layer.id.as_str());
        let allowed = self.scopes.iter().any(|scope| {
            let target_matches = scope.target.as_deref().is_none_or(|held| {
                let mut current = Some(target);
                for _ in 0..=16 {
                    let Some(id) = current else {
                        return false;
                    };
                    if held == id {
                        return true;
                    }
                    current = base
                        .core
                        .metadata
                        .layers
                        .iter()
                        .find(|layer| layer.id == id)
                        .and_then(|layer| layer.parent.as_deref());
                }
                false
            });
            target_matches
                && scope.rect.is_none_or(|held| {
                    bounds[0] >= held[0]
                        && bounds[1] >= held[1]
                        && bounds[2] <= held[2]
                        && bounds[3] <= held[3]
                })
        });
        if allowed {
            Ok(())
        } else {
            Err("Native proposed pixels exceed the normalized command write footprint".into())
        }
    }
}
impl CommandDescriptor {
    /// Run outside the Engine mutex after each method's pure normalizer. The
    /// retained JSON is the exact immutable input that the selected handler uses.
    pub(crate) fn from_normalized(
        base: &NativeDocument,
        revision: u64,
        commands: &[Value],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Arc<Self>> {
        check()?;
        if revision != base.metadata().revision() {
            return Err("Normalized native command revision is stale".into());
        }
        if commands.is_empty() || commands.len() > 100 {
            return Err("A batch must contain 1-100 commands".into());
        }
        for command in commands {
            let operation = command
                .get("op")
                .and_then(Value::as_str)
                .ok_or("Native command needs an operation")?;
            Method::from_operation(operation)?;
            if command
                .get("source_revision")
                .is_some_and(|revision| revision.as_u64() != Some(base.metadata().revision()))
                || command
                    .get("document_id")
                    .is_some_and(|id| id.as_str() != Some(base.metadata().id()))
            {
                return Err("Normalized native command source guard is stale".into());
            }
        }
        let commands: Admitted<Vec<Value>> = copy_admitted(&commands, base.budget(), true, check)?;
        let bytes = commands
            .len()
            .checked_mul(
                std::mem::size_of::<CommandPlan>() + std::mem::size_of::<std::ops::Range<usize>>(),
            )
            .and_then(|n| n.checked_add(std::mem::size_of::<CommandDescriptor>() + 128))
            .ok_or("Native descriptor footprint overflow")?;
        let charge = base
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let scope_bytes = base
            .core
            .metadata
            .layers
            .iter()
            .try_fold(0usize, |n, layer| {
                n.checked_add(layer.id.len())
                    .ok_or("Native scope footprint overflow")
            })?;
        let scope_bytes = scope_bytes
            .checked_mul(4)
            .and_then(|n| n.checked_add(commands.owner().bytes()))
            .and_then(|n| n.checked_add(commands.len() * 32 * 1024))
            .ok_or("Native scope footprint overflow")?;
        let temporary = MetadataCharge::reserve(base.budget(), scope_bytes, check)?;
        let mut plans = Vec::new();
        plans
            .try_reserve_exact(commands.len())
            .map_err(|_| "Native method allocation refused")?;
        let mut scope_ranges = Vec::new();
        scope_ranges
            .try_reserve_exact(commands.len())
            .map_err(|_| "Native scope range allocation refused")?;
        let mut scopes = Vec::new();
        let mut copied_roots = std::collections::HashSet::with_capacity(100);
        let mut gesture_end = 0;
        for (index, command) in commands.iter().enumerate() {
            check()?;
            if index == gesture_end {
                gesture_end = index + 1;
                while let Some(next) = commands.get(gesture_end) {
                    check()?;
                    let target = next["layer"].as_str().unwrap_or("");
                    if !crate::transform::shares_selection(command, next)
                        || commands[index..gesture_end]
                            .iter()
                            .any(|prior| prior["layer"].as_str().unwrap_or("") == target)
                    {
                        break;
                    }
                    gesture_end += 1;
                }
            }
            // Keep the original selection until every distinct target in the gesture has used it.
            let operation = command["op"].as_str().ok_or("Missing native operation")?;
            plans.push(CommandPlan {
                method: Method::from_operation(operation)?,
                preserve_selection: index + 1 < gesture_end,
                transforms_new_copy: matches!(operation, "move" | "transform")
                    && command["layer"].as_str().is_some_and(|id| copied_roots.contains(id)),
            });
            if operation == "layer.duplicate" && command.get("root_ids").is_some() {
                if let Ok(Some(ids)) = super::duplicate::requested_root_ids(base, command) {
                    if copied_roots.len() + ids.len() > 100 {
                        return Err("Native batch exceeds the copied layer limit".into());
                    }
                    copied_roots.extend(ids.values().filter_map(Value::as_str));
                }
            }
            let planned = super::scopes::scopes(&base.core.metadata, command);
            let start = scopes.len();
            scopes
                .try_reserve_exact(planned.len())
                .map_err(|_| "Native scope allocation refused")?;
            scopes.extend(planned);
            scope_ranges.push(start..scopes.len());
        }
        let intent = Intent::from_scopes(IntentKind::Capture, &scopes, base.budget())?;
        drop(scopes);
        drop(copied_roots);
        drop(temporary);
        check()?;
        Ok(Arc::new(Self {
            generation: base.key(),
            revision,
            identity: key(),
            offset: 0,
            commands,
            plans,
            scope_ranges,
            scopes: Arc::new(intent),
            _charge: charge,
        }))
    }
    pub fn commands<'a>(
        &'a self,
    ) -> impl ExactSizeIterator<Item = Command<'a>> + DoubleEndedIterator + 'a {
        self.commands
            .iter()
            .zip(self.plans.iter())
            .enumerate()
            .map(move |(index, (parameters, plan))| Command {
                descriptor: self.identity,
                generation: self.generation,
                index: self.offset + index as u32,
                method: plan.method,
                preserve_selection: plan.preserve_selection,
                transforms_new_copy: plan.transforms_new_copy,
                parameters,
                scopes: &self.scopes.scopes()[self.scope_ranges[index].clone()],
            })
    }
    pub fn scopes(&self) -> &[Scope] {
        self.scopes.scopes()
    }
    pub(super) fn allows_copy_scope(&self, target: &str) -> bool {
        let mut matching = self.commands().filter(|command| {
            command.write_scopes().iter().any(|scope| scope.target.as_deref() == Some(target))
        });
        let valid = |command: Command<'_>| command.transforms_new_copy && command.parameters["layer"] == target;
        matching.next().is_some_and(valid) && matching.all(valid)
    }
    pub fn generation(&self) -> DocumentKey {
        self.generation
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub(super) fn identity(&self) -> DocumentKey {
        self.identity
    }
    /// A transaction derives the next command from the immutable original
    /// parameters, while scopes and raster slots bind to its working generation.
    pub(super) fn step(
        &self,
        working: &NativeDocument,
        index: usize,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Arc<Self>> {
        let command = self
            .commands
            .get(index)
            .ok_or("Native transaction command is exhausted")?;
        let mut step =
            Self::from_normalized(working, self.revision, std::slice::from_ref(command), check)?;
        let step_mut =
            Arc::get_mut(&mut step).ok_or("Native step descriptor unexpectedly shared")?;
        step_mut.identity = self.identity;
        step_mut.offset = index as u32;
        step_mut.plans[0].preserve_selection = self.plans[index].preserve_selection;
        Ok(step)
    }
    pub(super) fn shared_scopes(&self) -> Arc<Intent> {
        self.scopes.clone()
    }
    pub(super) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.commands.owner().uses_budget(pool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn normalized_parameters_methods_and_command_tokens_cannot_change_after_capture() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let layer = base.metadata().layer(0).unwrap().id();
        let mut commands = vec![json!({"op":"layer.update","layer":layer,"visible":false})];
        let descriptor =
            CommandDescriptor::from_normalized(&base, 0, &commands, &|| Ok(())).unwrap();
        let token = descriptor.commands().next().unwrap().token();
        commands[0]["visible"] = json!(true);
        let command = descriptor.commands().next().unwrap();
        assert_eq!(command.method(), Method::Metadata);
        assert_eq!(command.parameters()["visible"], false);
        let copy = descriptor.clone();
        assert_eq!(copy.commands().next().unwrap().token(), token);
        let another = CommandDescriptor::from_normalized(
            &base,
            0,
            &[json!({"op":"layer.update","layer":layer,"visible":false})],
            &|| Ok(()),
        )
        .unwrap();
        assert_ne!(another.commands().next().unwrap().token(), token);
        let captured = base
            .capture_descriptor(descriptor.clone(), "worker", &[], 1, false, true)
            .unwrap();
        assert_eq!(
            captured
                .descriptor()
                .unwrap()
                .commands()
                .next()
                .unwrap()
                .token(),
            token
        );
        let prepared = captured.visibility(layer, false, &|| Ok(())).unwrap();
        let current = base.publish("worker", &[], 1, false, prepared).unwrap();
        assert!(!current.metadata().layer(0).unwrap().visible());
        assert!(current
            .capture_descriptor(descriptor, "worker", &[], 1, false, true)
            .is_err());
    }
    #[test]
    fn exact_engine_write_scopes_allow_disjoint_leases_and_refuse_outside_proposals() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let layer = base.metadata().layer(0).unwrap();
        let descriptor = CommandDescriptor::from_normalized(
            &base,
            0,
            &[json!({"op":"paint","layer":layer.id(),"points":[[1.,1.],[3.,3.]],"radius":1.})],
            &|| Ok(()),
        )
        .unwrap();
        let command = descriptor.commands().next().unwrap();
        assert_eq!(command.method(), Method::Paint);
        assert_eq!(command.write_scopes()[0].rect, Some([-2, -2, 6, 6]));
        command
            .check_write(&base, layer.pixels().slot(), [0, 0, 2, 2])
            .unwrap();
        assert!(command
            .check_write(&base, layer.pixels().slot(), [7, 7, 8, 8])
            .is_err());
        let lease = crate::engine::Lease {
            id: "elsewhere".into(),
            owner: "other".into(),
            description: "disjoint".into(),
            scopes: vec![Scope {
                target: Some(layer.id().into()),
                rect: Some([7, 7, 8, 8]),
            }],
            expires: 100,
        };
        let captured = base
            .capture_descriptor(
                descriptor.clone(),
                "worker",
                &[lease.clone()],
                1,
                false,
                true,
            )
            .unwrap();
        let output = OutputDraft::new(Surface::zero(8, 8, 16).unwrap(), vec![])
            .seal(&base, &super::super::output::tests::Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let prepared = captured
            .raster_patch(
                RasterPatch {
                    target: layer.pixels().slot(),
                    output,
                    retained: RetainedUpdate::Keep,
                    layer_origin: None,
                },
                &|| Ok(()),
            )
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        assert_eq!(prepared.intent().scopes(), descriptor.scopes());
        let current = base
            .publish("worker", &[lease], 1, false, prepared)
            .unwrap();
        assert_eq!(current.metadata().revision(), 1);
    }
    #[test]
    fn unsupported_or_stale_commands_and_changed_visibility_arguments_fail_closed() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let layer = base.metadata().layer(0).unwrap().id();
        assert!(
            CommandDescriptor::from_normalized(&base, 1, &[json!({"op":"paint"})], &|| Ok(()))
                .is_err()
        );
        assert!(CommandDescriptor::from_normalized(
            &base,
            0,
            &[json!({"op":"layer.hack"})],
            &|| Ok(())
        )
        .is_err());
        assert!(CommandDescriptor::from_normalized(
            &base,
            0,
            &[json!({"op":"paint","source_revision":1})],
            &|| Ok(())
        )
        .is_err());
        let descriptor = CommandDescriptor::from_normalized(
            &base,
            0,
            &[json!({"op":"layer.update","layer":layer,"visible":false})],
            &|| Ok(()),
        )
        .unwrap();
        let captured = base
            .capture_descriptor(descriptor, "worker", &[], 1, false, true)
            .unwrap();
        assert!(captured.visibility(layer, true, &|| Ok(())).is_err());
        assert_eq!(base.metadata().revision(), 0);
        assert_eq!(base.disk_budget().used().unwrap(), 0);
    }
}
