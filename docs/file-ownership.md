# Open-file ownership

This describes unreleased development behavior.

PeerBrush refuses a second open of an owned PSD or PSB. The native warning identifies the existing project; an attempt in the same workspace brings that tab forward without replacing its document. Its unsaved pixels, selection, saved view and history remain intact. Open, recent files, Photoshop document drops and MCP/CLI project opens use the same loader checks. The file is claimed before hashing and decoding, so pending opens also count.

Ownership compares canonical paths and physical file identities, including hard links and Windows case aliases. Different files with the same display name remain independent. Locks live in per-user temporary storage rather than alongside the document, so opening a source does not require writing to its directory. Separate instances share these OS locks even when their connection state directories differ. Lock records contain only a runtime project identifier; a stale record cannot keep a file locked after the process exits. Failed or cancelled loads release their claims.

Saving keeps the destination and replacement file identity owned across atomic publication. Save As cannot replace another open project's file, even with overwrite consent; a successful Save As releases the previous destination. Closing, source replacement and validated application exit release ownership, including claims referenced by derived preview snapshots. Recovered previous saves become independent unsaved documents.

These locks coordinate PeerBrush processes. Other applications do not participate, so the independent content and publication checks in [file safety](file-safety.md) remain essential. Native cross-platform verification must be reported separately; this change does not establish the cause of the previously reported data-loss incident.
