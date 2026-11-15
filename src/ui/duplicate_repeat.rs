use super::*;

// The engine retains at most 200 native edits, plus their starting UI state.
const MAX_REPEAT_STATES: usize = 201;

#[derive(Clone)]
pub(super) struct Repeat {
    project: String,
    document: String,
    revision: u64,
    native: Option<crate::engine::native::DocumentKey>,
    active_layer: String,
    selection: HashSet<String>,
    anchor: String,
    mask: bool,
    mask_step: Option<String>,
    transforms: Arc<[Value]>,
    pixel_copy: bool,
    pub(super) active_copy: Option<String>,
}

pub(super) struct RepeatHistory {
    states: Vec<Repeat>,
    cursor: usize,
    revision: u64,
    native: Option<crate::engine::native::DocumentKey>,
}

impl PeerBrush {
    pub(super) fn can_duplicate_with_transform(&self, doc: DocumentView<'_>) -> bool {
        !doc.read_only()
            && doc.find_layer(&self.selected).is_some()
            && !self.interaction_active(self.source_editor.is_some())
            && !self.edit_worker.pending()
            && !self.lifecycle_frame
            && !self.free_transform_frame
            && !self.pen.in_gesture()
            && self.selection_gesture_source.is_none()
            && self.drag_start.is_none()
            && self.primitive_drag.is_none()
            && self.fill_press.is_none()
            && self.sample_press.is_none()
            && self.workspace_stroke.is_none()
    }

    pub(super) fn clear_duplicate_if_selection_changed(&mut self) {
        if self
            .duplicate_repeat
            .as_ref()
            .is_some_and(|repeat| !repeat.matches_target(self))
        {
            self.duplicate_repeat = None;
        }
    }

    pub(super) fn reset_duplicate_transform(&mut self) {
        if let Some(repeat) = &mut self.duplicate_repeat {
            repeat.transforms = Arc::from([]);
        }
        if let Some(history) = &mut self.duplicate_repeat_history {
            // A new manipulator forgets the transform, but Undo and Redo still
            // need the active layer and pixel-copy scope of each history step.
            for state in &mut history.states {
                state.transforms = Arc::from([]);
            }
        }
    }

    pub(super) fn transform_entire_pixel_copy(&self, doc: DocumentView<'_>) -> bool {
        self.duplicate_repeat
            .as_ref()
            .is_some_and(|repeat| repeat.pixel_copy && repeat.observes(self, doc))
    }

    pub(super) fn duplicate_start(&self, doc: DocumentView<'_>) -> Repeat {
        self.duplicate_repeat.as_ref()
            .filter(|repeat| repeat.observes(self, doc))
            .cloned()
            .unwrap_or_else(|| Repeat::capture(self, doc))
    }

    pub(super) fn duplicate_result_still_current(
        &self,
        request: &edit_job::Request,
        reply: &server::EditReply,
    ) -> bool {
        let engine = self.shared.lock().unwrap();
        let doc = crate::engine::view::current(&engine);
        request.project == self.project_id
            && doc.id() == request.document
            && reply.data()["revision"].as_u64() == Some(doc.revision())
            && doc.native().map(|native| native.key()) == reply.native_key()
            && self.pending_duplicate_repeat.as_ref().is_none_or(|before| before.matches_target(self))
    }

