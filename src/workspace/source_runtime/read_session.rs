//! Nominal read-only admission. Explicit zero surfaces need no source namespace.
use super::*;
use crate::{
    engine::{native::DocumentKey, scene::SceneSnapshot},
    large_document::root::RootReadOperation,
    raster::{backed::Region, Surface, SurfaceRegion},
};
use std::cell::RefCell;

#[derive(Clone)]
pub(crate) struct SourceReadSession(ReadKind);
#[derive(Clone)]
enum ReadKind {
    Inventory(Arc<SourceSession>),
    Memory(Arc<MemoryRead>),
}
struct MemoryRead {
    runtime: Arc<SourceRuntime>,
    operation: RootReadOperation,
    key: DocumentKey,
    _admission: Reservation,
}

pub(crate) struct ReadChecks<'a> {
    session: Option<&'a SourceReadSession>,
    cancel: &'a mut dyn FnMut() -> Result<(), String>,
    check_before_cancel: bool,
}
impl<'a> ReadChecks<'a> {
    pub(crate) fn with_session(
        session: &'a SourceReadSession,
        cancel: &'a mut dyn FnMut() -> Result<(), String>,
    ) -> Self {
        Self {
            session: Some(session),
            cancel,
            check_before_cancel: true,
        }
    }
    pub(crate) fn with_session_after_cancel(
        session: &'a SourceReadSession,
        cancel: &'a mut dyn FnMut() -> Result<(), String>,
    ) -> Self {
        Self {
            session: Some(session),
            cancel,
            check_before_cancel: false,
        }
    }
    pub(crate) fn caller_only(cancel: &'a mut dyn FnMut() -> Result<(), String>) -> Self {
        Self {
            session: None,
            cancel,
            check_before_cancel: false,
        }
    }
    pub(crate) fn full(&mut self) -> Result<(), String> {
        if let Some(session) = self.session.filter(|_| self.check_before_cancel) {
            session.check()?;
        }
        (self.cancel)()?;
        if let Some(session) = self.session {
            session.check()?;
        }
        Ok(())
    }
    pub(crate) fn pinned(&mut self) -> Result<(), String> {
        if let Some(session) = self.session {
            session.check_pinned_work()?;
        }
        (self.cancel)()?;
        if let Some(session) = self.session {
            session.check_pinned_work()?;
        }
        Ok(())
    }

    pub(crate) fn surface_region(
        &mut self,
        source: &Surface,
        region: Region,
    ) -> Result<SurfaceRegion, String> {
        if self.session.is_none() {
            return self.with_backing(|check| {
                source.prepare_region(region, check).map_err(|error| error.to_string())
            });
        }
        let checks = RefCell::new(self);
        let interrupted = RefCell::new(None);
        let guard = |result: Result<(), String>| result.map_err(|error| {
            *interrupted.borrow_mut() = Some(error);
            crate::large_document::Error::Cancelled
        });
        let result = source.prepare_region_guarded(
            region,
            &mut || guard(checks.borrow_mut().pinned()),
            &mut || guard(checks.borrow_mut().full()),
        );
        result.map_err(|error| interrupted.into_inner().unwrap_or_else(|| error.to_string()))
    }

