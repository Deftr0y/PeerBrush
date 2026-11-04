# Parameter preview scheduling

Held numeric controls keep their desired commands separate from the current document. The existing canvas worker validates those commands through the shared engine and renders the real result. Layer controls and the document filter stack use the same accepted derived document; they do not replay edits while drawing a panel.

While a newer preview is pending, the workspace retains its last accepted rendered canvas. Only a result matching the engine instance, document, revision, gesture and complete pending commands may replace it. New input coalesces behind the single pending canvas job. A superseded job's result or error cannot replace newer work. Source changes, target changes, cancellation and lifecycle interruptions discard the pending gesture through the existing guards.

The derived cache holds one parameter document for the current gesture. Only a pure parameter request may populate it. A view-only redraw can render that accepted document again, while mixed tool, hover and transient commands retain their shared-engine ordering. Provisional numeric text still changes the command only on the control's existing Enter/focus-loss behavior.

Apply or pointer release performs a fresh shared-engine edit with the original source revision and current reservations. The preview cache grants no permission to commit. One successful commit creates one undo step; cancellation creates none. Native 16-bit source pixels remain at their original precision.

The prepared regression tests cover blocked-worker control input, coalescing, changed commands, actual native8/16 output, both panels, stale errors, source replacement, cancellation and reservations. Qualification of this source change is pending; no timing improvement or reproduction of the original physical-input regression is claimed. Shared-engine final commit remains synchronous, and an in-progress replay is superseded at stage boundaries rather than interrupted inside its algorithm.

Ordinary keyboard increments keep their existing immediate shared-engine edit path. The prepared control regression holds an obsolete worker through release and repeated arrow inputs to check cumulative values and undo. Accessibility action support is not enabled in the current dependency features, so no accessibility-action qualification is claimed.
