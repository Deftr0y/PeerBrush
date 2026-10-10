# Historical packaging correction

Release: `development-d70f64e`. Application version: `0.1.4`. Original application commit: `d70f64eca57083bf4bf43c39cd74eaad6fb8298a`. Compile with `python scripts/build_release.py` for path remapping. The explicit committed manifest governs packaged source, artwork, fonts and contributor guides. The application and dependency inputs are unchanged; packaging tooling is identified by the correction commit. Run `python scripts/package_checkpoint.py` after the remapped build. The executable has no version CLI; its MCP initialize reply supplies a headless version smoke check. All corrected executable hashes change and must be published with a dated correction label.

The development correction must include Windows, Linux and macOS rebuilt from the same historical application inputs. All three packages must pass combined archive, source, executable and license inspection.