    pub(super) fn duplicate_with_transform(&mut self, doc: DocumentView<'_>) {
        if !self.can_duplicate_with_transform(doc) {
            self.message = "Finish the current edit before duplicating with transform".into();
            return;
        }
        let roots = crate::tree::roots_view(doc, &self.selection_layers);
        let repeat = self.duplicate_repeat.as_ref().filter(|repeat| {
            repeat.observes(self, doc)
                && (repeat.pixel_copy || !repeat.transforms.is_empty())
                && repeat.transforms.iter().all(|transform| {
                    roots.iter().any(|root| transform["layer"] == root.as_str())
                })
        });
        if doc.selection().is_some() && !repeat.is_some_and(|repeat| repeat.pixel_copy) {
            self.pending_duplicate_repeat = Some(self.duplicate_start(doc));
            self.duplicate_selection(doc);
            return;
        }

        let copies: serde_json::Map<String, Value> = roots
            .iter()
            .map(|id| (id.clone(), json!(crate::engine::id())))
            .collect();
        let mut active_root = self.selected.as_str();
        for _ in 0..=16 {
            if copies.contains_key(active_root) {
                break;
            }
            let Some(parent) = doc.find_layer(active_root).and_then(|layer| layer.parent()) else {
                break;
            };
            active_root = parent;
        }
        let mut commands = vec![json!({"op":"layer.duplicate","layers":roots,"root_ids":&copies})];
        let repeats_transform = repeat.is_some_and(|repeat| !repeat.transforms.is_empty());
        for transform in repeat.into_iter().flat_map(|repeat| repeat.transforms.iter()) {
            let mut transform = match next_transform(transform) {
                Ok(transform) => transform,
                Err(error) => {
                    self.message = format!("Cannot repeat transform · {error}").into();
                    return;
                }
            };
            let id = transform["layer"].as_str().unwrap();
            transform["layer"] = copies[id].clone();
            transform["document_id"] = json!(doc.id());
            transform["source_revision"] = json!(doc.revision());
            commands.push(transform);
        }
        let mut before = repeat.cloned().unwrap_or_else(|| Repeat::capture(self, doc));
        before.active_copy = copies.get(active_root).and_then(Value::as_str).map(str::to_owned);
        self.pending_duplicate_repeat = Some(before);
        self.apply_action(
            commands,
            if repeats_transform { "Duplicate with Transform" } else { "Duplicate layers" },
            Some((doc.id(), doc.revision())),
            doc.native().map(|native| native.key()).map_or(
                edit_job::Origin::Duplicate,
                edit_job::Origin::NativeDuplicateRepeat,
            ),
        );
    }

    pub(super) fn record_duplicate_transform(
        &mut self,
        request: &edit_job::Request,
        reply: &server::EditReply,
        commands: &[Value],
    ) {
        let Some(revision) = reply.data()["revision"].as_u64() else {
            self.pending_duplicate_repeat = None;
            return;
        };
        if revision == request.revision || request.project != self.project_id {
            self.pending_duplicate_repeat = None;
            return;
        }
        if let edit_job::Origin::History(direction) = request.origin {
            self.restore_duplicate_transform(request, reply, revision, direction);
            return;
        }
        let before = if request.origin.is_duplicate() {
            self.pending_duplicate_repeat.take().map(|mut before| {
                before.active_copy = None;
                before
            })
        } else {
            self.duplicate_repeat.clone()
        };
        let transforms = if request.origin.is_duplicate() {
            commands.get(1..).unwrap_or_default()
        } else {
            commands
        };
        let before = before.filter(|before| {
            before.revision == request.revision && before.document == request.document
                && before.project == request.project
                && (request.origin.is_duplicate() || before.matches_target(self))
        });
        // These commands leave the layer coordinate system and manipulator delta intact.
        let preserves_transform = !request.origin.is_duplicate()
            && !commands.is_empty()
            && commands.iter().all(|command| {
                let op = command["op"].as_str().unwrap_or_default();
                matches!(op, "selection" | "path.to_selection" | "layer.update")
                    || ["selection.", "effect.", "mask."].into_iter().any(|prefix| op.starts_with(prefix))
                        && !matches!(op, "effect.bake" | "mask.step.bake")
            });
        if preserves_transform {
            self.duplicate_repeat = before.clone().map(|mut repeat| {
                repeat.revision = revision;
                repeat.native = reply.native_key();
                repeat
            });
            self.record_repeat_step(before);
            return;
        }
        let repeats = if self.mask {
            None
        } else {
            repeatable_transforms(transforms, &self.selection_layers)
        };
        let created_from_pixels = request.origin.is_duplicate()
            && commands.first().is_some_and(|command| command["op"] == "layer.duplicate_selection");
        // Losing the previous transform must not retarget a pixel copy to the
        // old marquee. Selection and document observations still fence reuse.
        let pixel_copy = created_from_pixels || before.as_ref().is_some_and(|before| before.pixel_copy);
        let track_step = request.origin.is_duplicate() || repeats.is_some() || pixel_copy;
        self.duplicate_repeat = repeats
            .or_else(|| (pixel_copy || request.origin.is_duplicate() && before.is_some()).then(Vec::new))
            .map(|transforms| Repeat {
                project: request.project.clone(),
                document: request.document.clone(),
                revision,
                native: reply.native_key(),
                active_layer: self.selected.clone(),
                selection: self.selection_layers.clone(),
                anchor: self.selection_anchor.clone(),
                mask: self.mask,
                mask_step: self.mask_step.clone(),
                transforms: transforms.into(),
                pixel_copy,
                active_copy: None,
            });
        if track_step {
            // A transform can start the chain before the first Shift+D. Keep
            // the pre-transform target so Undo and Redo can restore that step.
            if before.is_none() {
                self.duplicate_repeat_history = None;
            }
            let before = if request.origin.is_duplicate() {
                before
            } else {
                before.or_else(|| {
                    self.duplicate_repeat.as_ref().map(|after| Repeat {
                        revision: request.revision,
                        // History restoration binds this position to the key returned by Undo.
                        native: None,
                        transforms: Arc::from([]),
                        pixel_copy: false,
                        ..after.clone()
                    })
                })
            };
            self.record_repeat_step(before);
        } else {
            self.duplicate_repeat_history = None;
        }
    }

