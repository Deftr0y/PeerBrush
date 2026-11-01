//! Source-free structural edits over the same immutable normalized command.
use super::*;
use serde_json::Value;

impl CapturedEdit {
    pub(crate) fn metadata_command(
        self,
        token: CommandToken,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<PreparedDelta> {
        self.cancellation.check()?;
        check()?;
        let descriptor = self
            .descriptor()
            .ok_or("Native metadata edit requires a normalized command")?;
        let command = descriptor
            .commands()
            .next()
            .ok_or("Missing native metadata command")?;
        if descriptor.commands().len() != 1
            || command.token() != token
            || command.method() != Method::Metadata
        {
            return Err("Native metadata edit differs from its normalized command".into());
        }
        let parameters = command.parameters();
        let operation = parameters["op"]
            .as_str()
            .ok_or("Missing native metadata operation")?;
        if !matches!(
            operation,
            "layer.update"
                | "layer.add"
                | "layer.delete"
                | "layer.reorder"
                | "layer.clip"
                | "layer.parent"
                | "mask.remove"
                | "mask.toggle"
                | "mask.link"
        ) {
            return Err("Native metadata operation needs its typed structural producer".into());
        }
        let base = &self.base.core.metadata;
        let bytes = base
            .layers
            .len()
            .checked_mul(std::mem::size_of::<Layer36>() + 32)
            .and_then(|n| {
                n.checked_add(parameters.as_object().map_or(0, |o| {
                    o.values()
                        .filter_map(Value::as_str)
                        .map(str::len)
                        .sum::<usize>()
                }))
            })
            .and_then(|n| {
                n.checked_add(
                    parameters
                        .get("name")
                        .and_then(Value::as_str)
                        .map_or(0, str::len),
                )
            })
            .and_then(|n| n.checked_add(4096))
            .ok_or("Native structural metadata footprint overflow")?;
        let extra = MetadataCharge::reserve(self.base.budget(), bytes, check)?;
        let copied = copy_metadata(base, self.base.budget(), check)?.retain_owner(&extra, check)?;
        let resident = self.base.budget().clone();
        let disk = self.base.disk_budget().clone();
        let intent = self.planned_intent(Intent::from_scopes(
            IntentKind::Capture,
            command.write_scopes(),
            self.base.budget(),
        )?)?;
        let core = copied.map_unique(|mut metadata, _| {
            let mut added = None;
            if operation == "layer.add" {
                if metadata.layers.len() >= 100 {
                    return Err("Initial version supports up to 100 layers".into());
                }
                let kind = parameters["kind"].as_str().unwrap_or("paint");
                if !matches!(kind, "paint" | "fill" | "group") {
                    return Err("Unsupported layer kind".into());
                }
                let parent = parameters["parent"].as_str();
                if let Some(parent) = parent {
                    if !metadata
                        .layers
                        .iter()
                        .any(|l| l.id == parent && l.kind == "group")
                    {
                        return Err("Parent must be an existing group".into());
                    }
                    super::patch::unlocked(&metadata, parent)?;
                }
                let surface = Surface::zero(metadata.width, metadata.height, metadata.bit_depth)
                    .map_err(|e| e.to_string())?;
                let ordinal = u32::try_from(self.base.roster().len() + 1)
                    .map_err(|_| "Native source ordinal overflow")?;
                let layer = Layer36 {
                    id: crate::engine::id(),
                    name: owned(parameters["name"].as_str().unwrap_or("New layer"))?,
                    kind: owned(kind)?,
                    parent: parent.map(owned).transpose()?,
                    clip_to: None,
                    visible: true,
                    locked: false,
                    opacity: 1.,
                    blend: owned("normal")?,
                    x: 0,
                    y: 0,
                    pixels: RasterRef36 {
                        id: ordinal,
                        width: surface.width(),
                        height: surface.height(),
                        depth: surface.depth(),
                        retained: None,
                    },
                    color: if parameters.get("color").is_some() {
                        crate::engine::color(parameters)
                    } else {
                        [233, 84, 32, 255]
                    },
                    mask: None,
                    effects: vec![],
                    psd_metadata: vec![],
                    source: None,
                };
                metadata
                    .layers
                    .try_reserve_exact(1)
                    .map_err(|_| "Native layer allocation refused")?;
                metadata.layers.insert(0, layer);
                added = Some((ordinal, surface));
            } else {
                let target = parameters["layer"]
                    .as_str()
                    .ok_or("Choose a native layer")?;
                let index = metadata
                    .layers
                    .iter()
                    .position(|l| l.id == target)
                    .ok_or("Unknown native layer")?;
                let visibility_only = operation == "layer.update"
                    && parameters["visible"].is_boolean()
                    && parameters.as_object().is_some_and(|o| {
                        o.keys()
                            .all(|k| ["op", "layer", "visible"].contains(&k.as_str()))
                    });
                if !visibility_only {
                    if let Some(parent) = metadata.layers[index].parent.as_deref() {
                        super::patch::unlocked(&metadata, parent)?;
                    }
                    if metadata.layers[index].locked && operation != "layer.update" {
                        return Err("Layer is locked".into());
                    }
                }
                match operation {
                    "layer.update" => {
                        let layer = &mut metadata.layers[index];
                        if !visibility_only
                            && layer.locked
                            && parameters.get("locked") != Some(&Value::Bool(false))
                        {
                            return Err("Unlock the layer before changing it".into());
                        }
                        if let Some(name) = parameters["name"].as_str() {
                            layer.name = owned(name)?;
                        }
                        if let Some(value) = parameters["visible"].as_bool() {
                            layer.visible = value;
                        }
                        if let Some(value) = parameters["locked"].as_bool() {
                            layer.locked = value;
                        }
                        if let Some(value) = parameters["opacity"].as_f64() {
                            layer.opacity = (value as f32).clamp(0., 1.);
                        }
                        if let Some(value) = parameters["blend"].as_str() {
                            if !crate::raster::layer_blends(&layer.kind)
                                .any(|(mode, _)| mode == value)
                            {
                                return Err("Unsupported blend mode".into());
                            }
                            layer.blend = owned(value)?;
                        }
                        if parameters.get("color").is_some() {
                            layer.color = crate::engine::color(parameters);
                        }
                    }
                    "layer.delete" => {
                        let mut deleted = Vec::new();
                        deleted
                            .try_reserve_exact(metadata.layers.len())
                            .map_err(|_| "Native deletion allocation refused")?;
                        for layer in &metadata.layers {
                            deleted.push(descendant(&metadata, &layer.id, target)?);
                        }
                        if metadata
                            .layers
                            .iter()
                            .zip(&deleted)
                            .any(|(l, deleted)| *deleted && l.locked)
                        {
                            return Err(
                                "Unlock the selected layers and their children before deleting"
                                    .into(),
                            );
                        }
                        if metadata.layers.iter().zip(&deleted).any(|(l, removed)| {
                            !*removed
                                && l.clip_to.as_deref().is_some_and(|id| {
                                    metadata
                                        .layers
                                        .iter()
                                        .zip(&deleted)
                                        .any(|(l, removed)| *removed && l.id == id)
                                })
                        }) {
                            return Err("Release clipping before deleting its base".into());
                        }
                        let mut at = 0;
                        metadata.layers.retain(|_| {
                            let keep = !deleted[at];
                            at += 1;
                            keep
                        });
                    }
                    "layer.reorder" => {
                        let at = crate::engine::num(parameters, "index", 0.) as usize;
                        if at >= metadata.layers.len() {
                            return Err("Invalid layer index".into());
                        }
                        let layer = metadata.layers.remove(index);
                        metadata.layers.insert(at, layer);
                    }
                    "layer.clip" => {
                        metadata.layers[index].clip_to =
                            parameters["base"].as_str().map(owned).transpose()?
                    }
                    "layer.parent" => {
                        let parent = parameters["parent"].as_str();
                        let mut current = parent;
                        for depth in 0..=16 {
                            let Some(id) = current else { break };
                            if id == target {
                                return Err("A group cannot contain itself".into());
                            }
                            if depth == 16 {
                                return Err("Group nesting limit exceeded".into());
                            }
                            let node = metadata
                                .layers
                                .iter()
                                .find(|l| l.id == id && l.kind == "group")
                                .ok_or("Invalid parent group")?;
                            if node.locked {
                                return Err("Destination folder is locked".into());
                            }
                            current = node.parent.as_deref();
                        }
                        metadata.layers[index].parent = parent.map(owned).transpose()?;
                    }
                    "mask.remove" => metadata.layers[index].mask = None,
                    "mask.toggle" => {
                        let mask = metadata.layers[index]
                            .mask
                            .as_mut()
                            .ok_or("Layer has no mask")?;
                        mask.enabled = parameters["enabled"].as_bool().unwrap_or(!mask.enabled);
                    }
                    "mask.link" => {
                        let mask = metadata.layers[index]
                            .mask
                            .as_mut()
                            .ok_or("Layer has no mask")?;
                        mask.linked = parameters["linked"]
                            .as_bool()
                            .ok_or("Mask linked must be boolean")?;
                    }
                    _ => return Err("Unsupported native structural operation".into()),
                }
            }
            validate_clipping(&metadata)?;
            let roster = rebuild_from(
                &mut metadata,
                self.base.roster().len() + usize::from(added.is_some()),
                &resident,
                &disk,
                check,
                |reference| match &added {
                    Some((id, surface)) if *id == reference.id => Ok(surface.clone()),
                    _ => self.base.roster().get(reference).cloned(),
                },
            )?;
            metadata.revision = metadata
                .revision
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            self.cancellation.check()?;
            check()?;
            Ok(Core {
                metadata,
                roster,
                resident,
                disk,
                key: key(),
                render_key: OnceLock::new(),
            })
        })?;
        Ok(PreparedDelta {
            base: self.base.key(),
            candidate: NativeDocument { core },
            cancellation: self.cancellation,
            intent,
            outputs: vec![],
            admission: self.admission,
        })
    }
}
fn owned(value: &str) -> Result<String> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|_| "Native metadata string allocation refused")?;
    copy.push_str(value);
    Ok(copy)
}
fn descendant(metadata: &Document36, child: &str, parent: &str) -> Result<bool> {
    let mut current = Some(child);
    for _ in 0..=16 {
        let Some(id) = current else { return Ok(false) };
        if id == parent {
            return Ok(true);
        }
        current = metadata
            .layers
            .iter()
            .find(|l| l.id == id)
            .ok_or("Invalid native parent")?
            .parent
            .as_deref();
    }
    Err("Native group nesting limit exceeded".into())
}
fn validate_clipping(metadata: &Document36) -> Result<()> {
    for (i, layer) in metadata.layers.iter().enumerate() {
        if layer.blend == "pass_through" {
            if layer.kind != "group" {
                return Err("Pass Through is available only for folders".into());
            }
            if layer.effects.iter().any(|effect| effect.enabled) {
                return Err(
                    "Choose an isolated folder blend before enabling folder effects".into(),
                );
            }
            if layer.clip_to.is_some()
                || metadata
                    .layers
                    .iter()
                    .any(|node| node.clip_to.as_deref() == Some(&layer.id))
            {
                return Err("Release folder clipping before choosing Pass Through".into());
            }
        }
        if let Some(id) = &layer.clip_to {
            let base = metadata
                .layers
                .iter()
                .skip(i + 1)
                .filter(|node| node.parent == layer.parent)
                .find(|node| node.clip_to.is_none())
                .ok_or("Clipping needs a base layer below it")?;
            if base.id != *id || base.kind == "adjustment" {
                return Err(
                    "Keep clipped layers directly above their base, or release clipping first"
                        .into(),
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::scene::RasterCollector;
    use crate::engine::{Document, Engine, Layer};
    use serde_json::json;
    struct ZeroSources(Vec<Surface>);
    impl RasterCollector for ZeroSources {
        fn resident(&mut self, raster: &crate::Raster) -> Result<RasterRef36> {
            assert!(raster.tiles.is_empty());
            let surface = Surface::zero(raster.width, raster.height, raster.depth)
                .map_err(|e| e.to_string())?;
            self.surface(&surface)
        }
        fn surface(&mut self, surface: &Surface) -> Result<RasterRef36> {
            self.0.push(surface.clone());
            Ok(RasterRef36 {
                id: self.0.len() as u32,
                width: surface.width(),
                height: surface.height(),
                depth: surface.depth(),
                retained: None,
            })
        }
    }
    fn fixture(depth: u16) -> Document {
        let mut doc = Document::new_depth(8, 8, depth).unwrap();
        doc.layers.clear();
        for (id, kind, parent) in [
            ("top", "paint", None),
            ("bottom", "paint", None),
            ("group", "group", None),
            ("child", "paint", Some("group")),
        ] {
            let mut layer = Layer::new(id, kind, 8, 8);
            layer.id = id.into();
            layer.parent = parent.map(String::from);
            layer.pixels = crate::Raster::new_depth(8, 8, depth);
            doc.layers.push(layer);
        }
        doc
    }
    fn capture(doc: &Document, pool: &ResidentBudget) -> NativeDocument {
        let mut sources = ZeroSources(vec![]);
        let metadata = Document36::capture(doc, &mut sources).unwrap();
        let owner = MetadataCharge::reserve(pool, 128 * 1024, &|| Ok(())).unwrap();
        NativeDocument::from_pending(
            PendingScene::new(
                Admitted::from_parts(Arc::new(metadata), owner),
                RasterRoster::new(sources.0).unwrap(),
                pool.clone(),
                DiskBudget::new(1024 * 1024, 8).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }
    fn edit(base: &NativeDocument, command: &Value) -> Result<PreparedDelta> {
        let descriptor = CommandDescriptor::from_normalized(
            base,
            base.metadata().revision(),
            std::slice::from_ref(command),
            &|| Ok(()),
        )?;
        let token = descriptor.commands().next().unwrap().token();
        base.capture_descriptor(descriptor, "human", &[], 1, false, true)?
            .metadata_command(token, &|| Ok(()))
    }
    #[test]
    fn pure_structural_operations_match_the_actual_legacy_engine_in_both_depths() {
        for depth in [8, 16] {
            for command in [
                json!({"op":"layer.update","layer":"top","name":"New name","opacity":0.45,"blend":"multiply","color":[1,2,3,4]}),
                json!({"op":"layer.update","layer":"top","locked":true}),
                json!({"op":"layer.reorder","layer":"bottom","index":0}),
                json!({"op":"layer.clip","layer":"top","base":"bottom"}),
                json!({"op":"layer.parent","layer":"top","parent":"group"}),
                json!({"op":"layer.delete","layer":"group"}),
                json!({"op":"layer.delete","layer":"top"}),
            ] {
                let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
                let original = fixture(depth);
                let base = capture(&original, &pool);
                let source_key = base
                    .source(base.layer_slot("bottom").unwrap())
                    .unwrap()
                    .key();
                let mut legacy = Engine::new();
                legacy.doc = original;
                legacy
                    .edit(
                        "human",
                        std::slice::from_ref(&command),
                        None,
                        None,
                        "structural",
                    )
                    .unwrap();
                let mut sources = ZeroSources(vec![]);
                let expected = Document36::capture(&legacy.doc, &mut sources).unwrap();
                let native = base
                    .publish("human", &[], 1, false, edit(&base, &command).unwrap())
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(native.metadata()).unwrap(),
                    serde_json::to_value(expected).unwrap(),
                    "{depth} {command}"
                );
                assert_eq!(
                    native
                        .source(native.layer_slot("bottom").unwrap())
                        .unwrap()
                        .key(),
                    source_key
                );
                drop(native);
                drop(base);
                assert_eq!(pool.used(), 0);
            }
        }
    }
    #[test]
    fn creation_and_later_update_share_the_same_transaction_without_source_io() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = capture(&fixture(16), &pool);
        let commands = [
            json!({"op":"layer.add","kind":"fill","name":"New layer","color":[1,2,3,4],"parent":"group"}),
            json!({"op":"layer.update","layer":"top","opacity":0.5}),
        ];
        let descriptor =
            CommandDescriptor::from_normalized(&base, 0, &commands, &|| Ok(())).unwrap();
        let mut transaction = base
            .capture_descriptor(descriptor, "human", &[], 1, false, true)
            .unwrap()
            .transaction()
            .unwrap();
        for _ in 0..2 {
            let step = transaction.next(&|| Ok(())).unwrap();
            let token = step
                .descriptor()
                .unwrap()
                .commands()
                .next()
                .unwrap()
                .token();
            let prepared = step.metadata_command(token, &|| Ok(())).unwrap();
            transaction.complete(prepared, &|| Ok(())).unwrap();
        }
        let native = base
            .publish(
                "human",
                &[],
                1,
                false,
                transaction.finish(&|| Ok(())).unwrap(),
            )
            .unwrap();
        assert_eq!(native.metadata().revision(), 1);
        assert_eq!(native.metadata().layers().len(), 5);
        let new = native.metadata().layer(0).unwrap();
        assert_eq!(new.name(), "New layer");
        assert_eq!(new.kind(), "fill");
        assert_eq!(new.color(), [1, 2, 3, 4]);
        assert_eq!(new.parent(), Some("group"));
        assert_eq!(new.pixels().depth(), 16);
        assert!(native.source(new.pixels().slot()).unwrap().is_known_zero());
        assert_eq!(native.disk_budget().used().unwrap(), 0);
        drop(native);
        drop(base);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn invalid_clipping_parent_cycles_locks_and_foreign_tokens_preserve_the_base() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut resident = fixture(16);
        resident.layers[0].locked = true;
        let base = capture(&resident, &pool);
        let before = base.key();
        for command in [
            json!({"op":"layer.update","layer":"top","name":"locked"}),
            json!({"op":"layer.parent","layer":"group","parent":"group"}),
            json!({"op":"layer.clip","layer":"bottom","base":"top"}),
        ] {
            assert!(edit(&base, &command).is_err());
            assert_eq!(base.key(), before);
        }
        assert!(edit(
            &base,
            &json!({"op":"layer.update","layer":"top","visible":false})
        )
        .is_ok());
        let first = CommandDescriptor::from_normalized(
            &base,
            0,
            &[json!({"op":"layer.update","layer":"top","visible":false})],
            &|| Ok(()),
        )
        .unwrap();
        let token = first.commands().next().unwrap().token();
        let second = CommandDescriptor::from_normalized(
            &base,
            0,
            &[json!({"op":"layer.update","layer":"top","visible":false})],
            &|| Ok(()),
        )
        .unwrap();
        assert!(base
            .capture_descriptor(second, "human", &[], 1, false, true)
            .unwrap()
            .metadata_command(token, &|| Ok(()))
            .is_err());
        drop(first);
        drop(base);
        assert_eq!(pool.used(), 0);
    }
}
