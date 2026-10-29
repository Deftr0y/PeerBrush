//! Separate setup executable. Its public offer is supplied only by the reviewed
//! producer at compilation; command-line URLs/configuration are not accepted.
fn main() {
    std::process::exit(peerbrush::updates::windows_setup_entry(option_env!(
        "PEERBRUSH_SETUP_OFFER"
    )));
}