    fn record_repeat_step(&mut self, before: Option<Repeat>) {
        let history = self.duplicate_repeat_history.take();
        self.duplicate_repeat_history = before
            .zip(self.duplicate_repeat.as_ref())
            .map(|(before, after)| {
                let revision = after.revision;
                let native = after.native;
                let mut history = history
                    .filter(|history| {
                        let current = &history.states[history.cursor];
                        history.revision == before.revision
                            && history.native == before.native
                            && current.project == before.project
                            && current.document == before.document
                            && current.same_target(&before)
                    })
                    .unwrap_or_else(|| RepeatHistory {
                        states: vec![before],
                        cursor: 0,
                        revision,
                        native,
                    });
                // A new edit from an undone position replaces the redo branch.
                history.states.truncate(history.cursor + 1);
                if history.states.len() == MAX_REPEAT_STATES {
                    history.states.remove(0);
                }
                history.states.push(after.clone());
                history.cursor = history.states.len() - 1;
                history.revision = revision;
                history.native = native;
                history
            });
    }

    fn restore_duplicate_transform(
        &mut self,
        request: &edit_job::Request,
        reply: &server::EditReply,
        revision: u64,
        direction: crate::engine::native::Direction,
    ) {
        use crate::engine::native::Direction;
        let Some(mut history) = self.duplicate_repeat_history.take() else {
            self.duplicate_repeat = None;
            return;
        };
        let previous = &history.states[history.cursor];
        // An untracked edit breaks the chain, even if it leaves the same layers selected.
        if history.revision != request.revision || previous.project != request.project
            || previous.document != request.document || !previous.matches_target(self)
            || self.duplicate_repeat.as_ref().is_none_or(|repeat| {
                repeat.revision != history.revision || repeat.native != history.native
            })
        {
            self.duplicate_repeat = None;
            return;
        }
        let cursor = match direction {
            Direction::Undo => history.cursor.checked_sub(1),
            Direction::Redo => Some(history.cursor + 1),
        };
        let Some((cursor, mut restored)) = cursor.and_then(|cursor| {
            history.states.get(cursor).cloned().map(|state| (cursor, state))
        }) else {
            self.duplicate_repeat = None;
            return;
        };
        let current = {
            let engine = self.shared.lock().unwrap();
            let doc = crate::engine::view::current(&engine);
            doc.id() == request.document && doc.revision() == revision
                && doc.native().map(|native| native.key()) == reply.native_key()
                && doc.find_layer(&restored.active_layer).is_some()
                && restored.selection.iter().all(|layer| doc.find_layer(layer).is_some())
        };
        if !current {
            self.duplicate_repeat = None;
            return;
        }
        self.select_content(&restored.active_layer);
        self.select_channel(restored.mask);
        self.selection_layers.clone_from(&restored.selection);
        self.selection_anchor.clone_from(&restored.anchor);
        self.mask_step.clone_from(&restored.mask_step);
        restored.revision = revision;
        restored.native = reply.native_key();
        history.revision = revision;
        history.native = restored.native;
        history.cursor = cursor;
        self.duplicate_repeat = Some(restored);
        self.duplicate_repeat_history = Some(history);
    }
}

