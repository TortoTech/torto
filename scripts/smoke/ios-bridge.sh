#!/usr/bin/env bash
# Host ABI and lifecycle check; does not substitute for an iOS app build.
set -euo pipefail
cd "$(dirname "$0")/../.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/torto-ios-smoke.XXXXXXXX")"
trap 'rm -rf "$scratch"' EXIT
cargo rustc --locked -p torto-android-bridge --features ios-interop --lib --crate-type staticlib
cargo build --locked -p torto-android-bridge --features bindgen --bin torto-bindgen
"$CARGO_TARGET_DIR/debug/torto-bindgen" generate \
  --library "$CARGO_TARGET_DIR/debug/libtorto_android.a" --language swift --no-format --out-dir "$scratch"
cp apps/ios/Interop/TortoPixels.h "$scratch/"
sed '/header "torto_androidFFI.h"/a\
    header "TortoPixels.h"
' "$scratch/torto_androidFFI.modulemap" > "$scratch/module.modulemap"
swiftc -swift-version 5 -warnings-as-errors -typecheck -I "$scratch" \
  "$scratch/torto_android.swift" apps/ios/Sources/{ReaderRaster,ReaderModel,CoreErrorMessage}.swift
swiftc -swift-version 5 -I "$scratch" "$CARGO_TARGET_DIR/debug/libtorto_android.a" \
  -framework CoreText -framework Security -framework SystemConfiguration \
  "$scratch/torto_android.swift" apps/ios/Sources/ReaderRaster.swift \
  apps/ios/Smoke/BridgeSmoke.swift -o "$scratch/smoke"
mkdir -p "$scratch/profile"
# Include open test fonts for predictable text on CI and offline machines.
python3 - "$scratch/profile" <<'PY'
import json, shutil, sys
from pathlib import Path
assets = Path("assets/fonts")
manifest = json.loads((assets / "manifest.json").read_text())
for font in manifest["fonts"]:
    file = assets / font["file"]
    if not file.is_file() or file.stat().st_size != font["size"]:
        continue
    out = Path(sys.argv[1]) / "fonts" / manifest["version"] / (font["id"] + "-" + font["sha256"] + ".ttf")
    out.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(file, out)
PY
node scripts/smoke/fixtures.mjs "$scratch/fixtures"
fixture="$scratch/fixtures/two-sections.epub"
python3 - "$scratch/fixtures/startup.epub" "$fixture" <<'PYTHON'
import sys
from zipfile import ZipFile
with ZipFile(sys.argv[1]) as src, ZipFile(sys.argv[2], "w") as dst:
    for name in src.namelist():
        data = src.read(name)
        if name == "book.opf":
            data = data.replace(b"</manifest>", b'<item id="second" href="second.xhtml" media-type="application/xhtml+xml"/></manifest>')
            data = data.replace(b"</spine>", b'<itemref idref="second"/></spine>')
        elif name == "nav.xhtml":
            data = data.replace(b"</ol>", b'<li><a href="second.xhtml">Second section</a></li></ol>')
        dst.writestr(name, data)
    dst.writestr("second.xhtml", """<html xmlns="http://www.w3.org/1999/xhtml">
      <head><title>Second section</title></head>
      <body><p>This paragraph is the shared reading anchor. 中文阅读位置。</p>
      <p>The next paragraph must not replace the saved position.</p></body></html>""")
PYTHON
"$scratch/smoke" "$scratch/profile" write "$fixture"
"$scratch/smoke" "$scratch/profile" read "$fixture"
"$scratch/smoke" "$scratch/profile" update "$fixture"
