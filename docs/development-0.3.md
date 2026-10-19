# PeerBrush v0.3 — planned development

This is the next milestone for all existing unfinished feature work and acceptance, as requested after the 0.26.0 Mac/Linux release. It is a planning queue, not an implementation or release claim. Full original acceptance criteria, earlier verified work and unresolved reports are preserved in [FOLLOWUPS.MD](../FOLLOWUPS.MD#planned-v03--remaining-development-and-acceptance); the [0.26 record](development-0.26.md) remains historical evidence.

## Scope and order

The high-priority installation, DMG, settings, auto-update and production-automation foundations (PB-145/PB-146/PB-147/PB-155/PB-154) are a separate coordinated workstream with no assigned version. Do not silently fold them into v0.3 or treat their planning as implemented behavior. See [the release plan](release-signing-plan.md#high-priority-installation-and-production-foundations).

1. **PB-108 remains priority #1:** reproduce and measure startup and input-to-display delay, including the blank canvas, controls, navigation and real hardware. Preserve the earlier verified fixes and comparable baselines; prior CPU benchmarks do not close end-to-end latency acceptance.
2. **PB-122 remains #2:** conservative editable Photoshop imports and styled-source acceptance, with exact current standard-composite matching and protected unsupported originals.
3. **PB-110 remains #3:** the reported selection recurrence across copy/paste, selected transforms and cancellation. Earlier verified work does not close the recurrence.
4. Preserve the next relative order: recovery/lifecycle PB-080/PB-138, canvas/tablet ergonomics PB-072, then production export and receiving-application round trips PB-118/PB-070.
5. Other remaining work retains its detailed backlog criteria: keyboard layouts PB-023; smaller-model MCP discovery PB-043; text/OS fonts PB-066/PB-141; 16000 × 16000 documents PB-068; effects/clipboard/style acceptance PB-106/PB-133/PB-136; mask-link interchange PB-152; sliders and JXL/WebP PB-142/PB-143/PB-144; Info/menu/popups/clipboard canvas defaults PB-148–151; Free Transform/Warp PB-153; visual refinement PB-007/PB-042; historical-reference audit PB-139. PB-139's destructive-history actions still require separate explicit owner approval.

Standing precision, UI/MCP/CLI consistency, save safety and documented GPU/Photoshop fallbacks remain in force. Earlier completed rows and measurements remain historical records; only unresolved portions are scheduled here. Do not repeat implemented work or mark remaining native/hardware/receiving-application acceptance complete without evidence.

The existing future extensions for remote connections and plugin architecture also belong to planned v0.3; define their bounded requirements before implementation rather than assuming the current local MCP/proposal work implements them.

## Starting implementation later

Read AGENTS.md, the development guide, this queue and the full backlog first. Inspect active work and preserve uncommitted changes. Establish the v0.3 integration branch from synchronized dev under the [branch policy](development-guide.md#branches-and-pull-requests), then use focused feature/0.3 or fix/0.3 branches targeting version/0.3. Select the first bounded PB-108 investigation/fix with measurable acceptance before expanding scope; do not recreate the planning branch as an implementation branch.

Use exact tested commits and applicable engine/codec/protocol checks plus native visual/hardware evidence for behavior changes, preserving native 16-bit words, current PSD/PSB composites, history and human work. Keep release publication, deployment, Store submission, protected signing approvals and destructive history operations subject to their own explicit authorization. This planning update creates no version branch, bumps no package version and starts no implementation.
