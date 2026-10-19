# Sparkle preparation for macOS updates

This unreleased slice prepares a real Sparkle framework and native bridge while keeping updates disabled. No feed or signing key is configured. The application never calls the bridge, and the bridge exposes no updater start, check or installation method. Configuration rejects both activation and automatic installation even when public feed/key fields have been supplied. No keys are generated, imported, exported or stored by these tools.

The proposed stable feed is **https://peerbrush.com/updates/macos/stable/appcast.xml**. This is a proposed, unpublished location, not an available update service or download. Putting the URL in this document does not configure `SUFeedURL`, deploy a feed or authorize its publication.

## Exact dependency and platform boundary

The pin is Sparkle **2.10.0**, official source commit `eef1a539a373c1f1a320624b1130fc5de7b2e100`. Its [official Package.swift](https://github.com/sparkle-project/Sparkle/blob/eef1a539a373c1f1a320624b1130fc5de7b2e100/Package.swift) selects `Sparkle-for-Swift-Package-Manager.zip` with SHA-256 `17e28312b8e18ab7cdbbe09a6fb28cc55a5479ec6c371dbc07cdecd2a14fd959`. The archive verified during preparation is 10,193,895 bytes. The [source release](https://github.com/sparkle-project/Sparkle/releases/tag/2.10.0) and complete upstream/external license notice are recorded in `packaging/sparkle/`; the GPL license of PeerBrush is unchanged. A changed or republished upstream asset fails the committed hash pin.

Sparkle2.10 requires macOS12 or later. The disabled core application retains macOS11 support. Both preparation and the native probe reject unsupported systems before loading framework code. A future release must decide how to offer updates on macOS11 before embedding/enabling this dependency; this slice does not silently raise the application's minimum OS or substitute an older framework. shortcut: this is an external preparation/probe path only; complete the macOS compatibility and package-signing policy before app bundling.

The reviewed framework inventory contains **151 entries,9 exact symlinks and5 code objects**. It includes the original framework binary, Autoupdate tool, Updater.app and the Installer/Downloader XPC services, plus headers, resources, localization and signature seals. XPC services stay inside their original framework layout and are not enabled. The pin records every regular file's bytes/hash/mode, every directory's mode and every exact relative link target. Unexpected paths, changed bytes/modes, hard links, special files or escaping/dangling links fail. No prefix wildcard is added to any release or signing allowlist.

## Offline staging and native component check

Obtain the public official archive separately; the preparation script performs no download. Select a fresh output outside the checkout. On macOS12+:

```sh
python3 -B scripts/prepare_sparkle.py \
  --archive /absolute/downloads/Sparkle-for-Swift-Package-Manager.zip \
  --verify-only
python3 -B scripts/prepare_sparkle.py \
  --archive /absolute/downloads/Sparkle-for-Swift-Package-Manager.zip \
  --output /absolute/private/sparkle-preparation-r1
cargo run --locked --example sparkle_preparation -- \
  /absolute/private/sparkle-preparation-r1/Sparkle.framework
```

Archive-only verification is portable and does not load code. Staging checks the complete official archive checksum and exact selected inventory before writing, rejects every registered source checkout/worktree, creates a private immutable directory, copies the complete upstream license and emits a receipt with `release_ready:false`, updater not started and feed/key unconfigured. Existing outputs are never replaced. Failed external work remains for inspection; choose a fresh directory after resolving the cause. Keep receipts and framework/build output outside public source and artifacts.

The explicit native probe rechecks the compiled file/link/hash/mode pin and license before loading code. It uses macOS CommonCrypto for SHA-256 and verifies the five exact nested code objects with native `codesign --verify --strict`. The upstream signatures are ad hoc; this proves integrity of the selected preparation inputs, not PeerBrush Developer ID signing or notarization. It then loads the verified framework through NSBundle and creates [SPUStandardUpdaterController](https://sparkle-project.org/documentation/api-reference/Classes/SPUStandardUpdaterController.html) with `startingUpdater:NO`. Its main-thread owner retains the controller, framework and weakly referenced delegate. Real native selectors confirm the updater cannot check and the delegate vetoes checks, proceeding with an update, relaunch and permission prompts. There is no call to `startUpdater`, update checks, signing tools in signing mode, installer helpers, key tools or network services.

The native byte/signature verifier's scope is the pinned framework and license. The Python preparation script enforces external paths, private output permissions and the exact disabled receipt; the native byte verifier does not authenticate that receipt or assert its parent directory's ownership/privacy. Run the staging tool first for the native CI/component workflow.

The [pinned controller initializer](https://github.com/sparkle-project/Sparkle/blob/eef1a539a373c1f1a320624b1130fc5de7b2e100/Sparkle/SPUStandardUpdaterController.m) calls start only when its boolean argument is true. The adapter passes false and exposes no start route. Sparkle2.10 exposes no public started-state property: its [updater implementation](https://github.com/sparkle-project/Sparkle/blob/eef1a539a373c1f1a320624b1130fc5de7b2e100/Sparkle/SPUUpdater.m) keeps that state private and enables checking when start succeeds. The probe checks `canCheckForUpdates=false` for this newly constructed controller; that property alone is not a general proof that an arbitrary updater is stopped or that no installation is pending.

CI downloads only the hash-pinned public framework, stages it under the runner's temporary directory and runs the native component check on macOS26. It never uploads the framework or private preparation receipt as an application release. Portable synthetic tests cover input hashes, path/type/mode/link tampering, unsupported OS, output safety and disabled immutable receipts; Unix CI additionally exercises actual filesystem links and hard-link rejection. Native Rust tests check platform SHA-256 vectors and public configuration guards. Local Windows verification cannot establish that the macOS bridge compiles or runs; native CI and real installed acceptance must be recorded separately.

## Required setup before enabling an updater

The maintainer must first choose the macOS compatibility policy and authorize feed/key setup. Use Sparkle's [official setup](https://sparkle-project.org/documentation/) to create and securely retain an Ed25519 signing key in a protected maintainer signing environment. Only its public Ed25519 key belongs in application configuration; never commit or expose a private key, credential, recovery export or signing log. Sparkle archive/feed signatures are separate from Apple Developer ID signing/notarization. This preparation neither creates a key nor asks for a private key.

Deploy the approved HTTPS appcast location and verify its actual published bytes, signed feed/release notes, immutable exact-source signed/notarized archives, final size/checksum/signature, platform/minimum-OS/version and recovery behavior. The proposed URL must remain unavailable in update UI until those checks succeed. Preserve the application's original version/source in receipts; internal `-dev.N` candidates are manual replacements and cannot be signed as releases. Do not use the internal numeric bundle projection as a development update feed.

Before any application bundling, extend the package/source/notice inventory and signing verifier for this exact framework and its permitted symlinks/nested signatures. Sign inner code objects explicitly with the approved application identity, then the framework and app; no `--deep` signing, ad hoc release acceptance, broad signature-path allowance or entitlement exception. Inspect the final archive/image and all resources after signing/notarization. Current release tooling intentionally rejects an injected framework, so this preparation cannot silently enter a release.

Future app configuration should explicitly disable automatic checks, automatic downloading/installation, automatic-update permission and system profiling while setup is incomplete. Upstream's [customization guide](https://sparkle-project.org/documentation/customization/) describes `SUEnableAutomaticChecks`, `SUAutomaticallyUpdate`, `SUAllowsAutomaticUpdates` and `SUEnableSystemProfiling`; absence of a permission override can cause a second-launch prompt once an updater is started. Require archive verification before extraction and signed appcasts/release notes (`SUVerifyUpdateBeforeExtraction` and `SURequireSignedFeed`), with signed-feed failure expiration disabled, before serving real updates. These settings are preparation requirements, not changes to a user's defaults in this slice.

## Unsaved projects and termination are a separate gate

Before activation, connect installation to the shared update-specific lifecycle contract: obtain reviewed project identities/revisions from `update_readiness`, check live UI gestures/dialog/export/loading work, then use `finish_update_exit` to atomically revalidate the complete saved project set, source conflicts, reservations and in-flight operations before closing. Any project-set/revision/source/work change invalidates the review. Never cache a boolean readiness answer or substitute the ordinary exit path that allows Don't Save.

The real macOS termination path also needs this protection. Sparkle's [updater delegate contract](https://sparkle-project.org/documentation/api-reference/Protocols/SPUUpdaterDelegate.html) warns that the relaunch-postponement callback can be skipped for an already downloaded update or a no-relaunch choice, and an update scheduled on quit may install when the application terminates. A relaunch callback alone cannot guard unsaved work. Preserve winit's delegate and make the actual NSApplication termination/restart handoff respect the shared lifecycle and live UI blockers before permitting an installer continuation. This preparation avoids that risk by never starting the updater and vetoing progress/relaunch; it does not claim the installed update lifecycle is complete.

Final acceptance still needs native cold/warm launch, signed/quarantined app and image, user-initiated checks, progress/release notes, restart later, interrupted/failed download/install recovery, unsaved work and active gesture protection, settings retention/migration and upgrade/downgrade/revoked-signature scenarios on actual supported macOS versions. Those checks remain pending, as do configured feed/key and any authorized publication.
