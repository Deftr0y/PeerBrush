use super::*;
use crate::render::native_preview::{layer_thumbnail_key, LayerThumbnailKey};
use crate::workspace::native_dispatch::retryable_source_read;
use std::time::Instant;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::source_runtime::INVENTORY_CHECK_BUSY;
    #[test]
    fn layer_targets_share_one_pending_capture_and_source_changes_cancel_it() {
        for depth in [8, 16] {
            let (app, ctx) = crate::ui::workspace_tests::fixture(depth);
            let mut doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            let mut second = crate::engine::Layer::new("Second", "paint", doc.width, doc.height);
            second.pixels = crate::raster::Raster::new_depth(doc.width, doc.height, depth);
            doc.layers.push(second);
            // This fixture's native sources are explicit zero surfaces.
            for layer in &mut doc.layers {
                layer.pixels = crate::raster::Raster::new_depth(doc.width, doc.height, depth);
                layer.effects.clear();
                layer.mask = None;
            }
            let pool = crate::large_document::ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let native = crate::tree::presentation_tests::native(&doc, &pool);
            let replacement = crate::tree::presentation_tests::native(&doc, &pool);
            let mut thumbnails = Thumbnails::default();
            let shared = app.shared.clone();
            let guard = shared.lock().unwrap();
            assert!(thumbnails
                .get_layer(
                    &shared,
                    &app.project_id,
                    &native,
                    &doc.layers[0].id,
                    false,
                    &ctx
                )
                .unwrap()
                .is_none());
            let ticket = thumbnails
                .state
                .as_ref()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .1;
            assert!(thumbnails
                .get_layer(
                    &shared,
                    &app.project_id,
                    &native,
                    &doc.layers[1].id,
                    false,
                    &ctx
                )
                .unwrap()
                .is_none());
            let state = thumbnails.state.as_ref().unwrap();
            assert!(state.layer.is_none());
            assert_eq!(state.pending.as_ref().unwrap().0 .0, doc.layers[0].id);
            assert_eq!(state.pending.as_ref().unwrap().1, ticket);
            assert!(thumbnails
                .get_layer(
                    &shared,
                    &app.project_id,
                    &replacement,
                    &doc.layers[1].id,
                    false,
                    &ctx
                )
                .unwrap()
                .is_none());
            let state = thumbnails.state.as_ref().unwrap();
            assert_eq!(state.source, replacement.key());
            assert_ne!(state.pending.as_ref().unwrap().1, ticket);
            assert_eq!(state.pending.as_ref().unwrap().0 .0, doc.layers[1].id);
            thumbnails.clear();
            drop(guard);
            thumbnails.stop();
            assert!(thumbnails.state.is_none());
        }
    }

    #[test]
    fn busy_inventory_defers_thumbnail_but_ownership_failure_stays_visible() {
        let (app, ctx) = crate::ui::workspace_tests::fixture(16);
        let mut doc = app
            .shared
            .lock()
            .unwrap()
            .resident_state()
            .unwrap()
            .document
            .clone();
        let layer = &mut doc.layers[0];
        layer.pixels = crate::raster::Raster::new_depth(doc.width, doc.height, 16);
        layer.effects.clear();
        layer.mask = None;
        let id = layer.id.clone();
        let pool = crate::large_document::ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let native = crate::tree::presentation_tests::native(&doc, &pool);
        let mut thumbnails = Thumbnails::default();
        let guard = app.shared.lock().unwrap();
        let get = |thumbnails: &mut Thumbnails| {
            thumbnails.get_layer(&app.shared, &app.project_id, &native, &id, false, &ctx)
        };
        assert!(get(&mut thumbnails).unwrap().is_none());
        thumbnails.sampler.cancel();
        let state = thumbnails.state.as_mut().unwrap();
        let (target, first, _) = state.pending.take().unwrap();
        state.record_failure(target.clone(), INVENTORY_CHECK_BUSY.into());
        assert!(state.retry_after.is_some());
        assert!(state.errors.is_empty());
        state.retry_after = Some(Instant::now() + Duration::from_secs(5));
        assert!(get(&mut thumbnails).unwrap().is_none());
        assert!(thumbnails.state.as_ref().unwrap().pending.is_none());

        thumbnails.state.as_mut().unwrap().retry_after = Some(Instant::now() - Duration::from_secs(1));
        assert!(get(&mut thumbnails).unwrap().is_none());
        let state = thumbnails.state.as_mut().unwrap();
        let (next_target, second, _) = state.pending.take().unwrap();
        assert_eq!(next_target, target);
        assert_ne!(second, first);
        assert!(state.retry_after.is_none());

        let error = "Source inventory ownership changed; files retained";
        state.record_failure(target, error.into());
        assert_eq!(get(&mut thumbnails).err().as_deref(), Some(error));
        thumbnails.clear();
        drop(guard);
        thumbnails.stop();
    }
}
#[derive(Default)]
pub(super) struct Thumbnails {
    sampler: color_sample::Sampler<crate::render::mask_thumbnail::Thumbnail>,
    state: Option<State>,
    retirement: Option<native_texture::Retirement>,
}
type Key = (String, bool, Option<String>);
#[derive(Clone, Copy, PartialEq, Eq)]
enum ThumbnailKey {
    Mask([u8; 32]),
    Color(LayerThumbnailKey),
}
fn thumbnail_key(
    doc: &crate::engine::native::NativeDocument,
    layer: crate::engine::native::LayerMetadata<'_>,
    mask: bool,
    step: Option<&str>,
    profile: Option<Option<[u8; 32]>>,
) -> Option<ThumbnailKey> {
    if mask {
        crate::render::mask_thumbnail::cache_key(doc, layer, step).map(ThumbnailKey::Mask)
    } else {
        layer_thumbnail_key(doc, layer, profile?).map(ThumbnailKey::Color)
    }
}
struct State {
    engine: std::sync::Weak<std::sync::Mutex<crate::engine::Engine>>,
    project: String,
    document: String,
    revision: u64,
    source: crate::engine::native::DocumentKey,
    layer: Option<String>,
    profile: Option<Option<[u8; 32]>>,
    limit: usize,
    textures: HashMap<Key, (TextureHandle, Option<ThumbnailKey>)>,
    pending: Option<(Key, color_sample::Ticket, Option<ThumbnailKey>)>,
    errors: HashMap<Key, String>,
    retry_after: Option<Instant>,
    _charge: Arc<crate::psd::memory36::MetadataCharge>,
}
impl State {
    fn record_failure(&mut self, target: Key, error: String) {
        if retryable_source_read(&error) {
            self.retry_after = Some(Instant::now() + INVENTORY_RETRY_DELAY);
        } else {
            self.errors.insert(target, error.chars().take(256).collect());
        }
    }
}
impl Thumbnails {
    pub(super) fn clear(&mut self) {
        self.sampler.cancel();
        self.state = None;
    }
    pub(super) fn stop(&mut self) {
        self.clear();
        self.sampler.stop();
        self.retirement = None;
    }
    pub(super) fn get(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: &crate::engine::native::NativeDocument,
        layer: LayerView<'_>,
        index: usize,
        ctx: &egui::Context,
    ) -> Result<Option<TextureHandle>, String> {
        let step = layer
            .mask()
            .and_then(|mask| mask.step(index))
            .ok_or("Thumbnail mask step disappeared")?
            .id();
        self.get_target(shared, project, doc, layer.id(), true, Some(step), ctx)
    }
    pub(super) fn get_layer(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: &crate::engine::native::NativeDocument,
        layer: &str,
        mask: bool,
        ctx: &egui::Context,
    ) -> Result<Option<TextureHandle>, String> {
        self.get_target(shared, project, doc, layer, mask, None, ctx)
    }
    fn get_target(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: &crate::engine::native::NativeDocument,
        layer_id: &str,
        mask: bool,
        step: Option<&str>,
        ctx: &egui::Context,
    ) -> Result<Option<TextureHandle>, String> {
        let metadata = doc.metadata();
        let layer = metadata.layers().find(|layer| layer.id() == layer_id)
            .ok_or("Thumbnail layer disappeared")?;
        if self.retirement.as_ref().is_some_and(|owner| !owner.uses_budget(doc.budget())) {
            self.retirement = None;
        }
        if mask && layer.mask().is_none() {
            return Err("Thumbnail mask disappeared".into());
        }
        if let Some(id) = step {
            let channel = layer.mask().ok_or("Thumbnail mask disappeared")?;
            if !channel.steps().any(|step| step.id() == id) {
                return Err("Thumbnail mask step disappeared".into());
            }
        }
        let scope = step.map(|_| layer_id);
        let same_document = |state: &State| {
            state.engine.as_ptr() == Arc::as_ptr(shared)
                && state.project == project
                && state.document == metadata.id()
                && state.layer.as_deref() == scope
                && state._charge.uses_budget(doc.budget())
        };
        let mut previous = None;
        if self.state.as_ref().is_some_and(|state| {
            !same_document(state) || state.source != doc.key()
        }) {
            self.sampler.cancel();
            previous = self.state.take().filter(|state| {
                same_document(state) && metadata.revision() > state.revision
            });
        }
        if self.state.is_none() {
            let count = if step.is_some() {
                layer.mask().unwrap().steps().len()
            } else {
                metadata.layers().len()
                    .checked_mul(2)
                    .ok_or("Thumbnail metadata overflow")?
            };
            if count > if step.is_some() { 64 } else { 200 } {
                return Err("Thumbnail cache exceeds its bounded target count".into());
            }
            let mut bytes = count
                .checked_mul(4096)
                .and_then(|n| n.checked_add(4096))
                .ok_or("Thumbnail metadata overflow")?;
            let mut add = |n: usize| -> Result<(), String> {
                bytes = n
                    .checked_mul(4)
                    .and_then(|n| bytes.checked_add(n))
                    .filter(|n| *n <= 64 * 1024 * 1024)
                    .ok_or("Thumbnail metadata exceeds admission")?;
                Ok(())
            };
            add(project.len())?;
            add(metadata.id().len())?;
            if step.is_some() {
                add(layer.id().len())?;
                for step in layer.mask().unwrap().steps() {
                    add(step.id().len())?;
                }
            } else {
                for layer in metadata.layers() {
                    add(layer
                        .id()
                        .len()
                        .checked_mul(2)
                        .ok_or("Thumbnail metadata overflow")?)?;
                }
            }
            let charge =
                crate::psd::memory36::MetadataCharge::reserve(doc.budget(), bytes, &|| Ok(()))?;
            // The layer panel shares one profile digest across all color targets.
            let profile = if scope.is_none() {
                crate::color_profile::prepared::profile_binding(metadata.profile(), &mut || Ok(()))
                    .ok()
            } else {
                None
            };
            let textures = previous.as_mut().map_or_else(HashMap::new, |state| {
                state.textures.retain(|(id, mask, step), (_, old)| {
                    let Some(old) = old else { return false };
                    metadata.layers().find(|layer| layer.id() == id)
                        .and_then(|layer| thumbnail_key(doc, layer, *mask, step.as_deref(), profile))
                        .as_ref() == Some(&*old)
                });
                // The new cache charge reflects the current layer or step count.
                state.textures.shrink_to_fit();
                std::mem::take(&mut state.textures)
            });
            self.state = Some(State {
                engine: Arc::downgrade(shared),
                project: project.into(),
                document: metadata.id().into(),
                revision: metadata.revision(),
                source: doc.key(),
                layer: scope.map(str::to_owned),
                profile,
                limit: count,
                textures,
                pending: None,
                errors: HashMap::new(),
                retry_after: None,
                _charge: charge,
            });
        }
        let state = self.state.as_mut().unwrap();
        if let Some((step, ticket, rendered_key)) = &state.pending {
            match self.sampler.take_captured(*ticket, shared, project) {
                Ok(Some(image)) => {
                    let upload = (|| {
                        // Keep the retirement queue across edits, even when the
                        // document generation invalidates every cached texture.
                        if self.retirement.is_none() {
                            self.retirement = Some(native_texture::Retirement::new(doc.budget())?);
                        }
                        native_texture::thumbnail(image, self.retirement.as_ref().unwrap(), ctx)
                    })();
                    match upload {
                        Ok(texture) => {
                            state.textures.insert(step.clone(), (texture, *rendered_key));
                        }
                        Err(error) => {
                            state
                                .errors
                                .insert(step.clone(), error.chars().take(256).collect());
                        }
                    }
                    state.pending = None;
                }
                Err(error) => {
                    let target = step.clone();
                    state.record_failure(target, error);
                    state.pending = None;
                }
                Ok(None) => {}
            }
        }
        let key = (layer_id.to_owned(), mask, step.map(str::to_owned));
        if let Some(error) = state.errors.get(&key) {
            return Err(error.clone());
        }
        if let Some((texture, _)) = state.textures.get(&key) {
            return Ok(Some(texture.clone()));
        }
        if state.pending.is_none() {
            if let Some(deadline) = state.retry_after {
                if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                    ctx.request_repaint_after(remaining);
                    return Ok(None);
                }
                state.retry_after = None;
            }
            if state.textures.len() + state.errors.len() >= state.limit {
                return Err("Thumbnail cache is full".into());
            }
            let request = if let Some(id) = step {
                self.sampler
                    .request_mask_thumbnail(shared, project, doc.into(), layer_id, id, ctx)
            } else {
                self.sampler.request_layer_thumbnail(
                    shared,
                    project,
                    doc.into(),
                    layer_id,
                    mask,
                    ctx,
                )
            };
            let ticket = match request {
                Ok(ticket) => ticket,
                Err(error) => {
                    state
                        .errors
                        .insert(key, error.chars().take(256).collect());
                    return Err(error);
                }
            };
            let render_key = thumbnail_key(doc, layer, mask, step, state.profile);
            state.pending = Some((key, ticket, render_key));
        }
        Ok(None)
    }
}
