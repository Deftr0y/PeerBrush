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

`peerbrush_exports` shares PNG/WebP/JPEG XL production presets, rendered previews and cancellable selected layer/folder batches with the File menu and CLI `exports`. Presets live outside document history. Preview/start require observed source identity and revision; preset updates require the observed library revision. `settings.format` defaults to `png`, including old presets. PNG retains native 8/16-bit channels; lossless `webp` explicitly produces 8-bit RGBA after native-depth rendering/resampling, retaining the working document's original precision. PNG/WebP retain alpha and original ICC bytes without implicit profile conversion. Unreleased `jxl` retains native 8/16-bit output depth; optional `jxl` settings contain `lossless` (default true), `distance` (0.1-15, default 1) and `effort` (1-9, default 3). Alpha stays lossless; decoded lossy RGB uses an explicit SDR sRGB output when its default profile differs from Original, with the actual output ICC. Untagged RGB is assigned sRGB for JXL. Its preview decodes and manages that actual output profile. Plans/receipts distinguish `format`, output `bit_depth` and `native_source_depth`, report document bounds, and use explicit duplicate-name and existing-file policies. MCP previews return actual encoded output as PNG image content with `output_format` and `output_bit_depth`; CLI writes those preview PNGs into instance-local storage and returns their paths and coordinates. `document` action `export` uses the `.png`, `.webp` or `.jxl` destination extension and the same guards. Status/cancel report each output without changing editable sources. See [production export settings and limits](production-export.md).

For a large flattened export, call `exports` with `action: "start"`, an empty target list, and `settings.crop: "canvas"`. Poll `action: "status"` with the returned run ID. This keeps the export observable across CLI calls, whose synchronous requests have a 60-second timeout.

Observe before editing. Use layer IDs, document pixel coordinates, and `expected_revision`. The upper-left pixel is (0,0). Rectangles are `[left, top, right, bottom]`, with right and bottom excluded.

`peerbrush_filters` lists whole-image presets, renders thumbnails in `filter_thumbnail` coordinates, and previews an explicitly targeted current project without history. Preview returns actual PNG content with `document_rect` and frozen commands to apply through `peerbrush_edit`. `filter.add/update/delete/reorder` processes the entire composite and uses document-wide reservations, locks and revision guards. Observe `document.filters`; settings, strength and bypass remain editable at native 8/16-bit depth. Custom preset save/rename/delete/import/export lives outside history. See [whole-image filter behavior and limits](filters.md).

Color effects and mask steps accept `weight` in `effect.add/update` and `mask.step.add/update`. Use 0–1; omitted new weights default to 1 and omitted update weights keep the existing value. Weight blends the step's input with its result, independently of settings such as blur Radius or bloom Strength. Observe returns each weight. Native 16-bit samples, clipboard sources, PSD sources, reservations and undo share the same engine path.

```json
{"commands":[{"op":"paint","layer":"LAYER_ID","points":[[60,60],[180,120]],"radius":14,"color":[233,84,32,255]}],"expected_revision":0,"actor":"my-agent","label":"Paint orange accent"}
```

In 0.4, `paint`, `paint.fill`, `shape`, `gradient` and `smudge` also accept `color16` on 16-bit targets. Supply four RGBA integers from 0 to 65535. This takes precedence over `color` when both are present. Mask edits use red as the grayscale value and retain the supplied alpha. For Smudge, this is the color added by Load.

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

Unreleased: `peerbrush discover --connect [--state-dir PATH]` runs Connect AI
from the CLI for an already running workspace. It first checks the authenticated
local bridge, then publishes discovery and updates detected local client
configuration using the same launcher selection and backup rules as the app.
The JSON report shows the result for each client. Use `--status` for inspection.

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

Out-of-core native proposals now have bounded workspace storage and shared internal preparation, region/layer/mask/selection review and acceptance paths. Final publication rechecks the actual pending record; refusal retains the frozen candidate for retry. Preparing and cleanup owners count toward four slots per project, while source data uses the configured native resident/disk pools. Periodic cleanup and instance shutdown drain run on the existing recovery worker. The owned native protocol now routes create/list/preview/accept/reject through that store; proposal PNG responses retain admitted owners and explicit source revision/document coordinates. Detached Value transport refuses before mutation. Acceptance feedback observes the newly committed revision; unavailable feedback keeps the successful edit result with a warning. Native command capture freezes brush/filter preset settings before scope capture. Native UI review now renders the stored candidate on the bounded canvas worker and submits human acceptance through the shared cancellable edit worker. Pending-record proofs gate uploads, paint and acceptance; proposed pixels do not enter committed caches/capture receipts. Original/proposed switching, terminal invalidation and retry after refusal are covered by regression source. Native runtime qualification and ordinary native adoption remain unfinished; the 256 MiB COW policy below describes the resident path. This source work is unreleased.

```json
{"action":"create","actor":"my-agent","document_id":"DOCUMENT_ID","expected_revision":7,"label":"Softer background","commands":[{"op":"layer.update","layer":"LAYER_ID","opacity":0.7}]}
```

The native **AI tasks** menu opens a blue review with scopes and Original/Proposed result views. Accept becomes available after the proposed canvas renders. Acceptance requires `actor:"human"`, the reviewed `document_id` and exact `expected_revision`; it commits the frozen result as one AI-authored undoable edit. Local clients supply actor strings as in existing edit/history calls; this is a cooperative workspace convention, not authentication of human identity. Reject releases the draft without an edit. Close/Escape returns to the current project and leaves the proposal pending. Human canvas input closes review before editing.

Any project edit makes pending drafts stale, even if unrelated. New/open clears them. Expiry, task end and takeover release draft pixels; acceptance rechecks current reservations. An external image file changing after preparation cannot change the reviewed result. Task-bound proposals require the task to stay active during review; a finished reservation is never silently restarted. Up to four proposals share COW sources with a 256 MiB additional unique native-tile budget; each batch is bounded to 100 commands/8 MiB, expires after 15 minutes, and retains only bounded transient metadata afterward. Drafts and task-recovery records are absent from PSD sources and autosave.

