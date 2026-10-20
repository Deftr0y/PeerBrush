# JPEG XL build inputs (unreleased)

The JPEG XL codec uses GPL-compatible jpegxl-sys 0.13.0 with libjxl 0.12.0,
statically built from the full corresponding source in vendor/jpegxl-src.
The original jpegxl-src0.12.0 crate SHA256 is
`06f56a51e36f688517deebb0b7c10a11c598476936838938495c93764696180a`.
Its small build-helper patch selects committed native source, honors Cargo's
positive job count and permits the explicit Ninja/clang-cl generator route;
codec algorithms and native static/CRT definitions are unchanged.
Rust 1.92.0 is selected by rust-toolchain.toml. Existing installations of an
older Rust toolchain cannot compile these dependencies; select the committed
toolchain without changing another project's default.

Native builds require Clang, CMake and Ninja, plus the ordinary platform SDK
and C++ runtime. The candidate CI setup uses the tools already installed on
the hosted runner and checks their exact reviewed versions before configure;
it never downloads a latest compiler as a fallback. A moving runner-image
update can require a reviewed version-list adjustment. The initial native
matrix has not yet qualified this integration.

On Windows initialize the existing Visual Studio C++ developer environment
for amd64, use clang-cl for CC/CXX and CMAKE_GENERATOR=Ninja. This does not
require adding the VS ClangCL workload when a standalone reviewed clang-cl
already exists. On Linux use clang-18/clang++-18 and Ninja. On macOS use the
Xcode compiler/SDK and Ninja; retain MACOSX_DEPLOYMENT_TARGET=11.0. Standard
Cargo builds need these process-local variables set before compilation.
`python scripts/jxl_build.py` reports whether the installed tool versions are
the reviewed versions. `python scripts/jxl_build.py -- run --locked -- --state-dir .runtime` selects them for a child
Cargo command without changing the calling shell; use test/check/build/clippy
similarly. The Windows scripts/dev.ps1 wrapper uses that same route;
 `scripts/build_release.py` performs the same selection for its
child Cargo release build. CI exports only named build environment values.

Use `cargo build --locked --jobs 2` or CARGO_BUILD_JOBS=2 for the initial bounded build. The
vendored helper uses the positive Cargo NUM_JOBS value for native parallelism
as well. Native source is always selected relative to the committed vendored
manifest. DEP_JXL_PATH and DEP_JXL_LIB overrides are rejected by the supported
release build route; do not substitute an external source tree or system JXL.

All native source/build files and the toolchain file must appear in the
corresponding-source manifest. The vendored helper retains its root BSD
notice and GPL source header; packages include the GPL text and exact nested
libjxl, Highway, Brotli and skcms notices. Full corresponding source contains
additional upstream source notices. No external compiler/tool archive is an
application source asset or runtime dependency.

Static JXL linkage does not mean every C++ runtime is static. Linux links the
system libstdc++, macOS the system libc++, and Windows retains the existing
normal Rust/MSVC runtime policy with native JXL built /MT. Inspect the actual
packaged executable dependencies and minimum OS version; never ship private
development DLLs or debug runtimes. Release path remapping covers native
source/macro paths alongside Rust, and the existing binary privacy check must
still pass. Clean installed-runtime and native 8/16-bit codec acceptance are
required before calling a platform build ready.

The original crate includes three generated CTest scheduling-cache ZIPs,
not compiler source. They are omitted while all native/build source and
licenses remain. BUILD_TESTING and native tools are disabled. Exact omissions:

- `libjxl/tools/scripts/test_cost-x64.zip` (75116 bytes), SHA256 `d3129b959d26cae5482fb9de6427d2de958107044fb11ffc920196136ed24db1`
- `libjxl/tools/scripts/test_cost-i386.zip` (74941 bytes), SHA256 `f23d5e567874bfd19f89296e7382ad1bab08c6492db1629f638f7f6ce570d4d7`
- `libjxl/tools/scripts/test_cost-armhf.zip` (70763 bytes), SHA256 `a51c4b2624b5d0b25d472db7b6ff408df568074c28dfd7b491f9183185e721e1`

Integration codec tests launch the application worker in the matching Cargo profile directory. Run `python scripts/jxl_build.py -- build --locked --bin peerbrush` before the corresponding test command. The development test helper and all current native CI callers do so. Installed launches use their exact current executable, including renamed installations; no PATH-selected or configurable worker executable is accepted.

The reviewed hosted-tool versions come from GitHub's official [Windows 2025](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md), [Ubuntu 24.04](https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md) and [macOS 26](https://github.com/actions/runner-images/blob/main/images/macos/macos-26-Readme.md) inventories. Their presence is not a successful PeerBrush native build; CI must establish that separately.
