# Startup font diagnostic

Unreleased PB-108 investigation and nonblocking font discovery.

Run the ignored diagnostic explicitly:

```sh
cargo test --locked --lib ui::filename::tests::startup_system_fonts_diagnostic -- --exact --ignored --nocapture
```

The diagnostic launches the same test executable in a fresh child process with only this exact test enabled. Other UI tests cannot initialize its font catalog first. It explicitly constructs the catalog synchronously to measure that CPU/I/O component, followed by fallback installation, a repeat call on fresh font definitions, retained selected bytes/counts, shaping-database faces and egui setup/first ASCII/script/repeated-script frames. Production startup schedules discovery on a worker instead. The diagnostic uses egui's default fonts plus installed fallbacks, rather than the complete branded workspace. Repeat installation must reuse the same retained byte snapshots. Assertions cover existing bounds and invariants, with no timing threshold.

Output contains aggregate measurements only, without installed font names or paths. A fresh process does not flush operating-system filesystem/font caches. These measurements exclude window creation, GPU setup/presentation, project restore, physical input/display latency and comparison with an independently established known-good build. Use the same build profile and environment for comparisons; record native platform and architecture.

## Audited synchronous work

Before this change, `PeerBrush::init` invoked `filename::install_fallbacks` before setting its fonts and drawing the workspace. Catalog construction calls `fontdb::Database::load_system_fonts`, then selects up to six script faces. The locked fontdb 0.24 implementation recursively scans platform font directories and reads/parses every discovered font file and collection face before selection. Candidate coverage checks subsequently reopen font files, and retained selections are copied into an owned shaping database.

The existing 32 MiB selected-face and 64 MiB retained-catalog bounds apply after the initial font-directory scan. They do not bound that discovery workload. Catalog caching avoids repeating it on later application contexts in the same process. The selection algorithm, installed script coverage, collection indices, owned snapshots and missing-glyph behavior remain unchanged.

A separate opt-in Windows probe using the cached release fontdb dependency observed 520 faces in 497 files, with 435,800,926 aggregate file bytes. Its fresh-process first scan took 589.083 ms; another database in the same process took 257.193 ms. Repeating the scan into those databases took 268.683/266.060 ms and appended duplicate face records.

A subsequent fresh-process probe included the actual unmodified filename module and cached release dependencies. Its first synchronous catalog/fallback installation took 673.616 ms; a repeat installation took 0.003 ms. Five selected faces retained 53,264,588 bytes, with equal byte pointers, collection indices and family definitions across calls. Context setup took 0.021 ms, followed by the first ASCII/script/repeated-script frames at 1.828/0.215/0.021 ms.

## Nonblocking handoff

The workspace now installs its bundled Ubuntu fonts immediately and starts one named discovery worker. Filename drawing reads ready catalog data without waiting on discovery. Labels needing the installed script faces temporarily show `Loading language fonts…`, with their original logical text retained in control metadata and document state. A worker panic or inability to start produces the terminal `Language fonts unavailable` indication. Missing installed script coverage after successful discovery retains the existing missing-glyph presentation.

At the next draw after completion, the UI installs the same selected font snapshots once, clears derived shaped-label textures and requests repaint. The shared document, project paths, edits and history do not participate in this handoff. A fresh-process regression holds discovery on a channel while real label, layer-name and project-button controls finish drawing, then checks both successful and failed handoffs, cache invalidation and no retry. The ten-second synchronization guard detects deadlocks; it is not a performance threshold.

A fresh-process standalone probe including the changed production module compiled and ran successfully. In measurement order, initial font setup took 0.120 ms, first ASCII frame 2.331 ms, and the actual script-label frame while discovery was pending 2.068 ms. Discovery became ready after 308.751 ms; the one-time handoff took 0.052 ms. The first ready script-label frame took 2.808 ms, then 0.022 ms on repeat. These probes ran sequentially after prior UI/font work, with warm/uncontrolled OS caches; the scan durations do not establish an algorithm speedup or complete cold-launch improvement. They demonstrate that the measured initial UI path no longer waits for discovery. The whole-application diagnostic, controlled regression suite and native startup verification remain pending.
