//! A provisional native region becomes publishable only after every sample and
//! final cancellation check succeeds. Output retains its shared payload charge.
use crate::{
    large_document::{Error, PinnedBytes, ResidentBudget, ScratchBytes},
    raster::{backed::Region, project16, Pixel16},
};

pub(crate) fn native(
    region: Region,
    depth: u16,
    budget: &ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
    mut sample: impl FnMut(i32, i32) -> Result<Pixel16, String>,
) -> Result<PinnedBytes, String> {
    let stride = match depth {
        8 => 4,
        16 => 8,
        _ => {
            return Err(Error::Invalid("Regional output requires native depth 8 or 16").to_string())
        }
    };
    check()?;
    let length = region
        .pixels()
        .checked_mul(stride)
        .ok_or_else(|| Error::Budget.to_string())?;
    let mut output =
        ScratchBytes::new(budget, length, length).map_err(|error| error.to_string())?;
    let [left, top, right, bottom] = region.bounds();
    let mut cursor = 0;
    for y in top..bottom {
        check()?;
        for x in left..right {
            let pixel = sample(x, y)?;
            if depth == 16 {
                for channel in pixel {
                    output[cursor..cursor + 2].copy_from_slice(&channel.to_be_bytes());
                    cursor += 2;
                }
            } else {
                output[cursor..cursor + 4].copy_from_slice(&pixel.map(project16));
                cursor += 4;
            }
        }
    }
    check()?;
    Ok(output.freeze())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn native_low_bits_and_negative_coordinates_remain_exact_and_charged() {
        let budget = ResidentBudget::new(1024).unwrap();
        let bounds = Region::new([-2, -1, 3, 2]).unwrap();
        let sample = |x: i32, y: i32| [12347u16.wrapping_add(x as u16), 33559, y as u16, 65535];
        let output = native(bounds, 16, &budget, &mut || Ok(()), |x, y| Ok(sample(x, y))).unwrap();
        assert_eq!(budget.used(), 5 * 3 * 8);
        let expected: Vec<u8> = (-1..2)
            .flat_map(|y| {
                (-2..3).flat_map(move |x| sample(x, y).into_iter().flat_map(u16::to_be_bytes))
            })
            .collect();
        assert_eq!(&*output, &expected);
        let second = output.clone();
        drop(output);
        assert_eq!(budget.used(), expected.len());
        drop(second);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn sample_failure_or_late_cancel_returns_no_output_and_releases_charge() {
        let budget = ResidentBudget::new(1024).unwrap();
        let bounds = Region::new([0, 0, 5, 3]).unwrap();
        let calls = Cell::new(0);
        let result = native(bounds, 16, &budget, &mut || Ok(()), |_, _| {
            calls.set(calls.get() + 1);
            if calls.get() == 7 {
                Err(Error::Corrupt.to_string())
            } else {
                Ok([12347; 4])
            }
        });
        assert_eq!(result.err().unwrap(), Error::Corrupt.to_string());
        assert_eq!(budget.used(), 0);
        let mut check = || {
            if calls.get() == bounds.pixels() {
                Err(Error::Cancelled.to_string())
            } else {
                Ok(())
            }
        };
        calls.set(0);
        let result = native(bounds, 8, &budget, &mut check, |_, _| {
            calls.set(calls.get() + 1);
            Ok([12347; 4])
        });
        assert_eq!(result.err().unwrap(), Error::Cancelled.to_string());
        assert_eq!(calls.get(), bounds.pixels());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn insufficient_global_capacity_refuses_before_any_sampler_runs() {
        let budget = ResidentBudget::new(15).unwrap();
        let mut calls = 0;
        let result = native(
            Region::new([0, 0, 2, 2]).unwrap(),
            8,
            &budget,
            &mut || Ok(()),
            |_, _| {
                calls += 1;
                Ok([0; 4])
            },
        );
        assert_eq!(result.err().unwrap(), Error::Budget.to_string());
        assert_eq!(calls, 0);
        assert_eq!(budget.used(), 0);
    }
}
