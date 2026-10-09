//! Cancellable file progress and display-only saved-composite feedback.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
#[derive(Clone, Default)]
pub struct Control {
    canceled: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    source: Arc<Mutex<Option<(String, u64)>>>,
}
#[derive(Clone, Default)]
pub struct Status {
    pub stage: String,
    pub completed: usize,
    pub total: usize,
    pub preview: Option<(u32, u32, Vec<u8>)>,
}
impl Control {
    pub(crate) fn begin(&self) -> Result<(), String> {
        self.started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| "This opening request has already started".into())
    }
    pub fn bind(&self, document: String, revision: u64) {
        *self.source.lock().unwrap() = Some((document, revision));
    }
    pub(crate) fn source(&self) -> Option<(String, u64)> {
        self.source.lock().unwrap().clone()
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.canceled, &other.canceled)
    }
    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<(), String> {
        if self.canceled.load(Ordering::Acquire) {
            Err("Opening canceled · current work preserved".into())
        } else {
            Ok(())
        }
    }
    pub fn progress(&self, stage: &str, completed: usize, total: usize) -> Result<(), String> {
        self.check()?;
        let mut s = self
            .status
            .lock()
            .map_err(|_| "Loading status unavailable")?;
        s.stage = stage.into();
        s.completed = completed;
        s.total = total;
        Ok(())
    }
    pub fn status(&self) -> Status {
        let s = self.status.lock().unwrap();
        Status {
            stage: s.stage.clone(),
            completed: s.completed,
            total: s.total,
            preview: None,
        }
    }
    pub fn take_preview(&self) -> Option<(u32, u32, Vec<u8>)> {
        self.status.lock().unwrap().preview.take()
    }
    pub(crate) fn preview(
        &self,
        raster: &crate::raster::Raster,
        profile: Option<&[u8]>,
    ) -> Result<(), String> {
        self.check()?;
        let scale = (512. / raster.width.max(raster.height) as f64).min(1.);
        let w = (raster.width as f64 * scale).round().max(1.) as u32;
        let h = (raster.height as f64 * scale).round().max(1.) as u32;
        let profile = profile.filter(|p| crate::color_profile::supported(p).is_ok());
        let bytes = if raster.depth == 16 {
            let mut words = crate::render::rgba16(w, h, |x, y| {
                raster.get16((x as f64 / scale) as i32, (y as f64 / scale) as i32)
            });
            if let Some(profile) = profile {
                crate::color_profile::convert16(profile, &mut words)?;
            }
            words
                .chunks_exact(4)
                .flat_map(|p| crate::depth16::display_pixel(p.try_into().unwrap()))
                .collect()
        } else {
            let mut bytes = crate::render::rgba8(w, h, |x, y| {
                raster.get((x as f64 / scale) as i32, (y as f64 / scale) as i32)
            });
            if let Some(profile) = profile {
                crate::color_profile::convert8(profile, &mut bytes)?;
            }
            bytes
        };
        self.check()?;
        self.status.lock().unwrap().preview = Some((w, h, bytes));
        Ok(())
    }
}
