# PeerBrush 0.4.0 draft release notes

PeerBrush 0.4.0 is unreleased. These notes describe the changes on `version/0.4`.

## Editing

- Start a rectangular marquee in the gray workspace and drag onto the image. The selection commits on release and stays within the canvas. [Canvas controls](canvas-tablet.md)
- Edit text directly on the canvas. Use the field handles to move, resize and rotate it, or use Free Transform. The source remains editable through supported transforms. [Getting started](getting-started.md)
- Press **Shift+D** to duplicate the selection or selected layers. Transform the active copies and press **Shift+D** again to repeat that transform on the next copies. Each duplication and its repeated transform undo together. [Getting started](getting-started.md)
- **Ctrl+E** selects the Eraser by default. Magic Wand explains the difference between connected matching pixels and all matching pixels. Text alignment uses labeled icons, and color controls use interactive pickers. [Settings](settings.md)
- Whole-image filters show the effect on a bundled reference photo. Open **Canvas view…** for zoom, framing, rotation, mirroring and reset. [Filters](filters.md) · [Canvas controls](canvas-tablet.md)
- Production export offers filename placeholder buttons and shows the planned names alongside the output preview. [Production export](production-export.md)

## Large documents and Photoshop files

- The native large-document path handles workflows up to 16,000 × 16,000 pixels. Sparse previews and transforms avoid work in empty tiles where the source bounds allow it. Dense projects and costly commands can still reach resource limits. [Large documents](large-documents.md)
- Editable PSD and PSB support covers additional validated structures and metadata. Unsupported structures open with their saved composite in read-only mode. Working saves preserve native channel depth and include current standard raster, mask and merged image data. [Photoshop fidelity](photoshop-fidelity.md)

## Before publication

The production release gate, native acceptance and package verification remain pending for this source. Earlier development results are recorded in the [backlog](../FOLLOWUPS.MD). Scoop, Homebrew, apt and Nix definitions and support are in source. Their installs, upgrades and public delivery still need qualification. [GitHub issue #59](https://github.com/Deftr0y/PeerBrush/issues/59)
