# Production PNG, WebP and JPEG XL exports

Unreleased development feature. Choose **File → Production export…** to preview actual output, save named settings, and export a batch. The same engine plan, rendering and guarded publication serve the native UI, MCP `peerbrush_exports` and CLI `exports`.

## Output settings

Choose PNG, **WebP · lossless 8-bit** or **JPEG XL · native 8/16-bit**. JPEG XL defaults to lossless; its compact controls expose lossless/lossy, distance 0.1-15 (lower retains more detail) and effort 1-9. Effort trades encoding time for compression; it does not change the requested lossless contract. Quick File > Export JPEG XL uses lossless defaults. Presets store that format, a filename template, canvas or alpha bounds, transparent padding, sampling, duplicate-name policy and named size variants. Existing presets without a `format` field retain PNG behavior. Templates accept `{document}`, `{layer}` and `{variant}`. Invalid filename characters are replaced, trailing dots/spaces and reserved device names are handled, and names are bounded. Case-insensitive collisions stop planning unless numbered duplicates are explicitly chosen. File collision policy is separate: stop, skip or replace. Replacement retains the previous destination through the existing file-safety mechanism.

Illustrated **Lossless** and **Lossy** choices distinguish exact samples from reduced color detail. The adjacent preview shows the actual output for the chosen settings.

In **Filename template**, use **Document**, **Layer** or **Size variant** to insert a placeholder at the caret or replace selected text. The output list shows the resulting filenames.

Use **Duplicate names** to resolve two planned outputs with the same filename. Use **Existing files** to choose what happens when an output filename already exists in the destination folder. The trash button on a size variant removes that row from the export settings.

Canvas bounds retain document alignment. Alpha trimming uses actual rendered nonzero alpha, including low native16 alpha values; an entirely transparent target requires canvas bounds. Padding is measured in document pixels before scaling and may extend beyond the canvas. Output metadata reports `[left, top, right, bottom]` in document coordinates and actual rounded scale ratios. An output pixel center maps to `left + (x + 0.5) / scale_x`, `top + (y + 0.5) / scale_y`. Rendering is clipped to the document canvas; padding adds transparency rather than revealing off-canvas artwork.

Variants use scales from 0.01× to 8×. The **Sampling** buttons show how Nearest preserves hard pixel steps and how premultiplied-alpha Bilinear smooths them. The actual output preview uses the selected method. A batch supports up to 12 variants and 64 outputs. The native export planner allows up to 16,000 pixels per edge. The resident planner allows up to 8,192 pixels per edge and 32 × 1024 × 1024 pixels total. Rendering and encoding still depend on the document's resource budget. One batch runs at a time per instance, which keeps receipts for the latest eight completed runs.

## Source and precision

An empty target list exports the current document composite, including document filters. Selected layers/folders are canonical selected roots: selecting a folder and one of its descendants exports the folder once. Each root is rendered in isolation on transparency, with its own opacity, masks, effects, descendants and internal clipping. The selected root is made visible for this derived output; descendant visibility is retained. Ancestor folder effects/masks/opacity and document filters are excluded. A standalone adjustment or clipped layer requires its containing folder or the whole composite. External clipping references are rejected. Protected documents can export their preserved composite; unsupported editable layer recovery is not implied.

PNG files retain native 8/16-bit channels. WebP uses lossless RGBA8 encoding: native16 rendering, crop and resampling complete at the original depth, then only the derived output is projected to 8-bit channels. The working document, editable layers, history and later PSD/PSB/PNG saves retain their native precision. Alpha is retained at the output depth. PNG and WebP retain original ICC bytes, or embed the existing standard sRGB profile for tagged working RGB, without converting the output's RGB samples to another profile. Untagged working RGB remains untagged for those formats. Existing explicit sRGB-copy commands remain separate.

PNG file exports sample authoritative native-depth rows through the shared compositor and write them incrementally to the guarded temporary file. Crop, padding and resampling use the same native sample grid as other production output. The pixel transfer buffers cover about 128 KiPixels plus at most one row, below 2.125 MiB at native16 with current dimension limits; the PNG encoder also retains its row, chunk and compression buffers. Existing full effect, mask and document-filter preparation remains in place with its existing limits. APIs that return encoded PNG bytes still retain that encoded result in memory. This reduces output buffering; it does not bound total document memory or enable 16000 × 16000 documents. PSD/PSB streaming remains separate work.

JPEG XL renders, crops and resamples at the document native depth and encodes that same 8/16-bit RGBA depth. Lossless mode retains the supplied samples, including hidden RGB; lossy mode changes RGB but keeps alpha lossless. Supported RGB ICC profiles describe the supplied samples without implicit conversion. Lossless retains the exact profile. On decode, a differing default output profile selects explicit SDR sRGB; previews and reopening use the actual returned output ICC rather than attaching Original ICC to converted samples. Untagged working RGB is explicitly assigned sRGB for JXL. Reopening converts the actual decoded output profile through the existing native-depth import path. JXL is a single flattened image, separate from layered PSD/PSB saves.

Plans and output receipts report `format`, actual output `bit_depth`, and separate `native_source_depth`. Preview images show the actual encoded output as managed 8-bit PNG pixels with document coordinates, `output_format` and `output_bit_depth`; previews never become editable sources. A static exported WebP reopens through the existing image decoder as an 8-bit project. Supported original ICC profiles are then converted by the existing import color-management path. Animated WebP still requires an explicit frame; export creates a single flattened image.

