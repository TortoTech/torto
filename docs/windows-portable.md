# Windows portable build

The release publishes `Torto-<version>-windows-x86_64-portable.zip` alongside
the MSI. Both contain the same release executable; PDBs remain separate debug
artifacts. Extract the complete ZIP into a writable directory and run
`torto.exe`. No installer, registry registration or administrator permission is
required. The x64 MSVC build links the C runtime statically; Windows CI checks
the executable's actual imports for external MSVC runtime DLLs.

```text
torto.exe
torto-portable.json          {"version":1}
LICENSE
README.txt
Data/
  config/                   settings and plaintext credentials.json
  library/                  books and shelf records
  fonts/                    downloaded reader fonts
  cache/                    previews, downloads and staged updates
  ...                       annotations, progress, translations and statistics
```

The executable-adjacent marker selects the portable profile before any workers
or application services start. Paths follow the executable, independent of the
working directory or drive letter. Book storage names remain relative to the
library; credentials need no OS key to move between machines. A malformed marker,
read-only directory or profile already open in another process causes an error;
the reader never silently falls back to AppData. A held OS file lock in `Data`
is automatically released on exit/crash. Explicit smoke tests keep their existing
isolated profile and in-memory credentials.

The first release starts with an empty profile; import books or restore the
library through cloud sync. It does not automatically copy an installed profile.
Move the complete folder to carry local data. Rebuildable caches can be removed;
translation and AI results are retained. Font choices use family names and
downloaded fonts are looked up under the current profile, rather than an old
absolute installation path.

`config/portable-devices.json` assigns a separate sync actor to each opaque
Windows-machine/user hash. Returning to a computer reuses its actor. The shared
cloud password is carried to the new actor. On the next sync, pending progress
and annotations belonging to previous actors are uploaded under their original
identities, timestamps and vector clocks before syncing the current actor. A
failed upload stays pending. Copying a portable profile within the same computer
continues the same actor; independent simultaneous clones should use separate
profiles rather than edit the same cloud actor concurrently.

The updater selects the portable ZIP and a distinct Gitee completion marker,
`torto-windows-portable-update.json`. Downloads share GitHub Asset API transport,
mirror fallback and SHA-256/size validation with the installed build. ZIP staging
accepts only the known root-level program/marker/license/readme files and cannot
overwrite `Data`. Saving reading state, verification and staging run on workers.
The detached PowerShell helper waits for the reader to exit, rechecks the profile
lock and staged executable hash, then replaces only `torto.exe`, retaining the
previous executable. A first-presented-frame token completes the update; early
exit or a 45-second timeout restores the old executable and restarts it with a
rollback notice. Replacement failure preserves the old program and staged job.
The installed MSI and Microsoft Store update strategies remain distinct.

If the computer loses power during the update and the previous executable is
still in `Data/cache/updates/portable-*/previous.exe`, it can be restored manually
with Torto closed. The data directory is never part of the replacement operation.

For a manual update, close Torto and replace the ZIP's program files while
retaining `Data`. Do not apply an MSI to the portable directory. Unplugging the
drive while files are being written is not a supported update procedure.

## Verification

- `cargo test --locked --workspace`: shared credentials, migration failure,
  profile move/locking, portable release selection, unsafe archive rejection,
  sync actor changes and pending-state preservation.
- `python3 scripts/test-windows-package.py`: exact executable, marker, checksum
  and absence of user data/debug symbols in the ZIP.
- `node --test scripts/sync-gitee-release.test.mjs`: portable mirror completion
  metadata, artifact lookup and existing verified mirror behavior.
- `scripts/test-windows-portable.ps1`: Windows CI checks actual PE imports,
  fresh startup in a path containing spaces/Unicode, directory moves and the real
  detached helper's success, rollback and corrupt-staging paths. These Windows
  runtime checks require a Windows host; macOS unit tests do not substitute for them.
