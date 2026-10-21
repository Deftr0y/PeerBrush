# Large-document loading and derived caches

File → Open, dropping a PSD and recovering an autosave use the same cancellable loader as MCP and CLI. The progress window reports reading, saved-image decoding, layer decoding and editable-source restoration. Its saved-image preview is display-only; the current document stays editable until a complete replacement is ready. Cancel, a changed current document, a changed file or decoding failure preserves the existing project and history. Reservations are checked before replacement.

The codec streams raw, PackBits RLE, ZIP and ZIP-predicted 8/16-bit composites and color-layer channels into 256-pixel native tiles. It uses row-sized decompression buffers instead of complete channel and interleaved-image buffers. Encoded resources/layer sections are read in cancellable 1 MiB chunks; the complete encoded file is not buffered. A bounded saved-image projection is available before source restoration finishes. Valid PeerBrush definitions are restored directly after verifying the standard data checksum, avoiding a duplicate standard-layer allocation. Unsupported or invalid definitions retain the existing protected Photoshop fallback.

Large native previews send a coarse shared-engine frame before detailed sampling. Both frames are derived pixels and are rejected when their document, revision or view has changed. Source channels retain their original precision; previews remain 8-bit display projections.

The existing RAM budgets remain explicit: 256 MiB color effects at 8 bits, separate 128 MiB native color/mask caches, a 128 MiB 8-bit mask cache and a 64 MiB GPU tile atlas. Evicted 8/16-bit color-effect images may spill into a separate **512 MiB disk LRU**, with a **256 MiB per-entry cap**. A reload validates dimensions, depth, exact source key, length and checksum. Corruption, unavailable disk space or cache errors recompute the result from editable sources. Cache keys change with source edits, including live drafts.

Disk entries live under the workspace state directory's `derived-cache`, in an isolated instance directory. They are disposable, excluded from history and PSD sources, and abandoned recognized entries are cleared under the workspace instance lock. They are never substituted for original tiles, masks, retained transform sources, standard PSD channels or the merged composite.

Limits remain **256 MiB encoded files, 8192 pixels per edge, 32×1024² canvas pixels and 512 MiB document raster sources**, including retained originals. Unreleased [PB-104](photoshop-fidelity.md) adds PSB v2 alongside PSD v1 under the same limits. The [PB-105 investigation](large-document-investigation.md) records practical save boundaries and a phased streaming/out-of-core proposal; larger limits are not implemented. Encoded resources and layer sections remain bounded in-memory paths. Supplementary source restoration uses fast slice deserialization after capped decompression, with 64 KiB transfer scratch and cancellation between reads; the complete decoded JSON buffer, individual parser work and native allocations remain separate. The unreleased mask-row change below removes the complete decoded mask-plane buffer.

## Supplementary editable-source loading (unreleased, verification pending)

Supplementary JSON is decompressed into a capped buffer and parsed with `serde_json::from_slice`. Per-byte reader deserialization was rejected after a measured restoration/Open regression; the bounded slice correction requires integrated replay before its performance can be claimed. The unchanged decoded limits are 128 MiB for 8-bit documents and 512 MiB for 16-bit documents, including trailing JSON whitespace. The guarded reader retains an extra-byte budget check and must reach real zlib EOF before parsing, checking truncation and checksum even at the exact decoded limit. Slice parsing must consume JSON through its trailing whitespace. A fixed 64 KiB transfer chunk replaces the original 1 MiB temporary. Cancellation is checked before each decompression read, after EOF before parsing and after parsing before definitions can be accepted. Cancellation uses an I/O error that is not automatically retried; parse, compression or budget failures retain the existing protected saved-pixel fallback and current work remains preserved.

Source-format, native precision/layout, standard layer/composite checksum, metadata, dimensions and document-memory validation remain required. The complete decoded JSON buffer is a deliberate remaining PB-068 limit. Compressed resources, restored native tiles, vector capacity, deserializer-owned values/strings and the saved composite still allocate separately. The 64 KiB transfer bound is not a total-RAM guarantee, streaming restoration or larger-document support. All four strong parser/end/budget/cancellation and native8/16 PSD/PSB/source/history/composite methods are retained. A focused EOF-cancellation/read-error case is added. The opt-in same-source component now compares the original capped 1 MiB-chunk/full-buffer reader with the current guarded 64 KiB-chunk/full-buffer reader; both use slice parsing. Its fixed old-first dense/sparse samples have no timing threshold and do not replace a balanced normal-main Open comparison. The new correction and its tests remain pending integrated execution.

## Native mask loading (unreleased, verification pending)

