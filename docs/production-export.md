# Production PNG exports

Unreleased development feature. Choose **File → Production export…** to preview actual output, save named settings, and export a batch. The same engine plan, rendering and guarded publication serve the native UI, MCP `peerbrush_exports` and CLI `exports`.

## Output settings

Presets store a filename template, canvas or alpha bounds, transparent padding, sampling, duplicate-name policy and named size variants. Templates accept `{document}`, `{layer}` and `{variant}`. Invalid filename characters are replaced, trailing dots/spaces and reserved device names are handled, and names are bounded. Case-insensitive collisions stop planning unless numbered duplicates are explicitly chosen. File collision policy is separate: stop, skip or replace. Replacement retains the previous destination through the existing file-safety mechanism.

Canvas bounds retain document alignment. Alpha trimming uses actual rendered nonzero alpha, including low native16 alpha values; an entirely transparent target requires canvas bounds. Padding is measured in document pixels before scaling and may extend beyond the canvas. Output metadata reports `[left, top, right, bottom]` in document coordinates and actual rounded scale ratios. An output pixel center maps to `left + (x + 0.5) / scale_x`, `top + (y + 0.5) / scale_y`. Rendering is clipped to the document canvas; padding adds transparency rather than revealing off-canvas artwork.

Variants use scales from 0.01× to 8×, with nearest or premultiplied-alpha bilinear sampling. The maximum is 12 variants and 64 outputs per batch, each bounded to an 8192-pixel edge and 32 million pixels. Only one batch runs per instance; completed receipts for the latest eight runs remain available during that instance.

## Source and precision

An empty target list exports the current document composite, including document filters. Selected layers/folders are canonical selected roots: selecting a folder and one of its descendants exports the folder once. Each root is rendered in isolation on transparency, with its own opacity, masks, effects, descendants and internal clipping. The selected root is made visible for this derived output; descendant visibility is retained. Ancestor folder effects/masks/opacity and document filters are excluded. A standalone adjustment or clipped layer requires its containing folder or the whole composite. External clipping references are rejected. Protected documents can export their preserved composite; unsupported editable layer recovery is not implied.

PNG files retain native 8/16-bit channels and original ICC bytes, or the existing sRGB tag behavior. Presets do not convert depth or profiles. Preview images may be projected to managed 8-bit pixels and include the original depth and document coordinates; previews never become editable sources. Existing explicit sRGB-copy commands remain separate.

Presets are instance-local preferences outside document state, PSD sources and history. A preset update requires the observed library revision and rejects external file changes or corrupt libraries. Jobs bind to an observed project, document identity and revision. Source changes, closed projects, loaded source destinations, ownership conflicts and changed output files stop publication. Each output is written atomically, with cancellation checked before publication. Cancellation retains completed PNGs and reports written, skipped, failed and unattempted outputs; it does not roll back completed files or alter the source. Closing the export panel leaves a batch running; reopening shows its latest receipt. Exiting the application requests cancellation.

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
