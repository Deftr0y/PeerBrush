//! Load a pinned external Sparkle framework without starting any update operation.
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("Sparkle native preparation requires macOS12+");
}

#[cfg(target_os = "macos")]
fn main() {
    use peerbrush::sparkle::{macos, Configuration};
    let path = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("external verified framework path"),
    );
    objc2::rc::autoreleasepool(|_| {
        let mut blocked = Configuration {
            enabled: true,
            ..Default::default()
        };
        assert!(
            macos::probe(&path, &blocked).is_err(),
            "activation must fail before code load"
        );
        blocked.enabled = false;
        blocked.automatic_installation = true;
        assert!(
            macos::probe(&path, &blocked).is_err(),
            "installation must fail before code load"
        );
        let probe = macos::probe(&path, &Configuration::default())
            .expect("pinned native Sparkle preparation");
        probe.assert_disabled();
        println!("Verified Sparkle2.10.0 loaded; native controller created without starting; checks and relaunch vetoed; no feed/key/install configured");
    });
}
