#[path = "../../svg-origin-corpus.rs"]
mod corpus;
use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
struct Control;
impl resvg::tiny_skia::allocation::Cancellation for Control {
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
}
struct Budget {
    live: Arc<AtomicUsize>,
}
struct Lease {
    live: Arc<AtomicUsize>,
    bytes: usize,
}
impl Drop for Lease {
    fn drop(&mut self) {
        assert!(self.live.fetch_sub(self.bytes, Ordering::SeqCst) >= self.bytes);
    }
}
impl resvg::rows::RowBudget for Budget {
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
    fn reserve(&self, bytes: usize) -> Result<Box<dyn resvg::rows::MemoryLease>, String> {
        self.live
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current
                    .checked_add(bytes)
                    .filter(|n| *n <= 256 * 1024 * 1024)
            })
            .map_err(|_| "primitive reservation limit exceeded")?;
        Ok(Box::new(Lease {
            live: self.live.clone(),
            bytes,
        }))
    }
    fn path_cancellation(&self) -> Arc<dyn resvg::tiny_skia::allocation::Cancellation> {
        Arc::new(Control)
    }
}
fn main() {
    // Synthetic primitive oracle only. This fixture budget is not the app's
    // shared allocation ledger and provides no app-wide RAM qualification.
    let baseline = std::env::args().any(|argument| argument == "--baseline");
    let mut out = io::BufWriter::new(io::stdout().lock());
    let budget = Budget {
        live: Arc::new(AtomicUsize::new(0)),
    };
    for case in corpus::cases() {
        if baseline && case.name == "reused-identical-clip-sites" {
            continue;
        }

        let tree =
            resvg::usvg::Tree::from_str(&case.svg, &resvg::usvg::Options::default()).unwrap();
        let [sx, ky, kx, sy, tx, ty] = case.matrix;
        let transform = resvg::tiny_skia::Transform::from_row(sx, ky, kx, sy, tx, ty);
        for maximum in [1, 7] {
            let mut at = 0;
            while at < case.rows.len() {
                let y = case.rows[at];
                let mut count = 1;
                while count < maximum
                    && at + count < case.rows.len()
                    && case.rows[at + count] == y + count as u32
                {
                    count += 1;
                }
                let image = resvg::rows::render_rows(
                    &tree,
                    transform,
                    case.frame,
                    y,
                    count as u32,
                    &budget,
                )
                .unwrap_or_else(|e| panic!("{} row{y} count{count}: {e}", case.name));
                assert_eq!(
                    (
                        image.width(),
                        image.y(),
                        image.row_count(),
                        image.data().len()
                    ),
                    (
                        case.frame[0],
                        y,
                        count as u32,
                        case.frame[0] as usize * count * 4
                    )
                );
                out.write_all(image.data()).unwrap();
                drop(image);
                assert_eq!(budget.live.load(Ordering::SeqCst), 0);
                at += count;
            }
        }
        if !baseline {
            let mut plan = resvg::rows::RowPlan::new(
                &tree,
                transform,
                case.frame,
                resvg::rows::PlanLimits {
                    retained_bytes: 64 * 1024 * 1024,
                    maximum_invocations: 16384,
                    work: 1_000_000_000,
                },
                &budget,
            )
            .unwrap();
            let mut selected = 0;
            while !plan.is_complete() {
                let start = plan.next_y();
                let count = 7.min(case.frame[1] - start);
                let image = plan
                    .next(count)
                    .unwrap_or_else(|error| panic!("{} plan row{start}: {error}", case.name));
                while selected < case.rows.len() && case.rows[selected] < start + count {
                    let row = case.rows[selected];
                    assert!(row >= start);
                    let begin = (row - start) as usize * case.frame[0] as usize * 4;
                    out.write_all(&image.data()[begin..begin + case.frame[0] as usize * 4])
                        .unwrap();
                    selected += 1;
                }
                drop(image);
                assert_eq!(budget.live.load(Ordering::SeqCst), 64 * 1024 * 1024);
            }
            assert_eq!(selected, case.rows.len());
            eprintln!(
                "whole operation scan work: {} {}",
                case.name,
                plan.work_used()
            );
            drop(plan);
            assert_eq!(budget.live.load(Ordering::SeqCst), 0);
        }
        eprintln!(
            "bounded row: {} {:?} {:?}",
            case.name, case.frame, case.matrix
        );
    }
    out.flush().unwrap();
}
