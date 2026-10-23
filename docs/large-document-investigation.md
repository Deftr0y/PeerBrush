# Larger-document investigation — PB-105

Investigation completed on 2026-10-10 against the unreleased PSB implementation at `93d5af7`. **Keep the current limits until streaming and memory accounting are implemented.** PSB's wider lengths do not remove PeerBrush's allocation, effect, supplementary-source or save limits. This report proposes further development; it does not announce larger-document support.

## Measured costs

These are single local observations from fresh Windows x64 processes on a system with 32 GiB RAM. The shared Rust engine/codec used the development profile (`opt-level=1`), with CPU previews at a maximum edge of 1536. They are not release-performance guarantees, GPU measurements, tablet latency or cross-platform results. Save time covers encoding, not disk flushing; load time covers `decode_reader` with ordinary local filesystem caching. Working-set peaks include the process, source tiles, derived buffers and encoder/decoder allocations, but exclude a desktop window, GPU allocations and other project tabs.

Dense sources fill independent 256×256 RGBA tiles with deterministic RGB gradients and opaque alpha; native16 samples include values between 8-bit multiples. Sparse sources allocate only four corner tiles per layer. Three-layer fixtures use two translucent upper layers. The effect fixture adds a feathered elliptical mask and radius-4 blur. Each fixture receives a small three-point brush edit followed by exact source undo before saving. Successful loads compare every restored source raster sample at its original precision.

| Canvas | Depth / layers / content | Source tiles MiB | Preview ms | Encode ms | File MiB | Encode peak MiB | Load ms | Load peak MiB |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1024² | 8 / 1 dense | 4 | 9 | 136 | 11.8 | 44 | 62 | 29 |
| 1024² | 16 / 1 dense | 8 | 15 | 215 | 23.4 | 90 | 103 | 52 |
| 2048² | 8 / 1 dense | 16 | 21 | 591 | 47.2 | 161 | 225 | 96 |
| 2048² | 16 / 1 dense | 32 | 35 | 898 | 93.7 | 346 | 405 | 187 |
| 2048² | 8 / 3 dense | 48 | 201 | 1965 | 109.6 | 440 | 486 | 233 |
| 2048² | 16 / 3 dense | 96 | 214 | 2706 | 217.1 | 895 | 965 | 459 |
| 2048² | 16 / 1 dense, mask + blur | 32¹ | 1475 | 2329 | 102.5 | 479 | 517 | 300 |
| 4096×2048 | 8 / 1 dense | 32 | 12 | 1261 | 94.4 | 317 | 446 | 186 |
| 4096×2048 | 16 / 1 dense | 64 | 19 | 1894 | 187.4 | 684 | 795 | 365 |
| 4096×2048 | 16 / 3 sparse | 6 | 29 | 1974 | Rejected | 930 | — | — |
| 4096×2048 | 16 / 3 dense | 192 | 113 | 5924 | Rejected | 1781 | — | — |
| 4096² | 8 / 1 sparse | 1 | 14 | 1170 | 128.9 | 331 | 443 | 138 |
| 4096² | 16 / 1 sparse | 2 | 27 | 1543 | Rejected | 783 | — | — |
| 4096×8192 | 8 / 1 sparse | 1 | 10 | 2166 | Rejected | 651 | — | — |
| 8192×1 | 16 / 1 dense | 16 | <1 | 33 | 0.3 | 23 | —² | —² |

¹ Base color tiles before adding the mask; mask sources and derived work appear in the measured process peak. ² The thin-frame file also reopened with exact native sources; its extra verification run is excluded from this timing table.

The small brush command took 0.76–0.92 ms on the main fixtures; undo took 0.006–0.059 ms. These measure engine mutations and copy-on-write history, **not the subsequent canvas render**. The 1475 ms effect preview demonstrates why cheap edits alone do not establish interactive performance. Native16 mask/blur preparation still processes the source frame at full resolution even when the screen preview is small. Cache state and image content affect timings.

Cancellation was injected during resource reads, layer-section reads and saved-composite row decoding on ten successful files. All 30 checks returned a cancellation error within 7 ms of the injected signal. This excludes OS read stalls and uninterrupted JSON deserialization or other CPU phases. It does not establish a global cancellation bound. The current save encoder has no cancellation control.

The native application opened the 4096×2048 native16 fixture, rendered a stroke, and restored byte-identical PNG16 with one undo. After two empty full-frame layers were added, an oversized save left the prior PSB hash, document identity, revision, three-layer dirty state and native pixels unchanged. Canceling a subsequent asynchronous open retained the same work and original file. The workspace showed the rendered result, cancellation status and an explicit autosave failure for the unsaveable state. Generated measurement programs, fixtures, connection state and captures are not repository assets.

## Why increasing constants is insufficient

The [raster](../src/raster.rs) allows 8192 pixels per edge and **32×1024² pixels** per canvas, with native 8/16-bit sparse copy-on-write sources. [Engine](../src/engine.rs) validation bounds current source storage to 512 MiB; this is not a total process or retained-history budget. A tile costs 256 KiB at 8 bits or 512 KiB at 16 bits, including padding. The 8192×1 measurement allocates 16 MiB of native tiles for only 64 KiB of logical RGBA samples. Sparse and thin images cannot be budgeted from dimensions alone.

The [PSD/PSB writer](../src/psd.rs) emits uncompressed standard channel planes over each layer's storage frame, including empty frames, plus the current merged composite. For `L` full-frame raster layers with `P` pixels and `B` bytes per channel, the standard color data alone needs approximately:

`4 × B × P × (L + 1)` bytes.

