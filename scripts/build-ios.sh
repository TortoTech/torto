#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

profile="${1:-debug}"
case "$profile" in
  debug) cargo_profile=dev ;;
  release) cargo_profile=release ;;
  *) echo "Usage: scripts/build-ios.sh [debug|release]" >&2; exit 2 ;;
esac

if ! xcrun --sdk iphoneos --show-sdk-path >/dev/null 2>&1 ||
   ! xcrun --sdk iphonesimulator --show-sdk-path >/dev/null 2>&1; then
  echo "Select full Xcode with the iOS platform installed (see docs/ios.md)." >&2
  exit 1
fi
command -v xcodegen >/dev/null || { echo "Install XcodeGen (brew install xcodegen)." >&2; exit 1; }
export IPHONEOS_DEPLOYMENT_TARGET=18.0
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
build="$(mktemp -d "${TMPDIR:-/tmp}/torto-ios.XXXXXXXX")"
trap 'rm -rf "$build"' EXIT

for target in aarch64-apple-ios aarch64-apple-ios-sim; do
  case "$target" in
    *-sim) sdk=iphonesimulator ;;
    *) sdk=iphoneos ;;
  esac
  # Pass the SDK only to target builds; the binding generator runs on macOS.
  SDKROOT="$(xcrun --sdk "$sdk" --show-sdk-path)" cargo rustc --locked \
    -p torto-android-bridge --lib --crate-type staticlib \
    --target "$target" --profile "$cargo_profile"
done

device="$CARGO_TARGET_DIR/aarch64-apple-ios/$profile/libtorto_android.a"
simulator="$CARGO_TARGET_DIR/aarch64-apple-ios-sim/$profile/libtorto_android.a"
mkdir -p "$build/bindings" "$build/simulator-bindings" "$build/headers" "$build/output"
for variant in device simulator; do
  folder=bindings
  if [[ "$variant" == simulator ]]; then folder=simulator-bindings; fi
  cargo run --locked -p torto-android-bridge --features bindgen --bin torto-bindgen -- \
    generate --library "${!variant}" --language swift --no-format --out-dir "$build/$folder"
done
diff -r "$build/bindings" "$build/simulator-bindings"
cp "$build/bindings/torto_androidFFI.h" apps/ios/Interop/TortoPixels.h "$build/headers/"
sed '/header "torto_androidFFI.h"/a\
    header "TortoPixels.h"
' "$build/bindings/torto_androidFFI.modulemap" > "$build/headers/module.modulemap"
cp "$build/bindings/torto_android.swift" "$build/output/"
xcodebuild -create-xcframework \
  -library "$device" -headers "$build/headers" \
  -library "$simulator" -headers "$build/headers" \
  -output "$build/output/TortoCore.xcframework"

version="$(sed -n 's/^version = "\([0-9.]*\)"$/\1/p' Cargo.toml)"
printf 'MARKETING_VERSION = %s\n' "$version" > "$build/output/Version.xcconfig"
# Keep the generated app icon opaque; UIKit applies the system mask.
mkdir -p "$build/output/Assets.xcassets/AppIcon.appiconset"
swift apps/ios/Interop/GenerateIcon.swift assets/macos/torto-512@2x.png \
  "$build/output/Assets.xcassets/AppIcon.appiconset/Icon.png"
cat > "$build/output/Assets.xcassets/AppIcon.appiconset/Contents.json" <<'JSON'
{"images":[{"filename":"Icon.png","idiom":"universal","platform":"ios","size":"1024x1024"}],"info":{"author":"xcode","version":1}}
JSON
cat > "$build/output/Assets.xcassets/Contents.json" <<'JSON'
{"info":{"author":"xcode","version":1}}
JSON
python3 apps/ios/Interop/generate-symbols.py apps/android/app/src/main/res/drawable "$build/output/Assets.xcassets"
mkdir -p "$build/output/Assets.xcassets/AboutLogo.imageset"
cp "$build/output/Assets.xcassets/AppIcon.appiconset/Icon.png" "$build/output/Assets.xcassets/AboutLogo.imageset/Icon.png"
cat > "$build/output/Assets.xcassets/AboutLogo.imageset/Contents.json" <<'JSON'
{"images":[{"filename":"Icon.png","idiom":"universal"}],"info":{"author":"xcode","version":1}}
JSON
mkdir -p "$build/output/Notices"
cp apps/android/third_party/material-symbols/{NOTICE.md,LICENSE} "$build/output/Notices/"
rm -rf apps/ios/Generated
mv "$build/output" apps/ios/Generated
(cd apps/ios && xcodegen generate)