`peerbrush_task` action `recovery` reports active scopes and the latest 100 ended, expired or taken-over tasks. The menu shows recent interruptions and existing task undo. Already committed AI edits and human work remain; choose an explicit selective task compensation when needed. See [collaboration behavior](collaboration.md).

## Recovery discovery (unreleased native integration)

`peerbrush_recovery` action `list` retains the existing PSD `versions` array and adds `native_versions`, `native_catalog_error` and `native_discard_available`. Copy the selected native entry's complete `snapshot` value (`native:UUID`) into action `restore`; the prefix distinguishes it from a legacy PSD recovery with the same UUID. UI, MCP and CLI restoration use the same guarded source checks and create a fresh unsaved tab. Original files and recovery checkpoints are retained.

Native entries report document/project IDs, revision, precision, timestamp, marker length and protection state when these can be read. `display_name` is presentation text capped at 256 Unicode characters, with control characters replaced by spaces and an ellipsis when truncated. Restoration keeps the complete saved document name. Unreadable records have an `error` and null unknown metadata. A non-null `native_catalog_error` means inspection failed; it does not establish that no native recoveries exist.

Inspection uses the configured authenticated source service and leaves an unused source namespace uncreated. Native rows retain shared-pool memory admission through owned transport replies, as other native responses do; the unowned Rust dispatcher refuses these responses. The chooser performs discovery in a background worker. Restoration independently verifies checkpoint/source ownership and editable semantics again; list metadata supplies no write or retirement permission.

`native_discard_available` is true on Windows and false on Unix. Where available, action `discard` requires `actor: "human"` and an explicit `native:UUID`; the chooser also requires confirmation. Disposal consumes an exclusive source session, revalidates the complete checkpoint and removes only that physical marker. Active sessions, cursors or retained child operations cause refusal. The UI runs disposal in a worker and refreshes afterward, respecting a human dismissal. Shared pixel stores and other checkpoints remain retained and charged. Unreadable records are retained. An uncertain move/removal retains source roots and disk debt, reports the error and requires restart before retrying; no overwriting rollback or guessed refund occurs. Unix native discard stays disabled because name-based unlink cannot bind deletion to the held marker. Shared-source garbage collection, Unix native autosave/retention, ordinary native new/open adoption and native runtime/release qualification remain unfinished. Legacy PSD recovery behavior remains available. Source regressions have been compiled; they have not been executed.

Before publication, failed native checkpoint writes have a separate owned-staging cleanup path. On Windows, only the complete acknowledged prefix with matching SHA-256, physical identity and session may be removed and its bound quota settled. Changed/extra bytes, aliases, replacement files, failed creation and uncertain publication retain admission; cleanup cannot infer success from a missing path or caller-supplied digest. An unsettled publication makes discovery report uncertainty instead of an empty catalog, including an interruption after the canonical marker appears. Restart is required before retrying that inspection. This internal path does not relax the platform availability above.

Unreleased Windows native autosave now uses the existing five-second worker for native documents, including explicitly restored native tabs. It publishes complete source-backed checkpoints at the original channel depth, retaining three versions per project. The newly durable checkpoint is protected even if older timestamps are in the future. Failed retention stops further checkpoint growth and retries on a later tick. Changes before publication defer the checkpoint; newer human work after publication stays dirty and is picked up next time. Autosave does not change document history, saved revision or the original file. Saving or explicitly closing a project removes its owned autosave markers; original recovery versions from which a new tab was restored remain separate. Unchanged native generations use admitted worker bookkeeping instead of rewriting checkpoints. PSD recoveries and native marker metadata share the 1 GiB / 64-project limit; actual native pixel stores remain under the configured shared source-disk budget. Retention removes markers only, leaving shared pixel stores retained and charged. Unix native autosave remains unavailable pending safe retention; ordinary new/open adoption and native runtime qualification remain pending.

Recovery versions may select different committed prefixes from one backing file after restart. Later selections are fully verified into the shared read index while the original publication head and readonly policy stay intact. Re-checkpointing a readonly source reuses a registered complete selection when it covers every required reference. Otherwise, recovery capture and native provisional output sealing copy the required native tiles into an owned writable mirror, preserving channel bytes, precision and tile coordinates. Recovery records the actual copied owners; it does not infer authority from combined hashes or alter the original readonly source. Existing snapshots and document history remain intact. Abandoned persistent copies remain charged. Registered stores park their exact held backing/index after the last logical owner drops; a later admitted source session can resume that state without allocating another mirror or guessing a newest commit. Parking does not count as a live document root and grants no deletion authority. Its handle/index admission remains retained until verified collection or runtime teardown. A nominal parked claim and full retained-file verification are prepared; dropping a claim restores the same backing without I/O. Root census now includes full held-file SHA/identity checks for registered external-save journals and refuses missing, incomplete or changed records. Those observations alone grant no deletion or quota-refund permit. Windows recovery pruning can now collect an actual parked backing only under an exclusive source-session gate and complete source/root proofs. Bound file/directory handles retire the owned file and empty source directory; zero links, known-handle closure and namespace/absence checks precede a nominal quota-settlement receipt. Windows currently proves runtime removal; its shared parent-sync helper is a no-op, so namespace crash durability remains unqualified. Live owners and registered recovery snapshots retain their sources. Uncertain outcomes retain debt and block fresh admission. Unselected Windows startup files require independent whole-file inspection through their inventoried observation and an exclusively locked data handle. The private record/index proof grants retirement only; it never creates editable Store/Snapshot or newest-selection authority. Refusal restores the observer and admission, while successful retirement closes the observer before settlement. Old live source handles/locks and unregistered recovery roots prevent collection. Foreground source requests interrupt collection scans and queue until exclusive maintenance access leaves; queued readers acquire real permits before another collection can start. Reentrant probes and other exclusive work retain nonblocking refusal. Once unlink starts, that source retirement completes before yielding. Rejected source cleanup is deferred after actual-service/cancellation revalidation so it does not withhold a valid checkpoint; uncertain mutation still blocks fresh admission. Unix retirement, namespace crash durability, measured collection/input latency and native/runtime/release acceptance remain pending.

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

