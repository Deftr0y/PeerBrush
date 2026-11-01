//! Source spools retain authenticated session admission through closed save I/O.
use super::CompletedSave36;
#[cfg(test)]
use super::OwnedRoots;
use crate::{
    engine::scene::SceneSnapshot,
    large_document::{ByteRole, BytesCursor, CompletedBytes, LargeSourceTemporary},
    workspace::source_runtime::{SessionBytes, SessionTemporary, SourceSession},
};
use std::{
    io::{self, Read, Seek, SeekFrom},
    ops::Deref,
};

pub(super) trait StagingFactory<'a> {
    fn check(&self) -> Result<(), String>;
    fn cleanup_check(&self) -> Result<(), String>;
    fn create(
        &self,
        scene: &SceneSnapshot,
        length: u64,
        role: ByteRole,
    ) -> Result<Temporary<'a>, String>;
}
impl<'a> StagingFactory<'a> for &'a SourceSession {
    fn check(&self) -> Result<(), String> {
        SourceSession::check(self)
    }
    fn cleanup_check(&self) -> Result<(), String> {
        SourceSession::cleanup_check(self)
    }
    fn create(
        &self,
        scene: &SceneSnapshot,
        length: u64,
        role: ByteRole,
    ) -> Result<Temporary<'a>, String> {
        self.check()?;
        if !scene.budget().same_pool(&self.resident())
            || !scene.disk_budget().same_pool(&self.disk())
        {
            return Err("Native save session differs from captured resource pools".into());
        }
        let session: &'a SourceSession = *self;
        Ok(Temporary::Session(session.byte_temporary(length, role)?))
    }
}
#[cfg(test)]
impl<'a> StagingFactory<'a> for OwnedRoots<'_> {
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
    fn cleanup_check(&self) -> Result<(), String> {
        Ok(())
    }
    fn create(
        &self,
        scene: &SceneSnapshot,
        length: u64,
        role: ByteRole,
    ) -> Result<Temporary<'a>, String> {
        let root = match role {
            ByteRole::LayerData => self.layers,
            ByteRole::CompositeData => self.composite,
            ByteRole::SourceBulk => self.manifest,
            ByteRole::PhotoshopContainer => self.container,
            ByteRole::CompatibleJxlMode1 | ByteRole::StraightNativeRgba => {
                return Err("Photoshop fixture cannot stage another codec role".into());
            }
        };
        Ok(Temporary::Fixture(
            LargeSourceTemporary::create(
                root,
                length,
                role,
                scene.budget().clone(),
                scene.disk_budget().clone(),
            )
            .map_err(|e| e.to_string())?,
        ))
    }
}
pub(super) enum Temporary<'a> {
    Session(SessionTemporary<'a>),
    #[cfg(test)]
    Fixture(LargeSourceTemporary),
}
impl<'a> Temporary<'a> {
    pub(super) fn temporary(&mut self) -> Result<&mut LargeSourceTemporary, String> {
        match self {
            Self::Session(value) => {
                value.check()?;
                Ok(value.temporary())
            }
            #[cfg(test)]
            Self::Fixture(value) => Ok(value),
        }
    }
    pub(super) fn finish(
        self,
        binding: [u8; 32],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<StagedBytes<'a>, String> {
        match self {
            Self::Session(value) => Ok(StagedBytes::Session(
                value.finish(binding, &mut || check())?,
            )),
            #[cfg(test)]
            Self::Fixture(value) => Ok(StagedBytes::Fixture(
                value
                    .finish(binding, &mut || super::io_check(check))
                    .map_err(|e| e.to_string())?,
            )),
        }
    }
    pub(super) fn abort(self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        match self {
            Self::Session(value) => value.abort(&mut || check()),
            #[cfg(test)]
            Self::Fixture(value) => value
                .abort(&mut || super::io_check(check))
                .map_err(|e| e.to_string()),
        }
    }
}
pub(super) enum StagedBytes<'a> {
    Session(SessionBytes<'a>),
    #[cfg(test)]
    Fixture(CompletedBytes),
}
impl StagedBytes<'_> {
    pub(super) fn check(&self) -> Result<(), String> {
        match self {
            Self::Session(value) => value.check(),
            #[cfg(test)]
            Self::Fixture(_) => Ok(()),
        }
    }
    pub(super) fn retire(self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        match self {
            Self::Session(value) => value.retire(&mut || check()),
            #[cfg(test)]
            Self::Fixture(value) => value
                .retire(&mut || super::io_check(check))
                .map_err(|e| e.to_string()),
        }
    }
}
// Private driver implementation only. ClosedSave never exports this primitive
// reference in production; external readers retain its session guard instead.
impl Deref for StagedBytes<'_> {
    type Target = CompletedBytes;
    fn deref(&self) -> &CompletedBytes {
        match self {
            Self::Session(value) => value.bytes(),
            #[cfg(test)]
            Self::Fixture(value) => value,
        }
    }
}

/// A cursor cannot outlive the closed save or clone/export an unguarded source.
pub(crate) struct SaveCursor<'read, 'source> {
    input: BytesCursor<'read>,
    owner: &'read CompletedSave36<'source>,
}
impl<'read, 'source> SaveCursor<'read, 'source> {
    pub(super) fn new(owner: &'read CompletedSave36<'source>, input: BytesCursor<'read>) -> Self {
        Self { owner, input }
    }
}
impl Read for SaveCursor<'_, '_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.owner.check_cursor_guard().map_err(io::Error::other)?;
        let count = self.input.read(output)?;
        self.owner.check_cursor_guard().map_err(io::Error::other)?;
        Ok(count)
    }
}
impl Seek for SaveCursor<'_, '_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.owner.check_cursor_guard().map_err(io::Error::other)?;
        let position = self.input.seek(from)?;
        self.owner.check_cursor_guard().map_err(io::Error::other)?;
        Ok(position)
    }
}
