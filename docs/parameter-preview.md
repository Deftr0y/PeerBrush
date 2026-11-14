# Parameter preview scheduling

Held numeric controls keep their desired commands separate from the current document. The existing canvas worker validates those commands through the shared engine and renders the real result. Layer controls and the document filter stack use the same accepted derived document; they do not replay edits while drawing a panel.

While a newer preview is pending, the workspace retains its last accepted rendered canvas. Preview results bind to the engine instance, document, revision, gesture and exact commands. Source changes, target changes, cancellation and lifecycle interruptions discard the pending gesture through the existing guards.

Preview workers wake the workspace when results arrive. If a result changes the canvas after it has drawn, the workspace requests another paint immediately. An otherwise idle workspace keeps its normal background refresh cadence while a preview is pending.

Changing or dismissing the whole-image filter draft schedules the next canvas pass directly. The new settings or comparison selection reach the shared renderer even when the pointer stops moving.

Resident brush previews retain the changed region from replays that are superseded before drawing. The next canvas update covers the accumulated region, including any segment the renderer skipped. A change of stroke settings uses a complete render when the cached pixels belong to an earlier draft. Each mask paint draft also refreshes its derived feathered mask for both composite and isolated mask views.

The resident preview worker takes brush and eraser damage from the actual replay. Smudge uses the engine's conservative stroke bounds against the same captured selection it renders. Preview scheduling does not lock the live document again to calculate stroke bounds.

Resident stroke scheduling compares captured points, pressure samples and command settings directly. An unchanged stroke keeps the same preview identity while reduced-resolution mode waits for input to settle. The preview worker assembles the full command only when it starts a render, using the captured samples and the same fields as the commit path.

Disk-backed stroke previews let an active render finish while the gesture, target, document and sampling grid still match. The canvas can show that result while scheduling the latest samples. At full detail, compatible input skips building another stroke command. Reduced-resolution mode still checks the exact input for its settle timer and can replace a pending reduced frame when full detail is due.

Disk-backed pointer adjustments to layer and effect settings also let a compatible render finish. Values from later input coalesce behind it. The completed frame can show progress, and the next request uses the latest values. A failed older value yields to the latest input. Changes to the target, source, viewport or sampling grid supersede the active render immediately. Settled captures require a post-gesture frame. Exact entry and release retain their existing commit and cancellation behavior.

Layer reordering and visibility sweeps on disk-backed documents use the same scheduling. A compatible render can finish while the pointer moves. The next request uses the latest drop destination or set of affected layers. Releasing the pointer submits the final commands through the shared engine.

Free Transform can display intermediate disk-backed renders during a compatible handle drag, including when transforming editable text. Releasing the handle schedules the final transform. Apply waits for the frame rendered from the current command.

The Move, Rotate and Scale canvas handles also let a compatible render finish during a drag. On release, the shared engine applies the transform at the release coordinates in one edit.

Rectangle, Ellipse and Gradient drags use the same scheduling, including drags that start in the gray workspace. The release coordinates still determine the committed drawing.

Pen previews can finish a compatible render while you move an anchor or handle, or hover to extend an open path. The next preview takes the latest coordinates. Releasing a drag submits the recorded release coordinates through the shared engine.

Whole-image filter strength and settings can show completed renders during a compatible drag on resident and disk-backed documents. Changing the filter, comparison view or source makes older results ineligible. A superseded render cannot set the current draft's error. Apply submits the current settings through the shared engine, including while using a comparison view. The reference photo for the draft renders independently of the preset list. It shows the last finished preview while newer settings render, and changing the selected filter clears that preview.

On-canvas text fields can show rendered text while you place or resize the field. Disk-backed drafts capture the updated field in the same input frame. Intermediate renders can show the text, while caret placement uses the layout of the exact current draft. A rejected native render retains its command identity so the canvas can keep the last frame until the draft changes. Releasing the handle schedules the final reflow. Applying the draft records one edit.

On-canvas typing can also show completed renders while newer text is being entered. Keystrokes update the draft immediately. After 250 ms without a text change, or when the text field loses focus, the canvas requests a settled preview of the latest text. The preview still validates the exact draft before accepting caret positions.

The derived cache holds one parameter document for the current gesture. Only a pure parameter request may populate it. A view-only redraw can render that accepted document again, while mixed tool, hover and transient commands retain their shared-engine ordering. Provisional numeric text still changes the command only on the control's existing Enter/focus-loss behavior.

The layer list requests thumbnails for visible rows and the floating layer during a drag. Scrolling requests thumbnails for newly visible rows. Ready native sampling replies still verify the current document generation before display or use.

The layer list reads active reservation status under one engine lock and reuses the result for row animation and badges. Canvas reservation outlines also filter expired leases.

Apply or pointer release performs a fresh shared-engine edit with the original source revision and current reservations. The preview cache grants no permission to commit. One successful commit creates one undo step; cancellation creates none. Native 16-bit source pixels remain at their original precision.

Blend If, gradient-stop and contour handles sample the button-release position before committing. Later pointer motion in the same input frame does not move the released handle.

The prepared regression tests cover blocked-worker control input, coalescing, changed commands, actual native8/16 output, both panels, stale errors, source replacement, cancellation and reservations. Qualification of this source change is pending; no timing improvement or reproduction of the original physical-input regression is claimed. Shared-engine final commit remains synchronous, and an in-progress replay is superseded at stage boundaries rather than interrupted inside its algorithm.

Keyboard increments and supported accessibility value actions use the immediate shared-engine edit path. Prepared regression source covers cumulative arrow input, disabled controls and nonfinite accessibility values. Desktop screen-reader acceptance remains pending.