Unreleased: destructive `adjust` accepts `layer`, optional document-coordinate `rect`, `brightness` (clamped to −1–1, default 0), `contrast` and `saturation` (clamped to 0–3, default 1). It adjusts stored RGB at the original 8/16-bit precision and preserves alpha. The rectangle intersects the current selection; polygon and soft coverage apply once. Without either restriction it uses the complete layer frame, including off-canvas pixels. Zero RGBA samples, sparse holes and physical padding remain unchanged, and default settings preserve samples exactly. Transparent samples with nonzero stored RGB still receive the adjustment. Masks use mask effects; editable text/vector sources require explicit rasterization. Native worker preparation is implemented, but live native authority and runtime acceptance remain pending.

`peerbrush_capabilities` returns brush tip/settings, all blend modes and runnable commands. Use `paint` with `tip:dry|chalk|grain|bristle`, density/grain, pressure toggles and taper distances. Pressure points accept `[x,y,p]` with `p` in0–1; plain points remain compatible. `smudge` adds wetness/load/pickup and uses the same selection, layer and mask coordinates.

`adjustment.add` accepts `layers`, `kind` and editable `settings`. A paint target gets a clipped adjustment; a folder target gets a scoped child; multiple roots become a shared folder. An existing pixel selection becomes its mask. `layer.clip` sets `base` to the next unclipped sibling below; `base:null` releases clipping. Preserve whole clipping units when copying or reorganizing.

Color effects add `color_balance`, `hsl`, `bloom` and `liquify`. Balance uses shadows/midtones/highlights RGB complementary-axis arrays in −1–1, with preserve_luminosity. HSL uses hue degrees −180–180 and saturation/lightness −1–1. Bloom uses threshold 0–1, spread 0–64 document pixels and strength 0–3.

Unreleased: `levels` also accepts `output_black` and `output_white`, each 0–1 (defaults 0 and 1). The output range is applied after input black/white and gamma; equal endpoints produce a constant RGB value and reversed endpoints invert the tonal range. Layer effects, adjustment layers and whole-image filters share native 8/16-bit rendering and preserve alpha. Nondefault output settings require resident PSD source format 47 or later, or the native Source36 Levels output marker, including disabled effects. Standard raster/mask channels and the current merged composite remain present. Representable master-only Photoshop `levl` adjustments now import and save through the standard record; resident source format 48 binds that layout. Combined nonneutral master/channel changes and other unsupported Photoshop adjustments remain protected; see [Photoshop fidelity](photoshop-fidelity.md).

Levels created without a settings object now use `input_mapping:"exact"`. Supplied settings that omit this key retain the legacy minimum input span of 0.01; use `input_mapping:"exact"` explicitly for narrow input ranges. Exact mapping accepts input endpoints from 0–1 with black strictly below white. Both modes accept gamma 0.1–9.99. Exact native8 and GPU lookup tables derive samples directly from integer channels; native16 retains word precision. Resident source format 49 and native Levels marker version 2 protect the extended semantics, including disabled effects. Existing source mappings remain unchanged.

Unreleased Levels also accepts `channels:[red,green,blue]`, exactly three settings objects. Each permits `black`, `white`, `gamma`, `output_black`, `output_white` and optional `input_mapping:"exact"`; an empty object is neutral. Channel input mapping is always exact. Each channel applies before the root/master mapping with one final native-sample rounding. The inspector exposes RGB/Red/Green/Blue controls through the same engine commands. Resident source format 50 and native Levels marker version 3 protect any channel array, including disabled effects. Independent standard PSD/PSB RGB records are supported only with a neutral master; combined nonneutral master/channel Photoshop definitions remain protected pending calibration. Native combined settings retain their editable sources with a disclosed standard raster fallback.

Unreleased Curves accepts `channels:[red,green,blue]`. Each of the three objects contains `points` (2–16 ordered `[x,y]` pairs in 0–1) and `interpolation` (`linear` or `smooth`). Use `{"points":[[0,0],[1,1]],"interpolation":"linear"}` for a neutral channel. Each channel applies before the master curve, with one final rounding at native 8/16-bit precision. Layer effects, adjustment layers and whole-image filters share the mapping. Mask Curves use a single curve. Resident source format 53 and the native Source36 Curves channel marker preserve settings even when disabled. Standard PSD/PSB interchange is limited to the exact two-point layouts in [Photoshop fidelity](photoshop-fidelity.md).

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

`layer.update` accepts `color_label` with `none`, `red`, `orange`, `yellow`, `green`, `blue`, `violet`, `gray`, `seafoam`, `indigo`, `magenta`, or `fuchsia`. Layer observations return the same field. `none` clears the label. Edits use the usual layer locks and reservations and persist as standard PSD/PSB metadata.

Native 16-bit projects retain channel precision in the shared engine and PSD/PNG files. Create them with `peerbrush_document` action `new`, `bit_depth:16`. Observation images are compact 8-bit PNG projections. `peerbrush_capabilities.rendering` reports the actual adapter, supported automatic GPU effects and successful dispatch count; a CPU-only/headless process does not claim an attached GPU.

`peerbrush_capabilities.photoshop_files.native_limits` reports the native PSD/PSB admission ceilings for projects with a configured source service. The top-level file, edge and pixel limits apply to resident projects. Both paths enforce format and resource limits. Use PSB when the standard layer data or merged composite exceeds PSD capacity.


### Selection and project settings

`selection` accepts `kind` (`rectangle`, `ellipse`, `lasso`, `polygon`, `magnetic`, `wand`, `quick`, `object`, `row`, `column`), `mode` (`replace`, `add`, `subtract`, `intersect`), `rect` or `polygon`, `feather` (0–64 px), and color sampling options `layer`, `sample_merged`, `tolerance` (0–255), optional boolean `color_similarity`, `contiguous`, `point`, `points` and `radius`. Polygon/magnetic geometry is supplied as document-coordinate points. Row/column clients supply their explicit one-pixel rectangle. Clear with `selection.clear`, restore with `selection.reselect`, invert with `selection.invert`, and expand or contract with `selection.modify`, `mode` and `radius`. Empty selection bounds represent zero selected pixels. Unreleased `color_similarity:true` uses the maximum absolute rendered RGB-channel/alpha difference. `false` or omission uses root-mean-square RGB distance with alpha difference. Enabled-mode tolerance must be a finite number from 0 to 255. Flags must be boolean. Wand, Quick Selection and Object region share the metric. The **Sample** control defaults to the active layer. Clients supply the explicit `layer` ID to sample that layer. The stacked-layer icon corresponds to `sample_merged:true` and samples the rendered composite.

