# MCP and CLI

PeerBrush 0.2 supports `peerbrush_code` / CLI `code` for live procedural editing with native pixel buffers, loops and shared commands. Start with an exact project/source revision, owned AI task, description and declared scopes; poll the run for actual image content and document coordinates. Cancellation, takeover and source changes discard unfinished work. See the [code interface and limits](live-code.md).

MCP starts automatically with PeerBrush. Click **Connect AI** to register the running executable and its exact workspace with detected Codex, Claude Desktop, Cursor and Gemini CLI clients. Settings are updated atomically with backups; unrelated server entries, comments and preferences are preserved. No model or remote service is started. Reload or restart the configured client and approve PeerBrush when prompted. There is no separate MCP on/off switch. The stdio adapter attaches to the same live document. It can initialize and list tools while the editor is closed, and reconnects when it opens. Editing calls require the app to be running.

Unreleased: green **MCP available** means the local server is ready, including before a client connects. Green **MCP connected** confirms a live client; expired client presence returns to **MCP available**. Red **MCP unavailable** means the listening worker is unavailable, even if previous client records remain. The status tooltip distinguishes availability from connection; setup/retry results remain independent. Released builds used green for an attached client and red while waiting.

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

Unreleased: `peerbrush_document` / CLI `document` with `action:"save_report"`, observed `document_id` and `expected_revision` returns a view-only [PSD/PSB compatibility report](photoshop-fidelity.md#save-compatibility-report-unreleased). Include `project_id` for an explicit project. The report derives standard layer representation, bounds, baking and supplementary-source limitations from the same plan as the encoder; `has_editability_losses` flags those limits. No file is written, no profile is converted and history stays unchanged. A stale source is refused. When losses are reported, saving requires `accept_editability_loss:true` for the same `document_id` and exact `expected_revision`. Acceptance is a boolean and grants no permission for later revisions. Independently confirm `overwrite:true` for an existing Save As destination. Both guards preserve protected originals, loaded-file conflicts and changes during encoding. Receiving-application validation remains pending.

Tools: `peerbrush_observe`, `peerbrush_edit`, `peerbrush_proposal`, `peerbrush_task`, `peerbrush_document`, `peerbrush_history`, `peerbrush_capabilities`, `peerbrush_place_image`, `peerbrush_segment`, `peerbrush_brushes`, and `peerbrush_projects`.

Use `peerbrush_projects` with `action:"list"` to discover stable project IDs. Observe the intended `project_id`, then supply that ID, its `document_id` and exact `expected_revision` for every targeted mutation. Background edits never select the visible tab. `projects:new/open` creates another independent project; only human input can request activation or select a tab. The older `document:new/open` actions remain explicit replacements with dirty/source guards. See [project actions and transfer examples](projects.md).

`peerbrush_brushes` lists the same 21 original presets and custom brushes as the native Brush window. Use `action:list` for stable IDs and full settings, or `action:preview,id:"graphite"` for a real pressure-stroke PNG. Preview coordinates are explicitly `brush_preview`, separate from document coordinates. `action:save` with `name`, `category` and `settings` creates a custom brush; add its `id` to update it. Only custom brushes can be deleted. These preferences live in the instance's `brushes.json` and do not dirty a document or enter undo history.

Unreleased: `peerbrush_tool_settings` (CLI `tool_settings`) shares independent native tool preferences across this instance's project tabs. `action:"get"` lists profiles; add `tool` for Brush, Eraser, Smudge, Clone, Heal or Liquify (`brush`, `eraser`, `smudge`, `clone`, `heal`, `liquify`). Read the tool's `revision`, then send `action:"set"`, `actor`, `expected_tool_revision` and partial `settings`. A `preset` copies that preset's settings into this tool; explicit fields override them, and `preset:null` clears its association. Stale agent updates reject atomically. Liquify accepts only `radius`, `strength` and `mode` (`push`, `expand`, `pinch`, `restore`). Human controls merge only fields they changed, preserving newer independent fields.

```json
{"action":"set","tool":"eraser","actor":"my-agent","expected_tool_revision":0,"settings":{"radius":24,"opacity":0.4,"hardness":0.6}}
```

`action:"preview",tool:"eraser"` returns actual brush-stroke PNG content with `[0,0,240,48]` bounds in `brush_preview` coordinates. Liquify needs an explicitly targeted document stroke for rendered feedback. Preferences affect the next native gesture but never switch the visible tool, paint, dirty the document, or enter history/PSD data. They persist in the configured preferences directory as validated, guarded tool settings; a store without a configured path keeps them in memory. Default launches use the [per-user settings profile](settings.md), while explicit `--state-dir` workspaces retain instance-local preferences. Completed human controls flush once after release; agent updates save before publishing a new preference revision. Malformed or externally changed files are preserved and save failures are reported. Saved custom presets remain a separate persistent library. To use a profile through editing APIs, pass its returned settings explicitly to the corresponding shared stroke command with the observed project/document revision and reservations. Preference revisions are independent of document revisions. Active human gestures keep their captured settings until completion. AI updates briefly animate numeric controls in blue; human input stops the presentation animation without undoing the actual preferences.

Paint, smudge, clone and heal commands accept `preset:"brush-pen"` with optional explicit setting overrides. Settings are frozen before footprint/reservation checks and proposal creation. `pressure_gamma` (0.1–4, default 1) applies a shared response curve after normalizing supplied pressure between `pressure_min` and `pressure_max` (0–1, defaults 0 and 1, minimum gap 0.01); size/opacity toggles and taper still operate independently. Use `smudge` for Blend presets to carry existing pigment. No proprietary brush-set import is claimed.

`peerbrush_exports` shares PNG/WebP production presets, rendered previews and cancellable selected layer/folder batches with the File menu and CLI `exports`. Presets live outside document history. Preview/start require observed source identity and revision; preset updates require the observed library revision. `settings.format` defaults to `png`, including old presets. PNG retains native 8/16-bit channels; lossless `webp` explicitly produces 8-bit RGBA after native-depth rendering/resampling, retaining the working document's original precision. Both retain alpha and original ICC bytes without implicit profile conversion. Plans/receipts distinguish `format`, output `bit_depth` and `native_source_depth`, report document bounds, and use explicit duplicate-name and existing-file policies. MCP previews return actual encoded output as PNG image content with `output_format` and `output_bit_depth`; CLI writes those preview PNGs into instance-local storage and returns their paths and coordinates. `document` action `export` uses the `.png` or `.webp` destination extension and the same guards. Status/cancel report each output without changing editable sources. See [production export settings and limits](production-export.md).

Observe before editing. Use layer IDs, document pixel coordinates, and `expected_revision`. The upper-left pixel is (0,0). Rectangles are `[left, top, right, bottom]`, with right and bottom excluded.

`peerbrush_filters` lists whole-image presets, renders thumbnails in `filter_thumbnail` coordinates, and previews an explicitly targeted current project without history. Preview returns actual PNG content with `document_rect` and frozen commands to apply through `peerbrush_edit`. `filter.add/update/delete/reorder` processes the entire composite and uses document-wide reservations, locks and revision guards. Observe `document.filters`; settings, strength and bypass remain editable at native 8/16-bit depth. Custom preset save/rename/delete/import/export lives outside history. See [whole-image filter behavior and limits](filters.md).

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

Unreleased: `peerbrush discover --status` returns versioned JSON with separate manifest, local-listener/authentication and detected-client configuration results. It validates the published stdio route, checks that its executable/alias exists, inspects client registration without writing it, and sends one authenticated, read-only ping to that exact workspace. It never launches or focuses PeerBrush, registers client presence, changes settings or sends an edit. The `ready` connection category means the authenticated bridge answered; it does not prove a client connected, approved the server or used the same application version as this diagnostic executable. The original `peerbrush discover` manifest output remains available unchanged.

For a deterministic connection, run `peerbrush discover --status`, follow its fixed recovery instructions if needed, then use the returned `stdio.command` and `stdio.args` exactly. Initialize the adapter, list its tools and observe the intended project before editing. An explicit `--state-dir` must match the published route; a mismatch is reported before probing another instance. Missing/invalid manifests, missing executables, missing/unreadable/invalid connection metadata, an unreachable listener, stale authentication, interrupted requests and invalid replies have separate categories. An unreachable listener alone cannot establish whether the app is closed or still starting. Matching client configuration likewise does not prove a live connection; reload/restart and client approval may still be needed. Gemini allow/exclude policy blocks are reported without changing the policy.

Status output includes local executable/configuration paths and exact stdio arguments for use on that computer, but never bearer tokens, connection-file contents, client configuration contents or raw transport errors. A valid diagnostic command returns JSON even when a connection needs recovery. It does not claim representative smaller-model/client acceptance; those first-setup/restart trials remain pending. Local authenticated requests accept only the canonical `http://127.0.0.1:PORT` origin, disable redirects and environment proxies, and retain normal engine/tool errors for actual editing calls. After an interrupted edit, observe before retrying because the operation may already have completed.

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

Stdio client presence uses a heartbeat and disconnects at EOF. Abruptly killed adapters and idle direct HTTP clients expire. In the unreleased status UI, green MCP available means the listener is ready and green MCP connected additionally confirms client presence, as described above.

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

`posterize` uses an integer `levels` from 2 to 256 (default 4). It selects the nearest evenly spaced RGB channel levels at native 8/16-bit precision, preserves alpha and editable source pixels, and supports the ordinary independent `weight`, bypass, reorder, removal and preview/undo guards. It works on layers, isolated folders and explicitly requested adjustment layers; it is excluded from mask stacks. Fractional/out-of-range Levels reject atomically. PSD source format 13 prevents older readers from silently treating these sources as editable; standard rendered channels and the current merged image remain present.

```json
{"op":"effect.add","layer":"LAYER_ID","kind":"posterize","settings":{"levels":4},"weight":0.75}
```

`channel_clamp` selects one `channel`: `r`, `g`, `b` or `o` (opacity/alpha). Normalized `minimum` and `maximum` are 0–1, default 0 and 1. Bounds convert once to the nearest native sample; values inside them and all other channels stay exact at full strength. Reversed bounds, wrong types and unknown channels reject atomically. Stack instances for independent per-channel limits. Regular layer and isolated folder color stacks support ordinary strength, editable settings, bypass, removal and shared preview/undo/source/reservation guards. Mask and adjustment stacks exclude it; clipped adjustments preserve base opacity. Opacity increases cover empty tiles in the stored layer rectangle or folder's canvas frame. PSD source format 14 protects older readers while retaining standard rendered raster/mask channels and the current merged image.

```json
{"op":"effect.add","layer":"LAYER_ID","kind":"channel_clamp","settings":{"channel":"r","minimum":0.2,"maximum":0.8}}
```

`liquify.stroke` accepts document-space points, mode push/expand/pinch/restore, radius and strength. It appends to the latest enabled liquify effect, or creates one; optional `effect` chooses an existing effect ID. Stored strokes use layer-local coordinates and retain their selection clipping. Inspect the result, undo a poor stroke, revise and retry. `effect.update` edits the retained source settings after later work.

Supported 8-bit and 16-bit RGB PSD raster layers, masks and default grouped raster clipping remain editable at their native depth. Raster clipping relationships appear in each observed layer's `clip_to` and use ordinary shared commands, reservations and history. Standard PSD saves preserve those raster stacks; adjustment layers and clipping involving folders still require a standard composite bake. Unsupported Photoshop structures open protected from their saved composite, with specific reasons in `document.warnings`. Document action `compatible_copy` explicitly creates a flattened editable project at the original bit depth with no source filename, so saving requires a new path. This copy preserves channel precision; unsupported Photoshop source structure remains in the original file. See [current fidelity limits](photoshop-fidelity.md).

`layer.update` accepts `blend:"pass_through"` on folders. Nested children and adjustment layers use the outside backdrop until an isolated ancestor; folder opacity/masks attenuate their combined change. New folders retain Normal by default. Enabled folder color effects, clipping to/from a pass-through folder and merging it are explicitly rejected. Raster-only pass-through trees save standard editable PSD dividers/layers/masks. Supplementary format 10 preserves PeerBrush pass-through sources, including adjustments; older readers protect that source topology. Human previews, shared reservations, native-depth clipboard and undo use ordinary engine commands.

Native 16-bit projects retain channel precision in the shared engine and PSD/PNG files. Create them with `peerbrush_document` action `new`, `bit_depth:16`. Observation images are compact 8-bit PNG projections. `peerbrush_capabilities.rendering` reports the actual adapter, supported automatic GPU effects and successful dispatch count; a CPU-only/headless process does not claim an attached GPU.


### Selection and project settings

`selection` accepts `kind` (`rectangle`, `ellipse`, `lasso`, `polygon`, `magnetic`, `wand`, `quick`, `object`, `row`, `column`), `mode` (`replace`, `add`, `subtract`, `intersect`), `rect` or `polygon`, `feather` (0–64 px), and color sampling options `layer`, `sample_merged`, `tolerance` (0–255), optional boolean `color_similarity`, `contiguous`, `point`, `points` and `radius`. Polygon/magnetic geometry is supplied as document-coordinate points. Row/column clients supply their explicit one-pixel rectangle. Clear with `selection.clear`; restore with `selection.reselect`; invert with `selection.invert`; expand/contract with `selection.modify`, `mode` and `radius`. Empty selection bounds mean zero selected pixels, not deselection. Unreleased `color_similarity:true` uses the maximum absolute rendered RGB-channel/alpha difference; `false` or omission retains existing root-mean-square RGB distance with alpha difference. Enabled-mode tolerance must be a finite number from 0 to 255; flags must be boolean. Wand, Quick Selection and Object region share the metric. Native artwork is retained. The unreleased UI labels `sample_merged` as **All Layers** and defaults it off. Clients supply the explicit active `layer` ID to sample that layer; `sample_merged:true` samples the rendered composite. The shared sampling interpretation is unchanged.

Unreleased development: `selection.wand_update` changes `tolerance`, `feather`, `contiguous`, `sample_merged` or `color_similarity` from the most recent wand seed and its original replace/add/subtract/intersect baseline. Use `document_id` and `source_revision` guards. Artwork or other selection edits retire that source; click again to establish a new one. Undo/redo restores the corresponding runtime source. PSD saves retain current selection coverage and native artwork, but do not save a replay seed; reopening requires a new wand click. UI drags preview an actual selected-pixel cutout and commit once on release; Escape or concurrent edits cancel the draft. The **Live updates** checkbox defaults on; disabling it preserves the selection while changing controls, and enabling it applies those settings to a valid seed.

Unreleased development: `mask.add` initializes a new mask from an active selection's exact coverage in document coordinates. Selected regions reveal content, unselected regions hide it, and feathered confidence remains graded. An active empty selection creates a fully hiding mask. Current native color channels and existing masks are preserved; the edit is one undo step. With no selection, `value` initializes the usual fill and paint steps. Selection-dependent creation and `mask.from_selection` also respect `@selection` reservations. Include `document_id`/`source_revision` when acting on an observed selection.

`document.settings` accepts `width`, `height` and `bit_depth` (8 or 16), preserves layer content/positions, and commits one history item. It requires document-wide access. A 16-to-8 conversion is intentional quantization. `liquify.stroke` accepts an optional `effect` ID and captures selection coverage. Visibility-only `layer.update` commands (`op`, `layer`, `visible` only) remain usable during pixel reservations and do not conflict with pending pixel revisions. Other layer fields retain ordinary checks.

`clone` and `heal` use explicit document-pixel source anchors, optional source layers or merged artwork, shared brush settings and native 16-bit sampling. Include `source_revision` and `document_id` to require the exact sampled state. See [retouch commands](retouch.md).

Whole-document `crop`, `canvas.resize` and `image.resize` commands share native previews and history. They use document pixels, whole-document reservations, original channel depth and optional exact source guards. See [geometry commands](geometry.md).

Editable text/vector layers share `source.add`, `source.update` and `source.rasterize`. Observation exposes the complete `source` definition on each layer; retain its frame/matrix when updating content. Sources use native RGBA16 colors and explicit document origins, preserve masks/effects, and return ordinary rendered PNG feedback with coordinates. Unsupported fonts/scripts and pixel edits into editable color sources are refused. See [source schemas and limits](editable-sources.md).

Unreleased: [Pen commands](pen.md) create/update native Bézier sources, add/move/delete/convert anchors, move paired handles, split segments and convert closed contours to selections/masks. Positions and handles in `path.*` commands and observed `path_document_nodes` use document pixels, including after affine placement. Supplementary `source.content` uses its source frame/matrix. `capabilities.paths` advertises commands and limits. Shared locks, reservations, guarded revisions, native 8/16-bit raster channels and one undo per edit apply. Format-17 sources retain editable paths alongside current standard PSD/PSB raster/mask data and merged composites; Photoshop native Pen/vector-mask import/export remains unsupported.

Unreleased brush constraints: `paint`, `smudge`, `clone` and `heal` accept `stroke_axis` as `horizontal`, `vertical` or automatic dominant `axis`. Optional `stroke_axis_from` indexes the first constrained point; preceding points and pressure samples stay intact. Positions and axis anchors use document coordinates, with the usual reservations, revision guards, native precision and one command/undo behavior.
Whole-layer `transform` accumulates placement and samples retained native color/mask originals across gestures. Unreleased optional `skew_x`/`skew_y` degrees (-80 to 80) apply horizontal then vertical shear before scale/reflection and rotation. Nonzero selected-pixel skew rejects atomically; explicitly use `selection_only:false` for the whole target, keeping the selection unchanged. The capability metadata describes the same MCP/CLI contract; this engine slice does not add a unified Free Transform or Warp UI. Observation returns bounded `transform_source` metadata without tile samples; retained originals survive whole-layer clipboard and private PSD sources. Later pixel edits establish a new baseline for that raster. Use `selection_only:false` for a whole layer when a selection is active. See [retained transform behavior](retained-transforms.md).

`peerbrush_document` also accepts `open_async` and `cancel_open`. Observe `state.loading` for stage/completed/total and `state.file_status` for completion or failure; editing clients see only a complete current document. Opening requires saved work or explicit discard, and guards the captured document/revision through replacement. See [large-document limits](large-documents.md).


RGB matrix ICC PSDs have an sRGB display preview while their original samples/profile remain protected. Observe `document.color_profile` for `source`, `preview`, `conversion_available` and any unsupported-profile reason. To create an editable copy, use `{"action":"compatible_copy","convert_to_srgb":true,"expected_revision":REV}`. Conversion keeps the source bit depth and alpha, creates a new document, clears the save path and tags converted PSD/PNG outputs as sRGB. Out-of-gamut values clip under relative colorimetric intent. Missing explicit conversion or unsupported profiles reject atomically. Export of a protected original preserves its native samples and original ICC tag. See [ICC boundaries](photoshop-fidelity.md#rgb-icc-previews-and-copies).


RGB matrix ICC PSDs have an sRGB display preview while their original samples/profile remain protected. Observe `document.color_profile` for `source`, `preview`, `conversion_available` and any unsupported-profile reason. To create an editable copy, use `{"action":"compatible_copy","convert_to_srgb":true,"expected_revision":REV}`. Conversion keeps the source bit depth and alpha, creates a new document, clears the save path and tags converted PSD/PNG outputs as sRGB. Out-of-gamut values clip under relative colorimetric intent. Missing explicit conversion or unsupported profiles reject atomically. Export of a protected original preserves its native samples and original ICC tag. See [ICC boundaries](photoshop-fidelity.md#rgb-icc-previews-and-copies).

Use `layer.duplicate_selection` with `layer`, optional `document_id` and `source_revision` to duplicate the active selected rendered pixels at their document coordinates, retaining irregular/feathered coverage and native8/16 samples. Selection stays active and the result is one new paint sibling; masks/effects/opacity are baked once while the source remains editable. Without an area selection the active editable tree is duplicated. Empty selected pixels, adjustment-only pixel targets, stale sources, locks and reservations are refused atomically. Whole-layer clipping units must stay complete. The edit result identifies `created_roots` and the operation has one undo step.

Unreleased: the shared effect catalog adds twelve [layer-style effects](layer-styles.md) under Relief, Outlines, Shadows and Overlays. Use the regular guarded `effect.*` editing commands with the catalog keys and bounded settings. Protocol feedback returns actual canvas pixels and document coordinates; source format 16 preserves editable styles alongside standard rendered PSD/PSB rasters, masks and current composites. Photoshop descriptor/pattern import remains protected.

Unreleased: `effect.bake` takes `layer` and `effect`; `mask.step.bake` takes `layer` and `step`. Both bake the selected effect and its preceding stack into native raster pixels, keep subsequent effects, and commit one undoable edit. Commands ignore pixel selections and reserve the whole target, including a folder’s descendants. Supply current `expected_revision` and optional `source_revision`/`document_id` guards. Color baking explicitly rasterizes editable text/vector sources; folder Color baking removes its children after rendering them, including hidden sources restored by Undo. Mask baking preserves Color and hierarchy. Disabled prefixes can be removed without rasterization. Adjustment Color effects depend on their backdrop and cannot be baked independently. Expanding styles with an incompatible attached mask frame and any prefix that changes native rendering are refused atomically. No supplementary-source format change is required: saves retain existing editable suffixes, ordinary native PSD raster/mask data and a current merged composite.

Unreleased: `move` and `transform` support `snap:true` for an 8-document-pixel move grid, 15° angles and 0.1 scale-factor steps. Shared resolution runs before previews, reservations and commit history, preserves native depth and rejects invalid flags/raw values atomically. Use `snap:false` or omit it for free values. UI Shift scaling chooses equal factors before shared snapping; protocol callers can provide independent factors. See [transform behavior](retained-transforms.md).

Unreleased: shared `transform` commands support boolean `mirror_x`/`mirror_y` flags, with positive scale magnitudes and reflection before rotation. Mask targeting affects its selected paint step; Color and other steps stay intact. Whole-mask originals retain off-frame samples for inverse transforms after saving. Selection coverage, groups, locks, reservations and native depth use the same engine as toolbar actions; see [transform behavior](retained-transforms.md).

Unreleased: textured brush commands sample grain and interpolate dabs in document coordinates, independently of viewport zoom/pan or the layer's storage origin. Use the same explicit document-space points, tip, grain, density, angle, seed and pressure values for equivalent strokes. This applies to paint, mask paint, smudge and clone/heal coverage. The engine derives storage offsets internally; they are not saved brush preferences. Preview and commit share native channel precision and editing guards.

Unreleased: Pattern Overlay supports embedded static PNG tiles through ordinary `effect.add/update` settings. `capabilities.bitmap_patterns` describes native depth, base64 source bytes, document coordinates, scale/angle/phase, sampling and source format 18. PNG16 promotes the document without narrowing original channels. See [bitmap settings and explicit Photoshop limits](layer-styles.md#embedded-bitmap-patterns).

Unreleased: `peerbrush_document` / CLI `document` action `pattern_sources` requires the observed `document_id` and `expected_revision`. It returns bounded protected pattern metadata; optional zero-based `pattern_index` also returns actual native PNG image content. Images use `coordinate_space: "pattern_asset"`, `asset_rect: [left, top, right, bottom]`, original `native_depth`, source index, document identity/revision and `preview_only: true`. These coordinates describe the tile asset, without claiming a placement in the parent canvas. `color_space: "unmanaged_source_samples"` explicitly identifies raw samples without interpreting Photoshop profile/color handling. Original documents remain protected. See [inspection limits](layer-styles.md#protected-photoshop-pattern-inspection-unreleased).

Unreleased: `effect.add/update` for `texture` accepts the shared bitmap settings. Tile scale/angle/phase and sampling are separate from Texture `angle` (light direction), `depth`, and shadow/highlight `color`/`color2`. `capabilities.bitmap_patterns.texture_source_format` is 19; Pattern Overlay remains 18. Transparent bitmap samples are neutral luminance height, and PNG16 retains/promotes native precision.

Unreleased: Gradient Overlay supports `color_stops` and independent `alpha_stops` through ordinary shared effect commands. `capabilities.gradient_stops` defines bounded counts, normalized stop/segment fields and source format 20. Strict endpoints/order and unknown-field checks reject invalid edits atomically. Actual image feedback continues using document coordinates. See [multiple gradient stop settings](layer-styles.md#multiple-gradient-stops-unreleased).

Unreleased mask linking (PB-152): `mask.link` takes `layer` and strict boolean `linked`. New masks default to linked; inspect `layers[].mask.linked` before moving or transforming a mask. Linked transforms affect both Color and Mask; unlink for independent targets. Whole unlinked Mask transforms include all paint steps unless `step` explicitly selects one. Shared guards, reservations, native precision and gesture previews apply. Independent Color and selected linked transforms reject unsupported mask-effect stacks. Source format 29 preserves link choices; external Photoshop link editing is not written or verified and is disclosed by the save report. See [mask-link behavior and limits](retained-transforms.md).


Unreleased: `peerbrush_fonts` (CLI `fonts`) lists native installed document-font metadata, prepares an exact cached snapshot and refreshes only the derived catalog. List uses `search` (up to 128 characters), `offset` (0–8192) and `limit` (default 32/max128); status/generation, unsupported reasons and next offset are explicit. `prepare` takes the exact path-free font object returned by list and reports `ready` or loading/busy/missing/changed/unsupported. A ready object can replace the legacy bundled string in `source.content.font`; unknown fields, arbitrary paths and font bytes reject. Requests do not edit PSD/history. Source edits still use the usual project/revision/reservation guards and actual image feedback. Missing fonts preserve saved pixels/metadata and never trigger substitution. See [font limits and preservation](editable-sources.md#exact-fonts-and-missing-font-projects-unreleased) and `capabilities.document_fonts`.
