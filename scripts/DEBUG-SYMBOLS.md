# Release debug symbols

Installer workflows build with `CARGO_PROFILE_RELEASE_DEBUG=1` and
`CARGO_PROFILE_RELEASE_STRIP=none`. These overrides apply only to application
build steps, leaving ordinary development/test builds unchanged.

On macOS, application builds explicitly use `split-debuginfo=packed` so rustc
generates the dSYM before deleting temporary LTO archives. Running dsymutil later
can lose information from those archives. `prepare-macos-symbols.sh` preserves
the compiler-generated dSYM, verifies that it contains DWARF and matches the
executable UUID, then strips the bundled executable.
Signing and startup checks run afterward. The installer contains the stripped
executable; a separate Actions artifact contains the dSYM archive and build
identity. Symbol collection must run before release outputs are cleaned. The
macOS 27 compatibility build follows the same process.

On Windows/MSVC, debug information is already external to the executable in a
PDB. `collect-windows-symbols.ps1` archives the matching build's PDB and executable
hash without modifying the PE executable or its unwind tables. MSI/MSIX staging
does not include the PDB.

Debug artifacts are named by platform, architecture, commit and run attempt.
Actions retains them for 90 days. Download and retain the matching artifacts
elsewhere for releases that need longer-term support; they are not GitHub Release
assets and are not automatically kept forever. The macOS executable hash in
build-info is measured before signing; use the UUID to match signed crash reports
to the dSYM. For example:

```
xcrun dwarfdump --uuid torto.dSYM
xcrun atos -arch arm64 -o torto.dSYM/Contents/Resources/DWARF/torto -l LOAD_ADDRESS ADDRESS
```

For Windows crash dumps, load the matching PDB in WinDbg. Keep unwind metadata
and runtime-required imports/exports intact; do not run a generic Unix strip tool
over the Windows executable.