impl Repeat {
    fn capture(app: &PeerBrush, doc: DocumentView<'_>) -> Self {
        Self {
            project: app.project_id.clone(),
            document: doc.id().into(),
            revision: doc.revision(),
            native: doc.native().map(|native| native.key()),
            active_layer: app.selected.clone(),
            selection: app.selection_layers.clone(),
            anchor: app.selection_anchor.clone(),
            mask: app.mask,
            mask_step: app.mask_step.clone(),
            transforms: Arc::from([]),
            pixel_copy: false,
            active_copy: None,
        }
    }

    fn observes(&self, app: &PeerBrush, doc: DocumentView<'_>) -> bool {
        self.project == app.project_id
            && self.document == doc.id()
            && self.revision == doc.revision()
            && self.native == doc.native().map(|native| native.key())
            && !self.mask
            && self.matches_target(app)
    }

    fn matches_target(&self, app: &PeerBrush) -> bool {
        self.active_layer == app.selected && self.selection == app.selection_layers
            && self.mask == app.mask && self.mask_step == app.mask_step
    }

    fn same_target(&self, other: &Self) -> bool {
        self.active_layer == other.active_layer && self.selection == other.selection
            && self.mask == other.mask && self.mask_step == other.mask_step
    }
}

fn repeatable_transforms(commands: &[Value], selection: &HashSet<String>) -> Option<Vec<Value>> {
    if commands.is_empty() || commands.len() > selection.len() {
        return None;
    }
    let mut targets = HashSet::with_capacity(commands.len());
    commands
        .iter()
        .map(|command| {
            let layer = command["layer"].as_str()?;
            if !selection.contains(layer)
                || !targets.insert(layer)
                || !matches!(command["op"].as_str(), Some("move" | "transform"))
                || command["mask"] == true
                || command["selection_only"] != false
            {
                return None;
            }
            Some(command.clone())
        })
        .collect()
}

