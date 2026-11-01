//! Uncached CMS ownership for workers using the shared resident admission pool.
use super::{
    parse, ColorProfile, Layout, RenderingIntent, TransformOptions, Transforms, MAX_PROFILE_BYTES,
};
use crate::large_document::{Reservation, ResidentBudget, ScratchBytes};
use std::ops::{Deref, DerefMut};

/// Conservative pinned moxcms0.8.1 envelope:4MiB input/derived profile,
/// <=8MiB cumulative decoded text, six classic LUTs plus bounded auxiliary
/// gamut tables,65,536-entry
/// curves, both8/16 executors and their construction intermediates. Kept until
/// the final executor owner drops; never lent to the presentation cache.
const CMS_BYTES: usize = 128 * 1024 * 1024;
const TEXT_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct UncachedTransforms {
    transforms: Option<Transforms>,
    pool: ResidentBudget,
    _charges: [Reservation; 7],
}
impl UncachedTransforms {
    pub(crate) fn new(
        profile: &[u8],
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if profile.len() > MAX_PROFILE_BYTES {
            return Err("ICC profile exceeds the4MiB limit".into());
        }
        // Separate allocation groups, not one >64MiB buffer. Keep every group
        // until all executor objects are destroyed; partial admission unwinds.
        let reserve = |mib: usize| {
            pool.reserve_bytes(mib * 1024 * 1024, mib * 1024 * 1024)
                .map_err(|e| e.to_string())
        };
        let charges = [
            reserve(8)?,
            reserve(16)?,
            reserve(8)?,
            reserve(16)?,
            reserve(48)?,
            reserve(8)?,
            reserve(24)?,
        ];
        metadata_bound(profile)?;
        let source = parse(profile)?;
        check()?;
        let mut target = ColorProfile::new_srgb();
        target.cicp = None;
        let canonical = target.encode().map_err(|e| e.to_string())?;
        // Same immutable tag comparison as the legacy sRGB bypass, without
        // initializing or retaining a process-global profile/transform cache.
        if profile.len() == canonical.len()
            && profile[..24] == canonical[..24]
            && profile[36..] == canonical[36..]
        {
            check()?;
            return Ok(Self {
                transforms: None,
                pool: pool.clone(),
                _charges: charges,
            });
        }
        let options = TransformOptions {
            rendering_intent: if source.lut_a_to_b_colorimetric.is_none()
                && source.lut_a_to_b_perceptual.is_some()
            {
                RenderingIntent::Perceptual
            } else {
                RenderingIntent::RelativeColorimetric
            },
            prefer_fixed_point: false,
            allow_use_cicp_transfer: false,
            ..Default::default()
        };
        let bytes = source
            .create_transform_8bit(Layout::Rgba, &target, Layout::Rgba, options)
            .map_err(|e| e.to_string())?;
        check()?;
        let words = source
            .create_transform_16bit(Layout::Rgba, &target, Layout::Rgba, options)
            .map_err(|e| e.to_string())?;
        check()?;
        Ok(Self {
            transforms: Some(Transforms { bytes, words }),
            pool: pool.clone(),
            _charges: charges,
        })
    }
    pub(crate) fn convert8(&self, input: &[u8], output: &mut ScratchBytes) -> Result<(), String> {
        if input.len() % 4 != 0
            || input.len() > 16000 * 4
            || input.len() != output.len()
            || !output.uses_budget(&self.pool)
        {
            return Err("Prepared CMS RGBA8 length or pool differs".into());
        }
        if let Some(transforms) = &self.transforms {
            transforms
                .bytes
                .transform(input, output)
                .map_err(|e| e.to_string())?;
        } else {
            output.copy_from_slice(input);
        }
        for (source, target) in input.chunks_exact(4).zip(output.chunks_exact_mut(4)) {
            target[3] = source[3];
        }
        Ok(())
    }
    pub(crate) fn convert16(
        &self,
        input: &ChargedWords,
        output: &mut ChargedWords,
    ) -> Result<(), String> {
        if input.len() % 4 != 0
            || input.len() > 16000 * 4
            || input.len() != output.len()
            || !input.uses_budget(&self.pool)
            || !output.uses_budget(&self.pool)
        {
            return Err("Prepared CMS RGBA16 length or pool differs".into());
        }
        if let Some(transforms) = &self.transforms {
            transforms
                .words
                .transform(input, output)
                .map_err(|e| e.to_string())?;
        } else {
            output.copy_from_slice(input);
        }
        for (source, target) in input.chunks_exact(4).zip(output.chunks_exact_mut(4)) {
            target[3] = source[3];
        }
        Ok(())
    }
}

