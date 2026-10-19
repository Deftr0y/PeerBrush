# PeerBrush v0.3 — development

This is the next milestone for all existing unfinished feature work and acceptance, as requested after the 0.26.0 Mac/Linux release. Development is unreleased. Full original acceptance criteria, earlier verified work and unresolved reports are preserved in [FOLLOWUPS.MD](../FOLLOWUPS.MD#planned-v03--remaining-development-and-acceptance); the [0.26 record](development-0.26.md) remains historical evidence.

## Scope and order

The implementation scope now proposes **v0.3 inclusion of PB-145/PB-146/PB-147/PB-155/PB-154**, explicitly extending the earlier release-unassigned plan. Sequence these foundations after the first measurable PB-108 fix: persistent settings/migration; platform installation and consent-based file associations; channel-specific authenticated updates and safe restart; then protected production publishing and Store reconciliation. Independent packaging work may proceed alongside the performance investigation. Inclusion is a development target, not a release-readiness claim. See [the release plan](release-signing-plan.md#high-priority-installation-and-production-foundations).

1. **PB-108 remains priority #1:** reproduce and measure startup and input-to-display delay, including the blank canvas, controls, navigation and real hardware. Preserve the earlier verified fixes and comparable baselines; prior CPU benchmarks do not close end-to-end latency acceptance.
2. **PB-122 remains #2:** conservative editable Photoshop imports and styled-source acceptance, with exact current standard-composite matching and protected unsupported originals.
3. **PB-110 remains #3:** the reported selection recurrence across copy/paste, selected transforms and cancellation. Earlier verified work does not close the recurrence.
4. Preserve the next relative order: recovery/lifecycle PB-080/PB-138, canvas/tablet ergonomics PB-072, then production export and receiving-application round trips PB-118/PB-070.
5. Other remaining work retains its detailed backlog criteria: keyboard layouts PB-023; smaller-model MCP discovery PB-043; text/OS fonts PB-066/PB-141; 16000 × 16000 documents PB-068; effects/clipboard/style acceptance PB-106/PB-133/PB-136; mask-link interchange PB-152; sliders and JXL/WebP PB-142/PB-143/PB-144; Info/menu/popups/clipboard canvas defaults PB-148–151; Free Transform/Warp PB-153; visual refinement PB-007/PB-042; historical-reference audit PB-139. PB-139's destructive-history actions still require separate explicit owner approval.

Standing precision, UI/MCP/CLI consistency, save safety and documented GPU/Photoshop fallbacks remain in force. Earlier completed rows and measurements remain historical records; only unresolved portions are scheduled here. Do not repeat implemented work or mark remaining native/hardware/receiving-application acceptance complete without evidence.

The existing future extensions for remote connections and plugin architecture also belong to planned v0.3; define their bounded requirements before implementation rather than assuming the current local MCP/proposal work implements them.

## Implementation and review

The milestone and branch namespace remain v0.3; the approved application release version is **0.30.0** because SemVer orders 0.3.0 below published 0.26.0. Development packages use 0.30.0-dev.1. Keep Windows package versions and updater comparisons monotonic; never present development candidates as published upgrades.

`version/0.3` starts from synchronized `dev` and `main` history. Focused `feature/0.3` and `fix/0.3` branches target it under the [branch policy](development-guide.md#branches-and-pull-requests). The original planning commit from PR #32 is retained through a merge, without rewriting or duplicating its content. Existing local 0.26 work remains on its original branches; audit it against current shared code before porting anything.

Use exact tested commits and applicable engine/codec/protocol checks plus native visual/hardware evidence for behavior changes, preserving native 16-bit words, current PSD/PSB composites, history and human work. Keep release publication, deployment, Store submission, protected signing approvals and destructive history operations subject to their own explicit authorization.

## Initial audit

The shared 0.26 code already contains retained first-input rendering, same-frame navigation, immediate human controls, Windows low-latency presentation, guarded native file activation, MSIX packaging/associations and stable installed MCP aliases. Existing brush, shortcut, filter and export preferences persist. Apple signing/notarization/stapling exists for ZIPs. Reuse these implementations; their remaining installed/native acceptance stays open.

PB-108 investigation begins with smoothed stroke tails: appending input changes the preceding final dab, causing the current replay session to discard all accumulated coverage. Measure the same release-mode workloads before and after a focused fix, assert exact native8/16 output and unchanged-prefix retention, then verify the native workspace. Full dab generation, OS delivery, worker scheduling, GPU presentation and physical tablet latency remain distinct measurements.
