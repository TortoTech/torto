# macOS portable mode

Use the regular signed `Torto.app`; no separate portable application, installer,
release asset, or update feed is required. Put it in a writable directory and
create an ordinary `torto-portable-data` folder beside it before launching:

```text
Any folder name/
  Torto.app
  torto-portable-data/
    config/
    library/
    fonts/
    cache/
```

The outer folder and `.app` may be renamed. Only `torto-portable-data` has a
fixed name. Close Torto before moving the whole directory. Imported book paths,
settings, plaintext credentials, progress, annotations, statistics and persisted
AI results are resolved through the shared portable `AppPaths`. The data folder
is outside the signed bundle, so reading/writing it does not change the code
signature. Without that sibling folder the ordinary macOS user profile is used.

This mode starts with a fresh library. Import books or restore them through cloud
sync. It does not automatically move or merge the installed profile. A manual
full migration must account for the different config/cache directory mapping;
finish any old encrypted-credential migration in the ordinary app first.

The startup path is selected before credentials, fonts, library or workers start.
An invalid data-folder path, read-only destination or another process holding
the same profile lock is reported as an error, rather than silently opening a
different library. Symlinked portable data folders are rejected. A plain debug
executable outside an `.app` does not search arbitrary ancestor directories.

Gatekeeper can run a downloaded app from an App Translocation directory where its
original siblings are inaccessible. Torto reports that runtime location before
opening a default profile. Move the app through Finder into its final writable
directory and reopen it. The program does not remove quarantine attributes or
disable macOS security checks.

Portable sync uses a hash of the macOS platform UUID and numeric user ID, with a
macOS namespace. Raw hardware IDs are neither logged nor sent to the cloud.
The existing portable actor registry and previous-actor pending-change handling
are shared with Windows: returning to a computer reuses its identity and pending
records retain their original timestamps/vector clocks.

The existing macOS updater downloads the normal architecture-specific signed ZIP
and replaces the current `.app` in place, preserving the sibling data directory.
The existing first-frame confirmation and rollback protocol remains in use.
Portable update job metadata and the helper executable use a private directory
under the system temporary directory so ownership/mode checks don't depend on
an external drive's filesystem. Replacement staging stays beside the app, on the
installation volume. Finished helper/ZIP copies are removed and recent diagnostics
are bounded. Business data is never stored in the system temporary session.

## Verification

`cargo test --locked --workspace` covers path detection, renamed app/outer folder,
credential preservation, missing/invalid/read-only/symlinked directories,
translocation detection, stable macOS machine identity and private updater sessions.

After building the production binary, run:

```sh
python3 scripts/smoke/macos-portable.py target/debug/torto
```

The runner signs only temporary fixture bundles, opens a generated book through
the production reader and verifies a real portable profile, renames both the
outer folder and application, and reopens the imported book. Credentials, book
bytes, sync identity and bundle signature must remain intact. The same checks run
on a temporary exFAT disk image. It never uses the personal library or native
credential store. `--skip-exfat` explicitly limits the runner to the host filesystem.
The runner temporarily keeps the display awake, releasing its power assertions
on exit. `--output NEW_DIRECTORY` retains fixture reports for diagnosis.
CI runs both filesystem checks against the ordinary release bundle on each Mac
architecture; a working WindowServer, GPU and disk-image service are required.

`--smoke-test-portable OUTPUT [BOOK]` is the explicit fixture test entry point;
the fresh output directory must be beside the fixture `.app` and portable data.
It exercises the normal JSON credential store, unlike ordinary `--smoke-test`
which retains its independent profile and in-memory credentials.

The existing signed-update helper smoke runner also verifies portable sibling
data preservation during successful replacement, rollback and rejected updates.
It requires the normal publisher signing key, just like release CI.

Current local validation (macOS 27.0, Apple M6 / Metal): production build and
workspace tests passed (1,893 passed, 132 intentionally ignored). Real portable
GUI reading passed before and after renaming the outer folder and app on the host
filesystem; credentials, imported book bytes, sync identity and code signature
were preserved. The host disk-image service rejected exFAT image creation
(`Operation not permitted`), so exFAT runtime validation is pending the CI runner.
The full publisher-signed update smoke needs the release signing key, unavailable
in this local session; its data-preservation assertions are wired into release CI.
