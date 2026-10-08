# AI collaboration

AI can edit the shared project directly or offer an optional proposed result. Model choice remains in the client. Proposal preparation uses the same document commands, native precision, locks, source budgets and reservations as ordinary edits. A COW draft is transient: preparing, viewing, rejecting or closing it adds no source edit or undo step.

Open **AI tasks** and select a proposal. The blue review shows its actor, label, source revision, operations and affected document/layer regions. The canvas renders the draft through the shared engine. Original shows the current source; Proposed result returns to the frozen draft. Accept is enabled after the proposed image renders, and commits it once with AI attribution. Undo/Redo restores that complete accepted batch. Reject releases it; Close/Escape leaves it pending. Human canvas input dismisses review before editing.

A newer source revision invalidates every old draft rather than overwriting intervening work. Stale, expired or interrupted drafts release their native sources and show the reason. The canvas returns to current artwork; acceptance stays unavailable. Reservations acquired during review also guard acceptance. Changed/deleted external assets do not alter an already prepared result. New/open clears transient proposals.

Ending, expiring or taking over a task preserves committed edits. Recent task records show why it stopped and its original scopes. No rollback or reservation restart occurs automatically. Existing selective task undo can compensate retained task batches while preserving unrelated later human work, and refuses overlapping conflicts atomically.

Limits: four pending proposals, 256 MiB of additional unique native raster tiles, 100 commands/8 MiB per batch, 15 minutes per proposal, 16 proposal metadata records and 100 recent task records. Task-bound proposals need their reservation to remain active. Recovery here describes interrupted task feedback; the existing autosave mechanism remains separate. Proposal drafts, task records and preview images are never PSD source data.

The same flow is available through `peerbrush_proposal` and CLI `proposal`; see [MCP/CLI parameters](mcp.md). These APIs share the existing trusted local actor convention. The `human` actor is not a new identity authentication mechanism.
