//! Compile and run separately with RUSTFLAGS=-Cpanic=abort. No application code.
use std::sync::Arc;
struct RefuseCallbacks;
impl resvg::tiny_skia::allocation::Cancellation for RefuseCallbacks {
    fn check(&self) -> Result<(), String> {
        panic!("abort-profile entry called cancellation")
    }
}
impl resvg::rows::RowBudget for RefuseCallbacks {
    fn check(&self) -> Result<(), String> {
        panic!("abort-profile entry called check")
    }
    fn reserve(&self, _: usize) -> Result<Box<dyn resvg::rows::MemoryLease>, String> {
        panic!("abort-profile entry allocated")
    }
    fn path_cancellation(&self) -> Arc<dyn resvg::tiny_skia::allocation::Cancellation> {
        panic!("abort-profile entry acquired cancellation")
    }
}
fn main() {
    assert!(
        cfg!(panic = "abort"),
        "this fixture requires panic=abort for all packages"
    );
    let result = resvg::tiny_skia::allocation::with_limits(
        resvg::tiny_skia::allocation::Limits {
            bytes: 1024,
            elements: 128,
            work: 100,
            cancellation: Arc::new(RefuseCallbacks),
        },
        Box::new(()),
        || -> Result<(), ()> { panic!("abort-profile entry ran operation") },
    );
    assert!(matches!(
        result,
        Err(resvg::tiny_skia::allocation::Failure::Refused(
            resvg::tiny_skia::allocation::Refusal::UnsupportedPanicProfile
        ))
    ));
    let tree=resvg::usvg::Tree::from_str("<svg xmlns='http://www.w3.org/2000/svg' width='3' height='3'><rect width='3' height='3'/></svg>",&resvg::usvg::Options::default()).unwrap();
    let result = resvg::rows::render_row(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        [3, 3],
        0,
        &RefuseCallbacks,
    );
    assert!(matches!(result,Err(ref error)if error.contains("unwind-capable")));
    let plan = resvg::rows::RowPlan::new(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        [3, 3],
        resvg::rows::PlanLimits {
            retained_bytes: 4096,
            maximum_invocations: 1,
            work: 100,
        },
        &RefuseCallbacks,
    );
    assert!(matches!(plan,Err(ref error)if error.contains("unwind-capable")));
    println!("abort-profile entry refused before operation, callbacks or allocation");
}
