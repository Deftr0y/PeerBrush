# Selection refinement and local learned selection

Select → Refine selection shows the actual cutout, confidence mask or overlay while adjusting Smooth, Feather, Shift edge, Contrast and Follow image edge. Apply creates one history entry; Cancel restores the committed view. Select → Mask from selection creates an editable layer mask, and Refine layer mask keeps previous mask steps underneath the new result. Native 16-bit mask values and image channels retain their original precision. Edge guidance uses an 8-bit display projection; it does not replace source pixels.

Automatic selection uses an optional external executable. The editor sends one PNG preview with explicit document coordinates and imports the returned grayscale confidence mask through the same undoable selection command used by MCP and CLI. The application does not choose a model or bundle a Python installation/model weights. The existing Object region selection remains a deterministic perimeter-color tool.

## Reference CPU provider

Install Python 3.12 and create a separate environment. From the source directory:

```powershell
python -m venv C:\PeerBrush-provider
C:\PeerBrush-provider\Scripts\python.exe -m pip install -r integrations/segmentation-requirements.txt
```

The reference integration uses [rembg](https://github.com/danielgatis/rembg) and ONNX Runtime. Its requirements pin the CPU runtime verified on Windows; newer runtime packages can have different platform requirements. Choose a model in your provider configuration. For example, [U²-Net's lightweight U2Netp](https://github.com/xuebinqin/U-2-Net) is a learned foreground detector. The first call can download its weights; warm the provider separately before using the editor and check the selected model's distribution terms. PeerBrush does not redistribute those weights.

Save a configuration outside the repository, adjusting all paths:

```json
{
  "program": "C:\\PeerBrush-provider\\Scripts\\python.exe",
  "args": ["C:\\PeerBrush\\integrations\\segmentation_rembg.py", "--model", "u2netp"],
  "timeout_ms": 180000
}
```

Launch PeerBrush with the config path in its environment:

```powershell
$env:PEERBRUSH_SEGMENTATION_CONFIG = 'C:\PeerBrush-provider\provider.json'
.\peerbrush.exe
```

Select subject and Select subject in region become available in the Select menu. The result is a soft selection; refine it or create a layer mask afterward. For an object, MCP/CLI can supply a point inside the foreground component. The reference provider keeps the component containing that point, including its soft boundary. If the point is outside learned foreground, the operation fails without changing the selection. Detection quality depends on the externally chosen model and image.

## Engine and client commands

```json
{"method":"segment","params":{"actor":"agent","expected_revision":12,"rect":[100,80,740,560],"point":[300,240],"mode":"replace"}}
```

MCP exposes the same operation as `peerbrush_segment`. Observe first for the current revision, and include `task` when working under a reservation. Successful responses include the source revision, document ID, document rectangle and actual confidence PNG feedback. `peerbrush_observe` accepts `selection_view`: `cutout`, `mask` or `overlay`.

External clients can import their own confidence directly:

```json
{"op":"selection.import","rect":[100,80,740,560],"png":"BASE64_PNG","channel":"luma","mode":"replace","source_revision":12,"document_id":"OBSERVED_ID"}
{"op":"selection.refine","feather":3,"edge":70,"edge_radius":8,"sample_merged":true}
{"op":"mask.from_selection","layer":"LAYER_ID","mode":"replace"}
{"op":"mask.refine","layer":"LAYER_ID","smooth":2,"contrast":15}
```

Import supports a PNG path instead of base64 and alpha-channel confidence instead of luma. The explicit rectangle maps input pixel centers into document pixels with bilinear confidence resampling. Replace/add/subtract/intersect share ordinary selection semantics. Imports/refinement are limited to 16-megapixel documents or mask frames, with bounded PNG decoding and tiled scratch storage. Native masks keep 16-bit coverage; document selections currently store 8-bit confidence.

## Provider protocol

The configured executable receives one UTF-8 JSON object on stdin, then EOF. It must write only one JSON object on stdout and exit successfully. Diagnostics belong on stderr. Arguments are passed directly, without a shell. Request:

```json
{"version":1,"document_id":"ID","source_revision":12,"document_rect":[100,80,740,560],"width":640,"height":480,"png":"BASE64_RGBA_PREVIEW","point":[300,240]}
```

Response:

```json
{"version":1,"document_id":"ID","source_revision":12,"document_rect":[100,80,740,560],"channel":"luma","png":"BASE64_CONFIDENCE_PNG"}
```

The preview edge is at most 2048 pixels. JSON input/output is capped at 16 MiB, only one inference runs at a time, and the timeout is configurable from 1 to 180 seconds. Human changes or project replacement cancel the process/result; metadata must match the requested snapshot. The engine checks read-only state, reservations and task ownership before inference and again when committing. Provider failure, invalid output and stale results leave artwork and selection intact. Provider diagnostics can be inspected by running it independently; PeerBrush does not copy them into project files or protocol responses.

Image-guided refinement implements the local linear formulation from [Guided Image Filtering](https://people.csail.mit.edu/kaiming/eccv10/index.html), with bounded tile halos and native mask output.
