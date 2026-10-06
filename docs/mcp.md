# MCP and CLI

MCP starts automatically with PeerBrush. Click **Connect AI** for client configuration. Green means an attached MCP client; red means the editor is waiting for one. There is no separate MCP on/off switch. The stdio adapter attaches to the same live document. It can initialize and list tools while the editor is closed, and reconnects when it opens. Editing calls require the app to be running.

For Codex, register the executable you run:

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

Tools: `peerbrush_observe`, `peerbrush_edit`, `peerbrush_task`, `peerbrush_document`, `peerbrush_history`, and `peerbrush_capabilities`, and `peerbrush_place_image`.

Observe before editing. Use layer IDs, document pixel coordinates, and `expected_revision`. The upper-left pixel is (0,0). Rectangles are `[left, top, right, bottom]`, with right and bottom excluded.

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

Local HTTP clients can POST to `/mcp` or `/rpc` using the bearer token in the runtime `connection.json`. The service binds only to 127.0.0.1. Do not publish the token. Internet-facing clients, arbitrary scripts, and built-in model calls are not part of v0.1.


## Artistic iteration and visible activity

Observe → edit a small batch → inspect the actual result → undo if it is weak → refine and retry. Use redo to compare versions. Do not assume an initial edit is perfect. Publish concise natural-language progress with `peerbrush_task` begin/update descriptions; an empty `scopes` array publishes activity without locking anything.

Use the same unique `actor` in every call. AI undo/redo requires the current revision and only reverses its own latest batch. If another participant edited afterward, inspect again and preserve their changes; selective task undo is on the backlog. Human undo is chronological and can reverse either participant's latest batch.

```json
{"action":"undo","actor":"my-agent","expected_revision":7,"max_edge":768}
```

Undo and redo return PNG feedback by default; use `feedback:"request"` to suppress it. Ordinary edit batches group into one history entry. A new edit after undo clears redo history.

Stdio client presence uses a heartbeat and disconnects at EOF. Abruptly killed adapters and idle direct HTTP clients expire. The green indicator means client presence, not merely that the local server is listening.

## Generated and edited image placement

Use `peerbrush_place_image` to insert an image in one call. Pass exactly one absolute local PNG/JPEG `path` or base64 PNG `png`, plus an exact destination `rect`. The upper-left corner and size are document pixels, independent of UI zoom/pan. Placement resizes with premultiplied interpolation and is one undoable edit.

```json
{"path":"C:/art/generated.png","rect":[100,80,500,380],"layer":"CONTEXT_LAYER_OR_FOLDER_ID","new_layer":true,"name":"Generated detail","actor":"my-agent","expected_revision":7,"max_edge":768}
```

With `new_layer:true` (default), a folder context places a child inside it; a paint-layer context places a sibling above it. `parent` overrides the folder, and `parent:null` chooses the root. Without context it creates a root layer. The returned `placement.layer` is the new layer ID; `placement.rect` and the feedback view report exact coordinates.

For an edited patch, set `new_layer:false` and choose the destination `layer`. `mode:"over"` composites the pixels; `mode:"replace"` replaces the whole rectangular area, including transparent pixels. Pixels outside that rectangle, masks, and editable effects remain. The raster grows if necessary rather than clipping the patch silently. Layer/folder locks, reservations and conflicting revisions still apply.

CLI uses the same operation: `peerbrush.exe cli place_image '@placement.json'`. Atomic multi-command batches can use `{"op":"image.place", ...}` through `peerbrush_edit`; that command also supports natural source size at integer `x`/`y` when `rect` is omitted. CLI feedback contains a PNG path and coordinate manifest instead of printing encoded image data.

Blue marks show affected layers, relevant tools, and rectangular AI work areas. Agent edits animate as presentation feedback without delaying the committed shared state. Observe returns committed pixels and revision, so agents never have to wait for an interface animation to finish.

The Windows development helper `scripts/dev.ps1 run` uses the checkout's `.runtime` workspace rather than the packaged executable's default workspace. The **Connect AI** copied configuration already includes that instance's absolute `--state-dir`. If registering it manually, pass the same directory:

```powershell
codex mcp add peerbrush -- "C:\path\to\PeerBrush\target\release\peerbrush.exe" mcp --state-dir "C:\path\to\PeerBrush\.runtime"
```
