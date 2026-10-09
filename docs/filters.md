# Whole-image filters

Open **Image → Whole-image filters…** to process the entire composited image, independently of the selected layer. Source layers, folders, masks and layer effects remain editable. The document filter stack runs after layer compositing; the top filter runs last.

Search the library by name, category or effect ID. Fifteen original presets cover Tone, Color and Blur & light. Thumbnails render a shared native-depth reference image; choosing a preset previews the actual project. Strength sits beside the effect name, followed by its settings. **Filtered** shows the pending result, **Current image** shows the existing stack, and **Original** bypasses all whole-image filters for comparison.

**Apply filter** adds one undoable filter. Selecting an existing stack entry and **Apply changes** updates it in one edit. **Cancel**, Escape, closing the browser or switching projects discards the pending preview. Newer shared edits cancel it; a stale preview cannot overwrite newer work. **Image → Filter stack** provides bypass, removal, ordering and compact strength controls. Dragging strength previews actual pixels and commits once on release; double-click its value to type.

Use **Save new preset** to save the current kind, settings and strength under a name and category. Custom presets can be updated, renamed or deleted. Export/import uses a strict versioned JSON format; import creates a new custom ID. Curated presets are immutable. Presets belong to the running instance, are shared by its project tabs, and live outside document history and PSD source data. Invalid or externally changed library files are preserved rather than overwritten. Limits are 256 custom presets, 4 MiB per library/import and 64 KiB of settings per preset.

Whole-image edits reserve the entire document and honor every layer/folder lock, task ownership and revision check. They cannot claim a smaller layer or rectangle scope. Selective task undo preserves later human pixels and independent settings, and refuses conflicting or locked changes. AI edits commit immediately, show blue activity over the affected document, and animate only presentation; human input interrupts that feedback.

Native 8/16-bit samples are retained through filtering, strength, undo and PSD/PNG export. Display previews project to 8 bit. Derived images use a bounded 128 MiB cache with source tile identities; intermediate previews cannot replace authoritative sources or poison final renders. The document stack is limited to 32 filters, and native output/scratch limits apply before committing. Spatial filters require proportional image resizing. Liquify remains a layer effect.

PSD source format 15 retains the editable stack and original sources alongside a visible filtered standard raster layer and current merged composite. Original standard raster/mask channels are also written beneath it with their roots hidden; PeerBrush restores their actual visibility from validated sources. Existing standard folder-effect baking and the 200-record PSD limit apply. Photoshop sees the rendered result and preserved standard channels; this is not native Photoshop filter metadata. Older PeerBrush readers protect the newer source format and show its saved composite.

MCP/CLI clients use `filters` / `peerbrush_filters` for `list`, `thumbnail`, `preview`, `save`, `rename`, `delete`, `import` and `export`. Project previews require current `project_id`, `document_id` and `expected_revision`, return actual PNG content with `document_rect`, and include frozen commands to apply. Thumbnail rectangles use `filter_thumbnail` coordinates rather than document coordinates. Observe `document.filters` for IDs, settings, strength and bypass state.

```json
{"op":"filter.add","preset":"posterize","settings":{"levels":5},"weight":0.6}
```

Use the ordinary shared edit transaction to apply this command. `filter.update` accepts a filter ID and kind/settings/weight/enabled; `filter.delete` removes it; `filter.reorder` takes a zero-based stack index. Explicit settings override preset settings before validation and reservation checks. Proposals freeze preset definitions, so changing a custom preset later cannot alter a reviewed draft.
