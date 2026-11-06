# macOS automatic updates

macOS shares the desktop updater and the core release/download implementation
with Windows and Android. Settings → About supports checking, downloading, and
installing an update. Downloads use the GitHub Asset API with Gitee fallback.
DMGs remain available for first installation and manual recovery.

## Installation protocol

1. Validate the release's pinned Ed25519 publisher signature, ZIP size, and SHA-256.
2. Copy the current executable into a private cache session as `TortoUpdater`.
   Capture reading progress and flush statistics on a worker before requesting exit.
3. The helper extracts into a private sibling directory on the installation volume.
   It checks the bundle identifier, version, executable architecture, Apple code
   signature, and Team ID when the installed app has one. ZIP paths, symlinks,
   file count, and expanded size are bounded. Other instances of the same installed
   executable block installation.
4. Only after the helper reports readiness does the current application exit.
   The helper waits for that PID to exit, renames the installed app to a backup,
   moves the replacement into place, and starts it directly.
5. The new app confirms startup after its first submitted GUI frame. Failure to
   start or confirm within 30 seconds restores and relaunches the previous app.
   The restored GUI shows a rollback notice. If restoration itself fails, the
   helper preserves `previous.app` rather than deleting the remaining backup.

Installation directory permissions must allow replacement. Read-only volumes,
App Translocation, and symlinked installation paths require moving the app to a
writable directory (for example `~/Applications`) or manual update.
There is no privileged helper. The helper never modifies the user's library,
credentials, or reading configuration.

Ordinary-profile sessions live under the downloaded version's `sessions/<UUID>`
cache directory. In [portable mode](macos-portable.md), sessions use a private
system temporary directory while the library and download cache remain portable.
`state.json` records preparation, installation, completion, or failure. ZIP and
helper copies are removed when installation finishes; only small diagnostics
remain, with recent finished sessions retained. Staging and backups are removed
after success or successful rollback.

## Release artifacts and publisher key

For each `arm64` and `x86_64` build, the macOS workflow publishes:

- `Torto-<version>-macos-<arch>.dmg` and `.dmg.sha256`
- `Torto-<version>-macos-<arch>.zip`, `.zip.sha256`, and `.zip.sig.json`

`scripts/package-macos-update.py` packs the final signed application.
`sign-macos-update` signs a domain-separated message containing version, filename,
size, and hash. The client and mirror script pin the public key in
`assets/macos/update-public-key.hex`. Release builds require the repository Actions
secret `TORTO_MACOS_UPDATE_SIGNING_KEY` (Base64 PKCS#8 Ed25519 private key).
PR builds never need that secret. The private key must be backed up outside git;
losing it blocks updates accepted by existing clients. Rotation requires an
update signed by the old key that introduces trust for the next key.

Gitee publishes separate `torto-macos-<arch>-update.json` completion markers only
after verifying the release attachments and publisher signature. Historical
releases without ZIPs remain mirrorable. A partially uploaded ZIP release is
not marked complete. Windows and Android marker names remain unchanged.

This publisher key is independent of Apple Developer ID. Ad-hoc Apple signing
still does not provide notarization or stable Keychain identity. The existing
optional Developer ID signing configuration remains supported.

## Verification

Build the desktop binary and signing example, then run:

```sh
cargo build --locked -p rebook-desktop --bin torto
cargo build --locked -p rebook-app-core --example sign-macos-update
# Provide TORTO_MACOS_UPDATE_SIGNING_KEY through the environment, never argv.
python3 scripts/smoke/macos-update.py target/debug/torto target/debug/examples/sign-macos-update
node --test scripts/sync-gitee-release.test.mjs
cargo test --locked --workspace
```

The macOS smoke runner uses temporary signed fixture apps and the production
helper. It covers replacement, rollback, signature/hash rejection, unsafe paths,
symlinks, wrong versions, downgrades, read-only installations, and concurrent
instances. A real GUI startup validates the first-frame handshake with the
existing smoke mode's isolated profile and in-memory credentials. Neither the
installed `/Applications/Torto.app` nor the real library is used. Release CI
runs the same protocol checks for both architectures before uploading installers.