/// Aligned native words, including their construction capacity, remain charged
/// until the allocation drops. The underlying vector never escapes this owner.
pub(crate) struct ChargedWords {
    words: Vec<u16>,
    pool: ResidentBudget,
    _charge: Reservation,
}
impl ChargedWords {
    pub(crate) fn new(length: usize, pool: &ResidentBudget) -> Result<Self, String> {
        if length == 0 || length > 16000 * 4 {
            return Err("Prepared CMS row exceeds the native bound".into());
        }
        let capacity_bytes = length.checked_mul(4).ok_or("Prepared CMS row overflow")?;
        let charge = pool
            .reserve_bytes(capacity_bytes, capacity_bytes)
            .map_err(|e| e.to_string())?;
        let mut words = Vec::new();
        words
            .try_reserve_exact(length)
            .map_err(|_| "Prepared CMS row allocation failed")?;
        if words.capacity() > capacity_bytes / 2 {
            return Err("Prepared CMS allocator capacity exceeds admission".into());
        }
        words.resize(length, 0);
        Ok(Self {
            words,
            pool: pool.clone(),
            _charge: charge,
        })
    }
    fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.pool.same_pool(pool)
    }
}
impl Deref for ChargedWords {
    type Target = [u16];
    fn deref(&self) -> &[u16] {
        &self.words
    }
}
impl DerefMut for ChargedWords {
    fn deref_mut(&mut self) -> &mut [u16] {
        &mut self.words
    }
}

