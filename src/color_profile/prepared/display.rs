//! A captured display conversion never changes editable channels or the ICC tag.
use super::{ChargedWords, PreparationFailure, ResidentBudget, ScratchBytes, UncachedTransforms};
use ring::digest::{Context, SHA256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisplayStatus {
    Unprofiled,
    Mask,
    Converted,
    Unsupported,
}
impl DisplayStatus {
    pub(crate) fn warning(self) -> Option<&'static str> {
        (self == Self::Unsupported).then_some(
            "The embedded color profile is unsupported; the preview displays original RGB values.",
        )
    }
}
enum Conversion {
    Raw,
    Cms(UncachedTransforms),
}
pub(crate) struct DisplayPlan {
    conversion: Conversion,
    status: DisplayStatus,
    profile: Option<[u8; 32]>,
    pool: ResidentBudget,
}
impl DisplayPlan {
    pub(crate) fn prepare(
        profile: Option<&[u8]>,
        mask: bool,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        let binding = profile_binding(profile, check)?;
        let (conversion, status) = if mask {
            (Conversion::Raw, DisplayStatus::Mask)
        } else if let Some(profile) = profile {
            // Preview historically executes CMS even for canonical sRGB. Its
            // native16 rounding differs from an identity copy in the low bits.
            match UncachedTransforms::classified(profile, pool, check, false) {
                Ok(cms) => (Conversion::Cms(cms), DisplayStatus::Converted),
                Err(PreparationFailure::Unsupported(_)) => {
                    // Only an incompatible profile permits legacy raw feedback.
                    // Admission, cancellation and owner checks remain failures.
                    check()?;
                    (Conversion::Raw, DisplayStatus::Unsupported)
                }
                Err(PreparationFailure::Failed(reason)) => return Err(reason),
            }
        } else {
            (Conversion::Raw, DisplayStatus::Unprofiled)
        };
        check()?;
        Ok(Self {
            conversion,
            status,
            profile: binding,
            pool: pool.clone(),
        })
    }
    pub(crate) fn status(&self) -> DisplayStatus {
        self.status
    }
    pub(crate) fn profile_binding(&self) -> Option<[u8; 32]> {
        self.profile
    }
    pub(crate) fn warning(&self) -> Option<&'static str> {
        self.status.warning()
    }
    pub(crate) fn check_binding(
        &self,
        profile: Option<&[u8]>,
        mask: bool,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if !self.pool.same_pool(pool)
            || mask != (self.status == DisplayStatus::Mask)
            || profile_binding(profile, check)? != self.profile
        {
            return Err("Display profile or admission differs from its captured plan".into());
        }
        check()
    }
    pub(crate) fn convert8(
        &self,
        input: &ScratchBytes,
        output: &mut ScratchBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if !input.uses_budget(&self.pool)
            || !output.uses_budget(&self.pool)
            || input.len() != output.len()
            || input.len() % 4 != 0
            || input.len() > 16000 * 4
        {
            return Err("Display RGBA8 row or admission differs".into());
        }
        match &self.conversion {
            Conversion::Cms(cms) => cms.convert8(input, output)?,
            Conversion::Raw => output.copy_from_slice(input),
        }
        check()
    }
    pub(crate) fn convert16(
        &self,
        input: &ChargedWords,
        output: &mut ChargedWords,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if !input.uses_budget(&self.pool)
            || !output.uses_budget(&self.pool)
            || input.len() != output.len()
            || input.len() % 4 != 0
            || input.len() > 16000 * 4
        {
            return Err("Display RGBA16 row or admission differs".into());
        }
        match &self.conversion {
            Conversion::Cms(cms) => cms.convert16(input, output)?,
            Conversion::Raw => output.copy_from_slice(input),
        }
        check()
    }
}
pub(crate) fn profile_binding(
    profile: Option<&[u8]>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Option<[u8; 32]>, String> {
    check()?;
    if profile.is_some_and(|bytes| bytes.len() > super::super::MAX_PROFILE_BYTES) {
        return Err("Display profile exceeds the captured4MiB source bound".into());
    }
    profile
        .map(|bytes| {
            let mut hash = Context::new(&SHA256);
            for chunk in bytes.chunks(64 * 1024) {
                check()?;
                hash.update(chunk);
            }
            check()?;
            Ok(hash.finish().as_ref().try_into().unwrap())
        })
        .transpose()
}

#[cfg(test)]
#[path = "display/tests.rs"]
mod tests;
