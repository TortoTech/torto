#!/usr/bin/env bash
# Preserve compiler-generated debug information before stripping the executable.
# Run before codesign; keep the output outside the application/DMG staging tree.
set -euo pipefail

executable="${1:?Pass the bundled executable path}"
output="${2:?Pass the debug artifact directory}"
source_dsym="${3:?Pass the compiler-generated dSYM path}"
test -x "$executable"
test -d "$source_dsym"
mkdir -p "$output"
dsym="$output/torto.dSYM"
test ! -e "$dsym"
test ! -e "$output/torto.dSYM.tar.gz"

# rustc creates this before deleting temporary LTO archives. Running dsymutil
# afterward can silently lose debug information even while returning success.
ditto "$source_dsym" "$dsym"
# Cargo's dSYM can contain the dependency filename (torto-<hash>). Give the
# archived DWARF file the installed executable's name for symbolication tools.
dwarf_files=("$dsym/Contents/Resources/DWARF/"*)
test "${#dwarf_files[@]}" -eq 1
test -s "${dwarf_files[0]}"
dwarf="$dsym/Contents/Resources/DWARF/$(basename "$executable")"
if [[ "${dwarf_files[0]}" != "$dwarf" ]]; then
    mv "${dwarf_files[0]}" "$dwarf"
fi
xcrun dwarfdump --uuid "$executable" | awk '{print $2, $3}' > "$output/executable-uuid.txt"
xcrun dwarfdump --uuid "$dsym" | awk '{print $2, $3}' > "$output/dsym-uuid.txt"
test -s "$output/executable-uuid.txt"
cmp "$output/executable-uuid.txt" "$output/dsym-uuid.txt"
# A matching UUID alone also occurs for an empty dSYM. Require actual DWARF.
xcrun dwarfdump --debug-info --recurse-depth=0 "$dsym" > "$output/debug-info.txt"
grep -q 'DW_TAG_compile_unit' "$output/debug-info.txt"
rm "$output/debug-info.txt"

xcrun strip "$executable"
xcrun dwarfdump --uuid "$executable" | awk '{print $2, $3}' > "$output/stripped-uuid.txt"
cmp "$output/executable-uuid.txt" "$output/stripped-uuid.txt"
{
    printf 'commit=%s\n' "${GITHUB_SHA:-$(git rev-parse HEAD)}"
    printf 'run=%s\n' "${GITHUB_RUN_ID:-local}"
    xcrun dwarfdump --uuid "$dsym"
    shasum -a 256 "$executable"
} > "$output/build-info.txt"
tar -czf "$output/torto.dSYM.tar.gz" -C "$output" torto.dSYM
# Upload the archive, preserving dSYM permissions and bundle structure.
rm -r "$dsym"
