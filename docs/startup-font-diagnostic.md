# Startup font diagnostic

Unreleased PB-108 investigation; application behavior is unchanged.

Run the ignored diagnostic explicitly:

```sh
cargo test --locked --lib ui::filename::tests::startup_system_fonts_diagnostic -- --exact --ignored --nocapture
```

The diagnostic launches the same test executable in a fresh child process with only this exact test enabled. Other UI tests cannot initialize its font catalog first. It measures the first `install_fallbacks`, a repeat call on fresh font definitions, retained selected bytes/counts, shaping-database faces and egui setup/first ASCII/script/repeated-script frames. The latter uses egui's default fonts plus installed fallbacks, rather than the complete branded workspace. Repeat installation must reuse the same retained byte snapshots. Assertions cover existing bounds and invariants, with no timing threshold.

Output contains aggregate measurements only, without installed font names or paths. A fresh process does not flush operating-system filesystem/font caches. These measurements exclude window creation, GPU setup/presentation, project restore, physical input/display latency and comparison with an independently established known-good build. Use the same build profile and environment for comparisons; record native platform and architecture.

## Audited synchronous work

`PeerBrush::init` invokes `filename::install_fallbacks` before setting its fonts and drawing the workspace. The first catalog construction calls `fontdb::Database::load_system_fonts`, then selects up to six script faces. The locked fontdb 0.24 implementation recursively scans platform font directories and reads/parses every discovered font file and collection face before selection. Candidate coverage checks subsequently reopen font files, and retained selections are copied into an owned shaping database.

The existing 32 MiB selected-face and 64 MiB retained-catalog bounds apply after the initial font-directory scan. They do not bound that discovery workload. Catalog caching avoids repeating it on later application contexts in the same process. Installed script coverage, collection indices, missing-glyph behavior and original filename/document data must remain intact in any later optimization.

A separate opt-in Windows probe using the cached release fontdb dependency observed 520 faces in 497 files, with 435,800,926 aggregate file bytes. Its fresh-process first scan took 589.083 ms; another database in the same process took 257.193 ms. Repeating the scan into those databases took 268.683/266.060 ms and appended duplicate face records. This is library-only evidence with warm/uncontrolled OS caches after prior UI tests, not measured `install_fallbacks` or a complete cold application launch. The new application diagnostic and native startup comparison remain pending; no production optimization is claimed.
