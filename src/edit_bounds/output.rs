//! One source-session-owned provisional output plane. The coordinator owns
//! atomic multi-plane commands, metadata changes, Engine guards and history.
use super::input::{InputBinding, InputTile, RasterInput};
use crate::{
    engine::native::{NativeDocument, OutputDraft, RefusedOutput, StagedOutput},
    large_document::{
        BlockRef, Error, FreshFragment, PinnedBytes, Reservation, ScratchBytes, TileSpec,
    },
    raster::{surface::TilePatch, Surface, TILE},
    workspace::source_runtime::SourceSession,
};
use std::collections::BTreeMap;
#[path = "output/frame.rs"]
mod frame;
pub(crate) use frame::{stage_frame, stage_frame_with};

enum Replacement {
    Zero,
    Stored(BlockRef),
}
pub(crate) struct TileOutput<'d, 's> {
    base: &'d NativeDocument,
    source: &'d Surface,
    binding: InputBinding,
    session: &'s SourceSession,
    replacements: BTreeMap<(u32, u32), Replacement>,
    fragment: Option<FreshFragment>,
    poisoned: bool,
    // Covers mutable tree, final patch vector and fragment-vector overlap.
    _charge: Reservation,
}
#[must_use]
pub(crate) struct RefusedTiles<'d, 's> {
    reason: String,
    builder: TileOutput<'d, 's>,
}
impl RefusedTiles<'_, '_> {
    pub(crate) fn reason(&self) -> &str {
        &self.reason
    }
    pub(crate) fn abort(self) -> Result<(), String> {
        self.builder.abort()
    }
}
pub(crate) enum BuiltTiles<'d, 's> {
    Unchanged,
    Draft(TileDraft<'d, 's>),
}
pub(crate) struct TileDraft<'d, 's> {
    draft: OutputDraft,
    base: &'d NativeDocument,
    session: &'s SourceSession,
    _charge: Reservation,
}
#[must_use]
pub(crate) struct RefusedTileSeal<'s> {
    reason: String,
    refused: RefusedOutput,
    session: &'s SourceSession,
    _charge: Reservation,
}
impl RefusedTileSeal<'_> {
    pub(crate) fn reason(&self) -> &str {
        &self.reason
    }
    pub(crate) fn abort(self) -> Result<(), String> {
        let Self {
            refused,
            session,
            _charge,
            ..
        } = self;
        let result = refused.into_draft().abort(session, &|| Ok(()));
        drop(_charge);
        result
    }
}
impl<'d, 's> TileDraft<'d, 's> {
    pub(crate) fn seal(
        self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<StagedOutput, RefusedTileSeal<'s>> {
        let Self {
            draft,
            base,
            session,
            _charge,
        } = self;
        match draft.seal(base, session, check) {
            Ok(output) => {
                drop(_charge);
                Ok(output)
            }
            Err(refused) => Err(RefusedTileSeal {
                reason: refused.reason().to_owned(),
                refused,
                session,
                _charge,
            }),
        }
    }
    pub(crate) fn abort(self) -> Result<(), String> {
        let Self {
            draft,
            session,
            _charge,
            ..
        } = self;
        let result = draft.abort(session, &|| Ok(()));
        drop(_charge);
        result
    }
}
impl<'d, 's> TileOutput<'d, 's> {
    pub(crate) fn new(input: &RasterInput<'d>, session: &'s SourceSession) -> Result<Self, String> {
        session.check()?;
        let base = input.document();
        if base.metadata().read_only()
            || !base.budget().same_pool(&session.resident())
            || !base.disk_budget().same_pool(&session.disk())
        {
            return Err(
                "Native output requires the editable captured document and actual session pools"
                    .into(),
            );
        }
        let shape = input.shape();
        let count = (shape[0].div_ceil(TILE) as usize)
            .checked_mul(shape[1].div_ceil(TILE) as usize)
            .ok_or("Native output index footprint overflow")?;
        let bytes = count
            .checked_mul(512 + 2 * std::mem::size_of::<TilePatch>())
            .and_then(|n| n.checked_add(4096))
            .ok_or("Native output index footprint overflow")?;
        let charge = base
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let binding = input.binding();
        Ok(Self {
            base,
            source: base.source(binding.slot())?,
            binding,
            session,
            replacements: BTreeMap::new(),
            fragment: None,
            poisoned: false,
            _charge: charge,
        })
    }
    /// All file I/O is explicit worker staging. The unforgeable input token
    /// binds comparison to the same immutable generation/full padded tile.
    pub(crate) fn replace_tile(
        &mut self,
        before: &InputTile,
        output: PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.poisoned {
            return Err("Native output was refused and cannot publish partial work".into());
        }
        let result = self.replace_inner(before, output, check);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn replace_inner(
        &mut self,
        before: &InputTile,
        output: PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.session.check()?;
        check()?;
        let spec = TileSpec::new(TILE as u16, TILE as u16, self.binding.depth())
            .map_err(|e| e.to_string())?;
        if before.binding() != self.binding
            || !output.uses_budget(self.base.budget())
            || output.len() != spec.bytes()
            || before.bytes().len() != spec.bytes()
        {
            return Err("Native output tile source generation, frame or byte owner differs".into());
        }
        let mut same = true;
        let mut zero = true;
        for (old, new) in before
            .bytes()
            .chunks(64 * 1024)
            .zip(output.chunks(64 * 1024))
        {
            check()?;
            same &= old == new;
            zero &= new.iter().all(|b| *b == 0);
        }
        if same {
            self.replacements.remove(&before.key());
            return Ok(());
        }
        let replacement = if zero {
            Replacement::Zero
        } else {
            if self.fragment.is_none() {
                self.fragment = Some(self.session.fresh_fragment()?);
            }
            let reference = self
                .fragment
                .as_ref()
                .unwrap()
                .store()
                .put(spec, &output, &mut || {
                    self.session.check().map_err(|_| Error::Changed)?;
                    check().map_err(|_| Error::Cancelled)
                })
                .map_err(|e| e.to_string())?;
            Replacement::Stored(reference)
        };
        self.session.check()?;
        check()?;
        self.replacements.insert(before.key(), replacement);
        Ok(())
    }
    fn candidate(&self, check: &mut dyn FnMut() -> Result<(), String>) -> Result<Surface, String> {
        if self.poisoned {
            return Err("Native output was refused and cannot publish partial work".into());
        }
        self.session.check()?;
        check()?;
        let mut patches = Vec::new();
        patches
            .try_reserve_exact(self.replacements.len())
            .map_err(|_| "Native patch-vector allocation failed")?;
        if patches.capacity() > self.replacements.len() {
            return Err("Native patch-vector allocator exceeded admission".into());
        }
        let spec = TileSpec::new(TILE as u16, TILE as u16, self.binding.depth())
            .map_err(|e| e.to_string())?;
        let zero = if self
            .replacements
            .values()
            .any(|v| matches!(v, Replacement::Zero))
        {
            Some(
                ScratchBytes::new(self.base.budget(), spec.bytes(), spec.bytes())
                    .map_err(|e| e.to_string())?
                    .freeze(),
            )
        } else {
            None
        };
        for (&key, replacement) in &self.replacements {
            check()?;
            patches.push(match replacement {
                Replacement::Zero => {
                    TilePatch::zero(key, spec, zero.as_ref().unwrap(), self.base.budget())
                        .map_err(|e| e.to_string())?
                }
                Replacement::Stored(reference) => TilePatch::stored(
                    key,
                    self.fragment
                        .as_ref()
                        .ok_or("Native fragment is missing")?
                        .store()
                        .clone(),
                    *reference,
                ),
            });
        }
        let result = self
            .source
            .with_tile_patches(&patches, self.base.budget(), self.base.disk_budget())
            .map_err(|e| e.to_string())?;
        self.session.check()?;
        check()?;
        Ok(result)
    }
    pub(crate) fn finish(
        self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<BuiltTiles<'d, 's>, RefusedTiles<'d, 's>> {
        self.finish_inner(false, check)
    }
    /// A semantic fill→paint transition needs a paired complete output even
    /// when its rendered bytes are unchanged. Ordinary edits use finish().
    pub(crate) fn finish_for_metadata(
        self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<BuiltTiles<'d, 's>, RefusedTiles<'d, 's>> {
        self.finish_inner(true, check)
    }
    fn finish_inner(
        mut self,
        required_output: bool,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<BuiltTiles<'d, 's>, RefusedTiles<'d, 's>> {
        let mut surface = match self.candidate(check) {
            Ok(surface) => surface,
            Err(reason) => {
                return Err(RefusedTiles {
                    reason,
                    builder: self,
                })
            }
        };
        if self.replacements.is_empty() || surface.is_known_zero() {
            if let Some(fragment) = self.fragment.take() {
                if let Err(error) = self.session.abort_fragment(fragment, &mut || Ok(())) {
                    return Err(RefusedTiles {
                        reason: format!(
                            "Native unused-fragment cleanup retained inventory: {error}"
                        ),
                        builder: self,
                    });
                }
            }
            if self.replacements.is_empty() && !required_output {
                return Ok(BuiltTiles::Unchanged);
            }
        }
        if required_output && surface.physical_tiles().next().is_none() {
            surface = match Surface::zero(
                self.binding.shape()[0],
                self.binding.shape()[1],
                self.binding.depth(),
            ) {
                Ok(surface) => surface,
                Err(error) => {
                    return Err(RefusedTiles {
                        reason: error.to_string(),
                        builder: self,
                    })
                }
            };
        }
        let mut fragments = Vec::new();
        if self.fragment.is_some() {
            if fragments.try_reserve_exact(1).is_err() || fragments.capacity() != 1 {
                return Err(RefusedTiles {
                    reason: "Native fragment owner allocation refused".into(),
                    builder: self,
                });
            }
            fragments.push(self.fragment.take().unwrap());
        }
        let Self {
            base,
            session,
            replacements,
            _charge,
            ..
        } = self;
        drop(replacements);
        Ok(BuiltTiles::Draft(TileDraft {
            draft: OutputDraft::new(surface, fragments),
            base,
            session,
            _charge,
        }))
    }
    pub(crate) fn abort(self) -> Result<(), String> {
        let Self {
            session,
            replacements,
            fragment,
            _charge,
            ..
        } = self;
        drop(replacements);
        let result = fragment.map_or(Ok(()), |fragment| {
            session.abort_fragment(fragment, &mut || Ok(()))
        });
        drop(_charge);
        result
    }
}
