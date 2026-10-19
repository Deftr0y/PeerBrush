# Per-user preferences (PB-155 development)

Default launches keep preferences outside the installation and outside the temporary runtime workspace. These locations are an unreleased implementation; native installation, upgrade and uninstall acceptance is still pending.

| Platform | Configuration | Application data | Disposable cache |
| --- | --- | --- | --- |
| Windows | `%APPDATA%/PeerBrush` | `%LOCALAPPDATA%/PeerBrush/Data` | `%LOCALAPPDATA%/PeerBrush/Cache` |
| macOS | `~/Library/Application Support/PeerBrush/Config` | `~/Library/Application Support/PeerBrush/Data` | `~/Library/Caches/PeerBrush` |
| Linux | `$XDG_CONFIG_HOME/peerbrush`, default `~/.config/peerbrush` | `$XDG_DATA_HOME/peerbrush`, default `~/.local/share/peerbrush` | `$XDG_CACHE_HOME/peerbrush`, default `~/.cache/peerbrush` |

Relative or empty XDG overrides use their standard home-directory fallback. Missing absolute Windows profile locations fail with an error. `peerbrush settings paths` prints the resolved locations without reading private preference contents.

Update-check preferences are also stored as `updates.json`; see [update discovery and remaining installer work](updates.md).

The configuration directory contains `settings-v1`, a profile with the existing versioned `tool-settings.json`, `shortcuts.json`, `brushes.json`, `filters.json` and `export-presets.json` formats. These settings already use their existing validation, guarded writers and protected-file behavior. Tool profiles, custom brush/filter libraries, export presets, keyboard bindings and tablet button preferences survive restart and application replacement. This slice adds no preference controls or project-source settings.

The application-data location is defined for later work; this slice does not relocate recovery files or documents there. Discovery, MCP launch arguments, `connection.json`, `instance.lock`, recoveries and CLI preview paths retain their existing workspace locations. The Windows MSIX external-client AppData visibility guard remains unchanged. Preference files are internal application data, so Windows package virtualization does not expose an external MCP configuration path as usable.

An explicit `--state-dir PATH` keeps the existing instance-local preferences and cache. This includes development launches from `scripts/dev.ps1` and intentionally isolated workspaces. Their MCP clients continue using the same absolute state directory. `mcp`, `discover` and `cli` attach to the workspace without opening or migrating the user profile.

## Migration and failure behavior

On the first default application launch, PeerBrush copies the recognized preference files from the current default legacy workspace if the user profile does not exist. The installed Windows source remains `~/.peerbrush/state`; portable builds retain their existing temporary default workspace. Missing preference files use the existing defaults. Invalid or unsupported files stop this first migration with an error; they are preserved instead of silently resetting preferences.

To migrate a different portable or development workspace before the first default launch:

```sh
peerbrush settings migrate --from /absolute/path/to/legacy-workspace
```

Close the session using that source workspace first. A source-workspace lock and a per-user profile lock reject competing sessions. The per-user lock remains held for the default application session, including headless sessions; explicit isolated workspaces keep their own instance locks.

Migration reads only the known regular files with bounded sizes, copies their original bytes to a private staging directory, validates each copy with the existing format loader, and preserves a byte-exact `migration-backup` alongside the resulting profile. It preserves file access permissions and checks the source again, including files that were absent. A complete versioned profile is published by a same-filesystem directory rename that refuses an existing destination. No existing profile is merged, overwritten or downgraded. Existing malformed or future profile manifests remain protected.

An interrupted or failed attempt leaves the original workspace intact and no partially active profile. A retry uses a new staging directory. Inactive `migration-*` directories are retained for inspection; PeerBrush does not delete unknown files from them. They may contain private preference data and must not be committed, uploaded with diagnostics or included in packages. The completed profile's migration backup remains the original migration snapshot even after later settings edits. Runtime tokens, connection files, recovery indexes, project artwork and unrecognized files are never copied or deleted.

Reopening an existing profile does not import later changes from an old installation. An upgrade uses the same profile and existing preference versions. Replacing a damaged profile or choosing a different legacy source requires the user to preserve and relocate the existing profile explicitly; migration does not automate destructive recovery.

## Retention and remaining acceptance

Uninstall must retain preferences, migration backups and artwork by default. Any future installer option to remove settings must be explicit, separately reviewed and scoped to recognized preference data; this slice adds no removal operation. The current recovery/workspace retention policy is unchanged.

Focused automated coverage exercises platform path selection, first run, byte-exact migration, preferences after restart/reinstall, invalid and future versions, incomplete profiles, interrupted copies, source changes, competing locks, existing-destination protection and shortcut migration. Unix tests additionally exercise symbolic-link rejection and private directory permissions. Run `cargo test --locked settings::tests` and the existing shortcut/tool/library suites. Windows native acceptance, packaged MSIX path behavior and macOS/Linux installation/upgrade/reinstall checks remain required before closing PB-155.
