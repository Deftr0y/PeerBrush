# MCP and CLI

MCP starts automatically with PeerBrush. Click **Connect AI** to register the running executable and its exact workspace with detected Codex, Claude Desktop, Cursor and Gemini CLI clients. Settings are updated atomically with backups; unrelated server entries, comments and preferences are preserved. No model or remote service is started. Reload or restart the configured client and approve PeerBrush when prompted. Green means an attached MCP client; red means the editor is waiting for one. There is no separate MCP on/off switch. The stdio adapter attaches to the same live document. It can initialize and list tools while the editor is closed, and reconnects when it opens. Editing calls require the app to be running.

The neighboring settings button provides manual setup and recovery. Clients that require extra approval are reported there. A successful registration means the client has configuration; it does not mean the client is connected yet.

For manual Codex setup, register the executable you run:

```powershell
codex mcp add peerbrush -- "C:\path\to\PeerBrush\peerbrush.exe" mcp
```

Or add this to your Codex `config.toml` (replace the path for another checkout):

```toml
[mcp_servers.peerbrush]
command = 'C:\path\to\PeerBrush\peerbrush.exe'
args = ["mcp"]
```

Codex shares MCP configuration across desktop, CLI and IDE. Restart Codex or start a fresh session after registering to load the tools. [Official Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli).

Tools: `peerbrush_observe`, `peerbrush_edit`, `peerbrush_proposal`, `peerbrush_task`, `peerbrush_document`, `peerbrush_history`, `peerbrush_capabilities`, `peerbrush_place_image`, `peerbrush_segment`, `peerbrush_brushes`, and `peerbrush_projects`.

Use `peerbrush_projects` with `action:"list"` to discover stable project IDs. Observe the intended `project_id`, then supply that ID, its `document_id` and exact `expected_revision` for every targeted mutation. Background edits never select the visible tab. `projects:new/open` creates another independent project; only human input can request activation or select a tab. The older `document:new/open` actions remain explicit replacements with dirty/source guards. See [project actions and transfer examples](projects.md).

`peerbrush_brushes` lists the same 21 original presets and custom brushes as the native Brush window. Use `action:list` for stable IDs and full settings, or `action:preview,id:"graphite"` for a real pressure-stroke PNG. Preview coordinates are explicitly `brush_preview`, separate from document coordinates. `action:save` with `name`, `category` and `settings` creates a custom brush; add its `id` to update it. Only custom brushes can be deleted. These preferences live in the instance's `brushes.json` and do not dirty a document or enter undo history.

Paint, smudge, clone and heal commands accept `preset:"brush-pen"` with optional explicit setting overrides. Settings are frozen before footprint/reservation checks and proposal creation. `pressure_gamma` (0.1–4, default 1) applies a shared response curve to supplied pressure; size/opacity toggles and taper still operate independently. Use `smudge` for Blend presets to carry existing pigment. No proprietary brush-set import is claimed.

Observe before editing. Use layer IDs, document pixel coordinates, and `expected_revision`. The upper-left pixel is (0,0). Rectangles are `[left, top, right, bottom]`, with right and bottom excluded.

Color effects and mask steps accept `weight` in `effect.add/update` and `mask.step.add/update`. Use 0–1; omitted new weights default to 1 and omitted update weights keep the existing value. Weight blends the step's input with its result, independently of settings such as blur Radius or bloom Strength. Observe returns each weight. Native 16-bit samples, clipboard sources, PSD sources, reservations and undo share the same engine path.

```json
{"commands":[{"op":"paint","layer":"LAYER_ID","points":[[60,60],[180,120]],"radius":14,"color":[233,84,32,255]}],"expected_revision":0,"actor":"my-agent","label":"Paint orange accent"}
```

Optional selective reservation:

```json
{"action":"begin","actor":"my-agent","description":"Refining the character mask","scopes":[{"target":"LAYER_ID","rect":[20,20,240,240]}],"feedback":"batch"}
```

Pass the returned task ID with subsequent edits. Update or end it explicitly. Reservations expire after five idle minutes. If the user takes back control, stop and inspect; never silently reacquire.

