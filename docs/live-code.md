# Live procedural editing

AI clients can run Rhai code directly inside a live PeerBrush project through `code` / `peerbrush_code`. Loops, functions, arithmetic and native pixel buffers allow procedural drawing and local pixel processing in one request. The CLI, HTTP and MCP transports use the same implementation; model choice remains with the client.

Observe the intended project first, then begin an AI task with the editing scopes you need. Start a run with the exact `project_id`, `document_id`, `expected_revision`, named AI `actor`, owned `task`, concise `description`, and 1–16 declared `scopes`. Every declared scope must fit inside that task's reservations. Scopes use layer IDs and optional document-pixel rectangles `[left,top,right,bottom]`, with exclusive right/bottom edges. A folder scope includes its descendants. Whole-image filters need an owned whole-document scope.

```json
{
  "action": "start",
  "actor": "drawing-agent",
  "project_id": "PROJECT_ID",
  "document_id": "DOCUMENT_ID",
  "expected_revision": 12,
  "task": "TASK_ID",
  "description": "Drawing the warm sky texture",
  "scopes": [{"target":"LAYER_ID","rect":[20,20,84,84]}],
  "script": "let patch = begin_pixels(\"LAYER_ID\", [20,20,84,84]); for y in 20..84 { for x in 20..84 { let v = document.channel_max; write_pixel(patch, x, y, [v, v / 2 + (x % 7), v / 8, v]); } } commit_pixels(patch);"
}
```

The response immediately returns a `run` ID. Poll `action:"status"` with the same project/document IDs, actor and run ID. A committed response includes one shared revision, applied-command count, scopes, actual PNG image content, and `document_rect`. MCP returns an image block; CLI writes feedback PNG/coordinate files as usual. `action:"cancel"` discards an unfinished run. A completed run stays committed and can be undone through ordinary history or selective task undo. The latest four run receipts remain in memory per project.

The native workspace marks the declared areas and activity in blue before execution, and shows the actual result when it commits. **AI tasks** shows running, committed or discarded runs, affected layers/coordinates, errors and a Cancel action. Human takeover revokes the task and stops unfinished code. Code never reacquires a released reservation. Human input can interrupt presentation animations; those animations do not delay shared state.

| Script interface | Behavior |
| --- | --- |
| `document` | Frozen width, height, bit depth, `channel_max`, and layer metadata. |
| `read_pixel(layer,x,y)` | Original raw paint-layer RGBA at document coordinates, as four native integers. Reads remain frozen throughout the run. |
| `begin_pixels(layer,[l,t,r,b])` | Creates a private native buffer initialized from the original paint layer, inside the canvas and declared scopes. Returns an integer handle. |
| `write_pixel(handle,x,y,[r,g,b,a])` | Replaces one buffer sample at document coordinates. Channels must be 0–255 or 0–65535 according to document depth. |
| `commit_pixels(handle)` | Queues a shared `pixels.replace` command and releases the private buffer. Commit every opened buffer before the script ends. |
| `edit(#{op:...,layer:..., ...})` | Queues an exposed shared editing command. It does not mutate the live document during script execution. |

Exposed commands include Paint, Shape, Gradient, Fill, Adjust, Move/Transform, layer property updates, effect/mask/filter stack edits, editable source updates and selection/refinement. Use the command names/settings in capabilities. Mutable library presets, file/image imports, raw storage access, lock changes and source-identity overrides are excluded. Pixel buffers target existing paint-layer color; use mask commands for masks and source properties for editable text/vector layers. Buffer reads use original sources, so overlapping buffers or later commands intentionally follow their queued order rather than reading a previous buffer's provisional changes.

Every successive command footprint is checked against declared scopes before preparing the result. The engine then validates native precision, selections, clipping, budgets, locks and reservations. Preparation runs outside the live document mutex. Immediately before committing one atomic undo entry, it rechecks the exact source revision and task ownership. Any newer source edit, close/replacement, cancellation, expiry, takeover, invalid command or uncaught script error discards the whole unfinished result. No conflict override is available, and later human work remains intact.

Limits: 64 KiB script, 2 million Rhai operations, five seconds of script evaluation, 32 commands, 262,144 cumulative patch pixels, eight open buffers and 8 MiB command output. At most one run per project and two per process execute simultaneously. Language limits bound variables, call/expression depth, functions and aggregate strings/arrays/maps. Preparation has a 30-second deadline checked between native commands and before commit; a native engine operation already in progress completes before cancellation/deadline rejection. All existing document/effect memory limits remain. Feedback is capped at 1024 pixels per edge. This is an embedded language interface, not an OS process sandbox or unrestricted Python/JavaScript execution. Modules, `eval`, filesystem/network and operating-system APIs are unavailable. Rhai's [safety limits](https://rhai.rs/book/engine/options.html) and [progress callback](https://rhai.rs/book/safety/progress.html) underpin evaluation limits.

Native `pixels.replace` is also an ordinary shared command: supply the exact document `bit_depth`, a bounded rectangle, and base64 interleaved RGBA bytes (8-bit channels or little-endian 16-bit words). It preserves other native samples and applies the current hard/soft selection. Locks, reservations, source guards, rollback, history and standard PSD/PNG saves remain the engine's responsibility. Code scripts and run receipts are absent from PSD sources and history; only editable results are retained.
