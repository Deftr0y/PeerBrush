# PeerBrush portable package and source

PeerBrush is a GPL v3 image editor for humans and compatible AI agents working on the same editable canvas. This archive includes the application, public user guides, artwork and fonts, corresponding source, and third-party notices.

## Run the application

Extract the complete platform ZIP. On Windows run `peerbrush.exe` or `Start PeerBrush.cmd`. On macOS open `PeerBrush.app`; on Linux run `./peerbrush`. Rust is not required to run the application. These packages have limited PSD compatibility; signing and native desktop verification depend on the particular release. Consult its published notes.

See [getting started](docs/getting-started.md), [AI connection setup](docs/mcp.md), [live code editing](docs/live-code.md) and [Photoshop fidelity](docs/photoshop-fidelity.md). Optional segmentation integration is in `integrations/`; models and connection configuration are supplied separately.

## Build from corresponding source

Extract `{{SOURCE_ARCHIVE}}`, install stable Rust and platform prerequisites, then run:

```sh
cargo test --locked
cargo build --release --locked
```

Ubuntu builds need `libxkbcommon-dev`, `libwayland-dev`, `libxcb-shape0-dev` and `libxcb-xfixes0-dev`. Windows MSVC builds need Visual Studio C++ build tools; macOS needs Xcode command-line tools. Python 3.11 or newer is required for packaging scripts, not the desktop application. The source includes the vendored input adapter, engine/codec/protocol tests, shaders, artwork/fonts and optional segmentation integration.

Release packaging requires a reviewed Git commit and the explicit policy in `scripts/package-files.json`. From a committed checkout run `python scripts/build_release.py`, then `python scripts/package.py`. See [archive policy](docs/packaging.md). An extracted source ZIP can build the app without Git metadata; preparing a new release archive requires committing the reviewed source in a new Git repository. Source and executable hashes in `release.json` identify package inputs; the release workflow separately enforces the build's source revision.

## Licensing

PeerBrush retains GPL v3 in `LICENSE`. Fonts and dependencies retain their notices under `licenses/` in the platform ZIP and their source notices. Private operational records, credentials, runtime connection data, generated QA output and installed development tools are excluded.

Official [repository](https://github.com/Deftr0y/PeerBrush), [releases](https://github.com/Deftr0y/PeerBrush/releases) and [roadmap](https://github.com/Deftr0y/PeerBrush/blob/main/FOLLOWUPS.MD).

macOS candidates additionally require `python scripts/package.py --sparkle-archive /absolute/downloads/Sparkle-for-Swift-Package-Manager.zip` with the exact committed public Sparkle archive pin. Acquire that dependency separately; preparation performs no download. The dormant bundle does not enable updates or establish native installation acceptance. See [Sparkle preparation](docs/sparkle-preparation.md).

The dormant package/component preparation does not implement application update execution, verified native termination completion or failed-install recovery. Those source changes remain required in addition to feed/public-key/native-policy setup; updater installation cannot be activated by configuration in this build.
