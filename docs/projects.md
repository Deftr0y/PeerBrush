# Independent projects

PeerBrush keeps up to sixteen open project engines. Each owns its document, selections, undo/redo, reservations, proposals, dirty state, path and external-file version. Switching tabs preserves the layer selection, Color/Mask channel, effect selection, collapsed folders, pan and zoom. Unfinished gesture previews are canceled on switching; committed edits remain intact. Brush preferences and the internal clipboard belong to the workspace.

New/Open, a dropped PSD and autosave recovery create new tabs. Files are decoded before registration, so failed or canceled loading cannot replace existing work or leave a partial project. A completed open activates only if the initiating tab is still visible. Named recovery snapshots retain native depth and reopen as unsaved projects with no original save destination.

Copy layers or folders, switch tabs and paste to keep their editable children, masks, effects and native sources. Drag roots onto another tab for an actual temporary-engine destination preview. Drop copies; Shift-drop moves. Neither real history changes while hovering. Copy creates one destination undo step; move creates one step in each document. Revisions, source identities, locks and reservations are rechecked at commit. If either side fails, both engines roll back. Native16 trees promote an 8-bit destination. Protected Photoshop documents require an explicit compatible copy before tree transfer.

Closing a clean tab releases its engine and chooses another tab; closing the last creates a blank project. Dirty tabs currently refuse closing until saved or explicitly discarded through a guarded project action. Save/Don't Save/Cancel native lifecycle dialogs are tracked separately as PB-080. File operations retain their project engine and source identity across asynchronous work. Closed or replaced sources reject later callbacks. Saving an older snapshot of the same document can leave newer edits dirty; another tab or application changing its PSD prevents overwriting that newer file.

## Agent targeting

Call `peerbrush_projects` with `{"action":"list"}`, then observe the intended project. Observations expose `project_id` and `state.document.id/revision`. Runtime source identities change on replacement and distinguish duplicate opens of one PSD. A stable project ID remains independent of the visible tab.

```json
{"project_id":"PROJECT_ID","image":false}
```

Every explicitly targeted AI mutation needs the observed `document_id` and exact `expected_revision`. Missing, closed, stale or ambiguous targets reject without editing another project. Untargeted legacy calls remain compatible with a single project; agents must target writes when several are open. Only human input may select the visible tab. Slow HTTP operations run on bounded workers, and registry/UI tab reads do not wait for a busy background engine.

```json
{"project_id":"PROJECT_ID","document_id":"DOCUMENT_ID","expected_revision":7,"actor":"my-agent","commands":[{"op":"layer.update","layer":"LAYER_ID","name":"Background artwork"}],"feedback":"request"}
```

`peerbrush_projects` offers `list`, `new`, `open`, `select`, `close`, `copy`, `move` and `preview_transfer`. New/Open requires canvas dimensions/depth or a local PSD path. Agent-created projects stay in the background. Close requires `project_id`, `document_id`, `expected_revision` and explicit `discard:true` if dirty, and respects reservations.

Transfers target the destination with the ordinary three guards and supply matching source guards. `layers` selects source roots; `target` is the destination layer/folder context. Optional `source_task` and `task` refer to independent reservations. `preview_transfer` uses the same fields, plus `move:true` to validate a prospective move, and returns PNG image content with document coordinates and `preview_only:true`.

```json
{"action":"copy","actor":"my-agent","source_project_id":"SOURCE_PROJECT","source_document_id":"SOURCE_DOCUMENT","source_revision":3,"layers":["SOURCE_FOLDER"],"project_id":"DESTINATION_PROJECT","document_id":"DESTINATION_DOCUMENT","expected_revision":8,"target":"DESTINATION_LAYER"}
```

Use `action:"move"` for an atomic source cut and destination paste. Undo each affected project explicitly with `peerbrush_history`, using its current guards. Human or agent edits elsewhere remain subject to the existing history/conflict rules.