Mask planes, baked layers, records, profiles and supplementary sources add to that lower bound. At 4096×2048 native16, three full-frame layers already need 256 MiB before this overhead. One 4096² native16 layer also needs 256 MiB. One 32×1024²-pixel 8-bit layer reaches the same boundary. All exceed the current 256 MiB encoded-file cap even with almost-empty source tiles. Canvas validity therefore does not guarantee a saveable document; autosave uses the same codec and can fail too.

Encoding retains several large channel/section/output buffers and native composite/effect images before the final file-size check. That explains the 930 MiB peak for only 6 MiB of sparse sources. Loading streams composite rows into tiles but still buffers the entire resource and layer sections, inflates supplementary JSON, then deserializes base64 tile arrays. The supplementary serialization/decode cap is 128 MiB for 8-bit documents and 512 MiB for native16 documents; it is separate from the encoded-file and source-tile caps. Dense 8-bit source data can exhaust that cap before reaching the nominal tile allowance.

[Native16 derived color and mask budgets](../src/depth16.rs) are each 128 MiB, and [whole-image filters](../src/filters.rs) have separate output/scratch limits. One full RGBA16 frame at 4096² consumes 128 MiB; multiple full-frame effect outputs can exceed the aggregate budget. The [disk cache](../src/disk_cache.rs) stores disposable derived images, not durable editable source tiles. Its presence does not make documents out of core. Forty copy-on-write history entries and multiple project tabs can retain unique older source tiles beyond the current document's allowance.

## Implementation sequence and candidate limits

An unreleased first safeguard now computes a conservative lower bound for the existing writer's uncompressed standard color/mask channels and current merged composite before preparing effects or allocating output planes. It uses actual group/divider, baked-composite and hidden-source plans. Necessarily oversized PSD/PSB output is rejected early; the final encoded-byte limit remains mandatory. Added regression coverage checks failed saves preserve prior files, current native sources, dirty state, history and save identity. Execution of this new coverage is pending the combined batch. This is not streaming, total-process memory accounting, cancellation for saves or support for larger dimensions.

1. **Make existing limits predictable.** Compute checked standard-channel/record lower bounds and operation scratch estimates before rendering or allocating large output buffers. Explain saveability and autosave failures using the same shared engine/codec limits. Reject impossible output before expensive encoding; keep existing files and unsaved sources intact. Add explicit cancellable save progress while retaining source/revision and external-file checks at atomic publication.
2. **Stream codec input and output.** Seek bounded channel ranges and decode directly into native tiles; avoid retaining complete layer sections. Write standard channels and the current merged composite through a bounded temporary file, using valid per-channel compression where useful. Count actual bytes throughout, with checked PSD/PSB field widths and cancellation between rows/chunks. Serialize supplementary sources incrementally using a versioned native binary tile stream rather than a fully inflated base64 JSON array. Existing readers must receive a current protected standard appearance when they cannot restore newer sources. Standard raster/mask planes remain mandatory.
3. **Bound rendering and source residency.** Render/export composite strips or tiles; evaluate effect halos regionally with reference parity, including folders, clipping, masks and original native16 low bits. Introduce immutable content-addressed native source tiles, a bounded resident working set and a separate durable backing store. History, clipboard, tasks and project tabs reference immutable tile versions. Count unique retained versions across all owners. Pin dirty work until durable publication, journal atomic manifests, and handle full disks, missing/corrupt blocks and interrupted recovery without discarding human work. Derived-cache eviction must never delete source backing data.
4. **Qualify a larger envelope before enabling it.** A useful first target is **16384 per edge, 64×1024² canvas pixels and 1 GiB PSB input/output**, with a separately enforced resident/scratch budget and disk-backed sources. These are candidate qualification limits, not supported settings. Keep a conservative in-memory mode; retain the existing PSD cap initially. High-entropy channels, many layers and native16 composites may still exceed the proposed file cap and must fail explicitly. Compression cannot guarantee saveability. The resident budget must be measured across tabs, history and active operations, rather than treating a 512 MiB source quota as a process limit.

Before enabling that target, measure native8/16 sparse, dense, thin, masked, effect-heavy, nested/clipped and high-entropy documents at both current and proposed boundaries on Windows, macOS and Linux. Include cold/warm loads, encoding and durable writes, actual gesture-to-frame latency, concurrent tabs/tasks/history, cancellation during every phase and disk-full recovery. Require exact native source/undo/PNG samples and independently decoded standard raster/mask/current-composite parity, including metadata-free reopening and protected unsupported sources. File conflicts and later human edits must survive failed, canceled or stale operations. No larger limit should ship on the strength of this local investigation alone.

## Incremental native tile-array decoding (unreleased)

The next PB-068/PB-108 transient-memory slice replaces the native8/native16 deserializers' all-entry encoded vectors with ordinary Serde sequence visitors. Each tuple is validated and decoded before the next is requested, retaining the existing base64/legacy8 representation, native16 words, duplicate policy and caller protection. Supplementary restoration still uses the capped full JSON buffer and `serde_json::from_slice`; the previously slower per-byte reader is not restored. No source format, admission limit or backing store is changed.

This removes one aggregate encoded-entry container, not the JSON buffer or native source residency. Per-entry parser/legacy/decode allocations remain, including allocation before malformed oversized input is rejected; unique tile count and document memory still rely on their existing caller checks. Earlier tuple validation can report a semantic error before a later structural error, and never exposes a partially decoded raster. Prepared correctness/protected-fallback tests and balanced normal-main elapsed/observed-memory measurements must pass before claiming a practical improvement. The measured historical investigation above remains unchanged and does not qualify this new implementation or a 16000-square envelope.
