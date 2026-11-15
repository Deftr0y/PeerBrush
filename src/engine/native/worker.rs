//! Shared command preparation for UI previews, MCP and CLI edits. No step
//! publishes sources, changes the live document, or enters history.
use super::*;
use crate::{engine::authority::NativeCapture, workspace::source_runtime::SourceSession};
use crate::source::text_position::NativeCarets;
#[path = "worker/frame.rs"]
mod frame;
pub(crate) use frame::retouch_source as retouch_frame;
#[path = "worker/image_patch.rs"]
mod image_patch;
#[path = "worker/mask.rs"]
mod mask;
#[path = "worker/mask_color.rs"]
mod mask_color;
#[path = "worker/paint.rs"]
mod paint;
#[path = "worker/paint_mask_original.rs"]
mod paint_mask_original;
#[path = "worker/primitive.rs"]
mod primitive;
#[path = "worker/replacement.rs"]
mod replacement;
#[path = "worker/selection.rs"]
mod selection;
#[path = "worker/source.rs"]
mod source;

pub(crate) struct PreparedNative {
    document: NativeDocument,
    history: HistorySnapshot,
    delta: PreparedDelta,
    raster_damage: Option<[i32; 4]>,
}
#[derive(Clone, Copy)]
pub(crate) struct RasterDamage {
    pub(crate) base: DocumentKey,
    pub(crate) revision: u64,
    pub(crate) candidate: DocumentKey,
    pub(crate) bounds: [i32; 4],
}
#[derive(Debug)]
pub(crate) struct PreparationFailure {
    pub reason: String,
    pub cleanup_error: Option<String>,
}
impl PreparationFailure {
    fn new(reason: String) -> Self {
        Self {
            reason,
            cleanup_error: None,
        }
    }
    fn cleanup(&mut self, result: Result<()>) {
        if let Err(reason) = result {
            self.cleanup_error = Some(match self.cleanup_error.take() {
                None => reason,
                Some(previous) => format!("{previous}; {reason}"),
            });
        }
    }
}
impl PreparedNative {
    pub(crate) fn prepare_retirement(self) -> std::result::Result<AbortBatch, (Self, String)> {
        let Self {
            document,
            history,
            delta,
            raster_damage,
        } = self;
        match delta.prepare_retirement() {
            Ok(batch) => {
                drop((document, history));
                Ok(batch)
            }
            Err((delta, reason)) => Err((
                Self {
                    document,
                    history,
                    delta,
                    raster_damage,
                },
                reason,
            )),
        }
    }
    pub(crate) fn check(&self) -> Result<()> {
        self.delta.cancellation.check()
    }
    pub(crate) fn check_actor(&self, actor: &str) -> Result<()> {
        self.delta
            .admission
            .as_ref()
            .ok_or("Native proposal capture admission is missing")?
            .check_publication(&self.document, actor, &self.delta.intent)
    }
    pub(crate) fn prepare_commit(
        self,
        request: HistoryRequest<'_>,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedCommit, (Self, String)> {
        self.publish_with(request, check, Ok)
    }
    /// Keep the original history observation until publication succeeds, so a
    /// refused fence returns the same candidate and unique provisional tokens.
    pub(crate) fn publish_with<T>(
        self,
        request: HistoryRequest<'_>,
        check: &dyn Fn() -> Result<()>,
        publish: impl FnOnce(PreparedCommit) -> std::result::Result<T, RefusedCommit>,
    ) -> std::result::Result<T, (Self, String)> {
        let damage = self.raster_damage();
        let Self {
            document,
            history,
            delta,
            raster_damage,
        } = self;
        match history.prepare_commit(&document, delta, request, check) {
            Ok(pending) => match publish(pending.with_raster_damage(damage)) {
                Ok(result) => Ok(result),
                Err(refused) => {
                    let reason = refused.reason().to_owned();
                    Err((
                        Self {
                            document,
                            history,
                            delta: refused.into_prepared_delta(),
                    raster_damage,
                        },
                        reason,
                    ))
                }
            },
            Err(refused) => {
                let reason = refused.reason().to_owned();
                Err((
                    Self {
                        document,
                        history,
                        delta: refused.into_prepared(),
                    raster_damage,
                    },
                    reason,
                ))
            }
        }
    }
    pub(crate) fn shared_descriptor(&self) -> Result<Arc<CommandDescriptor>> {
        self.delta
            .admission
            .as_ref()
            .and_then(|admission| admission.shared_descriptor())
            .ok_or_else(|| "Native proposal command capture is missing".into())
    }
    pub(crate) fn scopes(&self) -> &[crate::engine::Scope] {
        self.delta.intent.scopes()
    }
    pub(crate) fn base(&self) -> &NativeDocument {
        &self.document
    }
    pub(crate) fn base_key(&self) -> DocumentKey {
        self.document.key()
    }
    pub(crate) fn created_layers(&self) -> impl Iterator<Item = &str> + '_ {
        self.preview()
            .metadata()
            .layers()
            .filter(|layer| {
                !self
                    .document
                    .metadata()
                    .layers()
                    .any(|old| old.id() == layer.id())
            })
            .map(|layer| layer.id())
    }
    pub(crate) fn preview(&self) -> &NativeDocument {
        self.delta.preview()
    }
    pub(crate) fn raster_damage(&self) -> Option<RasterDamage> {
        self.raster_damage.map(|bounds| RasterDamage {
            base: self.base_key(),
            revision: self.document.metadata().revision(),
            candidate: self.preview().key(),
            bounds,
        })
    }
    pub(crate) fn into_parts(self) -> (NativeDocument, HistorySnapshot, PreparedDelta) {
        (self.document, self.history, self.delta)
    }
    pub(crate) fn abort(
        self,
        session: &SourceSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let Self {
            document,
            history,
            delta,
            ..
        } = self;
        drop((document, history));
        delta.abort(session, cleanup)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handler {
    Metadata,
    Retained,
    Paint,
    Source,
    Selection,
    Mask,
    Filters,
    Effects,
    Liquify,
    Adjustment,
    Settings,
    Import,
    LayerPaste,
    SelectionDuplicate,
    Canvas,
    ImageResize,
    Merge,
    Bake,
}
/// The central route and compatibility report share this exact implementation
/// table. A missing producer never falls back to resident materialization.
pub(crate) fn handler(operation: &str) -> Option<Handler> {
    match operation {
        "effect.bake" | "mask.step.bake" => Some(Handler::Bake),
        "layer.merge" => Some(Handler::Merge),
        "image.resize" | "resize" => Some(Handler::ImageResize),
        "crop" | "canvas.resize" => Some(Handler::Canvas),
        "layer.duplicate_selection" => Some(Handler::SelectionDuplicate),
        "layer.paste" => Some(Handler::LayerPaste),
        "image.import" | "image.place" | "image.paste" => Some(Handler::Import),
        "document.settings" | "document.global_light" => Some(Handler::Settings),
        "layer.update"
        | "layer.add"
        | "layer.delete"
        | "layer.reorder"
        | "layer.clip"
        | "layer.parent"
        | "mask.remove"
        | "mask.toggle"
        | "mask.link"
        | "mask.step.add"
        | "mask.step.update"
        | "mask.step.delete"
        | "mask.step.reorder"
        | "effect.delete"
        | "effect.reorder"
        | "source.rasterize"
        | "group.create_selected" => Some(Handler::Metadata),
        "layer.duplicate" => Some(Handler::Metadata),
        "effect.add" | "effect.update" => Some(Handler::Effects),
        "liquify.stroke" => Some(Handler::Liquify),
        "adjustment.add" => Some(Handler::Adjustment),
        "filter.add" | "filter.update" | "filter.delete" | "filter.reorder" => {
            Some(Handler::Filters)
        }
        "move" | "transform" => Some(Handler::Retained),
        "source.add" | "source.update" | "path.create" | "path.update" | "path.node.add"
        | "path.node.split" | "path.node.delete" | "path.node.move" | "path.handle.move"
        | "path.node.convert" => Some(Handler::Source),
        "selection"
        | "selection.clear"
        | "selection.reselect"
        | "selection.invert"
        | "selection.modify"
        | "selection.refine"
        | "selection.import"
        | "selection.wand_update"
        | "path.to_selection" => Some(Handler::Selection),
        "mask.add" | "mask.from_selection" | "mask.from_color" | "mask.refine" | "path.to_mask" => {
            Some(Handler::Mask)
        }
        "paint"
        | "shape"
        | "gradient"
        | "fill"
        | "adjust"
        | "pixels.replace"
        | "image.patch"
        | "paint.fill"
        | "paint.clear_selection"
        | "smudge"
        | "clone"
        | "heal" => Some(Handler::Paint),
        _ => None,
    }
}
pub(super) fn cleanup(reason: String, result: Result<()>) -> String {
    match result {
        Ok(()) => reason,
        Err(error) => format!("{reason}; provisional source retirement refused: {error}"),
    }
}
fn apply_batch(
    edit: CapturedEdit,
    batch: RasterBatch,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    match edit.batch(batch, check) {
        Ok(delta) => Ok(delta),
        Err(refused) => {
            let reason = refused.reason().to_owned();
            Err(cleanup(
                reason,
                refused.abort(session, &|| session.cleanup_check()),
            ))
        }
    }
}
pub(crate) fn prepare(
    capture: NativeCapture,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<PreparedNative, PreparationFailure> {
    prepare_with_clipboard(capture, None, None, session, check, None)
}
pub(crate) fn prepare_text_preview(
    capture: NativeCapture,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
    carets: &mut Option<NativeCarets>,
) -> std::result::Result<PreparedNative, PreparationFailure> {
    prepare_with_clipboard(capture, None, None, session, check, Some(carets))
}
pub(crate) fn prepare_scoped(
    capture: NativeCapture,
    declared: &[crate::engine::Scope],
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<PreparedNative, PreparationFailure> {
    prepare_with_clipboard(capture, None, Some(declared), session, check, None)
}
/// Typed layer owners never pass through the command JSON or a resident raster.
pub(crate) fn prepare_layer_paste(
    capture: NativeCapture,
    clipboard: &crate::clipboard::native::layers::LayerCopyPlan,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<PreparedNative, PreparationFailure> {
    prepare_with_clipboard(capture, Some(PasteSource::Native(clipboard)), None, session, check, None)
}
pub(crate) fn prepare_resident_layer_paste(
    capture: NativeCapture,
    clipboard: &crate::layer_clipboard::Layers,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<PreparedNative, PreparationFailure> {
    prepare_with_clipboard(capture, Some(PasteSource::Resident(clipboard)), None, session, check, None)
}
pub(crate) fn prepare_image_paste(
    capture: NativeCapture,
    image: crate::clipboard::native::PasteImage<'_>,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<PreparedNative, PreparationFailure> {
    prepare_with_clipboard(capture, Some(PasteSource::Image(image)), None, session, check, None)
}
#[derive(Clone, Copy)]
enum PasteSource<'a> {
    Native(&'a crate::clipboard::native::layers::LayerCopyPlan),
    Resident(&'a crate::layer_clipboard::Layers),
    Image(crate::clipboard::native::PasteImage<'a>),
}
fn prepare_with_clipboard(
    capture: NativeCapture,
    clipboard: Option<PasteSource<'_>>,
    declared: Option<&[crate::engine::Scope]>,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
    carets: Option<&mut Option<NativeCarets>>,
) -> std::result::Result<PreparedNative, PreparationFailure> {
    let guarded = || {
        check()?;
        session.check()
    };
    guarded().map_err(PreparationFailure::new)?;
    let (document, history, edit) = capture.into_parts();
    if !document.budget().same_pool(&session.resident())
        || !document.disk_budget().same_pool(&session.disk())
    {
        return Err(PreparationFailure::new(
            "Native worker uses another source admission".into(),
        ));
    }
    let commands = edit
        .descriptor()
        .ok_or_else(|| PreparationFailure::new("Native capture missing".into()))?;
    let count = commands.commands().len();
    // Only an isolated text-editor preview can request a caret map.
    let mut carets = if count == 1 { carets } else { None };
    match clipboard {
        Some(PasteSource::Image(_)) => {
            let image_command = commands.commands().next().is_some_and(|step| {
                let c = step.parameters();
                c["op"] == "image.paste"
                    && ["path", "png", "frame", "page", "variant"]
                        .iter()
                        .all(|key| c.get(*key).is_none())
            });
            if count != 1 || !image_command {
                return Err(PreparationFailure::new(
                    "Clipboard image paste requires exactly one command with its typed source".into(),
                ));
            }
        }
        _ => {
            let typed = commands.commands().any(|step| step.parameters()["op"] == "layer.paste");
            if typed != clipboard.is_some() || (typed && count != 1) {
                return Err(PreparationFailure::new(
                    "Layer paste requires exactly one command and its typed clipboard".into(),
                ));
            }
        }
    }
    // Refuse unsupported operations before any earlier batch step writes files.
    // Handler coverage expands only with the actual nominal producer below.
    for command in edit
        .descriptor()
        .ok_or_else(|| PreparationFailure::new("Native worker requires normalized capture".into()))?
        .commands()
    {
        let operation = command.parameters()["op"]
            .as_str()
            .ok_or_else(|| PreparationFailure::new("Native worker operation is missing".into()))?;
        if handler(operation).is_none() {
            return Err(PreparationFailure::new(
                "Native command has no connected bounded producer".into(),
            ));
        }
    }
    let mut transaction = edit.transaction().map_err(PreparationFailure::new)?;
    let count = transaction.captured_command_count();
    let mut imported_bytes = 0;
    let mut raster_damage = None;
    let mut raster_only = true;
    for _ in 0..count {
        let step = match transaction.next(&guarded) {
            Ok(step) => step,
            Err(reason) => {
                let mut failure = PreparationFailure::new(reason);
                failure.cleanup(transaction.abort(session, &|| session.cleanup_check()));
                return Err(failure);
            }
        };
        if declared.is_some_and(|declared| {
            step.descriptor().is_none_or(|descriptor| {
                descriptor.commands().any(|command| {
                    !crate::engine::view::scopes_contained(
                        transaction.working().into(),
                        declared,
                        command.write_scopes(),
                    )
                })
            })
        }) {
            let mut failure = PreparationFailure::new(
                "Native edit exceeds its declared scopes; current work preserved".into(),
            );
            drop(step);
            failure.cleanup(transaction.abort(session, &|| session.cleanup_check()));
            return Err(failure);
        }
        let mut step_damage = None;
        let delta = match prepare_step(
            step, session, clipboard, &mut imported_bytes, &mut step_damage, &guarded,
            check, carets.as_deref_mut(),
        ) {
            Ok(delta) => delta,
            Err(reason) => {
                let mut failure = PreparationFailure::new(reason);
                failure.cleanup(transaction.abort(session, &|| session.cleanup_check()));
                return Err(failure);
            }
        };
        raster_only &= step_damage.is_some();
        if let Some(bounds) = step_damage {
            raster_damage = Some(raster_damage.map_or(bounds, |old| crate::engine::reframe_scope::union(old, bounds)));
        }
        if declared.is_some_and(|declared| {
            !crate::engine::view::scopes_contained(
                transaction.working().into(),
                declared,
                delta.intent().scopes(),
            )
        }) {
            let mut failure = PreparationFailure::new(
                "Native output exceeds its declared scopes; current work preserved".into(),
            );
            failure.cleanup(delta.abort(session, &|| session.cleanup_check()));
            failure.cleanup(transaction.abort(session, &|| session.cleanup_check()));
            return Err(failure);
        }
        if let Err(refused) = transaction.complete(delta, &guarded) {
            let mut failure = PreparationFailure::new(refused.reason().to_owned());
            failure.cleanup(refused.abort(session, &|| session.cleanup_check()));
            failure.cleanup(transaction.abort(session, &|| session.cleanup_check()));
            return Err(failure);
        }
    }
    match transaction.finish(&guarded) {
        Ok(delta) => Ok(PreparedNative {
            document,
            history,
            delta,
            raster_damage: raster_damage.filter(|_| raster_only),
        }),
        Err(refused) => {
            let mut failure = PreparationFailure::new(refused.reason().to_owned());
            failure.cleanup(refused.abort(session, &|| session.cleanup_check()));
            Err(failure)
        }
    }
}
fn prepare_step(
    edit: CapturedEdit,
    session: &SourceSession,
    clipboard: Option<PasteSource<'_>>,
    imported_bytes: &mut u64,
    raster_damage: &mut Option<[i32; 4]>,
    check: &dyn Fn() -> Result<()>,
    check_active: &dyn Fn() -> Result<()>,
    carets: Option<&mut Option<NativeCarets>>,
) -> Result<PreparedDelta> {
    session.check()?;
    check()?;
    let command = edit
        .descriptor()
        .ok_or("Native step descriptor missing")?
        .commands()
        .next()
        .ok_or("Native step command missing")?;
    let token = command.token();
    let kind = handler(
        command.parameters()["op"]
            .as_str()
            .ok_or("Native operation missing")?,
    )
    .ok_or("Native producer missing")?;
    match kind {
        Handler::Paint => paint::prepare(edit, session, raster_damage, check, check_active),
        Handler::Source => source::prepare(edit, session, check, carets),
        Handler::Selection => selection::prepare(edit, session, check, check_active),
        Handler::Mask => mask::prepare(edit, session, check),
        Handler::Filters => super::filters::prepare(edit, check),
        Handler::Effects => super::effects::prepare(edit, session, check),
        Handler::Liquify => super::liquify::prepare(edit, check),
        Handler::Import => {
            let image = match clipboard {
                Some(PasteSource::Image(image)) => Some(image),
                _ => None,
            };
            super::import::prepare(edit, session, imported_bytes, image, check_active)
        }
        Handler::SelectionDuplicate => super::selection_duplicate::prepare(edit, session, check_active),
        Handler::LayerPaste => match clipboard.ok_or("Typed layer clipboard missing")? {
            PasteSource::Native(snapshot) => super::layer_paste::prepare(edit, snapshot, session, check),
            PasteSource::Resident(snapshot) => super::layer_paste::prepare_resident(edit, snapshot, session, check),
            PasteSource::Image(_) => Err("Typed layer clipboard missing".into()),
        },
        Handler::Settings => super::settings::prepare(edit, session, check),
        Handler::Canvas => super::canvas::prepare(edit, check),
        Handler::ImageResize => super::image_resize::prepare(edit, session, check_active),
        Handler::Merge => super::merge::prepare(edit, session, check),
        Handler::Bake => super::bake::prepare(edit, session, check),
        Handler::Adjustment => super::adjustment::prepare(edit, session, check),
        Handler::Metadata => edit.metadata_command(token, check),
        Handler::Retained => {
            let mut work = crate::transform::native::Work::for_document();
            crate::retained::transaction::prepare_command(
                edit, token, session, &mut work, check_active,
            )
        }
    }
}

#[cfg(test)]
#[path = "worker/adjustment_tests.rs"]
mod adjustment_tests;
#[cfg(test)]
#[path = "worker/bake_tests.rs"]
mod bake_tests;
#[cfg(test)]
#[path = "worker/bmp_tests.rs"]
mod bmp_tests;
#[cfg(test)]
#[path = "worker/canvas_tests.rs"]
mod canvas_tests;
#[cfg(test)]
#[path = "worker/duplicate_tests.rs"]
mod duplicate_tests;
#[cfg(test)]
#[path = "worker/effect_create_tests.rs"]
mod effect_create_tests;
#[cfg(test)]
#[path = "worker/effect_stack_tests.rs"]
mod effect_stack_tests;
#[cfg(test)]
#[path = "worker/fill_tests.rs"]
mod fill_tests;
#[cfg(test)]
#[path = "worker/filter_tests.rs"]
mod filter_tests;
#[cfg(test)]
#[path = "worker/frame_tests.rs"]
mod frame_tests;
#[cfg(test)]
#[path = "worker/grouping_tests.rs"]
mod grouping_tests;
#[cfg(test)]
#[path = "worker/image_resize_tests.rs"]
mod image_resize_tests;
#[cfg(test)]
#[path = "worker/liquify_tests.rs"]
mod liquify_tests;
#[cfg(test)]
#[path = "worker/mask_create_tests.rs"]
mod mask_create_tests;
#[cfg(test)]
#[path = "worker/mask_stack_tests.rs"]
mod mask_stack_tests;
#[cfg(test)]
#[path = "worker/merge_tests.rs"]
mod merge_tests;
#[cfg(test)]
#[path = "worker/pnm_tests.rs"]
mod pnm_tests;
#[cfg(test)]
#[path = "worker/primitive_tests.rs"]
mod primitive_tests;
#[cfg(test)]
#[path = "worker/qoi_tests.rs"]
mod qoi_tests;
#[cfg(test)]
#[path = "worker/replacement_tests.rs"]
mod replacement_tests;
#[cfg(test)]
#[path = "worker/retouch_tests.rs"]
mod retouch_tests;
#[cfg(test)]
#[path = "worker/selection_duplicate_tests.rs"]
mod selection_duplicate_tests;
#[cfg(test)]
#[path = "worker_tests.rs"]
pub(crate) mod tests;
#[cfg(test)]
#[path = "worker/tga_tests.rs"]
mod tga_tests;

#[cfg(test)]
#[path = "worker/image_patch_tests.rs"]
mod image_patch_tests;

#[cfg(test)]
#[path = "worker/settings_tests.rs"]
mod settings_tests;

#[cfg(test)]
#[path = "worker/import_tests.rs"]
mod import_tests;

#[cfg(test)]
#[path = "worker/placement_tests.rs"]
mod placement_tests;

#[cfg(test)]
#[path = "worker/paste_tests.rs"]
mod paste_tests;

#[cfg(test)]
#[path = "worker/svg_import_tests.rs"]
mod svg_import_tests;

#[cfg(test)]
#[path = "worker/apng_tests.rs"]
mod apng_tests;
#[cfg(test)]
#[path = "worker/gif_import_tests.rs"]
pub(crate) mod gif_import_tests;
#[cfg(test)]
#[path = "worker/ico_tests.rs"]
pub(crate) mod ico_tests;
#[cfg(test)]
#[path = "worker/jpeg_tests.rs"]
pub(crate) mod jpeg_tests;
#[cfg(test)]
#[path = "worker/jxl_tests.rs"]
mod jxl_tests;
#[cfg(test)]
#[path = "worker/tiff_tests.rs"]
pub(crate) mod tiff_tests;
#[cfg(test)]
#[path = "worker/webp_tests.rs"]
mod webp_tests;

#[cfg(test)]
#[path = "worker/layer_paste_tests.rs"]
mod layer_paste_tests;

#[cfg(test)]
#[path = "worker/source_budget_tests.rs"]
mod source_budget_tests;