Feedback defaults to an image after each edit batch. Set `feedback` to `request` to avoid automatic images, or `batch`/`always` to receive them. `max_edge` controls image size. Observation also supports `rect`, `layer`, `mask`, `image:false`, and `since_revision` to skip unchanged images. PNGs are delivered as actual MCP image content, not screenshots of interface chrome.

CLI examples (PowerShell):

```powershell
.\peerbrush.exe cli observe '{"max_edge":768}'
.\peerbrush.exe cli edit '@edit.json'
.\peerbrush.exe cli document '{"action":"export","path":"C:/art/result.png"}'
```

Use `--state-dir PATH` consistently for an isolated instance. CLI image results include absolute file paths plus a crop-coordinate manifest; base64 image data is not printed.

## Local discovery and HTTP

Connect AI writes a token-free manifest at `~/.peerbrush/mcp.json`. Run `peerbrush discover` to print it for a local agent. It includes the executable, stdio arguments, exact connection-file location, and an HTTP endpoint template. This is an explicit discovery file; MCP does not make all agents scan or connect automatically.

Direct clients read `url` and `token` from that workspace's `connection.json` and POST to `${url}/mcp` using `Authorization: Bearer <token>`. The service binds only to 127.0.0.1 and validates host and origin. Keep the token local. `/rpc` remains the application/CLI bridge; it is not the public MCP transport.

Streamable HTTP supports **2026-07-28**, **2025-11-25**, and **2025-06-18**. Modern requests carry protocol/client metadata and `Mcp-Method` / `Mcp-Name` headers; legacy clients initialize and use a session ID. Notifications return an empty HTTP 202 response. Legacy sessions can be deleted. Unsupported versions and mismatched headers return protocol errors. See the official [modern transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http) and [legacy transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports) specifications.

