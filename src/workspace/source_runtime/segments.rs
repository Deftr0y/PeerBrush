//! Session-owned compatible JXL logs. They grant no encoder or destination authority.
use super::{
    staging::{Namespace, SessionBytes, SessionTemporary},
    SourceSession,
};
use crate::large_document::{
    segments::{CompletedSegments, SegmentEvent, SegmentLimits, SegmentLog},
    ByteRole, CompletedBytes, ScratchBytes,
};

pub(crate) struct SessionSegmentLog<'a> {
    log: SegmentLog,
    namespace: Namespace<'a>,
}
pub(crate) struct SessionSegments<'a> {
    log: CompletedSegments,
    namespace: Namespace<'a>,
}
pub(crate) struct SessionJxlOutput<'a> {
    canonical: SessionBytes<'a>,
    log: SessionSegments<'a>,
}
impl SourceSession {
    pub(crate) fn segment_log(
        &self,
        limits: SegmentLimits,
        nonce: [u8; 16],
        binding: [u8; 32],
    ) -> Result<SessionSegmentLog<'_>, String> {
        self.check()?;
        limits.physical_ceiling().map_err(|e| e.to_string())?;
        if nonce == [0; 16] || binding == [0; 32] {
            return Err("Output log lacks its operation binding".into());
        }
        self.reserve_directory()?;
        let id = *uuid::Uuid::new_v4().as_bytes();
        let path = self
            .directory
            .join(format!("segments-{}", uuid::Uuid::from_bytes(id)));
        let log = SegmentLog::create(&path, limits, nonce, binding, self.resident(), self.disk())
            .map_err(|e| e.to_string())?;
        let namespace =
            Namespace::register(self, id, path, log.filesystem_identity(), "segments.pbj")?;
        Ok(SessionSegmentLog { log, namespace })
    }
}
impl<'a> SessionSegmentLog<'a> {
    pub(crate) fn append(
        &mut self,
        sequence: u64,
        event: SegmentEvent,
        payload: Option<(&ScratchBytes, usize)>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let mut failure = None;
        self.log
            .append(sequence, event, payload, &mut || {
                self.namespace.io_check(check, &mut failure, false)
            })
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))
    }
    pub(crate) fn finish(
        self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<SessionSegments<'a>, String> {
        let Self { log, namespace } = self;
        let mut failure = None;
        let log = log
            .finish(&mut || namespace.io_check(check, &mut failure, false))
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        namespace.check()?;
        Ok(SessionSegments { log, namespace })
    }
    pub(crate) fn abort(
        self,
        cleanup: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let Self { log, namespace } = self;
        let mut failure = None;
        log.abort(&mut || namespace.io_check(cleanup, &mut failure, true))
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        namespace.retire(cleanup)
    }
}
impl<'a> SessionSegments<'a> {
    pub(crate) fn payload_length(&self) -> u64 {
        self.log.payload_length()
    }
    pub(crate) fn binding(&self) -> [u8; 32] {
        self.log.binding()
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.namespace.check()?;
        self.log.check_owner().map_err(|e| e.to_string())
    }
    /// The borrowed source session survives in both output guards. The caller
    /// must retain this log guard through validation/publication or retirement.
    pub(crate) fn canonical(
        self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<SessionJxlOutput<'a>, String> {
        self.namespace.check()?;
        let mut output = self
            .namespace
            .session
            .byte_temporary(self.payload_length(), ByteRole::CompatibleJxlMode1)?;
        let SessionTemporary {
            temporary,
            namespace,
        } = &mut output;
        let mut failure = None;
        let replay = self
            .log
            .replay(temporary, &mut || {
                self.namespace.io_check(check, &mut failure, false)?;
                namespace.io_check(&mut || Ok(()), &mut failure, false)
            })
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        let output = output.finish(self.binding(), &mut || {
            self.check()?;
            check()
        })?;
        if replay.bytes != output.bytes().payload_length()
            || replay.sha256 != output.bytes().payload_sha256()
            || output.bytes().role() != ByteRole::CompatibleJxlMode1
            || output.bytes().binding() != self.binding()
        {
            return Err("Canonical JXL replay receipt differs; owned bytes retained".into());
        }
        self.check()?;
        check()?;
        Ok(SessionJxlOutput {
            canonical: output,
            log: self,
        })
    }
    pub(crate) fn retire(
        self,
        cleanup: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let Self { log, namespace } = self;
        let mut failure = None;
        log.retire(&mut || namespace.io_check(cleanup, &mut failure, true))
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        namespace.retire(cleanup)
    }
}

impl SessionJxlOutput<'_> {
    pub(crate) fn bytes(&self) -> &CompletedBytes {
        self.canonical.bytes()
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.log.check()?;
        self.canonical.check()
    }
    pub(crate) fn check_session(&self, session: &SourceSession) -> Result<(), String> {
        self.canonical.check_session(session)?;
        if !std::ptr::eq(self.log.namespace.session, session) {
            return Err("JXL output belongs to another source operation".into());
        }
        self.check()
    }
    pub(crate) fn retire(
        self,
        cleanup: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.canonical.retire(cleanup)?;
        self.log.retire(cleanup)
    }
}