Standard PSD/PSB masks now decode raw, PackBits, ZIP and predicted rows directly into the local native raster. The decoder retains one native row and any existing RLE length table, checks cancellation each row and validates all encoded coverage, including pixels outside the destination. Final ZIP completion/checksum and exact decoded size remain required. Mask offsets, defaults, enabled state, original precision, unsupported-document protection and all current admission limits are unchanged. Partial decode never replaces the current project.

Four focused regression methods are prepared and independently source-reviewed; combined and native execution remain pending. Encoded sections, final native tiles, saved composite and private source restoration still allocate separately. This changes transfer scratch, not the total document-memory bound or16000-square support.

## Standard PSD/PSB save transfer (unreleased)

Working Save serializes native8/16 standard layer, mask and current merged-composite channels by row into an exclusively created same-directory spool. A fixed 16 KiB row plus a 64 KiB buffered transfer/copy bounds standard pixel-transfer scratch; the write buffer is dropped before copying. The original separate layer/composite checksums and supplementary source formats are unchanged. The final guarded file is assembled from the checked spool, which is removed before publication. Temporary files, overwritten originals, source/revision/saved-state checks, file ownership and actual published-byte verification retain their working-save protections.

Both spool and final staging remain capped at 256 MiB; together they may temporarily require up to 512 MiB disk space, in addition to the destination and existing previous-save versions. Full render/effect/mask preparation, record metadata, compressed private source data and public Vec-returning codec APIs still allocate separately. The Save transfer change itself does not modify recovery or decoding. Row/chunk checks can reject stale or interrupted serialization, but this does not add a Save cancellation/progress endpoint or bound cancellation inside full preparation. It does not bound total RAM, enable 16000-square documents or complete PB-068. The focused stream/save regressions require integrated execution before release qualification.

## MCP / CLI

Existing `document` action `open` remains synchronous. `open_async` returns immediately; observe `state.loading` (`stage`, `completed`, `total`) and `state.file_status`. `cancel_open` cancels the active loader. Opening still requires a saved document or explicit `discard:true`; `expected_revision` can guard the request. Replacement also checks the exact source document/revision captured when loading starts.

```json
{"action":"open_async","path":"C:/art/project.psd","expected_revision":12}
```

```json
{"action":"cancel_open"}
```

No partial loaded document is exposed to editing clients. Observe the current document ID and revision again after loading completes.

## Native PNG import transfer (unreleased, verification pending)

Static noninterlaced RGB/RGBA8/16 PNGs with normal orientation decode rows into the existing native tiles through shared Open/import/placement/file-paste preparation. One native RGBA row and a fixed 64 KiB checked input buffer replace the full DynamicImage/RGBA transfer copy for this layout. Existing decoder state, CMS buffers/tables, profiles, encoded bytes and final tiles still allocate separately. Sparse zero rows avoid creating empty tiles while hidden RGB beneath zero alpha remains native data.

Header/profile/orientation/APNG checks, the conservative decoded-coexistence/import budget, native8/16 precision, document memory limits and source/file/ownership/revision guards remain unchanged. Open checks cancellation per row and before accepting final PNG completion; unsupported row layouts retain the existing decoder rather than silently changing content. No canvas/file cap is raised. Full decoded supplementary JSON, full effect/filter preparation, other image layouts and original native residency remain PB-068 limits. The complete 16000-square edit/save/reopen/export envelope is not qualified by this transfer change.

## Opt-in actual PSD Open phase diagnostic (unreleased)

The ignored `psd::source_stream_tests::actual_dense_psd_open_phase_diagnostic` runs six actual synchronous shared-server Opens of synthetic dense 2048-square PSDs, three at each native depth. Its test-only scoped observer separates initial ownership/version checks, section reads, native saved-composite decoding/fingerprinting, resource/profile preview, capped supplementary decompression, slice parsing, JSON return cleanup, acceptance/hash/native/metadata validation, decoder return cleanup, final source verification and replacement/return. No production API, configuration, native input or application instrumentation is added. Run this exact ignored method alone only after regular qualification closes, with the committed toolchain/native prerequisites and matching test profile.

The fixed `PeerBrushOpenPhaseDiagnosticV1` record contains clean public build metadata, finite durations, strict phase order/counts and native/source/current-standard-composite/file/history preservation checks. Fixture generation/encoding and correctness oracles are outside timed Open; observer/test-profile overhead and uncontrolled caches/allocator/scheduling remain. Final intervals include lock acquisition and cleanup; they are not measurements of total lock-held time. This is one-source core attribution without CLI/IPC, not a before/after speed claim, whole-process memory bound, cold startup, native input/presentation or larger-document acceptance. Source implementation and prepared tests require integrated replay.