Local Claude Desktop, Cursor and Gemini setup follows their [Claude guide](https://modelcontextprotocol.io/docs/develop/connect-local-servers), [Cursor documentation](https://cursor.com/docs/mcp), and [Gemini CLI documentation](https://geminicli.com/docs/tools/mcp-server/). Unsupported clients can use the manifest or manual stdio configuration. Remote/cloud agents require a separately configured bridge; this release does not publish a public server or call models itself.


## Artistic iteration and visible activity

Observe → edit a small batch → inspect the actual result → undo if it is weak → refine and retry. Use redo to compare versions. Do not assume an initial edit is perfect. Publish concise natural-language progress with `peerbrush_task` begin/update descriptions; an empty `scopes` array publishes activity without locking anything.

Use the same unique `actor` in every call. AI chronological undo/redo requires the current revision and only reverses its own latest batch. Human undo is chronological and can reverse either participant's latest batch. To undo an agent task across interleaved edits, inspect its inverse with `history` action `inspect_task`, then use `undo_task` with the same `task` ID and current `expected_revision`. `list` includes tasks and their active batch counts. Human calls can set `task_actor` to choose an agent; agents can compensate only their own tasks.

Selective undo preserves unrelated pixel edits, mask pixels, source properties and individual effect settings. Inspection returns affected `scopes`, batch revisions, `can_undo` and structured `conflicts` with target, field and document rectangle where available. Overlapping changes, later edits to task-created sources, incompatible frames/hierarchy, reservations and protected documents prevent the entire undo. There is no conflict override. A task whose batches were evicted from the 40-step in-memory history is refused rather than partially undone. History resets on new/open; saved PSD source data does not contain task history.

The task inverse commits as one new history step. Undoing that compensation restores exactly the document immediately before task undo; redo applies it again. New edits still clear redo. Native 16-bit pixels remain native; comparisons do not use display projections. The native **AI tasks** menu exposes the same scope/conflict review and operation.

```json
{"action":"undo","actor":"my-agent","expected_revision":7,"max_edge":768}
```

Undo and redo return PNG feedback by default; use `feedback:"request"` to suppress it. Ordinary edit batches group into one history entry. A new edit after undo clears redo history.

Stdio client presence uses a heartbeat and disconnects at EOF. Abruptly killed adapters and idle direct HTTP clients expire. The green indicator means client presence, not merely that the local server is listening.

## Optional proposed edits

Direct edits remain available. To offer a result for human review, call `peerbrush_proposal` with `action:"create"`, the observed `document_id`, exact `expected_revision`, `actor`, optional `task`/`label`, and an ordinary `commands` batch. The engine prepares a frozen native-depth draft without changing project pixels, revision, dirty state or history. Default feedback includes actual image content with `document_rect`, `proposal` and `preview_only:true`; `feedback:"request"` suppresses the image. `action:"preview"` with the returned proposal ID always returns draft image content. CLI uses the same actions and writes its PNG feedback to the isolated workspace.

```json
{"action":"create","actor":"my-agent","document_id":"DOCUMENT_ID","expected_revision":7,"label":"Softer background","commands":[{"op":"layer.update","layer":"LAYER_ID","opacity":0.7}]}
```

The native **AI tasks** menu opens a blue review with scopes and Original/Proposed result views. Accept becomes available after the proposed canvas renders. Acceptance requires `actor:"human"`, the reviewed `document_id` and exact `expected_revision`; it commits the frozen result as one AI-authored undoable edit. Local clients supply actor strings as in existing edit/history calls; this is a cooperative workspace convention, not authentication of human identity. Reject releases the draft without an edit. Close/Escape returns to the current project and leaves the proposal pending. Human canvas input closes review before editing.

Any project edit makes pending drafts stale, even if unrelated. New/open clears them. Expiry, task end and takeover release draft pixels; acceptance rechecks current reservations. An external image file changing after preparation cannot change the reviewed result. Task-bound proposals require the task to stay active during review; a finished reservation is never silently restarted. Up to four proposals share COW sources with a 256 MiB additional unique native-tile budget; each batch is bounded to 100 commands/8 MiB, expires after 15 minutes, and retains only bounded transient metadata afterward. Drafts and task-recovery records are absent from PSD sources and autosave.

`peerbrush_task` action `recovery` reports active scopes and the latest 100 ended, expired or taken-over tasks. The menu shows recent interruptions and existing task undo. Already committed AI edits and human work remain; choose an explicit selective task compensation when needed. See [collaboration behavior](collaboration.md).

## Generated and edited image placement

Use `peerbrush_place_image` to insert an image in one call. Pass exactly one absolute local PNG/JPEG `path` or base64 PNG `png`, plus an exact destination `rect`. The upper-left corner and size are document pixels, independent of UI zoom/pan. Placement resizes with premultiplied interpolation and is one undoable edit.

```json
{"path":"C:/art/generated.png","rect":[100,80,500,380],"layer":"CONTEXT_LAYER_OR_FOLDER_ID","new_layer":true,"name":"Generated detail","actor":"my-agent","expected_revision":7,"max_edge":768}
```

With `new_layer:true` (default), a folder context places a child inside it; a paint-layer context places a sibling above it. `parent` overrides the folder, and `parent:null` chooses the root. Without context it creates a root layer. The returned `placement.layer` is the new layer ID; `placement.rect` and the feedback view report exact coordinates.

For an edited patch, set `new_layer:false` and choose the destination `layer`. `mode:"over"` composites the pixels; `mode:"replace"` replaces the whole rectangular area, including transparent pixels. Pixels outside that rectangle, masks, and editable effects remain. The raster grows if necessary rather than clipping the patch silently. Layer/folder locks, reservations and conflicting revisions still apply.

CLI uses the same operation: `peerbrush.exe cli place_image '@placement.json'`. Atomic multi-command batches can use `{"op":"image.place", ...}` through `peerbrush_edit`; that command also supports natural source size at integer `x`/`y` when `rect` is omitted. CLI feedback contains a PNG path and coordinate manifest instead of printing encoded image data.

Blue marks show affected layers, relevant tools, and rectangular AI work areas. Agent edits animate as presentation feedback without delaying the committed shared state. Observe returns committed pixels and revision, so agents never have to wait for an interface animation to finish.

The Windows development helper `scripts/dev.ps1 run` uses the checkout's `.runtime` workspace rather than the packaged executable's default workspace. Automatic Connect AI registration and the settings panel's copied configuration include that instance's absolute `--state-dir`. If registering it manually, pass the same directory:

```powershell
codex mcp add peerbrush -- "C:\path\to\PeerBrush\target\release\peerbrush.exe" mcp --state-dir "C:\path\to\PeerBrush\.runtime"
```

## Hierarchy and fill commands

`peerbrush_edit` shares the same engine operations as the interface:

```json
{"commands":[{"op":"layer.duplicate","layers":["LAYER_OR_FOLDER_ID"]}],"actor":"my-agent","expected_revision":7}
```

```json
{"commands":[{"op":"group.create_selected","layers":["LAYER_A","LAYER_B"],"name":"Details"}],"actor":"my-agent","expected_revision":7}
```

```json
{"commands":[{"op":"layer.merge","layers":["UPPER_ID","LOWER_ID"],"name":"Merged detail"}],"actor":"my-agent","expected_revision":7}
```

Merge bakes the selected sources, masks and effects into a regular raster layer. A selected folder flattens its children; one selected raster merges with the sibling below it. Multiple roots must be adjacent siblings with normal external blend, so unrelated layers and backdrop-dependent blends are not silently changed. Use a folder to retain internal blend interactions before flattening. Rendering happens outside the shared engine lock for a single merge request; commit requires the document to remain unchanged. Undo restores the editable sources.

Regular layers and folders are the creation choices. `paint.fill` floods a regular layer, or clips to the current selection. A rotated selection is returned as `selection_polygon` along with its bounding `selection` rectangle. Use observation to get the current exact shape and revision before editing it.


## Painting and color tools

`peerbrush_capabilities` returns brush tip/settings, all blend modes and runnable commands. Use `paint` with `tip:dry|chalk|grain|bristle`, density/grain, pressure toggles and taper distances. Pressure points accept `[x,y,p]` with `p` in0–1; plain points remain compatible. `smudge` adds wetness/load/pickup and uses the same selection, layer and mask coordinates.

`adjustment.add` accepts `layers`, `kind` and editable `settings`. A paint target gets a clipped adjustment; a folder target gets a scoped child; multiple roots become a shared folder. An existing pixel selection becomes its mask. `layer.clip` sets `base` to the next unclipped sibling below; `base:null` releases clipping. Preserve whole clipping units when copying or reorganizing.

Color effects add `color_balance`, `hsl`, `bloom` and `liquify`. Balance uses shadows/midtones/highlights RGB complementary-axis arrays in −1–1, with preserve_luminosity. HSL uses hue degrees −180–180 and saturation/lightness −1–1. Bloom uses threshold 0–1, spread 0–64 document pixels and strength 0–3.

`liquify.stroke` accepts document-space points, mode push/expand/pinch/restore, radius and strength. It appends to the latest enabled liquify effect, or creates one; optional `effect` chooses an existing effect ID. Stored strokes use layer-local coordinates and retain their selection clipping. Inspect the result, undo a poor stroke, revise and retry. `effect.update` edits the retained source settings after later work.

Supported 8-bit and 16-bit RGB PSD raster layers, masks and default grouped raster clipping remain editable at their native depth. Raster clipping relationships appear in each observed layer's `clip_to` and use ordinary shared commands, reservations and history. Standard PSD saves preserve those raster stacks; adjustment layers and clipping involving folders still require a standard composite bake. Unsupported Photoshop structures open protected from their saved composite, with specific reasons in `document.warnings`. Document action `compatible_copy` explicitly creates a flattened editable project at the original bit depth with no source filename, so saving requires a new path. This copy preserves channel precision; unsupported Photoshop source structure remains in the original file. See [current fidelity limits](photoshop-fidelity.md).

`layer.update` accepts `blend:"pass_through"` on folders. Nested children and adjustment layers use the outside backdrop until an isolated ancestor; folder opacity/masks attenuate their combined change. New folders retain Normal by default. Enabled folder color effects, clipping to/from a pass-through folder and merging it are explicitly rejected. Raster-only pass-through trees save standard editable PSD dividers/layers/masks. Supplementary format 10 preserves PeerBrush pass-through sources, including adjustments; older readers protect that source topology. Human previews, shared reservations, native-depth clipboard and undo use ordinary engine commands.

Native 16-bit projects retain channel precision in the shared engine and PSD/PNG files. Create them with `peerbrush_document` action `new`, `bit_depth:16`. Observation images are compact 8-bit PNG projections. `peerbrush_capabilities.rendering` reports the actual adapter, supported automatic GPU effects and successful dispatch count; a CPU-only/headless process does not claim an attached GPU.


### Selection and project settings

`selection` accepts `kind` (`rectangle`, `ellipse`, `lasso`, `polygon`, `magnetic`, `wand`, `quick`, `object`, `row`, `column`), `mode` (`replace`, `add`, `subtract`, `intersect`), `rect` or `polygon`, `feather` (0–64 px), and color sampling options `layer`, `sample_merged`, `tolerance` (0–255), `contiguous`, `point`, `points` and `radius`. Polygon/magnetic geometry is supplied as document-coordinate points. Row/column clients supply their explicit one-pixel rectangle. Clear with `selection.clear`; restore with `selection.reselect`; invert with `selection.invert`; expand/contract with `selection.modify`, `mode` and `radius`. Empty selection bounds mean zero selected pixels, not deselection.

`document.settings` accepts `width`, `height` and `bit_depth` (8 or 16), preserves layer content/positions, and commits one history item. It requires document-wide access. A 16-to-8 conversion is intentional quantization. `liquify.stroke` accepts an optional `effect` ID and captures selection coverage. Visibility-only `layer.update` commands (`op`, `layer`, `visible` only) remain usable during pixel reservations and do not conflict with pending pixel revisions. Other layer fields retain ordinary checks.

`clone` and `heal` use explicit document-pixel source anchors, optional source layers or merged artwork, shared brush settings and native 16-bit sampling. Include `source_revision` and `document_id` to require the exact sampled state. See [retouch commands](retouch.md).

Whole-document `crop`, `canvas.resize` and `image.resize` commands share native previews and history. They use document pixels, whole-document reservations, original channel depth and optional exact source guards. See [geometry commands](geometry.md).

Editable text/vector layers share `source.add`, `source.update` and `source.rasterize`. Observation exposes the complete `source` definition on each layer; retain its frame/matrix when updating content. Sources use native RGBA16 colors and explicit document origins, preserve masks/effects, and return ordinary rendered PNG feedback with coordinates. Unsupported fonts/scripts and pixel edits into editable color sources are refused. See [source schemas and limits](editable-sources.md).
Whole-layer `transform` accumulates placement and samples retained native color/mask originals across gestures. Observation returns bounded `transform_source` metadata without tile samples; retained originals survive whole-layer clipboard and private PSD sources. Later pixel edits establish a new baseline for that raster. Use `selection_only:false` for a whole layer when a selection is active. See [retained transform behavior](retained-transforms.md).

`peerbrush_document` also accepts `open_async` and `cancel_open`. Observe `state.loading` for stage/completed/total and `state.file_status` for completion or failure; editing clients see only a complete current document. Opening requires saved work or explicit discard, and guards the captured document/revision through replacement. See [large-document limits](large-documents.md).


RGB matrix ICC PSDs have an sRGB display preview while their original samples/profile remain protected. Observe `document.color_profile` for `source`, `preview`, `conversion_available` and any unsupported-profile reason. To create an editable copy, use `{"action":"compatible_copy","convert_to_srgb":true,"expected_revision":REV}`. Conversion keeps the source bit depth and alpha, creates a new document, clears the save path and tags converted PSD/PNG outputs as sRGB. Out-of-gamut values clip under relative colorimetric intent. Missing explicit conversion or unsupported profiles reject atomically. Export of a protected original preserves its native samples and original ICC tag. See [ICC boundaries](photoshop-fidelity.md#rgb-icc-previews-and-copies).


RGB matrix ICC PSDs have an sRGB display preview while their original samples/profile remain protected. Observe `document.color_profile` for `source`, `preview`, `conversion_available` and any unsupported-profile reason. To create an editable copy, use `{"action":"compatible_copy","convert_to_srgb":true,"expected_revision":REV}`. Conversion keeps the source bit depth and alpha, creates a new document, clears the save path and tags converted PSD/PNG outputs as sRGB. Out-of-gamut values clip under relative colorimetric intent. Missing explicit conversion or unsupported profiles reject atomically. Export of a protected original preserves its native samples and original ICC tag. See [ICC boundaries](photoshop-fidelity.md#rgb-icc-previews-and-copies).
