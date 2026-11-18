# PeerBrush 0.5.0

Unreleased. These notes describe development on `version/0.5`.

## Editing

- Adjustment layers have a dedicated stack icon and a consistent default name. Existing names and names supplied when creating a layer are preserved.
- Right-click a layer or folder and choose **Color** for presets or a custom RGB color. The custom picker previews the accent on the layer row and applies it with one undo step. Cyan and custom colors are preserved in PeerBrush editable sources.
- Copy followed by Paste during a selection edit waits for the selected pixels to be copied and confirms that the clipboard still contains that copy.

## Photoshop files and responsiveness

- The protection report identifies unsupported document-level Photoshop settings. Protected documents show the saved composite at its original precision.
- Layer thumbnails reuse the source metadata already available in the layer stack. Edits that retain the same display profile also reuse its digest.
