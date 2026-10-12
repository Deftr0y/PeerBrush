# Publisher-signing setup and release gates

Preparation supports Microsoft Store MSIX distribution on Windows and Developer ID signed, notarized direct distribution on macOS. The existing unsigned ZIPs remain historical artifacts. No package is published by these scripts, and preparation is not proof of Store approval or Apple acceptance.

## Configured Windows product

The reserved PeerBrush product has public identity `Deftroy.PeerBrush`, Publisher `CN=438C1F79-CBE8-4F97-8C50-F9AC34C7D2AC`, PublisherDisplayName `Deftroy`, package family `Deftroy.PeerBrush_wszhgmjq5tjta`, and Store ID `9PNR6T1Q2ZL9`. The exact Publisher derives the supplied family. These values are committed in the MSIX configuration. The initial planned package version is `1.25.0.0`; there is no supplied prior published package version. Native SDK and account/certification verification are still required.

## Setup and remaining public values

### Microsoft Store

1. Confirm an active Partner Center Windows developer account and reserve the intended PeerBrush product name.
2. From the app's **Product management > Product identity** page, copy the exact **Package/Identity/Name**, **Package/Identity/Publisher** distinguished name, and **Package/Properties/PublisherDisplayName**. Capitalization, punctuation and spaces matter. Put these in an untracked identity JSON using the example's keys. No guessed production publisher is built in.
3. For an existing Store product, provide its latest published package version. Keep the same identity and choose a strictly greater three-component version with the fourth component zero. For a first app version 0.25.0, the default map is 1.25.0.0, subject to the product's actual history.
4. Prepare the product listing, screenshots, support/privacy URLs, age-rating answers, markets/pricing and a factual `runFullTrust` explanation before the separately approved Store submission. Native GUI tests and the Windows App Certification Kit remain required.

An MSIX submission does not need a purchased public-CA signing certificate. Microsoft applies its certificate after successful certification. This does not make the existing direct-download Windows ZIP Microsoft-signed, nor guarantee that every operating-system prompt disappears.

### Apple Developer ID

1. Confirm the Apple Developer Program team and its public ten-character Team ID. Keep the app bundle identifier `com.peerbrush.PeerBrush` unless an intentional migration is reviewed.
2. On an authorized Mac, the user securely creates or selects a **Developer ID Application** certificate with its matching private key in Keychain. A Developer ID Installer certificate or an Apple Development certificate is not a substitute for signing this app.
3. The user configures an Apple-supported `notarytool` authentication profile in Keychain. Share only the local profile name and public signing identity selector needed by the script. Enter passwords, private keys and API credentials only through the appropriate secure account/Keychain flow; never put them in chat, repository files, command examples or build logs.
4. Use the ordered workflow in [macOS signing](macos-signing.md) on that Mac. It signs with hardened runtime and a secure timestamp, waits for an accepted notarization result, staples and verifies the ticket, then validates the final re-extracted distribution archive before producing final checksums.

A dedicated protected macOS signing runner is optional future infrastructure. Creating it, installing credentials or granting persistent access requires its own explicit authorization. These changes neither provision such a runner nor import certificates or secrets.

## Version and source provenance

The published v0.25 base is `f5083d2e33a0158c9976db5de89549bd34ee7bc3`. The MSIX integration adds executable alias, writable-state and file-activation behavior, so resulting executables must identify a **new reviewed source commit**. They are not the unchanged v0.25 binaries.

Before publication, select an explicitly named revision or new app version and update release notes accordingly. Do not retag v0.25 or silently replace its unsigned assets. Build Windows, macOS and Linux from the same reviewed commit and preserve corresponding GPL source and notices. Keep historical version records and download links intact. A packaging-only Apple signing pass over an existing binary is a separate option only if exact input archive/source provenance is verified and no runtime-source change is falsely attributed to that binary.

## Completion evidence

- Exact source commit, per-platform version/architecture and native CI result
- Actual MSIX made by the Windows SDK, exact Partner Center identity, package/block-map hashes, WACK result and clean-machine install/update/uninstall checks
- Store certification result, actual public product link and verified Store-distributed installation/signature
- Apple Developer ID verification, notarization submission ID with Accepted status, stapler validation and Gatekeeper assessment of the final re-extracted archive
- Final archive SHA256, signed-executable SHA256, matching source archive, licenses and artifact inventory
- Fresh download and native launch on a clean user profile, including MCP pipes, file associations, unsaved work, Unicode paths and update-stable aliases
- Approved publication destinations read back after publication

Checks requiring Windows/macOS, account access or distribution approval remain explicit gates. Synthetic Linux packaging tests cannot establish any of those results.

## Official references

- [Microsoft Store package requirements and signing](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements)
- [Microsoft app identity details](https://learn.microsoft.com/en-us/windows/apps/publish/view-app-identity-details)
- [Apple Developer ID](https://developer.apple.com/developer-id/)
- [Apple notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)
