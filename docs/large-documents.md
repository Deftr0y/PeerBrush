# Large-document loading and derived caches

File → Open, dropping a PSD and recovering an autosave use the same cancellable loader as MCP and CLI. The progress window reports reading, saved-image decoding, layer decoding and editable-source restoration. Its saved-image preview is display-only; the current document stays editable until a complete replacement is ready. Cancel, a changed current document, a changed file or decoding failure preserves the existing project and history. Reservations are checked before replacement.

The codec streams raw, PackBits RLE, ZIP and ZIP-predicted 8/16-bit composites and color-layer channels into 256-pixel native tiles. It uses row-sized decompression buffers instead of complete channel and interleaved-image buffers. Encoded resources/layer sections are read in cancellable 1 MiB chunks; the complete encoded file is not buffered. A bounded saved-image projection is available before source restoration finishes. Valid PeerBrush definitions are restored directly after verifying the standard data checksum, avoiding a duplicate standard-layer allocation. Unsupported or invalid definitions retain the existing protected Photoshop fallback.

Large native previews send a coarse shared-engine frame before detailed sampling. Both frames are derived pixels and are rejected when their document, revision or view has changed. Source channels retain their original precision; previews remain 8-bit display projections.

The existing RAM budgets remain explicit: 256 MiB color effects at 8 bits, separate 128 MiB native color/mask caches, a 128 MiB 8-bit mask cache and a 64 MiB GPU tile atlas. Evicted 8/16-bit color-effect images may spill into a separate **512 MiB disk LRU**, with a **256 MiB per-entry cap**. A reload validates dimensions, depth, exact source key, length and checksum. Corruption, unavailable disk space or cache errors recompute the result from editable sources. Cache keys change with source edits, including live drafts.

Disk entries live under the workspace state directory's `derived-cache`, in an isolated instance directory. They are disposable, excluded from history and PSD sources, and abandoned recognized entries are cleared under the workspace instance lock. They are never substituted for original tiles, masks, retained transform sources, standard PSD channels or the merged composite.

Limits remain **256 MiB encoded files, 8192 pixels per edge, 32×1024² canvas pixels and 512 MiB document raster sources**, including retained originals. Unreleased [PB-104](photoshop-fidelity.md) adds PSB v2 alongside PSD v1 under the same limits. The [PB-105 investigation](large-document-investigation.md) records practical save boundaries and a phased streaming/out-of-core proposal; larger limits are not implemented. Encoded resources and layer sections remain bounded in-memory paths. Supplementary source restoration now parses through a 64 KiB decoded read buffer with cancellation at refills; individual deserializer work and native allocations remain separate. The unreleased mask-row change below removes the complete decoded mask-plane buffer.

## Supplementary editable-source loading (unreleased, verification pending)

Supplementary JSON is parsed directly from its zlib stream rather than retaining a complete 128 MiB/512 MiB decompressed JSON buffer alongside restored native tiles. The unchanged decoded limits are 128 MiB for 8-bit documents and 512 MiB for 16-bit documents, including trailing JSON whitespace. Successful parsing must reach the end of JSON and the real zlib stream, checking truncation and checksum even at the exact decoded limit. Cancellation is checked before decoded-buffer refills and after parsing before definitions can be accepted. Parse, compression or budget failures retain the existing protected saved-pixel fallback; cancellation preserves the current work.

Source-format, native precision/layout, standard layer/composite checksum, metadata, dimensions and document-memory validation remain required. The compressed resources, restored tiles, deserializer-owned values/strings and saved composite still allocate separately. This removes a decoded transfer copy, not a total-RAM guarantee or larger-document support. Four focused methods are prepared for parser/end/budget/cancellation and legacy native8/16 PSD/PSB/source/history/composite parity. A separate opt-in same-source component compares full-buffer and streaming restoration of dense/sparse native8/16 sources without a timing threshold; performance and combined execution remain pending.

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