fn next_transform(command: &Value) -> Result<Value, String> {
    let mut resolved = crate::transform::resolve_command(command)?;
    let mut offset = crate::transform::translation(&resolved)?;
    let projective = crate::projective::parse(&resolved)?;
    let warp = crate::warp::parse(&resolved)?;
    if let Some(rect) = warp.as_ref().map(|grid| grid.rect).or(projective.map(|spec| spec.rect)) {
        let center = |rect: [f64; 4]| [(rect[0] + rect[2]) * 0.5, (rect[1] + rect[3]) * 0.5];
        let pivot: [f64; 2] = resolved.get("pivot").map_or_else(
            || Ok(projective.map_or_else(|| center(rect), |spec| spec.center())),
            |value| serde::Deserialize::deserialize(value).map_err(|_| "Transform pivot requires two numbers"),
        )?;
        let map = crate::warp::CommandMap::new(&resolved, pivot)?;
        // The custom pivot can lie outside the source frame.
        let anchor = center(rect);
        let destination = map.point(anchor)?;
        offset = [destination[0] - anchor[0], destination[1] - anchor[1]];
        let shift = |[x, y]: [f64; 2]| [x + offset[0], y + offset[1]];
        if let Some(spec) = projective {
            let [left, top, right, bottom] = spec.rect;
            let [left, top] = shift([left, top]);
            let [right, bottom] = shift([right, bottom]);
            resolved["projective"] = json!({
                "rect":[left, top, right, bottom], "quad":spec.quad.map(shift),
            });
        }
        if let Some(grid) = warp {
            let bounds = map.bounds(grid.rect)?;
            // Leave room for the retained placement's floating-point round trip.
            let margin = bounds.into_iter().map(f64::abs).fold(1., f64::max) * 1e-9;
            let [left, top, right, bottom] = bounds;
            let next = [left - margin, top - margin, right + margin, bottom + margin];
            let points = grid.points.map(|point| std::array::from_fn(|axis| {
                let t = (point[axis] - grid.rect[axis]) / (grid.rect[axis + 2] - grid.rect[axis]);
                next[axis] + t * (next[axis + 2] - next[axis])
            }));
            resolved["warp"] = json!(crate::warp::Grid { rect: next, points });
        }
        resolved["pivot"] = json!(pivot);
    }
    resolved["snap"] = json!(false);
    if resolved["op"] == "transform" && !resolved["pivot"].is_null() {
        let [x, y]: [f64; 2] = serde::Deserialize::deserialize(&resolved["pivot"])
            .map_err(|_| "Transform pivot requires two numbers")?;
        resolved["pivot"] = json!([x + offset[0], y + offset[1]]);
    }
    crate::transform::resolve_command(&resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projective_repeat_preserves_the_local_map_at_the_copy() {
        let command = json!({
            "op":"transform", "layer":"source", "selection_only":false,
            "projective":{
                "rect":[0., 0., 20., 12.],
                "quad":[[1., 0.], [18., 2.], [19., 11.], [0., 12.]],
            },
            "pivot":[9., 4.], "dx":5., "dy":-3., "angle":15.,
        });
        let original = crate::projective::combined(&command).unwrap().unwrap();
        let center = [10., 6.];
        let destination = original.point(center).unwrap();
        let offset = [destination[0] - center[0], destination[1] - center[1]];
        let shift = |[x, y]: [f64; 2]| [x + offset[0], y + offset[1]];
        let repeat = next_transform(&command).unwrap();
        let repeated = crate::projective::combined(&repeat).unwrap().unwrap();
        for point in [[0., 0.], center, [19., 11.]] {
            let expected = shift(original.point(point).unwrap());
            let actual = repeated.point(shift(point)).unwrap();
            assert!((actual[0] - expected[0]).abs() < 1e-8);
            assert!((actual[1] - expected[1]).abs() < 1e-8);
        }
    }

    #[test]
    fn warp_repeat_covers_the_placed_source_and_preserves_the_grid_shape() {
        let rect = [-2., -2., 22., 14.];
        let grid = crate::warp::Grid::preset(rect, "arc", 0.20).unwrap();
        let mut command = json!({
            "op":"transform", "layer":"source", "selection_only":false,
            "warp":grid, "pivot":[9., 4.], "dx":5., "dy":-3., "angle":15.,
        });
        let mut layer = crate::engine::Layer::new("source", "paint", 20, 12);
        layer.pixels.set16(9, 4, [11223, 22334, 33445, 65535]);
        for stage in 1..=3 {
            let prior = crate::warp::parse(&command).unwrap().unwrap();
            crate::transform::layer(&mut layer, &command).unwrap();
            let original = layer.pixels.retained.as_ref().unwrap();
            assert_eq!(original.warps.len(), stage);
            let source = original.support_bounds_in(crate::projective::Map::affine_matrix([
                1., 0., 0., 1., f64::from(layer.x), f64::from(layer.y),
            ])).unwrap();
            command = next_transform(&command).unwrap();
            let grid = crate::warp::parse(&command).unwrap().unwrap();
            grid.prepare().unwrap().covers(source).unwrap();
            for (before, after) in prior.points.into_iter().zip(grid.points) {
                for axis in 0..2 {
                    let ratio = |value: f64, bounds: [f64; 4]| {
                        (value - bounds[axis]) / (bounds[axis + 2] - bounds[axis])
                    };
                    assert!((ratio(before[axis], prior.rect) - ratio(after[axis], grid.rect)).abs() < 1e-8);
                }
            }
        }
    }
}
