//! Advance native presentation only after the previous frame was published.
#[derive(Default)]
pub(super) struct Clock {
    step: u8,
    signature: Option<[u8; 32]>,
    displayed: Option<f64>,
    last: Option<([u8; 32], f64)>,
}
impl Clock {
    pub(super) fn fraction(&mut self, time: f64) -> f32 {
        if self
            .displayed
            .is_some_and(|shown| time >= shown + super::DURATION / 12.)
        {
            self.step = self.step.saturating_add(1).min(12);
            self.signature = None;
            self.displayed = None;
        }
        f32::from(self.step) / 12.
    }
    pub(super) fn captured(&mut self, signature: [u8; 32]) {
        self.signature = Some(signature);
        self.displayed = self
            .last
            .filter(|(shown, _)| *shown == signature)
            .map(|(_, time)| time);
    }
    pub(super) fn presented(&mut self, signature: [u8; 32], time: f64) {
        if time.is_finite() && self.signature == Some(signature) && self.displayed.is_none() {
            self.displayed = Some(time);
            self.last = Some((signature, time));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_pending_failed_and_unrelated_frames_cannot_run_the_clock_ahead() {
        let mut clock = Clock::default();
        assert_eq!(clock.fraction(1.), 0.);
        clock.captured([1; 32]);
        assert_eq!(
            clock.fraction(2.),
            0.,
            "Pending work keeps its exact request"
        );
        clock.presented([2; 32], 2.);
        assert_eq!(clock.fraction(3.), 0.);
        clock.presented([1; 32], f64::NAN);
        assert_eq!(clock.fraction(3.), 0.);
        for step in 0..12 {
            let time = 4. + f64::from(step);
            clock.captured([step + 1; 32]);
            clock.presented([step + 1; 32], time);
            assert_eq!(clock.fraction(time), f32::from(step) / 12.);
            // Repeated callbacks cannot postpone progress or acknowledge a later step.
            clock.presented([step + 1; 32], time + 0.02);
            assert_eq!(clock.fraction(time + 0.04), f32::from(step + 1) / 12.);
            assert_eq!(clock.fraction(time + 0.5), f32::from(step + 1) / 12.);
        }
    }
    #[test]
    fn quantized_identical_pixels_can_reuse_the_published_frame_but_new_views_wait() {
        let mut clock = Clock::default();
        clock.captured([1; 32]);
        clock.presented([1; 32], 1.);
        assert_eq!(clock.fraction(1.04), 1. / 12.);
        clock.captured([1; 32]);
        assert_eq!(clock.fraction(1.08), 2. / 12.);
        clock.captured([2; 32]);
        assert_eq!(clock.fraction(2.), 2. / 12.);
        clock.presented([1; 32], 2.);
        assert_eq!(clock.fraction(2.1), 2. / 12.);
        clock.presented([2; 32], 2.1);
        assert_eq!(clock.fraction(2.14), 3. / 12.);
    }
}
