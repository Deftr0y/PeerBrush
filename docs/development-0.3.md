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

`version/0.3` starts from synchronized `dev` and `main` history. Focused `feature/0.3` and `fix/0.3` branches target it under the [branch policy](development-guide.md#branches-and-pull-requests). The original planning commit from PR #32 is retained through a merge, without rewriting or duplicating its content. Audit earlier implementations against current shared code before porting anything.

Use exact tested commits and applicable engine/codec/protocol checks plus native visual/hardware evidence for behavior changes, preserving native 16-bit words, current PSD/PSB composites, history and human work. Keep release publication, deployment, Store submission, protected signing approvals and destructive history operations subject to their own explicit authorization.

## Initial audit

The shared 0.26 code already contains retained first-input rendering, same-frame navigation, immediate human controls, Windows low-latency presentation, guarded native file activation, MSIX packaging/associations and stable installed MCP aliases. Existing brush, shortcut, filter and export preferences persist. Apple signing/notarization/stapling exists for ZIPs. Reuse these implementations; their remaining installed/native acceptance stays open.

PB-108 investigation begins with smoothed stroke tails: appending input changes the preceding final dab, causing the current replay session to discard all accumulated coverage. Measure the same release-mode workloads before and after a focused fix, assert exact native8/16 output and unchanged-prefix retention, then verify the native workspace. Full dab generation, OS delivery, worker scheduling, GPU presentation and physical tablet latency remain distinct measurements.

## PB-108 changed-tail replay

The shared brush session now finds the exact unchanged dab prefix. Reshaped/removed tails restore only their affected tile region from the original source and replay all current dabs contributing to that region in their original order. Other tiles and coverage remain retained. Full validation and stroke work limits still run before mutation. Tails that change or overlap the whole stroke still require that work; dab generation and validation remain linear in total stroke length.

Release-mode CPU measurements on Windows, Intel Core i7-14650HX, using `cargo run --locked --offline --release --example input_performance` and a 1536-edge preview:

| 2048² canvas, smoothing 0.35 | Before median / p95 | After median / p95 |
| --- | --- | --- |
| Native8, 120 continuing inputs | 3.96 / 8.29 ms | 2.26 / 3.60 ms |
| Native16, 120 continuing inputs | 8.41 / 17.66 ms | 5.92 / 11.36 ms |

The before run used the unchanged shared brush implementation at the initial planning merge. A paired, same-process 2048×96 smoothed/tapered 120-input brush-only workload compares complete replay with the retained session and asserts exact output after every input: native8 median/p95 6.56/12.22 ms versus 1.21/2.48 ms; native16 14.80/49.99 ms versus 3.45/8.88 ms. Retained replay processes 4,244 contributing dabs versus 18,606. These samples include normal scheduling variation; they measure CPU work, not physical input-to-display latency or a GPU improvement.

All 29 focused brush-session/equivalence/texture/native16-paint/preview tests passed. New structural coverage verifies exact native8/16 source parity, pressure, textured tips, quantized opacity/flow, erase, polygon/rectangle clipping, taper, tail shrink/re-expansion and retained prefix tile identity. The complete local run passed 923 Rust tests with eight opt-in checks ignored, plus formatting, all-target Clippy with existing warnings and 21 packaging/privacy checks. Native Windows rendered workspaces showed the actual smoothed stroke at both depths; independent PNG decoding verified every original sample and exact undo/redo, and independent PSD/PSB merged-plane decoding plus editable reopening matched the current artwork exactly. Generated QA files stay outside committed source.

Exact-head Windows/macOS/Linux CI remains separate. Local MSIX tests passed 23 checks; their native SDK validation failed on the installed 10.0.18362 SDK, below the documented 10.0.22000 requirement. No manifest requirement was weakened. Startup, controls/navigation stalls, physical tablet latency and user acceptance remain open; this slice does not close PB-108.

## Foundation implementation slices

All work below is unreleased, in focused draft PRs targeting `version/0.3`. The acceptance branch combines reviewed slices for integration testing; it is not a production promotion.

| Item | Reviewable implementation | Evidence and remaining acceptance |
| --- | --- | --- |
| PB-155 | [PR #34](https://github.com/Deftr0y/PeerBrush/pull/34): platform configuration/cache locations, persistent versioned profile, validated legacy migration with byte-exact backup, lifetime locking and no-overwrite publication | Native macOS and Linux CI passed tests, Clippy, package inspection and builds at `49e3c2a`; Windows passed tests and Clippy while its release build continued. Installed upgrade/uninstall retention, MSIX virtualization and combined native acceptance remain open. |
| PB-145/PB-146 | [PR #35](https://github.com/Deftr0y/PeerBrush/pull/35): Ubuntu 24.04 amd64 DEB, desktop/MIME registration, exact-source packaging, package-manager update ownership | Ten portable packaging checks passed locally. Disposable Ubuntu installation/upgrade/removal acceptance is wired to CI. Cross-platform fixture corrections and that native run remain pending; other Linux distributions, real older application upgrades and desktop opening remain unverified. |
| PB-145 | [PR #36](https://github.com/Deftr0y/PeerBrush/pull/36): drag-to-Applications DMG preparation plus resumable image signing/notarization/stapling and exact mounted-payload verification | Synthetic preparation/signing tests passed; credential-free native image checks are wired to macOS CI. No real Apple submission, signed image installation or quarantine acceptance has run. Bundle icon/file-opening integration is a separate slice. |
| PB-154 | [PR #37](https://github.com/Deftr0y/PeerBrush/pull/37): main-triggered exact-source candidates, immutable inventory/checksum verification and durable Store submission reconciliation | Offline checks passed without provider calls. Publication remains disabled: readiness stays false until installed/native-signature gates, protected approval, durable hosted receipts and public asset/website readback are implemented and verified. Certification is distinct from public availability. |

PB-146's OS-owned default-editor choice and macOS installed file-event handling are being implemented after auditing the existing guarded activation flow. PB-147 begins with authenticated release discovery, persistent check preferences and shared restart safety; a direct installer must verify trusted native signatures and provide platform-supported recovery before it can replace the application. Package identity alone does not establish Store update ownership. No signing keys, trust configuration, Store submission or release publication is authorized by these development slices.
