# Historical packaging correction

Release: `v0.1.2`. Application version: `0.1.2`. Original application commit: `f1c3476b0ec098a5d772f62a35042f21c2fc8dd8`. Compile with `python scripts/build_release.py` for path remapping. The explicit committed manifest governs packaged source, artwork, fonts and contributor guides. The application and dependency inputs are unchanged; packaging tooling is identified by the correction commit. Run `python scripts/package_checkpoint.py` after the remapped build. The executable has no version CLI; its MCP initialize reply supplies a headless version smoke check. All corrected executable hashes change and must be published with a dated correction label.

This checkpoint has only a historical Windows download; the correction does not add macOS or Linux release assets.