Unreleased development: `selection.wand_update` changes `tolerance`, `feather`, `contiguous`, `sample_merged` or `color_similarity` from the most recent wand seed and its original replace/add/subtract/intersect baseline. Use `document_id` and `source_revision` guards. Artwork or other selection edits retire that source; click again to establish a new one. Undo/redo restores the corresponding runtime source. PSD saves retain current selection coverage and native artwork, but do not save a replay seed; reopening requires a new wand click. UI drags preview an actual selected-pixel cutout and commit once on release; Escape or concurrent edits cancel the draft. The **Live updates** checkbox defaults on; disabling it preserves the selection while changing controls, and enabling it applies those settings to a valid seed.

Unreleased development: `mask.add` initializes a new mask from an active selection's exact coverage in document coordinates. Selected regions reveal content, unselected regions hide it, and feathered confidence remains graded. An active empty selection creates a fully hiding mask. Current native color channels and existing masks are preserved; the edit is one undo step. With no selection, `value` initializes the usual fill and paint steps. Selection-dependent creation and `mask.from_selection` also respect `@selection` reservations. Include `document_id`/`source_revision` when acting on an observed selection.

Unreleased native mask update: `mask.add` with a selection, `mask.from_selection`, `path.to_mask`, `mask.refine` and `mask.from_color` use bounded native workers through the 16000-pixel edge limit instead of the resident 16-megapixel area cap. Shared memory/disk admission, contour and stack limits, cancellation and reservations still apply. Edge-guided refinement requires an exact uncropped document frame and now streams its full-detail guide. Native mask precision and current selection remain intact. Color-mask flood work scales with image area within its hard budget. Other per-command budgets and native acceptance remain separate requirements.

Unreleased native selection update: Wand, Quick and Object Selection, `selection.import` and `selection.refine` now use the 16000-pixel native edge limit. Shared sampled pixels, original wand-replay baselines, scalar coverage, contour limits, source ownership, reservations and atomic history remain enforced. PNG selection sources support native8/16 input through 16000 pixels per edge; selection coverage is scalar8 and artwork is not converted. Native file inputs use the shared streamed PNG reader with a 3 GiB encoded limit; inline base64 inputs retain the 64 MiB limit. Both retain complete checksum validation, and file inputs are held and rehashed after sampling to refuse changes. Temporary decoded file sources are retired before publication. Discovery exposes the native file limit as `native_selection_png_file_bytes`; `selection_png_encoded_bytes` remains the inline limit. Runtime and native acceptance remain pending. Discovery exposes `native_selection_pixels` and `native_selection_max_edge` separately from the resident `selection_pixels` limit.

`document.settings` accepts `width`, `height` and `bit_depth` (8 or 16), preserves layer content/positions, and commits one history item. It requires document-wide access. A 16-to-8 conversion is intentional quantization. `liquify.stroke` accepts an optional `effect` ID and captures selection coverage. Visibility-only `layer.update` commands (`op`, `layer`, `visible` only) remain usable during pixel reservations and do not conflict with pending pixel revisions. Other layer fields retain ordinary checks.

`document.global_light` accepts an integer `angle` from 0 to 359. It requires document-wide access and updates the Photoshop Global Angle and its linked shadows in one history item. Read the current value from `document.global_light_angle` in state. Set `use_global_light` to `true` in `effect.add` or `effect.update` to link an Inner or Drop Shadow. The first link creates the document angle from the shadow’s direction and requires document-wide access. Set `use_global_light` to `false` to unlink while keeping the rendered direction. Editing an individual shadow’s direction through `effect.update` also makes that direction local.

`clone` and `heal` use explicit document-pixel source anchors, optional source layers or merged artwork, shared brush settings and native 16-bit sampling. Include `source_revision` and `document_id` to require the exact sampled state. See [retouch commands](retouch.md).

Whole-document `crop`, `canvas.resize` and `image.resize` commands share native previews and history. They use document pixels, whole-document reservations, original channel depth and optional exact source guards. See [geometry commands](geometry.md).

Editable text/vector layers share `source.add`, `source.update` and `source.rasterize`. Observation exposes the complete `source` definition on each layer. Retain its frame and matrix when updating content. Sources use native RGBA16 colors and explicit document origins, preserve masks and effects, and return rendered PNG feedback with coordinates. Optional `source.content.layout:"shaped"` enables exact-font ligatures, combining marks, script itemization and bidirectional ordering. Use `"paragraph"` to also wrap text to the field width. Omitted or `"legacy"` retains historical layout, serialization and pixels. Shaped letter spacing is between clusters. Joining and complex scripts require zero tracking. Resident formats 34 and 128 protect shaped and paragraph sources respectively. Saves keep standard native raster and mask channels and a current composite. Missing exact fonts retain saved pixels and metadata until explicit replacement. See [source schemas and limits](editable-sources.md).

Unreleased: [Pen commands](pen.md) create/update native Bézier sources, add/move/delete/convert anchors, move paired handles, split segments and convert closed contours to selections/masks. Positions and handles in `path.*` commands and observed `path_document_nodes` use document pixels, including after affine placement. Supplementary `source.content` uses its source frame/matrix. `capabilities.paths` advertises commands and limits. Shared locks, reservations, guarded revisions, native 8/16-bit raster channels and one undo per edit apply. Format-17 sources retain editable paths alongside current standard PSD/PSB raster/mask data and merged composites; Photoshop native Pen/vector-mask import/export remains unsupported.

