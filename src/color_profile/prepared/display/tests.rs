use super::*;
use crate::color_profile::{self, ColorProfile};
use std::sync::Arc;

fn pool() -> ResidentBudget {
    ResidentBudget::new(192 * 1024 * 1024).unwrap()
}
fn row8(pool: &ResidentBudget, values: &[u8]) -> ScratchBytes {
    let mut row = ScratchBytes::new(pool, values.len(), 16000 * 4).unwrap();
    row.copy_from_slice(values);
    row
}
fn row16(pool: &ResidentBudget, values: &[u16]) -> ChargedWords {
    let mut row = ChargedWords::new(values.len(), pool).unwrap();
    row.copy_from_slice(values);
    row
}

#[test]
fn supported_display_matches_legacy_native_rows_and_keeps_executor_admission_to_last_owner() {
    for profile in [
        color_profile::srgb_profile().to_vec(),
        ColorProfile::new_display_p3().encode().unwrap(),
    ] {
        let original = ring::digest::digest(&SHA256, &profile);
        let pool = pool();
        let plan =
            Arc::new(DisplayPlan::prepare(Some(&profile), false, &pool, &mut || Ok(())).unwrap());
        assert_eq!(plan.status(), DisplayStatus::Converted);
        assert_eq!(plan.warning(), None);
        assert_eq!(pool.used(), super::super::CMS_BYTES);
        let other = plan.clone();
        drop(plan);
        assert_eq!(pool.used(), super::super::CMS_BYTES);
        other
            .check_binding(Some(&profile), false, &pool, &mut || Ok(()))
            .unwrap();
        let bytes = [
            17, 83, 219, 0, 241, 59, 107, 1, 71, 19, 127, 128, 33, 67, 129, 255,
        ];
        let raw = row8(&pool, &bytes);
        let mut output = row8(&pool, &[0; 16]);
        let mut expected = bytes;
        color_profile::convert8(&profile, &mut expected).unwrap();
        other.convert8(&raw, &mut output, &mut || Ok(())).unwrap();
        assert_eq!(&*output, &expected);
        assert_eq!(&*raw, &bytes);
        let words = [
            12347, 33559, 51237, 0, 51237, 33559, 12347, 128, 17003, 33007, 51011, 129, 60317,
            12001, 23003, 45679,
        ];
        let raw16 = row16(&pool, &words);
        let mut output16 = row16(&pool, &[0; 16]);
        let mut expected16 = words;
        color_profile::convert16(&profile, &mut expected16).unwrap();
        other
            .convert16(&raw16, &mut output16, &mut || Ok(()))
            .unwrap();
        assert_eq!(&*output16, &expected16);
        assert_eq!(&*raw16, &words);
        for (converted, expected) in output16.chunks_exact(4).zip(expected16.chunks_exact(4)) {
            assert_eq!(
                crate::depth16::display_pixel(converted.try_into().unwrap()),
                crate::depth16::display_pixel(expected.try_into().unwrap())
            );
        }
        assert_eq!(
            crate::depth16::display_pixel(output16[..4].try_into().unwrap()),
            [0; 4]
        );
        assert_eq!(
            crate::depth16::display_pixel(output16[4..8].try_into().unwrap()),
            [0; 4]
        );
        assert_ne!(
            crate::depth16::display_pixel(output16[8..12].try_into().unwrap())[3],
            0
        );
        assert_eq!(
            ring::digest::digest(&SHA256, &profile).as_ref(),
            original.as_ref()
        );
        drop(raw);
        drop(output);
        drop(raw16);
        drop(output16);
        drop(other);
        assert_eq!(pool.used(), 0);
    }
}

#[test]
fn incompatible_profiles_produce_exact_raw_feedback_with_a_typed_warning() {
    let mut unsupported = ColorProfile::new_display_p3().encode().unwrap();
    unsupported[16..20].copy_from_slice(b"CMYK");
    for profile in [vec![0; 132], unsupported] {
        assert!(color_profile::supported(&profile).is_err());
        let pool = pool();
        let plan = DisplayPlan::prepare(Some(&profile), false, &pool, &mut || Ok(())).unwrap();
        assert_eq!(plan.status(), DisplayStatus::Unsupported);
        assert!(plan.warning().is_some());
        assert_eq!(pool.used(), 0);
        let input = row8(&pool, &[71, 19, 127, 0]);
        let mut output = row8(&pool, &[0; 4]);
        plan.convert8(&input, &mut output, &mut || Ok(())).unwrap();
        assert_eq!(&*input, &*output);
        let words = row16(&pool, &[12347, 33559, 51237, 129]);
        let mut converted = row16(&pool, &[0; 4]);
        plan.convert16(&words, &mut converted, &mut || Ok(()))
            .unwrap();
        assert_eq!(&*words, &*converted);
        drop(input);
        drop(output);
        drop(words);
        drop(converted);
        drop(plan);
        assert_eq!(pool.used(), 0);
    }
}

