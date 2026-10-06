# Development

Use stable Rust with native platform build prerequisites. The desktop shell uses egui/eframe and wgpu; document edits, PSD handling, and transports remain separate modules.

```sh
cargo test
cargo run
cargo build --release
```

On this Windows checkout, `scripts/dev.ps1` uses an isolated official GNU Rust distribution under `.dev-tools` with the bundled LLD linker. It does not change the user's persistent PATH or require Visual Studio. The toolchain is development-only and ignored by Git.

```powershell
.\scripts\dev.ps1 test
.\scripts\dev.ps1 run
.\scripts\dev.ps1 build
```

Sparse raster tiles are copy-on-write, so snapshots share unchanged content. Atomic command batches validate reservations and conflicting revisions before mutation; failures restore the pre-batch document. History is bounded to 40 batches. Tile sources use compact base64 encoding streamed into compressed metadata, avoiding large temporary arrays. Metadata sources are compressed in PSD resource 4000, identified by `PeerBrush.v1` and `PBR1`; hashes guard against stale definitions after external edits.

Initial limits: 8192 pixels per edge, 32 megapixels, 100 editable layers, 512 MiB allocated raster tiles, 256 MiB PSD input, and 128 MiB decoded application metadata. These limits are deliberately explicit while streaming and stronger cache budgets are developed.

Roadmap: GPU tile compositor and incremental preview updates; text/vector source layers; clone/heal; crop/resize and non-destructive transforms; additional selection tools and subject segmentation; liquify distortion maps; selective task undo; isolated/pass-through group fidelity; remote connection and plugin support.

Dependency and embedded-font licenses must accompany packaged builds. The application is GPL v3; dependencies retain their original licenses. CI builds and tests on Windows, macOS, and Linux. Cross-platform support is not considered validated until those jobs run successfully.


To package a release with source and third-party notices, run `python scripts/package.py` after `cargo build --release`. On this isolated checkout, supply `--registry .dev-tools/cargo-home/registry/src`.

Only one editor owns a given runtime workspace. Opening PeerBrush again focuses that window; `--state-dir` creates an independent workspace. This prevents an agent from silently switching to a second document. Unsaved content is also flushed to recovery at normal shutdown.

Derived mask and color caches have 128 MiB and 256 MiB budgets respectively and do not enter undo snapshots or editable PSD source data. Mask and color source keys survive undo, and group color keys invalidate when descendants change. Parameter gestures coalesce into one history entry until another participant edits. CPU previews share the command engine and accept the newest completed image from an active gesture, so continuous pointer motion does not suppress feedback.

Embedded format 3 supports color effects and extended mask settings. Older PeerBrush builds open these PSDs as a read-only merged preview rather than dropping effect sources. Compatible versions restore editable sources only when standard PSD hashes still match.

Clipboard reads, selection rendering and PNG preparation run on a dedicated worker. Pixel paste uses the shared `image.paste` command; whole-layer paste uses a typed copy-on-write transaction, including revision/reservation checks and transactional undo. The clipboard provider stays alive for Linux ownership; arboard includes Wayland data-control support with X11 fallback. `vendor/egui-winit` contains a single input adapter patch so image-only clipboard shortcuts reach the canvas; see its patch note and upstream licenses. Windows clipboard behavior is tested locally; macOS/Linux validation depends on CI and native desktop checks.