    pub(crate) fn with_backing<T>(
        &mut self,
        work: impl FnOnce(&mut dyn FnMut() -> crate::large_document::Result<()>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut interrupted = None;
        let result = work(&mut || {
            self.full().map_err(|error| {
                interrupted = Some(error);
                crate::large_document::Error::Cancelled
            })
        });
        match interrupted {
            Some(error) => Err(error),
            None => result,
        }
    }
}

impl From<Arc<SourceSession>> for SourceReadSession {
    fn from(session: Arc<SourceSession>) -> Self {
        Self(ReadKind::Inventory(session))
    }
}
impl SourceRuntime {
    /// All roster entries, including retained originals, must be explicit Zero.
    /// A stored empty plane still takes ordinary inventory/held-file admission.
    pub(crate) fn begin_read(
        self: &Arc<Self>,
        scene: &SceneSnapshot,
    ) -> Result<SourceReadSession, String> {
        if !scene.budget().same_pool(&self.resident) || !scene.disk_budget().same_pool(&self.disk) {
            return Err("Read scene belongs to another source service".into());
        }
        let guard = if scene
            .rasters()
            .iter()
            .all(|(_, source)| source.is_known_zero())
        {
            let admission = self
                .resident
                .reserve_bytes(1024, 1024)
                .map_err(|e| e.to_string())?;
            let operation = self.context.begin_observation()?;
            SourceReadSession(ReadKind::Memory(Arc::new(MemoryRead {
                runtime: self.clone(),
                operation,
                key: scene.key(),
                _admission: admission,
            })))
        } else {
            SourceReadSession::from(Arc::new(self.begin_foreground()?))
        };
        guard.check_scene(scene)?;
        Ok(guard)
    }
}
impl SourceReadSession {
    pub(crate) fn uses_runtime(&self, runtime: &Arc<SourceRuntime>) -> bool {
        match &self.0 {
            ReadKind::Inventory(session) => session.uses_runtime(runtime),
            ReadKind::Memory(held) => Arc::ptr_eq(&held.runtime, runtime),
        }
    }
    /// Final publication stays memory-only and retains this exact instance
    /// request until the caller leaves the fence and releases the read owner.
    pub(crate) fn with_live_fence<T>(&self, commit: impl FnOnce() -> T) -> Result<T, String> {
        match &self.0 {
            ReadKind::Inventory(session) => session.with_live_fence(commit),
            ReadKind::Memory(held) => held.operation.with_live_fence(commit),
        }
    }
    /// Global derived stages need temporary storage, even for a zero source.
    /// Ordinary zero-scene observations keep their memory-only admission.
    pub(crate) fn begin_derived(
        &self,
        scene: &SceneSnapshot,
    ) -> Result<Arc<SourceSession>, String> {
        self.check_scene(scene)?;
        let session = match &self.0 {
            ReadKind::Inventory(session) => session.clone(),
            ReadKind::Memory(held) => Arc::new(held.runtime.begin_foreground()?),
        };
        self.check_scene(scene)?;
        session.check()?;
        Ok(session)
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        match &self.0 {
            ReadKind::Inventory(session) => session.check(),
            ReadKind::Memory(held) => {
                if held.runtime.tainted.load(Ordering::Acquire) {
                    return Err("Source service ownership changed; observation refused".into());
                }
                held.operation.check()
            }
        }
    }
    /// Only for work over already pinned pixels. Source I/O and output handoff
    /// still require the full check, including the held filesystem inventory.
    pub(crate) fn check_pinned_work(&self) -> Result<(), String> {
        match &self.0 {
            ReadKind::Inventory(session) => session.check_pinned_work(),
            ReadKind::Memory(held) => {
                held.operation.check_active()?;
                if held.runtime.tainted.load(Ordering::Acquire) {
                    return Err("Source service ownership changed; observation refused".into());
                }
                Ok(())
            }
        }
    }
    pub(crate) fn check_scene(&self, scene: &SceneSnapshot) -> Result<(), String> {
        self.check()?;
        if !scene.budget().same_pool(&self.resident())
            || !scene.disk_budget().same_pool(&self.disk())
        {
            return Err("Read session uses another scene admission pool".into());
        }
        if let ReadKind::Memory(held) = &self.0 {
            if held.key != scene.read_source_key()
                || !scene
                    .rasters()
                    .iter()
                    .all(|(_, source)| source.is_known_zero())
            {
                return Err(
                    "Memory-only read permit does not match its explicit zero scene".into(),
                );
            }
        }
        Ok(())
    }
    pub(crate) fn resident(&self) -> ResidentBudget {
        match &self.0 {
            ReadKind::Inventory(s) => s.resident(),
            ReadKind::Memory(s) => s.runtime.resident(),
        }
    }
    pub(crate) fn disk(&self) -> DiskBudget {
        match &self.0 {
            ReadKind::Inventory(s) => s.disk(),
            ReadKind::Memory(s) => s.runtime.disk(),
        }
    }
}