#[test]
fn masks_and_unprofiled_feedback_bypass_cms_but_keep_exact_binding_and_pool_checks() {
    let pool = ResidentBudget::new(4096).unwrap();
    for (profile, mask, status) in [
        (None, false, DisplayStatus::Unprofiled),
        (Some(&[0u8; 132][..]), true, DisplayStatus::Mask),
    ] {
        let plan = DisplayPlan::prepare(profile, mask, &pool, &mut || Ok(())).unwrap();
        assert_eq!(plan.status(), status);
        assert_eq!(plan.warning(), None);
        assert_eq!(pool.used(), 0);
        plan.check_binding(profile, mask, &pool, &mut || Ok(()))
            .unwrap();
        assert!(plan
            .check_binding(profile, !mask, &pool, &mut || Ok(()))
            .is_err());
        assert!(plan
            .check_binding(Some(&[7; 132]), mask, &pool, &mut || Ok(()))
            .is_err());
        let other = ResidentBudget::new(4096).unwrap();
        assert!(plan
            .check_binding(profile, mask, &other, &mut || Ok(()))
            .is_err());
        let input = row8(&pool, &[17, 83, 219, 0]);
        let mut output = row8(&pool, &[0; 4]);
        plan.convert8(&input, &mut output, &mut || Ok(())).unwrap();
        assert_eq!(&*input, &*output);
        let mut foreign = row8(&other, &[0; 4]);
        assert!(plan.convert8(&input, &mut foreign, &mut || Ok(())).is_err());
        assert!(plan
            .convert8(&foreign, &mut output, &mut || Ok(()))
            .is_err());
        drop(input);
        drop(output);
        drop(foreign);
        drop(plan);
        assert_eq!(pool.used(), 0);
        assert_eq!(other.used(), 0);
    }
}

#[test]
fn cancellation_and_admission_failure_never_masquerade_as_unsupported_profile() {
    let profile = ColorProfile::new_display_p3().encode().unwrap();
    let pool = pool();
    for stop in [1, 5, 6, 7, 8] {
        let mut checks = 0;
        let reason = DisplayPlan::prepare(Some(&profile), false, &pool, &mut || {
            checks += 1;
            if checks == stop {
                Err("unsupported is just a cancellation message".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert_eq!(reason, "unsupported is just a cancellation message");
        assert_eq!(checks, stop);
        assert_eq!(pool.used(), 0);
    }
    let small = ResidentBudget::new(127 * 1024 * 1024).unwrap();
    assert!(DisplayPlan::prepare(Some(&profile), false, &small, &mut || Ok(())).is_err());
    assert_eq!(small.used(), 0);
    // A malformed profile plus a failed admission is still a failed operation.
    assert!(DisplayPlan::prepare(Some(&[0; 132]), false, &small, &mut || Ok(())).is_err());
    assert_eq!(small.used(), 0);
    let plan = DisplayPlan::prepare(Some(&profile), false, &pool, &mut || Ok(())).unwrap();
    let raw = row16(&pool, &[12347, 33559, 51237, 45679]);
    let mut out = row16(&pool, &[0; 4]);
    let mut checks = 0;
    assert_eq!(
        plan.convert16(&raw, &mut out, &mut || {
            checks += 1;
            if checks == 2 {
                Err("cancel after row conversion".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err(),
        "cancel after row conversion"
    );
    drop(raw);
    drop(out);
    drop(plan);
    assert_eq!(pool.used(), 0);
}

#[test]
fn exact_profile_bytes_and_row_shape_are_bound_without_a_global_source_cache() {
    let profile = ColorProfile::new_display_p3().encode().unwrap();
    let pool = pool();
    let plan = DisplayPlan::prepare(Some(&profile), false, &pool, &mut || Ok(())).unwrap();
    let mut changed = profile.clone();
    changed[24] ^= 1;
    assert!(plan
        .check_binding(Some(&changed), false, &pool, &mut || Ok(()))
        .is_err());
    assert!(plan
        .check_binding(None, false, &pool, &mut || Ok(()))
        .is_err());
    let raw = row8(&pool, &[1, 2, 3, 4, 5, 6, 7, 8]);
    let mut short = row8(&pool, &[0; 4]);
    assert!(plan.convert8(&raw, &mut short, &mut || Ok(())).is_err());
    let malformed = row8(&pool, &[1, 2, 3]);
    let mut out = row8(&pool, &[0; 3]);
    assert!(plan.convert8(&malformed, &mut out, &mut || Ok(())).is_err());
    drop(raw);
    drop(short);
    drop(malformed);
    drop(out);
    drop(plan);
    assert_eq!(pool.used(), 0);
}

#[test]
fn typed_native_allocation_and_lane_errors_are_operational_failures() {
    use super::super::PreparationFailure;
    use moxcms::CmsError;
    for error in [
        CmsError::OutOfMemory(4096),
        CmsError::LaneSizeMismatch,
        CmsError::LaneMultipleOfChannels,
        CmsError::InvalidLayout,
        CmsError::InvalidRenderingIntent,
        CmsError::OverflowingError,
    ] {
        assert!(matches!(
            PreparationFailure::native(error),
            PreparationFailure::Failed(_)
        ));
    }
    for error in [
        CmsError::InvalidProfile,
        CmsError::UnsupportedColorPrimaries(255),
        CmsError::UnsupportedProfileConnection,
    ] {
        assert!(matches!(
            PreparationFailure::native(error),
            PreparationFailure::Unsupported(_)
        ));
    }
}

#[test]
fn oversized_source_profile_is_refused_before_hash_or_executor_admission() {
    let profile = vec![0; color_profile::MAX_PROFILE_BYTES + 1];
    let pool = ResidentBudget::new(4096).unwrap();
    for mask in [false, true] {
        let mut checks = 0;
        let reason = DisplayPlan::prepare(Some(&profile), mask, &pool, &mut || {
            checks += 1;
            Ok(())
        })
        .err()
        .unwrap();
        assert_eq!(
            reason,
            "Display profile exceeds the captured4MiB source bound"
        );
        assert_eq!(checks, 1);
        assert_eq!(pool.used(), 0);
    }
}