The desktop preview follows the exact native document generation. Replacing that generation or switching between selected layers and the composite invalidates the previous preview. Switching back to the composite also clears an empty-selection error.

For a batch with several outputs, choose a filename above the preview to inspect that layer and size variant. Switching outputs reuses the current plan. Changing settings keeps the selected layer and variant when they still exist in the new plan. The preview reports the encoded byte count when it encodes a file, including JPEG XL. A render error for one output leaves the other planned outputs available to select.

The desktop batch button captures the exact native generation observed by the panel. If it changes before capture, the batch is refused before it creates a run or writes files. Desktop, CLI and MCP batches share planning, cancellation and guarded output publication.

If the operating system refuses to start a batch worker, the app reports the error, keeps the completed receipts and leaves the batch slot available for retry. A preview launch failure also reports an error without leaving the preview pending.

Presets are instance-local preferences outside document state, PSD sources and history. A preset update requires the observed library revision and rejects external file changes or corrupt libraries. Jobs bind to an observed project, document identity and revision. Source changes, closed projects, loaded source destinations, ownership conflicts and changed output files stop publication. Each output is written atomically, with cancellation checked before publication. PNG checks cancellation between strips, output rows and file writes; existing effect/filter preparation may finish its current operation first. WebP encoding may finish its current image before observing cancellation. JXL runs in the isolated child described in [image import](image-import.md#jpeg-xl-boundary-unreleased); control polling can terminate a child during transfer or encoding. A canceled image cannot publish. Cancellation retains completed exports and reports written, skipped, failed and unattempted outputs; it does not roll back completed files or alter the source. Closing the export panel leaves a batch running; reopening shows its latest receipt. Exiting the application requests cancellation.

## Protocol example

Observe the project and call `exports` with `action: "list"` before using stored presets. `save_preset` and `delete_preset` require `expected_library_revision`; plan/preview/start require the observed source identity and revision. This example uses synthetic identifiers and an existing destination directory:

```json
{
  "action": "start",
  "actor": "agent",
  "project_id": "observed-project",
  "document_id": "observed-document",
  "expected_revision": 12,
  "preset": "export-observed-preset",
  "expected_library_revision": 3,
  "targets": ["observed-folder"],
  "directory": "/existing/output",
  "existing": "error"
}
```

Poll `action: "status", run: "returned-run"`; use `cancel` with the owning actor or human input. `preview` accepts `output_index` and `max_edge` and returns actual PNG content. MCP returns image blocks plus text metadata; CLI writes preview PNGs into instance-local storage and returns paths with explicit coordinates. This feature does not establish Photoshop or other receiving-application round-trip fidelity; see [Photoshop fidelity](photoshop-fidelity.md).

For explicit settings, use `"format":"webp"` or `"format":"jxl","jxl":{"lossless":false,"distance":1.0,"effort":3}` alongside the existing template/crop/padding/resampling/duplicate-name/variants fields. Omitted format defaults to PNG; unsupported formats reject. The shared `document` action `export` selects PNG, WebP or JXL by the destination extension (.png/.webp/.jxl) and uses the same guarded writer. WebP never accepts a requested 16-bit output. A destination alias resolving to the other format rejects before publication.

## Acceptance coverage

Native lossless WebP export and native VP8L reopen share a 3 GiB encoded-file
limit. The row encoder verifies the complete container length before it writes
the output. Native reopen admits file and decoder resources separately.

Regression coverage passed in the combined run at `9b3eace`: native8/native16 alpha and profile retention, native-depth crop/resampling before WebP projection, old PNG preset loading, actual format-control/preset/batch interactions, PNG preview metadata, static reopening and animated frame choice, existing-file error/skip/replace, failed encoding, late cancellation, later human edits and loaded-source protection. Non-sRGB checks compare retained raw output samples/profile with the existing color-managed reopen path. Native Windows preview/direct/batch exports at both project depths retained original file bytes, native PNG samples and exact source/history/redo. macOS/Linux native export inspection and external receiving-application acceptance remain pending.

Hash-bound Windows headless application acceptance at clean 3bc148c passed lossless/lossy native8/16 JPEG XL export, inspect, actual encoded-preview, separate-project reopen and PNG readback, with exact alpha/source/history/overwrite guards. Synthetic sRGB lossy preview matched the reopened output display projection exactly; this does not qualify arbitrary ICC profiles or perceptual quality. The ordinary Rust suite stopped at a separate test-fixture compilation error, and updated source still requires combined/native verification. Default PNG/WebP presets omit the new JXL options from serialized settings to preserve their existing representation; customized JXL options persist. Cross-platform builds, clean installed-runtime execution and native workspace acceptance remain pending. A separate small Windows native-library proof is not application acceptance.

An unreleased Windows 0.4.0 headless build exported a 16,000 × 525 lossless sRGB RGB16 JPEG XL in 306.9 s and reopened it in 40.9 s. Both the independent libjxl decode and the PNG readback from the reopened project matched all 33,600,000 native RGBA samples. The replay also checked dimensions and source protection. These times describe this fixture on the Windows development host.
