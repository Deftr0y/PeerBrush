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

Native 16-bit sources use sparse copy-on-write u16 tiles. A separate full-precision compositor keeps color, masks, selection transformations and effect sources native; screen and MCP previews project only at the output boundary. Standard PSD layer/mask channels and PNG exports retain the document depth. Native color and spatial-mask derived caches are independently bounded to 128 MiB. Blur/bloom use a bounded u32 premultiplied working image with u64 running sums. See the checkpoint for measured GPU coverage and remaining CPU paths.


## Windows 0.1.4 selection and preview changes

`selection::Coverage` stores a sparse 8-bit coverage mask, its document-space origin/bounds and separate boundary contours. An active empty selection has zero bounds and a coverage object; deselection has no selection. Raster edits apply coverage to source changes at native channel precision. Clipboard alpha uses the same coverage, selected transforms move both source alpha and coverage, and Liquify snapshots a local coverage source for each stroke. Derived previews and contours never replace standard PSD raster channels. Invalid/oversized coverage is rejected by document validation.

Preview requests take the latest committed document after UI mutations and accept only that document/revision, while retaining the last displayed pixels until the replacement finishes. Active gesture images may arrive from an earlier point within the same gesture. Regression tests cover release/commit, move, transform, reorder and visibility rejection of stale images.

Visibility-only updates have an internal visibility conflict scope and still use ordinary revisions/history. They neither overlap pixel reservations nor invalidate pixel-only expected revisions. Commands mixing visibility with other changes use the ordinary layer scope. Structural scope collection names source trees and destinations instead of locking unrelated siblings. Reservation visuals call the same ancestor-aware overlap routine as engine checks.

Private PSD source format 6 identifies selection coverage, smooth curve interpolation and coverage-aware liquify sources. Older readers reject that version and use the protected standard saved appearance. Documents without these sources retain the earlier compatible format where possible.

`document.settings` changes canvas dimensions without resampling sources and explicitly converts all color/mask rasters to the requested native depth in one undo transaction. Selection geometry is cleared after canvas/depth changes. Build and QA must use a separate target executable and `--state-dir` when a user's editor is already running.

## Retained regional effects (PB-075)

Gesture previews retain separate derived 8-bit/native-16 color and mask images. Source changes re-evaluate only conservative padded windows, including nested folder and clipped-adjustment dependencies. Padding includes each Gaussian/legacy mask-blur pass. Raw isolated masks refresh outward clamped edge strips. Liquify/unknown filter support uses the full renderer. Metadata/revision/isolation changes and errors reset the gesture cache; copy-on-write isolates global baseline images. Native uniform masks return to constant storage when painted coverage is removed. Derived images obey the existing color/mask budgets and never enter editable sources or history; one gesture may retain its own bounded images alongside the global caches.

Regional GPU effects keep the full layer's dispatch threshold, so small input windows use the same arithmetic as a full GPU render. Device failure/busy fallback remains intact. Verification passed 97 engine/codec/protocol/UI checks, seven final preview checks, three actual-device parity checks (including padded windows), and native failure injection. Real native16 painting through levels/blur/bloom/GPU HSL rendered correctly; one undo restored byte-identical PNG16 output.

Release measurements on the Windows NVIDIA RTX 3070 Ti Laptop system, with nine layers and a 1536-pixel preview, compare five successive brush updates. Every retained result was asserted equal to its full-render reference. Effects ran on CPU for this benchmark; timings are local observations, not project-wide guarantees.

| Canvas / channels | Full replay + rendering | Retained replay + regional rendering |
| --- | ---: | ---: |
| 2048² / 8-bit, levels + blur | 7392 ms | 338 ms |
| 4096² / 8-bit, levels + blur | 37354 ms | 567 ms |
| 2048² / native 16-bit, levels + blur | 12864 ms | 621 ms |
| 4096² / native 16-bit, levels + blur | 65044 ms | 565 ms |

Run `cargo run --release --example preview_performance` or add `-- --effects16` for native effects. Tiled GPU composition, finer dirty-tile tracking and progressive large-document handling remain queued.
