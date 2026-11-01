//! Nominal read-only admission. Explicit zero surfaces need no source namespace.
use super::*;
use crate::{
    engine::{native::DocumentKey, scene::SceneSnapshot},
    large_document::root::RootReadOperation,
};

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
            SourceReadSession::from(Arc::new(self.begin()?))
        };
        guard.check_scene(scene)?;
        Ok(guard)
    }
}
impl SourceReadSession {
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
    pub(crate) fn check_scene(&self, scene: &SceneSnapshot) -> Result<(), String> {
        self.check()?;
        if !scene.budget().same_pool(&self.resident())
            || !scene.disk_budget().same_pool(&self.disk())
        {
            return Err("Read session uses another scene admission pool".into());
        }
        if let ReadKind::Memory(held) = &self.0 {
            if held.key != scene.key()
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