/// Bound repeated/overlapping ICC localized strings BEFORE moxcms decodes them.
/// Raw profile size alone does not bound repeated references in an mluc table.
fn metadata_bound(profile: &[u8]) -> Result<(), String> {
    if profile.len() < 132 || profile.len() > MAX_PROFILE_BYTES {
        return Err("Invalid bounded ICC profile size".into());
    }
    let count = super::number(profile, 128)?;
    if count > 128 || 132 + count * 12 > profile.len() {
        return Err("Invalid bounded ICC tag table".into());
    }
    let mut cost = 0usize;
    for tag in profile[132..132 + count * 12].chunks_exact(12) {
        let offset = super::number(tag, 4)?;
        let length = super::number(tag, 8)?;
        let data = profile
            .get(
                offset
                    ..offset
                        .checked_add(length)
                        .ok_or("ICC tag offset overflow")?,
            )
            .ok_or("Invalid bounded ICC tag")?;
        if matches!(
            &tag[..4],
            b"A2B0" | b"A2B1" | b"A2B2" | b"B2A0" | b"B2A1" | b"B2A2"
        ) {
            super::classic_lut(data)?;
        }
        if &tag[..4] == b"gamt"
            && matches!(
                data.get(..4),
                Some(b"mAB ") | Some(b"mBA ") | Some(b"mft1") | Some(b"mft2")
            )
        {
            if data.get(8).is_none_or(|v| !(1..=16).contains(v))
                || data.get(9).is_none_or(|v| !(1..=16).contains(v))
            {
                return Err("Auxiliary ICC gamut channels exceed the worker bound".into());
            }
        }
        if !matches!(
            &tag[..4],
            b"cprt" | b"desc" | b"vued" | b"dmnd" | b"dmdd" | b"targ"
        ) {
            continue;
        }
        let offset = super::number(tag, 4)?;
        let length = super::number(tag, 8)?;
        let data = profile
            .get(
                offset
                    ..offset
                        .checked_add(length)
                        .ok_or("ICC text offset overflow")?,
            )
            .ok_or("Invalid bounded ICC text tag")?;
        if data.get(..4) == Some(b"mluc") {
            let records = super::number(data, 8)?;
            let end = records
                .checked_mul(12)
                .and_then(|v| v.checked_add(16))
                .ok_or("ICC text table overflow")?;
            if records == 0 || end > data.len() {
                return Err("Invalid bounded ICC localization table".into());
            }
            for record in data[16..end].chunks_exact(12) {
                let bytes = super::number(record, 4)?;
                let at = super::number(record, 8)?;
                if at.checked_add(bytes).is_none_or(|v| v > data.len()) {
                    return Err("Invalid bounded ICC localization string".into());
                }
                cost = cost
                    .checked_add(
                        bytes
                            .checked_mul(8)
                            .and_then(|v| v.checked_add(256))
                            .ok_or("ICC text budget overflow")?,
                    )
                    .ok_or("ICC text budget overflow")?;
                if cost > TEXT_BYTES {
                    return Err("ICC text exceeds the bounded worker memory envelope".into());
                }
            }
        } else {
            cost = cost
                .checked_add(length.checked_mul(8).ok_or("ICC text budget overflow")?)
                .ok_or("ICC text budget overflow")?;
            if cost > TEXT_BYTES {
                return Err("ICC text exceeds the bounded worker memory envelope".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncached_conversion_matches_existing_native_math_and_holds_charge_to_last_owner() {
        let profile = ColorProfile::new_display_p3().encode().unwrap();
        let pool = ResidentBudget::new(192 * 1024 * 1024).unwrap();
        let owner =
            std::sync::Arc::new(UncachedTransforms::new(&profile, &pool, &mut || Ok(())).unwrap());
        assert_eq!(pool.used(), CMS_BYTES);
        let clone = owner.clone();
        drop(owner);
        assert_eq!(pool.used(), CMS_BYTES);
        let input8 = [17, 83, 219, 127, 241, 59, 107, 0];
        let mut expected8 = input8;
        super::super::convert8(&profile, &mut expected8).unwrap();
        let mut output8 = ScratchBytes::new(&pool, 8, 8).unwrap();
        clone.convert8(&input8, &mut output8).unwrap();
        assert_eq!(&*output8, &expected8);
        let mut input16 = ChargedWords::new(8, &pool).unwrap();
        input16.copy_from_slice(&[12347, 33559, 51237, 45679, 51237, 33559, 12347, 0]);
        let mut expected16 = input16.to_vec();
        super::super::convert16(&profile, &mut expected16).unwrap();
        let mut output16 = ChargedWords::new(8, &pool).unwrap();
        clone.convert16(&input16, &mut output16).unwrap();
        assert_eq!(&*output16, &expected16);
        drop(output8);
        drop(output16);
        drop(input16);
        drop(clone);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn invalid_profile_cancel_foreign_pool_and_refusal_release_owned_work() {
        let pool = ResidentBudget::new(192 * 1024 * 1024).unwrap();
        assert!(UncachedTransforms::new(&[0; 132], &pool, &mut || Ok(())).is_err());
        assert_eq!(pool.used(), 0);
        let profile = ColorProfile::new_display_p3().encode().unwrap();
        let mut checks = 0;
        assert!(UncachedTransforms::new(&profile, &pool, &mut || {
            checks += 1;
            if checks > 1 {
                Err("Cancelled CMS".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(pool.used(), 0);
        let small = ResidentBudget::new(1024).unwrap();
        assert!(UncachedTransforms::new(&profile, &small, &mut || Ok(())).is_err());
        assert_eq!(small.used(), 0);
        let owner = UncachedTransforms::new(&profile, &pool, &mut || Ok(())).unwrap();
        let mut output = ScratchBytes::new(&small, 4, 4).unwrap();
        assert!(owner.convert8(&[17, 83, 219, 127], &mut output).is_err());
        drop(output);
        drop(owner);
        assert_eq!(pool.used(), 0);
        assert_eq!(small.used(), 0);
    }
    #[test]
    fn repeated_icc_localizations_cannot_amplify_small_profile_into_unbounded_strings() {
        let mut profile = vec![0; 132 + 12 + 16384];
        profile[128..132].copy_from_slice(&1u32.to_be_bytes());
        profile[132..136].copy_from_slice(b"desc");
        profile[136..140].copy_from_slice(&144u32.to_be_bytes());
        profile[140..144].copy_from_slice(&16384u32.to_be_bytes());
        profile[144..148].copy_from_slice(b"mluc");
        profile[152..156].copy_from_slice(&128u32.to_be_bytes());
        profile[156..160].copy_from_slice(&12u32.to_be_bytes());
        for record in profile[160..160 + 512 * 12].chunks_exact_mut(12) {
            record[4..8].copy_from_slice(&2048u32.to_be_bytes());
            record[8..12].copy_from_slice(&8192u32.to_be_bytes());
        }
        //128*2048*8 is below8MiB, then increase repeats while retaining a bounded table.
        assert!(metadata_bound(&profile).is_ok());
        profile[152..156].copy_from_slice(&512u32.to_be_bytes());
        assert!(metadata_bound(&profile).is_err());
    }
}
