//! Interpreter transport only. The parent retains actual source admission,
//! task/reservation checks and the shared engine publication fence.
mod child;
mod parent;
mod process;
mod protocol;
mod resources;
pub(super) use child::error_text;
pub(super) use process::commands;
pub(super) fn supported() -> Result<(), String> {
    resources::Parent::supported()
}

pub(super) fn run() -> Result<(), String> {
    let permit = resources::acquire()?;
    child::serve(std::io::stdin(), std::io::stdout(), build_label(), &permit)
}
fn build_label() -> [u8; 32] {
    ring::digest::digest(&ring::digest::SHA256, crate::build_info::json().as_bytes())
        .as_ref()
        .try_into()
        .unwrap()
}
