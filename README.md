<div align="center">

<img src="assets/peerbrush-logo.png" alt="PeerBrush P" width="132" />

# PeerBrush

### You and your AI. Same canvas.

**An image editor where humans and AI agents work in the same editable project.**

[![Build](https://github.com/Deftr0y/PeerBrush/actions/workflows/ci.yml/badge.svg)](https://github.com/Deftr0y/PeerBrush/actions/workflows/ci.yml)
[![License: GPL v3](https://img.shields.io/badge/license-GPLv3-orange.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/built_with-Rust-dea584.svg)](Cargo.toml)
[![MCP](https://img.shields.io/badge/AI_interface-MCP-5aaaff.svg)](docs/mcp.md)

[Get started](docs/getting-started.md) · [Connect your AI](docs/mcp.md) · [Development](docs/development.md) · [Roadmap](FOLLOWUPS.MD)

</div>

PeerBrush is an image editing application designed from the ground up for humans and AI agents to work on the same project together.

Instead of treating AI as a separate prompt box, PeerBrush gives both the user and the agent access to the actual editing environment, including layers, selections, masks, transforms, brushes, effects, and document state.

The goal is simple: **Make AI a participant in the creative workflow, not just a generator.**

![The native PeerBrush workspace](docs/images/workspace.png)

*Actual native workspace. Ember orange identifies human interaction; blue identifies AI work.*

## A shared creative workspace

Most AI image workflows are built around **Prompt → Generate → Result**. PeerBrush is built around **Human + AI → Shared Project**.

You stay in control while AI works directly alongside you inside the same editing environment. Both participants use the current document state, the same editing commands, and visible, editable, reversible changes.

| Create and edit | Work with AI | Keep control |
| --- | --- | --- |
| Paint, erase, fill, shapes and selections | MCP and CLI access to the live document | Shared undo/redo with participant checks |
| Layer and folder hierarchy, multi-selection | Actual PNG views with pixel coordinates | Atomic edits and conflicting-revision checks |
| Separate editable color and mask effect stacks | Place generated images into exact regions | Optional layer/region reservations and takeover |
| Levels, curves, blur and color adjustments | Blue activity markers and animated changes | PSD working files and PNG export |

### Built for the way you work

- **Visual, compact controls.** White tool glyphs, Ubuntu Sans, uncluttered range values and live canvas feedback.
- **A useful hierarchy.** Distinct folders, multi-layer editing, drag into or out of folders, animated reorder previews, and visibility sweep gestures.
- **Editable effects.** Each layer or folder has independent color and mask stacks. Return to an effect and change its settings after later edits.
- **Familiar navigation.** Maya-style W/E/R gizmos, Q to hide, F to frame, middle drag to pan, wheel or Alt + right drag to zoom.
- **A practical brush.** Hardness, flow, opacity, spacing, angle, roundness and smoothing; live size changes, Alt eyedropper, two colors and X to swap.
- **Real clipboard support.** Copy a selection or composite and paste an editable layer; accept images copied in other applications.

## Run PeerBrush

The Windows portable build runs directly from `peerbrush.exe`. **No Rust installation is needed to use it.** Open/import/drop an image, work in layers, save PSD, and export PNG.

To develop from source:

```sh
cargo run
```

```sh
cargo test --locked
cargo build --release --locked
```

Windows, macOS and Linux builds are configured in CI. This checkpoint has been tested on Windows; other platforms still require successful CI and native desktop verification. See [getting started](docs/getting-started.md) and [platform development notes](docs/development.md).

## Connect an AI agent

MCP starts automatically with the editor. The top bar reports whether a client is attached. The adapter can register while the editor is closed, then reconnect when you open it.

For Codex, register the executable you run:

```powershell
codex mcp add peerbrush -- "C:\path\to\PeerBrush\peerbrush.exe" mcp
```

Agents can observe a layer, mask, or cropped canvas region; edit through shared commands; place generated or edited pixels; and undo, inspect, refine, or redo an attempt. Model choice stays outside PeerBrush, so different compatible models and agents can plug into the same workflow.

```json
{
  "path": "C:/art/generated.png",
  "rect": [100, 80, 500, 380],
  "layer": "CONTEXT_LAYER_OR_FOLDER_ID",
  "new_layer": true,
  "name": "Generated detail",
  "actor": "my-agent",
  "expected_revision": 7
}
```

*Arguments for `peerbrush_place_image`. Placement is undoable and returns the layer ID plus cropped PNG feedback. Set `new_layer:false` and `mode:"replace"` for an edited patch on an existing layer.*

See [MCP and CLI](docs/mcp.md) for setup, tools, reservations, coordinate conventions and artistic iteration.

## Core idea

PeerBrush is built around a shared creative workspace where:

- You can edit manually at any time
- AI agents can inspect and modify the same project
- Both sides work with the current document state
- Changes remain visible, editable, and reversible
- Tools can be used through the interface or programmatically
- Different AI models and agents can plug into the same workflow

## Vision and planned features

The long-term goal is an MCP-native creative application where compatible AI agents inspect, understand, and manipulate the same project the user is actively editing.

- Layer-based image editing
- Masks and selections
- Transform and drawing tools
- Non-destructive editing where possible
- Shared document state
- MCP support and agent-accessible editing tools
- Local and remote model support through connected agents
- Extensible tool and plugin architecture

The next slices include incremental/GPU rendering, broader selections and subject masking, liquify, text/vector source layers, stronger PSD fidelity, and selective task undo. The living [FOLLOWUPS.MD](FOLLOWUPS.MD) records requests and their status.

## Status

🚧 **Early development — [v0.1.1 checkpoint](docs/checkpoint-0.1.1.md)**

PeerBrush is currently experimental and under active development. Features, APIs, and project structure are expected to change.

Editable PSD support currently targets **PSD v1, 8-bit RGB**, basic raster layers, isolated groups, supported blend modes and raster masks. PeerBrush preserves editable effect sources in private metadata and writes current standard raster/mask data and a merged composite. Unsupported Photoshop features open as a read-only merged preview. Full Photoshop compatibility, PSB and full color management are future work.

The shell uses Rust, egui/eframe and wgpu. Sparse copy-on-write tiles share unchanged image data with history. Compositing currently runs on a CPU worker, with bounded effect caches and screen-sized previews. Full incremental and GPU compositing remain on the roadmap.

## License

GNU General Public License v3. See [LICENSE](LICENSE). Bundled fonts and dependencies retain their own licenses; portable packages include source and third-party notices.
