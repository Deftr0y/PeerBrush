# PeerBrush backlog

This file tracks requests, priorities, acceptance, verification and release status. Update it in the same change as the relevant implementation, verification or release. GitHub issues may link to a PB item for discussion. Historical evidence stays in [FOLLOWUPS.MD](FOLLOWUPS.MD).

The [0.5 owner intake](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#peerbrush-05-owner-requested-intake) defines the 15 checks for this milestone. Earlier work with outstanding acceptance is tracked under Later. A change in `main` or a passing development check does not establish shipment. The latest public release at reconciliation was [v0.30.1](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.1). The [0.4 development baseline](https://github.com/Deftr0y/PeerBrush/commit/102970a0aeb8dd33fc90a999f435913626a00790) was on `main` and the 0.5 worktree has additional local changes. The records reviewed include [open issues](https://github.com/Deftr0y/PeerBrush/issues?q=is%3Aissue+is%3Aopen), [open PRs](https://github.com/Deftr0y/PeerBrush/pulls?q=is%3Apr+is%3Aopen) and [release tags and assets](https://github.com/Deftr0y/PeerBrush/releases).

| Status | Evidence required |
| --- | --- |
| Open | A request or report has a remaining fix or reproduction. |
| In progress | Work has started and some acceptance criteria still require implementation or investigation. |
| Implemented / verification pending | Source addresses the criteria. Record acceptance still owed, including its platform. |
| Verified but unreleased | Record the exact build, platforms and passed checks. |
| Shipped | Record the release tag, platform or distribution channel, and actual published evidence. |

## 0.5 owner checklist

| Intake | Check | Canonical item |
| --- | --- | --- |
| 01 | Autosave at the 256 MiB channel limit | [PB-080](#pb-080), recovery |
| 02 | Save with a full previous-version store | [PB-138](#pb-138), full store |
| 03 | Image drops into consecutive existing project tabs | [PB-079](#pb-079), image drops |
| 04 | Close blocked after an operation | [PB-080](#pb-080), close |
| 05 | Folder composite thumbnail | [PB-029](#pb-029), icon |
| 06 | Contextual layer creation | [PB-049](#pb-049) |
| 07 | Darker hidden rows | [PB-014](#pb-014) |
| 08 | Predictable layer and folder drag sort | [PB-029](#pb-029), insertion |
| 09 | Large multi-layer Duplicate and Copy/Paste | [PB-046](#pb-046) |
| 10 | Stroke appears on the canvas | [PB-136](#pb-136) |
| 11 | Minimal AI reservations and human takeover | [PB-069](#pb-069), scope |
| 12 | AI-created layers paintable after task end | [PB-069](#pb-069), task end |
| 13 | Active task animation in the top bar | [PB-002](#pb-002) |
| 14 | Arrow-key nudges in W/E/R and Ctrl+T | [PB-004](#pb-004), includes Free Transform ([PB-153](#pb-153)) |
| 15 | Reduced-resolution previews opt-in | [PB-157](#pb-157) |

### PB-080

**Recovery and close lifecycle · 0.5 · first batch · Implemented / verification pending**

- **01 Recovery.** Reproduce `Autosave failed: Uncompressed Photoshop channels alone exceed the 256 MiB file limit` on the affected build. Capture, discover and reopen a usable checkpoint for a representative large editable project. Compare native depth, hierarchy, masks and pixels and verify that the original and earlier good recovery survive. Verify the default native document path and the resident path that emits this error. [PB-105 investigation](docs/large-document-investigation.md) · [File safety](docs/file-safety.md) · [Legacy checkpoint](src/recovery.rs) · [Native checkpoint](src/recovery/native/autosave.rs)
  - Reproduced on signed Windows Store package 1.30.1.0 (v0.30.1) with a synthetic 4096 × 2048, 16-bit, three-layer document. The worker reported `Autosave failed · Untitled.psd: Uncompressed Photoshop channels alone exceed the 256 MiB file limit; use a smaller canvas or fewer full-frame layers`. The new compressed standard channels and recovery/reopen regression are in source. Runtime acceptance for the fix is pending. [Published package evidence](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.1)
- **04 Close.** Reproduce the “finish the current operation” stall. While a file or edit operation is genuinely active, prevent destructive closure. After completion or cancel, reach the normal close review. Preserve unsaved work on Cancel or Save failure and keep other tabs intact. Exercise tab close and app exit. [Project behavior](docs/projects.md) · [Close review](src/ui/lifecycle.rs)
  - The close request now waits for the relevant operation and resumes the ordinary unsaved-work review. The regression holds a job open, settles it successfully or with cancellation, and confirms that canceling close preserves both dirty projects. Exact baseline reproduction and native close acceptance remain pending.

### PB-138

**Save safety · 0.5 · first batch · Implemented / verification pending**

- **02 Full backup store.** Fill the previous-save store and save from the normal editor workflow. Reopen the current editable PSD and compare layers, masks, native samples and merged composite. Confirm original file and backup bytes remain intact. If the current destination cannot be safely replaced, provide a guarded way to save to a fresh PSD with the user's choice of path. Test Cancel, failed saves, existing targets and external changes. Keep the earlier stale-overwrite incident and its unknown original cause in the [file-safety audit](docs/file-safety.md). [File lease](src/file_safety.rs) · [Native publication](src/server/native_save.rs)
  - Normal Save now offers a fresh-file picker when backup capacity is exhausted. Both save paths require an absent destination under the existing lease for this fallback. Regression source covers a full resident store, refusal to replace the original, and editable 16-bit reopen. Native execution remains pending.
  - The signed Windows v0.30.1 baseline refused overwrite at the 1 GiB previous-save cap, retained the original SHA-256 and the backup length, and saved to a fresh PSD path. The new automatic prompt still needs candidate UI acceptance. The older two-instance report remains a later reproduction under this ticket. [Historical audit](docs/file-safety.md)

### PB-079

**Independent project tabs and cross-tab editing · 0.5 · first batch · Implemented / verification pending**

- **03 Image drops.** With two existing tabs, drop a different image into each and switch while the first import is pending. Verify native drag-and-drop, on-tab and canvas routing, correct projects, unchanged original layers and independent undo. Exercise concurrent inspections and cancel a content choice in one project without touching the other. Keep empty-strip new-project drop under the existing PB-119 behavior. [Project behavior](docs/projects.md) · [File-drop entry point](src/ui/file_drop.rs)
  - Source routes file drops to visible tab targets and owns import inspection, content choices and commits by project. Regression source exercises two tabs with independent 16-bit image data and undo. Native drag-and-drop execution remains pending.
- Earlier cross-tab typed clipboard, copy/move preview, independent undo and AI isolation have development evidence. Recheck the affected flows alongside the import change. [Historical cross-tab record](FOLLOWUPS.MD#earlier-requested-work--implementation-records)

### PB-029

**Folder preview and tree drag sorting · 0.5 · layer batch · Implemented / verification pending**

- **05 Folder icon.** Show the composited contents inside the icon. Use the shared renderer with masks, visibility, nesting and actual pixels. Verify refresh on a child edit and no stale preview after a tab switch.
- **08 Drag.** Show before, after and into-folder insertion, including collapsed folders and multi-selection. The committed tree and rendered canvas must match the preview. Reject folders into their own descendants, honor locks/reservations and verify one-step undo. [Historical tree behavior](FOLLOWUPS.MD#implemented-work-and-historical-acceptance-records) · [Layer panel](src/ui.rs)
- Folder icons request the existing rendered group preview. Outdenting over a nested row now uses the actual top-level anchor. Verify both against rendered native UI, especially a child edit, a collapsed folder and a different tab. [Layer panel](src/ui/layers.rs)

### PB-049

**Contextual layer creation · 0.5 · layer batch · Implemented / verification pending**

- **06 Creation.** With a folder selected, create the new layer inside it. With a layer selected, insert immediately above it in the same parent. Verify nested and root cases, Color/Mask target and one-step undo. Retain explicit placement for shared engine callers and existing folder collection behavior. [Historical layer creation](FOLLOWUPS.MD#implemented-work-and-historical-acceptance-records)
- The paint-layer action now sends a relative layer ID through the shared engine, which resolves an insertion slot and validates the parent and reservations. [Shared placement](src/grouping.rs)

### PB-014

**Hidden layer rows · 0.5 · layer batch · Implemented / verification pending**

- **07 Visibility styling.** Shade hidden rows slightly darker gray. Verify readable selected, hovered, locked, reserved and AI states, including a hidden folder. Preserve existing visibility gestures and keyboard access. [Historical layer controls](FOLLOWUPS.MD#implemented-work-and-historical-acceptance-records)
- The layer panel now shades hidden rows and uses a darker selected state while retaining the existing selection and AI markers. [Layer panel](src/ui/layers.rs)

### PB-046

**Whole-layer Duplicate and Copy/Paste · 0.5 · layer batch · In progress**

- **09 Large selections.** Reproduce intermittent failures with many selected layers and folders. Check selected roots, sibling order, folder relationships, editable effects and masks, native depth and undo. Exercise both Duplicate and Copy/Paste, including an explicit error when a real clipboard or document limit prevents the full operation. PB-110 separately tracks selection-pixel copying. [Clipboard behavior](docs/projects.md) · [Earlier layer clipboard verification](FOLLOWUPS.MD#implemented-work-and-historical-acceptance-records)
- The active clipboard target mode now follows its project tab. An immediate Paste can follow a pending layer Copy with the observed clipboard receipt. Source coverage exercises a 31-layer forest, an intervening replacement and one-step Duplicate and Paste undo. The original intermittent Duplicate report and native clipboard flows still need reproduction and acceptance. [Clipboard worker](src/clipboard.rs)

### PB-136

**Native Stroke effect · 0.5 · layer batch · Implemented / verification pending**

- **10 Rendering.** Reproduce Stroke doing nothing on an affected layer. Inspect the actual canvas at 8 and 16 bits when adding or editing Stroke. Compare inside/outside placement and a masked layer, verify bypass and undo, and preserve editable source and live preview. The broader Photoshop authored-style import criteria remain in [PB-122](#pb-122). [Native layer styles](docs/layer-styles.md) · [Original acceptance](FOLLOWUPS.MD#other-open-features-refinements-and-acceptance)
- The Windows v0.30.1 baseline returned identical rendered PNGs before and after adding default Outside Stroke to a full-canvas opaque layer. Explicit Inside produced a different rendered PNG. New Stroke effects now default to Inside. Candidate canvas and existing saved settings still need verification. [Layer style implementation](src/effects/styles.rs)

### PB-069

**AI reservation scope and task end · 0.5 · collaboration batch · Implemented / verification pending**

- **11 Scope.** Update agent instructions and help to reserve only the layers or regions that need protection for the current edit. Verify unrelated user edits proceed. Verify user takeover interrupts the AI and that the agent cannot silently reclaim the target.
- **12 Task end.** Have an AI task create a layer, finish or surrender it, and paint as the user. Test normal release, expiry and takeover. Respect an intentional document lock and any independent reservation. PB-069 was priority 7 in the older queue. [Collaboration](docs/collaboration.md) · [MCP](docs/mcp.md) · [PB-100 historical behavior](FOLLOWUPS.MD#earlier-refinements--implementation-records)
- MCP initialization and contributor instructions now describe minimal reservations, task release and takeover. Regression source covers human painting outside a reservation, normal release, expiry, takeover and an intentional lock. [Collaboration engine](src/collaboration.rs)
- **12 baseline shipped and verified.** Windows Store 1.30.1.0 (v0.30.1) let human work proceed outside the reservation and allowed painting on the AI-created layer after task end. An explicit layer lock still blocked paint. The resulting two-layer 16-bit PSD reopened editable. Candidate coverage includes expiry and takeover. [Published Windows build](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.1)

### PB-002

**Current AI task area animation · 0.5 · collaboration batch · Implemented / verification pending**

- **13 Presentation.** Animate the current task area while AI work is active, including when the same text remains visible. Settle on stop and clear the animation on project switch. Verify readable blue presentation, human interruption and no effect on document history. [AI collaboration](docs/collaboration.md) · [Historical activity text](FOLLOWUPS.MD#implemented-work-and-historical-acceptance-records)
- The top-bar area now pulses during active AI reservations, code runs and MCP work. Switching tabs refreshes the indication from the new project. Native desktop acceptance remains pending. [Top bar](src/ui.rs)

### PB-004

**Transform arrow-key nudges · 0.5 · editing batch · Implemented / verification pending**

- **14 Nudging.** In W/E/R and Ctrl+T (Cmd+T on macOS), each position arrow press moves exactly one document pixel independent of zoom. Rotation and scale use fine mode-appropriate increments. Verify selected pixels, layers, linked/unlinked masks, text or number focus, cancellation and undo. Respect reservations and the current mode's appropriate preview/apply behavior. Check Free Transform and editable text against [PB-153](#pb-153). [Transform behavior](docs/retained-transforms.md)
- Move uses one document pixel, Rotate uses 0.1°, and Scale uses 1% per press. Free Transform updates the draft for one guarded Apply. Regression source checks zoom-independent Move and draft Move/Rotate/Scale. [Free Transform](src/ui/free_transform.rs)

### PB-157

**Full-resolution editing previews by default · 0.5 · editing batch · Implemented / verification pending**

- **15 Default and opt-in.** Verify fresh and previously unset profiles use full resolution for live edits. Exercise the saved reduced-resolution setting, persistence, toggling, and final full-quality render. Audit normal edits, AI presentation and preview commands; source dimensions, native precision, saves and exports must stay exact. [Settings](docs/settings.md) · [Original requirements](FOLLOWUPS.MD#next-version-requests--2026-11-05)
- Source inspection confirms an absent preference initializes full resolution and only an explicit saved opt-in enables reduced previews. Existing tests cover preference and rendering paths. Candidate results remain pending. [Preview settings source and tests](src/ui/preview_quality.rs)

## Later work and historical reconciliation

Priority numbers in this section preserve the original reliability ordering. High-priority installation foundations remain high priority. Pending platform or receiving-editor checks apply to the specific development work recorded below. The older combined queues do not determine the 0.5 owner gate.

### PB-108

**Investigate slow startup and editing responsiveness · Later · priority 1 · In progress**

Reproduce and measure cold/warm startup, first present, editing, pan and tablet latency on an exact native build. Compare representative native 8/16-bit canvases and layer/effect counts and record hardware/cache conditions. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L541)

### PB-122

**Complete editable Photoshop layer-feature support · Later · priority 2 · In progress**

Verify remaining native-depth Photoshop adjustment, masks, compositing, style, text and smart-object compatibility with actual Photoshop reopen and editable round trips. Keep unsupported sources protected and compare standard layer/mask/composite data. Includes the broader Photoshop style-import follow-up from PB-136. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L465) · [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L542)

### PB-110

**Fix selection correctness, including selected copy/paste and W/E/R transforms · Later · priority 3 · In progress**

Replay reported Wand selection-copy and W/E/R scope failures with same-frame and delayed native edits. Check feathered coverage, exact position, selected Cut/Delete, reservation/clipboard takeover and native 8/16-bit undo. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L543)

### PB-145

**Installed PeerBrush and standard platform installers · Later · priority high · In progress**

The Store package, Apple silicon DMG and Ubuntu DEB shipped in v0.30.1. Qualify remaining installed upgrade/uninstall and supported delivery. A direct Windows installer requires a delivery decision consistent with current Store-only policy. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L513) · [v0.30.1 release](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.1)

### PB-146

**PSD/PSB/project file associations, Open with and default-editor choice · Later · priority high · Implemented / verification pending**

The default-editor choice shipped on the three supported platforms in v0.30.1. Complete remaining installed Open With/association lifecycle checks, passing actual PSD/PSB paths through native launch and native OS consent. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L514) · [v0.30.1 release](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.1)

### PB-147

**Safe auto-update experience for installed software · Later · priority high · In progress**

Verify channel-owned discovery, opt-in updates, editing/quit guards, native upgrade, interruption, rollback and document/preferences retention. Check the Update and website What’s New actions and the requested post-install What’s New video separately. Current source has platform-specific update actions and release information. No post-install video acceptance is recorded. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L515) · [Update UI](src/ui/updates.rs) · [Update channels](docs/updates.md)

### PB-154

**Main-to-production release and store automation · Later · priority high · In progress**

Qualify exact-source native artifacts, signing/provenance, immutable publication, website history, and Store certification/availability as distinct guarded operations. Review draft notes digest for the exact candidate. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L517)

### PB-155

**Per-user persistent settings and migration · Later · priority high · Implemented / verification pending**

Per-user preferences preservation shipped in v0.30.0. Qualify remaining legacy migration, invalid data, backup/retry, uninstall retention and package upgrade on native installs. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L516) · [v0.30.0 release](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0)

### PB-158

**Scoop, Homebrew, apt and Nix distribution ([GitHub #59](https://github.com/Deftr0y/PeerBrush/issues/59)) · Later · priority existing queue · In progress**

Verify Scoop, Homebrew, signed apt and Nix separately for immutable download, install, upgrade, launch/MCP, uninstall and settings preservation. Close [GitHub #59](https://github.com/Deftr0y/PeerBrush/issues/59) only after actual publication and native readback. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L464)

### PB-023

**Reliable keyboard shortcuts across keyboard layouts and control focus · Later · priority existing queue · In progress**

Replay shortcuts when switching English/Hebrew keyboard layouts and with ordinary control focus. Confirm text stays isolated and actions trigger once. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L565)

### PB-043

**Reliable local MCP registration, discovery and connection for smaller models · Later · priority existing queue · In progress**

Verify registration, discovery and real MCP connection with smaller or less capable clients. Preserve user settings and redacted diagnostics. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L567)

### PB-066

**Non-destructive text fields/layers, installed fonts and editable vector shapes · Later · priority existing queue · In progress**

Verify installed fonts and fallback/replacement, editable text and vector sources, preview and exact native save/undo in representative native sessions. Coordinate direct text editing with PB-161. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L568)

### PB-068

**Large-document handling up to 16000 × 16000 pixels · Later · priority existing queue · In progress**

Qualify 16000 × 16000 native source handling across supported platforms, precision and commands. Expand beyond the earlier Windows sparse exports and dense single-layer PSB round trip. The owner recovery regression is PB-080. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L569)

### PB-070

**Color management and stronger Photoshop fidelity · Later · priority after canvas/tablet · In progress**

Exercise receiving-application and production color-profile/compositing round trips for the supported subset and explicit protection for unsupported data. Own the remaining receiving check for PB-118. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L570)

### PB-072

**General brush improvements, preset library and canvas/tablet ergonomics · Later · priority after recovery · Implemented / verification pending**

Verify real tablet and eraser-end input, calibration and brush ergonomics; do not infer hardware acceptance from offscreen output. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L571)

### PB-106

**Improve effects-stack UI/UX · Later · priority existing queue · Implemented / verification pending**

Verify the native effects inspector, held previews, focus, removal and guarded prefix bake. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L572)

### PB-133

**Cut selected content from the original layer and paste onto a new layer · Later · priority existing queue · Implemented / verification pending**

Verify real native clipboard transport for selected Cut and Paste, exact coverage/origin and failure safety. Coordinate selected pixels with PB-110. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L574)

### PB-141

**Use system fonts for multilingual UI and reduce package size · Later · priority existing queue · Implemented / verification pending**

Verify installed system font selection and multilingual filenames on macOS/Linux and retain Windows and size evidence. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L578)

### PB-142

**Slightly reduce slider sensitivity for more precise adjustments · Later · priority existing queue · Implemented / verification pending**

Verify fine sensitivity and full-range reachability in native controls with precise entry and live preview. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L579)

### PB-143

**JPEG XL (.jxl) opening, import and export · Later · priority existing queue · Implemented / verification pending**

Native-depth JPEG XL export shipped in v0.30.0. Qualify remaining Open/import/export and representative native 8/16-bit, transparent and unsupported-format cases. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L580) · [v0.30.0 release](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0)

### PB-144

**Complete WebP support with image export · Later · priority existing queue · Implemented / verification pending**

Lossless WebP export shipped in v0.30.0. Verify remaining cross-platform/receiving checks, alpha, animated frame selection and explicit 8-bit output for 16-bit sources. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L581) · [v0.30.0 release](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0)

### PB-148

**In-app Info section with the installed version · Later · priority existing queue · Implemented / verification pending**

Version/source information shipped in v0.30.0. Verify installed and prerelease presentation across supported platforms. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L582) · [v0.30.0 release](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0)

### PB-149

**Visually connected main-menu buttons · Later · priority existing queue · Implemented / verification pending**

Windows development presentation checked. Verify connected menu focus, accessibility and native interaction on remaining platforms. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L583)

### PB-150

**Cohesive, polished popup messages · Later · priority existing queue · Implemented / verification pending**

Verify popup readability, safe/default actions and focus at native window sizes and display scaling. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L584)

### PB-151

**Suggest clipboard image dimensions for a new canvas · Later · priority existing queue · Implemented / verification pending**

Clipboard-size suggestions shipped in v0.30.0. Finish remaining native clipboard checks including invalid clipboard input and manual overrides. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L585) · [v0.30.0 release](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0)

### PB-152

**Link or unlink layer masks and content for transforms · Later · priority existing queue · Implemented / verification pending**

Linked and independent mask transforms shipped in v0.30.0. Verify receiving-editor agreement for standard PSD/PSB state and exact reopen. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L586) · [v0.30.0 release](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0)

### PB-153

**Photoshop-style Ctrl/Cmd+T unified Free Transform, including Warp · Later · priority existing queue · In progress**

Free Transform and nine-point Warp shipped in v0.30.0. Finish native coverage of newer Distort, Perspective, presets and editable text interactions. PB-004 owns the 0.5 keyboard-nudge check. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L587) · [v0.30.0 release](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0)

### PB-156

**Ctrl+E selects the Eraser · Later · priority existing queue · Implemented / verification pending**

Verified but unreleased on the earlier Windows 0.4 development build for Ctrl+E Eraser. Check remaining platforms, focus, layout changes and preservation of custom bindings. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L462)

### PB-159

**Start canvas tools in the surrounding workspace · Later · priority existing queue · Implemented / verification pending**

Verify remaining gray-workspace starts across supported paint/drawing/selection/fill tools, platform input, clipping and cancellation. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L559)