Unreleased brush constraints: `paint`, `smudge`, `clone` and `heal` accept `stroke_axis` as `horizontal`, `vertical` or automatic dominant `axis`. Optional `stroke_axis_from` indexes the first constrained point; preceding points and pressure samples stay intact. Positions and axis anchors use document coordinates, with the usual reservations, revision guards, native precision and one command/undo behavior.
Whole-layer `transform` accumulates placement and samples retained native color/mask originals across gestures. Unreleased optional `skew_x`/`skew_y` degrees (-80 to 80) apply horizontal then vertical shear before scale/reflection and rotation. Selected raster and painted-mask skew shares one original-sampling pass with polygon/scalar coverage, including soft edges and holes; explicitly use `selection_only:false` for the whole target, keeping the selection unchanged. Existing editable-source and mask-effect restrictions still reject atomically. The capability metadata describes the same MCP/CLI contract; the native Free Transform session uses these same commands; grid and preset Warp remain unavailable. Observation returns bounded `transform_source` metadata without tile samples; retained originals survive whole-layer clipboard and private PSD sources. Later pixel edits establish a new baseline for that raster. Use `selection_only:false` for a whole layer when a selection is active. See [retained transform behavior](retained-transforms.md).

Unreleased projective placement uses `{"op":"transform","layer":"LAYER_ID","projective":{"rect":[0,0,32,24],"quad":[[0,0],[30,1],[28,23],[2,24]]},"selection_only":false}`. The exclusive rectangle and TL/TR/BR/BL quad are document coordinates. The strict object maps the rectangle to a finite, convex, same-winding quad; existing affine values apply afterward (A * P). Invalid, folded, horizon-crossing, unstable, oversized or over-budget mappings reject atomically, including the full retained original frame. Selected Color, painted masks, scalar selection coverage and folder descendants share the map; existing locks, reservations and source/revision checks still apply. Editable text/vector sources require explicit rasterization at their original precision, including hidden descendants; no automatic flattening occurs. Whole-target projective originals use supplementary PSD/PSB format32 alongside current standard channels and the composite. The pending native Distort/Perspective controls use the same operation and one guarded Apply. Source review and prepared tests do not establish combined, native desktop or receiving-application acceptance.


`peerbrush_document` also accepts `open_async` and `cancel_open`. Observe `state.loading` for stage/completed/total and `state.file_status` for completion or failure; editing clients see only a complete current document. Opening requires saved work or explicit discard, and guards the captured document/revision through replacement. See [large-document limits](large-documents.md).

