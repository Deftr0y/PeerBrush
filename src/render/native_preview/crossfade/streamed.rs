//! Paired shared-engine renders with one queued before-stripe, never full frames.
use super::*;
use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

pub(crate) struct Streamed {
    before: Request,
    after: Request,
    tag: Tag,
    origin: Arc<Origin>,
    fraction: f32,
}
impl Blend {
    pub(crate) fn streamed(
        &self,
        before: Request,
        after: Request,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Streamed> {
        check()?;
        self.validate_tags(&before.tag(), &after.tag())?;
        let (tag, origin) = self.output_tag(&after.tag())?;
        before.check()?;
        after.check()?;
        check()?;
        Ok(Streamed {
            before,
            after,
            tag,
            origin,
            fraction: self.fraction,
        })
    }
}
impl Streamed {
    pub(crate) fn origin(&self) -> Arc<Origin> {
        self.origin.clone()
    }
}
struct Stop<'a>(&'a AtomicBool);
impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
struct Done(mpsc::SyncSender<()>);
impl Drop for Done {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
impl DisplayRequest for Streamed {
    fn key(&self) -> &Key {
        self.tag.key()
    }
    fn tag(&self) -> Tag {
        self.tag.clone()
    }
    fn budget(&self) -> &ResidentBudget {
        &self.tag.0.pool
    }
    fn check(&self) -> Result<()> {
        self.before.check()?;
        self.after.check()
    }
    fn check_pinned_work(&self) -> Result<()> {
        self.before.check_pinned_work()?;
        self.after.check_pinned_work()
    }
    fn render_into(
        &self,
        check: &mut dyn FnMut() -> Result<()>,
        consume: &mut dyn FnMut([u32; 2], PinnedBytes) -> Result<()>,
    ) -> Result<Completed> {
        check()?;
        self.check()?;
        const STACK: usize = 2 * 1024 * 1024;
        let _worker = self
            .budget()
            .reserve_bytes(STACK + 8192, STACK + 8192)
            .map_err(|e| e.to_string())?;
        let stopped = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (send, receive) = mpsc::sync_channel(1);
            let (done, finished) = mpsc::sync_channel(1);
            let stop = &stopped;
            let before = self.before.clone();
            let worker = std::thread::Builder::new()
                .name("peerbrush-fade-before".into())
                .stack_size(STACK)
                .spawn_scoped(scope, move || {
                    let _done = Done(done);
                    let result = before.render_into(
                        &mut || {
                            if stop.load(Ordering::Acquire) {
                                Err("Canvas fade cancelled".into())
                            } else {
                                Ok(())
                            }
                        },
                        &mut |rows, bytes| {
                            send.send((rows, bytes))
                                .map_err(|_| "Canvas fade receiver retired".into())
                        },
                    );
                    drop(before);
                    result
                })
                .map_err(|_| "Canvas fade worker could not start")?;
            let _stop = Stop(&stopped);
            let checked = RefCell::new(check);
            let rendered =
                self.after
                    .render_into(&mut || (checked.borrow_mut())(), &mut |rows, bytes| {
                        let (old_rows, old) = loop {
                            (checked.borrow_mut())()?;
                            match receive.recv_timeout(Duration::from_millis(20)) {
                                Ok(stripe) => break stripe,
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(mpsc::RecvTimeoutError::Disconnected) => {
                                    return Err("Canvas fade before-frame stream ended".into())
                                }
                            }
                        };
                        let stride = self.key().width as usize * 4;
                        if rows != old_rows
                            || rows[0] >= rows[1]
                            || rows[1] > self.key().height
                            || bytes.len() != old.len()
                            || bytes.len() != (rows[1] - rows[0]) as usize * stride
                        {
                            return Err("Canvas fade stripe coverage differs".into());
                        }
                        let mut output =
                            ScratchBytes::new(self.budget(), bytes.len(), MAX_REGION_PIXELS * 4)
                                .map_err(|e| e.to_string())?;
                        for ((a, b), out) in old
                            .chunks_exact(stride)
                            .zip(bytes.chunks_exact(stride))
                            .zip(output.chunks_exact_mut(stride))
                        {
                            (checked.borrow_mut())()?;
                            for ((a, b), out) in a
                                .chunks_exact(4)
                                .zip(b.chunks_exact(4))
                                .zip(out.chunks_exact_mut(4))
                            {
                                pixel(a, b, self.fraction, out);
                            }
                        }
                        drop((old, bytes));
                        consume(rows, output.freeze())
                    });
            if rendered.is_err() {
                stopped.store(true, Ordering::Release);
            }
            drop(receive); // Also releases a producer blocked in send on cancellation/panic.
            let mut cancelled = None;
            loop {
                if cancelled.is_none() {
                    if let Err(error) = (checked.borrow_mut())() {
                        stopped.store(true, Ordering::Release);
                        cancelled = Some(error);
                    }
                }
                match finished.recv_timeout(Duration::from_millis(20)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                }
            }
            let before = worker
                .join()
                .map_err(|_| "Canvas fade before-render could not complete")?;
            let (before, mut after) = match (before, rendered) {
                (Ok(before), Ok(after)) => (before, after),
                (Err(error), Ok(_)) | (Ok(_), Err(error)) => return Err(error),
                (Err(before), Err(after)) => {
                    return Err(format!("{after}; before-render: {before}"))
                }
            };
            if let Some(error) = cancelled {
                return Err(error);
            }
            (checked.borrow_mut())()?;
            self.check()?;
            if before.display_status != after.display_status
                || !before.tag.same_allocation(&self.before.tag())
                || !after.tag.same_allocation(&self.after.tag())
            {
                return Err("Canvas fade completion differs from its captured renderers".into());
            }
            after.tag = self.tag.clone();
            Ok(after)
        })
    }
}
