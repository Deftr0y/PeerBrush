# Native source editor ownership

The source window reads borrowed `DocumentView` and `SourceView` metadata. A
native draft shares the actual admitted source definition and reserves mutable
draft capacity from the same document pool before copying it. It does not copy
a raster or a native document. Existing native channel words remain unchanged.

Native commands retain `Admitted<Value>` through submission, preview and terminal
completion. A streaming source fingerprint selects the cached owner; an invalid
or changed draft cannot return an older command. The raw `command()` API is for
resident compatibility. Source-window Apply reports queued admission and keeps
the draft pending; only matching terminal success closes it. A late completion
from an older editor nonce cannot close a replacement draft.

Mutable SVG, text, font-search and numeric strings have capacity admitted before
the widget is called. Numeric pointer dragging uses the pinned stock DragValue
behavior; its focused text entry uses a fixed64-byte UTF-8 buffer. Source-only
widgets reject oversized event payloads before TextEdit clones filtered events.
Refused insertion restores the old draft, including a selected range that stock
TextEdit deletes before requesting insertion. No global controls are changed.

Each native editor has private, unique TextEdit IDs and retains cheap clones of
their actual shared undo state. The admission accounts for the pinned100-state
undo limit, redo/flux overlap, deque/vector capacity and bounded event/temporary
copies. A closed nonce cannot prepare another widget. Close clears the shared
undoers before removing precisely those Context entries and refunding their
admission. A teardown interrupted before acknowledgement retains accounting
debt instead of treating uncertain destruction as a refund.

This is not combined application RAM qualification. Galley, nested LayoutJob and
Row owners, font caches, and retained platform output or clipboard copies need
independent admission and actual destruction acknowledgements. The stock font
catalog remains a separate owner. Native source validation/rendering belongs to
the captured worker with its actual FontOwner and source session; the native
source window does not use the legacy global font snapshot as validation proof.

The central native dispatcher and preview transport must retain the owned
command, and native Pen handles must consume the actual tagged worker source
projection. This source-window packet does not provide a shadow document or
qualify Pen preview by itself. The application canvas limits remain closed until
the complete native routes and resource owners have passed acceptance.