Unreleased working Save: `save` remains synchronous. `save_async` requires observed `document_id` and `expected_revision`, plus the same `path`, `overwrite` and exact-source `accept_editability_loss` consent as synchronous Save. Observe `state.saving` for its exact `run`, source document/revision, stage, completed/total work, `can_cancel`, `cancel_requested`, `finished` and terminal `error`; terminal status is retained for that run. Encoding and pixel comparison count channel rows. Assembly and whole-file checksum passes count MiB. `cancel_save` requires that exact `run` and returns `cancel_accepted` plus current status. Cooperative cancellation preserves the destination and unsaved project before final publication. Late cancellation is refused while verified publication/readback finishes. Full existing preparation, backup and individual OS operations may finish before cancellation is observed. Native Save and Save before close use this shared control. CLI passes the same actions through `peerbrush cli document JSON`; no separate Save engine or cancellation semantics apply. See [Save limits and cancellation](large-documents.md#working-save-progress-and-cancellation-unreleased).



RGB matrix ICC PSDs have an sRGB display preview while their original samples/profile remain protected. Observe `document.color_profile` for `source`, `preview`, `conversion_available` and any unsupported-profile reason. To create an editable copy, use `{"action":"compatible_copy","convert_to_srgb":true,"expected_revision":REV}`. Conversion keeps the source bit depth and alpha, creates a new document, clears the save path and tags converted PSD/PNG outputs as sRGB. Out-of-gamut values clip under relative colorimetric intent. Missing explicit conversion or unsupported profiles reject atomically. Export of a protected original preserves its native samples and original ICC tag. See [ICC boundaries](photoshop-fidelity.md#rgb-icc-previews-and-copies).


RGB matrix ICC PSDs have an sRGB display preview while their original samples/profile remain protected. Observe `document.color_profile` for `source`, `preview`, `conversion_available` and any unsupported-profile reason. To create an editable copy, use `{"action":"compatible_copy","convert_to_srgb":true,"expected_revision":REV}`. Conversion keeps the source bit depth and alpha, creates a new document, clears the save path and tags converted PSD/PNG outputs as sRGB. Out-of-gamut values clip under relative colorimetric intent. Missing explicit conversion or unsupported profiles reject atomically. Export of a protected original preserves its native samples and original ICC tag. See [ICC boundaries](photoshop-fidelity.md#rgb-icc-previews-and-copies).

Use `layer.duplicate_selection` with `layer`, optional `document_id` and `source_revision` to duplicate the active selected rendered pixels at their document coordinates, retaining irregular/feathered coverage and native8/16 samples. Selection stays active and the result is one new paint sibling; masks/effects/opacity are baked once while the source remains editable. Without an area selection the active editable tree is duplicated. Empty selected pixels, adjustment-only pixel targets, stale sources, locks and reservations are refused atomically. Whole-layer clipping units must stay complete. The edit result identifies `created_roots` and the operation has one undo step.

Selected-pixel duplicates store the rendered samples in the working document’s color space. The embedded profile applies to the duplicate when PeerBrush renders it for display.

Set `mask: true` to duplicate the selected region of a layer’s rendered mask as an independent grayscale paint layer. This requires an active pixel selection and a layer with a mask. The new layer sits above the original clipping unit if the source participates in one.

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

Unreleased: `transform` also accepts optional numeric `dx`/`dy` (-100000 to 100000 document pixels, rounded; default zero). Translation follows the linear placement in the same sampling pass, including selected raster and scalar coverage. Explicit fields share the existing 8-pixel snap grid. Use this single combined command for selected move/scale/rotation, rather than applying multiple commands which each establish a new selected-pixel raster baseline. Existing source/document guards, translated destination reservations, link behavior and native depth still apply. The native Ctrl/Cmd+T session builds this shared command per target; movement without an affine adjustment remains `move`.

### Image patch precision and clipping (unreleased)

`image.patch` alpha-composites a file onto an existing color paint layer. `x` and
`y` are integer document coordinates (default zero); an optional `rect` and the
current selection limit its writes. Soft coverage applies once and takes precedence
over the polygon inside the selection bounds. The layer frame does not expand.
Use a 16-bit document for a 16-bit source; floating-point patches are refused.
The patch retains the existing raw-channel/default-frame behavior; image import
and placement provide their own color-management and orientation handling.
The pending native worker currently admits PNG files only. Other resident patch
formats remain available; native codec admission and live activation are unfinished.

Unreleased precision-reservation update: in an 8-bit document, image import,
image placement, image paste, layer paste and bitmap-effect source updates may
promote every color/mask plane. These commands conservatively reserve the whole
document before source decoding. In 16-bit documents, their ordinary destination
scopes apply. Existing-layer image placement reserves the whole layer, including
when a destination rectangle is supplied, because frame expansion can reframe
masks and affect layer-wide results. Static PNG import and file/base64 placement
now have native producers, along with static PNG image paste. Other native
codecs, animated sources, UI/OS clipboard binding and live activation remain
pending. Typed whole-layer paste and selected-pixel duplication now also have
native producers; rendered clipboard preparation uses the shared engine. Placement keeps its existing selection-independent
behavior and preserves native channel precision.

Unreleased image-paste update: pasting clears active selection bounds, polygon
and coverage while retaining the previous selection for Reselect. It reserves
that selection and every moved clipping-unit member, or the destination folder
when inserting a sibling. Root-level wrapping keeps clipped layers with their
base; insertion within a folder stays above the complete clipping unit. All
moved members must be unlocked. Paste position still uses `origin`, or centers
the image when `origin` is null/absent. Pixel insertion, folder creation and any
8-to-16-bit promotion remain one shared-engine undo step.

`image.paste` accepts exactly one absolute `path` or base64 `png`. File input
uses the same image-choice validation as placement; explicit frame/page/variant
choices are required when the format has multiple items. Resident paste reuses
the decoded raster directly. The pending native worker streams static PNG,
QOI, Netpbm, TGA and BMP files, preserving their source precision, orientation
and supported color handling. It refuses unsupported native codecs and choices.
Native paste shares the batch's 256 MiB import accounting; cancellation or
budget refusal retires staged sources without inserting partial layers.
Native SVG/SVGZ import, placement and file paste also use the bounded vector
renderer. Import retains editable markup; placement and paste rasterize it.
Static SVG rejects all frame/page/variant selectors. SVG's existing RGBA8
coverage/color projection remains explicit when promoting into native16 artwork.
The pending native worker also accepts GIF import, placement and paste, using
explicit `frame` selection for animations. It decodes bounded rows and stages
interlace/disposal composition while retaining the existing static-image and
animation transparency semantics. GIF patches use raw first-frame pixels and
the existing unmanaged patch color policy; import/placement/paste retain ICC
refusal. Patch clipping, selection coverage and destination precision still apply.

Unreleased native clipboard update: whole-layer paste accepts an owned typed
snapshot from the same source service; command JSON alone cannot supply those
source owners. Pasted headers receive fresh identifiers while unchanged pixels
remain shared. Selected-pixel duplication uses rendered source-backed copy at
the document depth and keeps the active selection. Native8 copies retain the
existing preview color-conversion contract; native16 words are not projected
to display bytes. Temporary styles retire before publication, and cancellation
or a late reservation leaves no partial insertion. Native copy accepts up to
16000 pixels per edge; rendered work and bitmap transport have separate byte
budgets. Native runtime and installed OS transport acceptance remain pending.

Unreleased native canvas geometry: `crop` and `canvas.resize` now share resident
dimension, coordinate and anchor validation. They retain off-canvas source pixels,
masks and transform originals, update layer/effect placement, and clear active
and previous selections atomically. Locked layers block movement; an unchanged
origin can resize the canvas without moving them. The whole document remains
reserved for this edit. Native `image.resize`/`resize` also stages original-depth
resampling, editable-source rendering, mask reframing and historic Liquify
coverage through the same guarded worker. Existing spatial-effect proportionality,
canvas and work limits remain enforced. Image resizing clears active/previous
selection when dimensions change; an unchanged size preserves them. Live native
authority activation and runtime acceptance remain pending.

Unreleased native merge update: `layer.merge` now prepares original-depth pixels
through the shared native compositor and publishes the replacement tree in one
guarded history step. Merge-down, adjacent siblings and isolated folders share
resident hierarchy, locking, clipping and output-bounds rules. Off-canvas pixels
remain eligible; current selection does not crop the result. Layer masks/effects
and opacity are baked into the raster, while document filters remain separate.
A single folder retains its external blend mode. Effect/mask prefix baking and
live native activation remain pending; compile-only checks are not acceptance.


Unreleased native prefix baking: `effect.bake` and `mask.step.bake` prepare
original-depth prefix rasters in one guarded history transaction. They retain
suffix effects, selection and external layer properties. Raw target, raw mask,
filtered composite and isolated-tree comparisons must preserve native samples;
inexact prefixes are refused without adoption. Folder baking checks every child
lock; expanding styles cannot silently change an existing mask frame. Existing
8192-pixel limits remain. Live native activation and runtime acceptance are pending.


Unreleased native retouch update: `clone` and `heal` now support
`sample_merged:true` and composed source masks when `mask:true` omits
`source_step`. Explicit paint-step sampling remains available. Samples use the
immutable pre-stroke result after any required frame expansion; selection affects
destination writes only. Native16 channels remain native, while native8 merged
sampling retains the existing preview color conversion. Native merged sampling
uses bounded windows on canvases up to 16000 pixels per edge. Resident merged
sampling retains its 16-megapixel limit. Temporary frame and style sources retire
before publication. Cancellation and late reservations cannot adopt a partial
edit. Sparse 16000-square clone and heal diagnostics verified native 8-bit and
16-bit samples on Windows. Cross-platform and dense-canvas verification remain
pending.


Unreleased native QOI update: file-based `image.import`, `image.place` and
`image.patch` now accept QOI through a bounded streaming decoder. QOI imports
accept the optional static `frame:0`; page/variant and nonzero frame choices
are refused. RGB/RGBA samples retain the existing QOI interpretation and promote
to native16 when the destination requires it. Encoded and decoded full frames
are not allocated. File identity, consumed-byte hashes, complete stream endings,
command budgets, reservations and atomic history remain checked. Other native
codecs/animations and runtime acceptance remain pending.


Unreleased native Netpbm update: file-based `image.import`, `image.place` and
`image.patch` accept `.pbm`, `.pgm`, `.ppm`, `.pam` and `.pnm` through bounded
original-precision decoding. Import/placement accept optional static `frame:0`;
page/variant and nonzero frame choices refuse. Native16 patches require a
16-bit destination. A 64 KiB header cap and charged row buffers avoid whole-frame
allocations; held-file identity, byte hashes, cancellation, reservations and
atomic history remain enforced. Only the first image is decoded, matching the
existing Netpbm import behavior. Live native activation and runtime acceptance
remain pending.


Unreleased native TGA update: file-based `image.import`, `image.place` and
`image.patch` accept basic `.tga` sources through bounded palette/row decoding.
Import/placement accept optional static `frame:0`; page/variant and nonzero
frame choices refuse. Palette, packed-color, gray/alpha and raw/RLE data retain
the installed decoder's channel interpretation; image origins are normalized.
Interleaved layouts, invalid indices, truncated samples and excess packets
refuse before adoption. File identity, consumed-byte hashes, reservations,
precision promotion and atomic history remain enforced. Remaining native
codecs, live activation and runtime acceptance are still pending.


Unreleased native BMP update: file-based `image.import`, `image.place` and
`image.patch` accept basic `.bmp` files through bounded palette/bitfield rows
and sparse RLE4/RLE8 decoding. Import/placement accept optional static `frame:0`;
page/variant and nonzero frame choices refuse. Shared engine precision promotion,
selection, reservations and atomic history apply after orientation. Bitfields
wider than eight bits per channel and calibrated/profiled BMP color encodings
refuse explicitly before unsupported conversion. Invalid offsets, truncated data
and out-of-frame RLE refuse before adoption. Remaining native codecs, live
activation and runtime acceptance remain pending.


Unreleased clipboard integration update: UI pixel paste and pixel/layer cuts use
the same shared edit coordinator as agent commands, retaining observed document
identity/revision, reservations, atomic history and post-commit cleanup warnings.
Copy completes before a destructive cut is dispatched. Whole-layer paste also
uses the coordinator with typed resident/native snapshots; native sources go
directly to the source-backed worker and cross-storage conversion refuses.
Whole-layer copy/cut workers accept immutable resident/native snapshots; native
layer trees retain their source owners without pixel materialization. Cut also
checks the observed storage/native generation before deletion. Protected
Photoshop sources require an explicit compatible copy before editable layer
capture. Native pixel-copy/cut workers render the captured shared scene, retain
original 16-bit words and document origin in admitted transport buffers, and
retire temporary sources before an OS clipboard write. Native PNG/DIB transport
requires separate allocation admission. Native clipboard limits are 16000-pixel
edges and 256 MiB of retained pixel data, with separate decode/import budgets
and a 128 MiB encoded PNG cap. Coordinated UI/authority activation and
runtime/platform acceptance remain pending;
these routing changes alone do not activate native documents. UI copy/cut
shortcuts submit the immutable frame snapshot directly, retaining native owners
and sharing resident frames; selection precedence and keyboard focus gates
remain unchanged. UI paste also retains its destination frame. Document ID,
revision, storage kind and native generation are checked before clipboard reads
and again by the final layer/pixel edit dispatcher, preserving concurrent work.
Matching retained clipboard images are borrowed without duplicating native
samples. Native PNG/base64 transport admission remains alive through dispatch;
budget refusal preserves the retained copy for retry. External 16-bit samples
are preserved when the retained matching display image has only 8-bit data.
Public resident clipboard request callers remain supported.
For native destinations, OS clipboard file lists now submit file-paste commands
without decoding resident images or re-encoding them as PNG. The existing
16-file limit and observed-destination checks still apply. Resident clipboard
file decoding remains outside the engine lock. Live authority activation and
native runtime acceptance are still pending.


Unreleased native ICO producers now cover `image.import`, `image.place`,
file-backed `image.paste` and `image.patch`. Multi-image imports require an
explicit zero-based `variant`; `frame` and `page` are rejected. PNG variants
retain native16 through import, placement and paste. BMP variants retain alpha
and AND masks. Patching preserves its default-best-variant, raw-channel policy
and existing RGBA8 restriction for PNG variants. All routes retain shared
reservation, precision, cancellation and history guards. Regression source and
compile checks do not qualify live native activation or runtime/release acceptance.


Unreleased native TIFF producers cover `image.import`, `image.place`, file-backed
`image.paste` and `image.patch`. Multi-page import/placement/paste requires a
zero-based `page`; `frame` and `variant` are rejected. Native16 channels, selected
ICC profiles and orientation retain shared engine behavior. Patches retain raw
first-page semantics. Lossless strip/tile codecs are admitted; JPEG compression,
extra sample layouts and larger coding units retain the explicit limits in
[image import](image-import.md#native-source-backed-tiff-unreleased-verification-pending).
These producers do not activate native document authority or establish release readiness.

## Nix profile launch paths (unreleased)

On Linux, Connect AI uses a matching conventional Nix profile launcher when the running process belongs to the default Nix store. It checks the XDG and legacy user profile links first. It can also use the managed default profile at `$XDG_STATE_HOME/nix/profiles/profile` when the user links are missing or resolve to another build. Other candidates include the NixOS per-user profile, `/nix/var/nix/profiles/per-user/$USER/profile`, the active system profile and the default Nix profile. HOME, XDG_STATE_HOME and USER supply only candidate locations. The launcher must resolve to the current executable or its recognized `.peerbrush-wrapped` sibling in the same immutable store object. The lookup reads links and file metadata. See the [Nix profile layout](https://nix.dev/manual/nix/2.35/command-ref/files/profiles).

An absolute launch through a verified custom `nix profile` takes precedence. If the launch path and conventional candidates do not identify a matching profile, Connect AI can discover a custom profile through its absolute `bin` directory on PATH or its `share` directory in XDG_DATA_DIRS. The latter works when a desktop entry launches the store wrapper directly. A custom profile must link through its own numbered generation to a Nix store profile tree with a regular `manifest.json`. The launcher must match the running build, and the generation is checked again before selection. If multiple custom profiles match, launch PeerBrush through the intended profile’s `bin/peerbrush`. [Nix profile generations](https://nix.dev/manual/nix/2.35/command-ref/files/profiles.html) · [XDG data directories](https://specifications.freedesktop.org/basedir/latest/#variables)

The saved command stays on the profile path so Nix can change the target during upgrades and rollbacks. Connect AI skips profile paths that point straight to a package output. If every candidate is missing, non-executable, pinned to a package output, pointed at another build or changing during selection, Connect AI reports the problem before writing client settings. Ephemeral launches require a matching stable profile to register.

Use Connect AI once after upgrading an older registration that stored a Nix `.peerbrush-wrapped` path. Discovery recognizes that old default-store route only with the MCP argument, uses its normal exact-original backup/update behavior, and preserves unrelated server entries, settings and comments. A similarly named wrapper outside a valid Nix store object is not claimed as PeerBrush-owned.

This is source preparation, not proof of Nix package availability or client/native upgrade acceptance. A pinned derivation and qualified native toolchain, actual profile install/upgrade/rollback and client reconnect checks remain required. Standard profile locations follow the [Nix profiles manual](https://nix.dev/manual/nix/2.35/command-ref/files/profiles.html); NixOS per-user profiles are defined in the [NixOS users module](https://github.com/NixOS/nixpkgs/blob/master/nixos/modules/config/users-groups.nix).


## Scoop launch paths (unreleased)

On Windows, Connect AI recognizes the generated Scoop package's flat `apps/peerbrush/VERSION/peerbrush.exe` layout from the observed process image. The selected version directory and the installed manifest must match the running application version. Bounded regular local `scoop-manifest.json`/`scoop-install.json` records, or the legacy `manifest.json`/`install.json` names, establish a local package observation. UTF-8 BOMs are accepted; malformed, duplicate required fields, oversized, linked and unsupported records are refused. This observation does not authenticate a publisher or establish an available bucket update.

Registration keeps `apps/peerbrush/current/peerbrush.exe`, after checking that its resolved target matches the running image. It reads no SCOOP/PATH executable override and invokes no shim or package-manager process. If an upgrade has already moved `current`, open the newly selected application before retrying Connect AI. A missing or different-build launcher fails before discovery/client settings are written. Existing version-pinned PeerBrush MCP entries migrate through the normal exact-original backup path, preserving unrelated servers, settings and comments.

The initial scope is the generated x64 stable-version package with Scoop's normal current junction. Custom extraction layouts, renamed application entries, other architectures and `NO_JUNCTION` installs need a separately verified stable route; Connect AI does not silently register a versioned fallback for a recognized installation. Ordinary portable builds and MSIX family aliases retain their existing routes. User documents/settings stay outside package replacement. Native Scoop install/upgrade/removal and actual AI-client reconnect acceptance remain unverified. These layouts follow Scoop's [installer/current-junction implementation](https://github.com/ScoopInstaller/Scoop/blob/master/lib/install.ps1) and [installed-receipt readers](https://github.com/ScoopInstaller/Scoop/blob/master/lib/manifest.ps1).


## Homebrew launch paths (unreleased)

On macOS, the generated arm64 cask is observed through `Caskroom/peerbrush/.metadata/INSTALL_RECEIPT.json`, its exact stable version, and the moved app's Caskroom link. The disk image stages the app at the version root, while the ZIP stages it inside its named release folder. A unique link must resolve to the running `PeerBrush.app`. The local receipt establishes neither publisher trust nor an available tap update. Unrelated direct copies retain their ordinary command.

Connect AI keeps the matching `PREFIX/bin/peerbrush` command. Its target must resolve to the running executable and be executable; a missing, foreign or retargeted launcher refuses registration before client writes. Older PeerBrush MCP paths migrate through the existing exact-original backup path, preserving other servers, settings and comments. Ownership observation writes no file and invokes no `brew`, cask Ruby, shell wrapper or candidate program.

Candidate locations include `/opt/homebrew`, `/usr/local`, an absolute inherited `HOMEBREW_PREFIX`, and the prefix of an observed `bin` launch. Hints only locate candidates; they cannot establish ownership without the receipt and actual app link. Relative paths and parent escapes are ignored. Custom-prefix Finder launches without a discoverable prefix, renamed casks/bundles and other archive/architecture layouts remain outside this initial scope. No available Homebrew package or general custom-install qualification is claimed.

Receipts are limited to 128 KiB and must be regular non-alias files. Required duplicate fields, invalid/stale versions, unsupported architecture, changed records and multiple matching prefixes stay uncertain. If the app link still identifies this build but its receipt is missing, Connect AI and built-in replacement remain held. Keep the existing client entry and retry after correcting the local installation; this source does not repair Homebrew records automatically.

Actual macOS/Homebrew installation, upgrade, uninstall and client reconnect acceptance remain pending. The layouts follow Homebrew's [Caskroom implementation](https://github.com/Homebrew/brew/blob/main/Library/Homebrew/cask/caskroom.rb), [moved-app links](https://github.com/Homebrew/brew/blob/main/Library/Homebrew/cask/artifact/moved.rb) and [cask receipt](https://github.com/Homebrew/brew/blob/main/Library/Homebrew/cask/tab.rb). Homebrew CLI/app artifacts are described in its [Cask Cookbook](https://docs.brew.sh/Cask-Cookbook).
