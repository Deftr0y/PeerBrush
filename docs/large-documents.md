# Large-document loading and derived caches

File → Open, dropping a PSD and recovering an autosave use the same cancellable loader as MCP and CLI. The progress window reports reading, saved-image decoding, layer decoding and editable-source restoration. Its saved-image preview is display-only; the current document stays editable until a complete replacement is ready. Cancel, a changed current document, a changed file or decoding failure preserves the existing project and history. Reservations are checked before replacement.

The codec streams raw, PackBits RLE, ZIP and ZIP-predicted 8/16-bit composites and color-layer channels into 256-pixel native tiles. It uses row-sized decompression buffers instead of complete channel and interleaved-image buffers. Encoded resources/layer sections are read in cancellable 1 MiB chunks; the complete encoded file is not buffered. A bounded saved-image projection is available before source restoration finishes. Valid PeerBrush definitions are restored directly after verifying the standard data checksum, avoiding a duplicate standard-layer allocation. Unsupported or invalid definitions retain the existing protected Photoshop fallback.

Large native previews send a coarse shared-engine frame before detailed sampling. Both frames are derived pixels and are rejected when their document, revision or view has changed. Source channels retain their original precision; previews remain 8-bit display projections.

The existing RAM budgets remain explicit: 256 MiB color effects at 8 bits, separate 128 MiB native color/mask caches, a 128 MiB 8-bit mask cache and a 64 MiB GPU tile atlas. Evicted 8/16-bit color-effect images may spill into a separate **512 MiB disk LRU**, with a **256 MiB per-entry cap**. A reload validates dimensions, depth, exact source key, length and checksum. Corruption, unavailable disk space or cache errors recompute the result from editable sources. Cache keys change with source edits, including live drafts.

Disk entries live under the workspace state directory's `derived-cache`, in an isolated instance directory. They are disposable, excluded from history and PSD sources, and abandoned recognized entries are cleared under the workspace instance lock. They are never substituted for original tiles, masks, retained transform sources, standard PSD channels or the merged composite.

Limits remain **PSD v1 / 256 MiB encoded files, 8192 pixels per edge, 32 megapixels and 512 MiB document raster sources**, including retained originals. This work does not enable PSB, unlimited document sizes, arbitrary Photoshop features or out-of-core editable sources. Mask-channel buffers and private JSON restoration remain bounded in-memory paths; cancellation is checked between chunks/rows/sources rather than inside an individual JSON parse.

## MCP / CLI

Existing `document` action `open` remains synchronous. `open_async` returns immediately; observe `state.loading` (`stage`, `completed`, `total`) and `state.file_status`. `cancel_open` cancels the active loader. Opening still requires a saved document or explicit `discard:true`; `expected_revision` can guard the request. Replacement also checks the exact source document/revision captured when loading starts.

```json
{"action":"open_async","path":"C:/art/project.psd","expected_revision":12}
```

```json
{"action":"cancel_open"}
```

No partial loaded document is exposed to editing clients. Observe the current document ID and revision again after loading completes.