### PB-160

**Photographic whole-image filter thumbnails and a single Canvas view popup · Later · priority existing queue · Implemented / verification pending**

Earlier Windows development evidence covers photographic preset thumbnails and the Canvas popup. Recheck the draft comparisons, cache changes, canvas output and broader native behavior. PB-081 retains the historical filter-library implementation. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L560)

### PB-161

**Direct canvas text editing and Free Transform · Later · priority existing queue · Implemented / verification pending**

Verify in-canvas text editing, caret, sizing/reflow under transforms, installed font behavior and rotation/scale. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L561)

### PB-162

**Settings controls suited to their values · Later · priority existing queue · In progress**

Qualify accessible controls and their actual continuous preview across color, icons, angles, pressure, gradients and style comparisons. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L562)

### PB-163

**Shift+D Duplicate with Transform · Later · priority existing queue · Implemented / verification pending**

Earlier Windows and native8/16 development checks exist. Verify latest Shift+D selected copy transforms, repeated offsets, editable text, active roots and undo/redo on native builds. [Historical criteria and evidence](https://github.com/Deftr0y/PeerBrush/blob/15da63fe42a821eabd805f33bcc49fcaa9f54260/FOLLOWUPS.MD#L563)

### PB-164

**Adjustment layer naming and a distinct layer-stack icon · Later · priority higher refinement · Implemented / verification pending**

Verify Adjustment layer naming and the distinct normal/drag icon, with explicit names preserved across UI/MCP/CLI, no unused color thumbnail, and ordinary mask thumbnails. [Implementation](docs/release-0.5.0.md) · [Layer panel](src/ui.rs)

### PB-165

**Layer color labels from the right-click menu · Later · priority higher refinement · Implemented / verification pending**

Verify the layer-context presets, Cyan and Custom picker on paint, folders and adjustments. Check saved/reopened color data, duplication, native picker and distinct blue AI styling, undo and reservations. [Implementation](docs/release-0.5.0.md) · [Layer color handling](src/engine.rs)

## Shipped slices and other decisions

- **Shipped in v0.30.1.** Standard installers (PB-145) and default-editor choice (PB-146) on macOS Apple silicon DMG, Ubuntu 24.04 amd64 DEB and Windows Microsoft Store build 1.30.1.0. The release records those slices. [v0.30.1 assets and notes](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.1)
- **Shipped in v0.30.0.** Installed fonts (PB-066), JPEG XL and WebP export (PB-143/PB-144), version/source information (PB-148), clipboard canvas sizing (PB-151), linked masks and basic Free Transform/Grid Warp (PB-152/PB-153), per-user preferences (PB-155), and the documented 16-bit Photoshop output correction (PB-122). macOS Apple silicon and Ubuntu 24.04 amd64 assets are attached to the tag. Windows delivery is through the Microsoft Store; the v0.30.1 release provides the explicitly confirmed live Store package version. [v0.30.0 assets and notes](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0)
- **PB-074 shipped** in [v0.1.4 for Windows x64](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.1.4). **PB-103 shipped** in [v0.2 for Windows x64, macOS Apple silicon and Linux x64](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.2). Its historical signing scope was deferred.
- **PB-118.** Earlier production export implementation and verification remain in the historical record. Its open receiving-editor work maps to [PB-070](#pb-070).
- **PB-042 deferred.** Keep the existing historical logo/design record. The separate November 16 logo change was withdrawn and has no active item.
- **PB-139 deferred.** Retain archival review and approval evidence. Historical maintenance does not authorize destructive work. [Corrections record](docs/release-packaging-corrections.md)
- **PB-105 and PB-140 completed historical decisions.** Keep the completed limit investigation and README attribution decision. Large-document engineering remains PB-068. [Limit investigation](docs/large-document-investigation.md) · [README](README.md)
- **PB-081.** The full-image filter library has earlier implementation and rendered verification. Keep the newer thumbnail and native inspection work in [PB-160](#pb-160). Favorites, recents and reusable stacks remain historical ideas.

## Old-to-new mapping

- **Same ID in 0.5:** PB-002, PB-004, PB-014, PB-029, PB-046, PB-049, PB-069, PB-079, PB-080, PB-136, PB-138, PB-157. PB-100 remains the earlier reservation-status evidence.
- **Same ID in Later:** PB-023, PB-043, PB-066, PB-068, PB-070, PB-072, PB-106, PB-108, PB-110, PB-122, PB-133, PB-141, PB-142, PB-143, PB-144, PB-145, PB-146, PB-147, PB-148, PB-149, PB-150, PB-151, PB-152, PB-153, PB-154, PB-155, PB-156, PB-158, PB-159, PB-160, PB-161, PB-162, PB-163, PB-164, PB-165. PB-164 and PB-165 retain the IDs assigned in the 0.5 source after two older draft labels collided with the established PB-104 and PB-105 records.
- **Preceding release or completed/deferred decision:** PB-042, PB-074, PB-103, PB-105, PB-139, PB-140. See the decisions above.
- **Remaining acceptance combined:** PB-118 → PB-070. PB-136 remains the single 0.5 Stroke ticket, with authored-style import under PB-122.
- **Historical transform implementation retained:** PB-067 preserves the verified non-destructive transform work, originally priority 5.
- **Historical records retained in place:** PB-001, PB-003, PB-005, PB-006, PB-007, PB-008, PB-009, PB-010, PB-011, PB-012, PB-013, PB-015, PB-016, PB-017, PB-018, PB-019, PB-020, PB-021, PB-022, PB-024, PB-025, PB-026, PB-027, PB-028, PB-030, PB-031, PB-032, PB-033, PB-034, PB-035, PB-036, PB-037, PB-038, PB-039, PB-040, PB-041, PB-044, PB-045, PB-047, PB-048, PB-050, PB-051, PB-052, PB-053, PB-054, PB-055, PB-056, PB-057, PB-058, PB-059, PB-060, PB-061, PB-062, PB-063, PB-064, PB-065, PB-071, PB-073, PB-075, PB-076, PB-077, PB-078, PB-081, PB-082, PB-083, PB-084, PB-085, PB-086, PB-087, PB-088, PB-089, PB-090, PB-091, PB-092, PB-093, PB-094, PB-095, PB-096, PB-097, PB-098, PB-099, PB-100, PB-101, PB-102, PB-104, PB-107, PB-109, PB-111, PB-112, PB-113, PB-114, PB-115, PB-116, PB-117, PB-119, PB-120, PB-121, PB-123, PB-124, PB-125, PB-126, PB-127, PB-128, PB-129, PB-130, PB-131, PB-132, PB-134, PB-135, PB-137. Their old implementation and verification wording remains evidence for the recorded checkpoint. Use a matching tag and platform record for any new shipment claim.

The mapping accounts for all 165 distinct PB IDs in the compared repo backlog and local 0.5 source. PB-122 had two legacy scopes and now has one canonical entry. The intake table accounts for all 15 numbered checks. Reconciliation is limited to the repository records.
