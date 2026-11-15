//! Session-owned derived style fields, with explicit cleanup and no source authority.
use super::{staging::Namespace, SourceSession};
use crate::large_document::{CompletedScalar, ScalarScratch, ScalarSpec};

pub(crate) struct SessionScalarScratch<'a> {
    scratch: ScalarScratch,
    namespace: Namespace<'a>,
}
pub(crate) struct SessionScalar<'a> {
    scalar: CompletedScalar,
    namespace: Namespace<'a>,
}
impl SourceSession {
    pub(crate) fn scalar_temporary(
        &self,
        spec: ScalarSpec,
    ) -> Result<SessionScalarScratch<'_>, String> {
        self.check()?;
        self.reserve_directory()?;
        let id = *uuid::Uuid::new_v4().as_bytes();
        let path = self
            .directory
            .join(format!("scalar-{}", uuid::Uuid::from_bytes(id)));
        let scratch = ScalarScratch::create(&path, spec, self.resident(), self.disk())
            .map_err(|e| e.to_string())?;
        let namespace =
            Namespace::register(self, id, path, scratch.filesystem_identity(), "field.pbf")?;
        Ok(SessionScalarScratch { scratch, namespace })
    }
}
impl<'a> SessionScalarScratch<'a> {
    pub(crate) fn check(&self) -> Result<(), String> {
        self.namespace.check()
    }
    pub(crate) fn append_rows(
        &mut self,
        y: u32,
        bytes: &[u8],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let mut failure = None;
        self.scratch
            .append_rows(y, bytes, &mut || {
                self.namespace.io_check(check, &mut failure, false)
            })
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))
    }
    pub(crate) fn finish(
        self,
        binding: [u8; 32],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<SessionScalar<'a>, String> {
        let Self { scratch, namespace } = self;
        let mut failure = None;
        let scalar = scratch
            .finish(binding, &mut || {
                namespace.io_check(check, &mut failure, false)
            })
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        namespace.check()?;
        Ok(SessionScalar { scalar, namespace })
    }
    pub(crate) fn abort(
        self,
        cleanup: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let Self { scratch, namespace } = self;
        let mut failure = None;
        scratch
            .abort(&mut || namespace.io_check(cleanup, &mut failure, true))
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        namespace.retire(cleanup)
    }
}
impl SessionScalar<'_> {
    pub(crate) fn check_session(&self, session: &SourceSession) -> Result<(), String> {
        if !std::ptr::eq(self.namespace.session, session) {
            return Err("Style field belongs to another source operation".into());
        }
        self.check()
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.namespace.check()?;
        self.scalar.check_owner().map_err(|e| e.to_string())
    }
    /// Retain this session guard through the last read and its publication check.
    pub(crate) fn scalar(&self) -> &CompletedScalar {
        &self.scalar
    }
    pub(crate) fn retire(
        self,
        cleanup: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let Self { scalar, namespace } = self;
        let mut failure = None;
        scalar
            .retire(&mut || namespace.io_check(cleanup, &mut failure, true))
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        namespace.retire(cleanup)
    }
}
