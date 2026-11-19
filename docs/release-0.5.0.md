# PeerBrush 0.5.0

Unreleased. These notes describe source changes on `version/0.5`. Candidate verification is tracked in the [0.5 checklist](https://github.com/Deftr0y/PeerBrush/blob/main/BACKLOG.md#05-owner-checklist).

## Saving, recovery and projects

- Save offers a new editable PSD or PSB destination when previous-save storage is full. The original file and backups remain intact.
- Legacy large projects can use compressed Photoshop channels in their recovery checkpoints. Recovery retains native depth, editable layers and masks, and the current merged composite.
- Drop images on an existing project tab to import them there. A close request waits for a busy operation to settle before offering the normal unsaved-work choices.

## Editing

- Adjustment layers have a dedicated stack icon and a consistent default name. Existing names and names supplied when creating a layer are preserved.
- Right-click a layer or folder and choose **Color** for presets or a custom RGB color. The custom picker previews the accent on the layer row and applies it with one undo step. Cyan and custom colors are preserved in PeerBrush editable sources.
- Copy followed by Paste during a selection edit waits for the selected pixels to be copied and confirms that the clipboard still contains that copy.
- Folder icons show rendered contents. New paint layers go inside a selected folder or immediately above the selected layer in its current folder. Hidden layer rows are darker.
- Arrow keys nudge by one document pixel in Move. Rotate and Scale use fine increments, and Free Transform shows the draft before applying it.
- New Stroke effects start inside the layer boundary. Existing Stroke settings are retained.
- The top bar indicates active AI work. Reservations can be scoped to the layers or regions needed for the task.

## Photoshop files and responsiveness

- The protection report identifies unsupported document-level Photoshop settings. Protected documents show the saved composite at its original precision.
- Layer thumbnails reuse the source metadata already available in the layer stack. Edits that retain the same display profile also reuse its digest.
