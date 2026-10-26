#!/usr/bin/env bash
# Optional Developer ID signing for GitHub Actions; no credentials are bundled.
set -euo pipefail
umask 077

mode="${1:-}"
identity="${MACOS_SIGNING_IDENTITY:-}"
keychain="${RUNNER_TEMP:?RUNNER_TEMP is required}/torto-signing.keychain-db"
marker="$RUNNER_TEMP/torto-signing-created"

case "$mode" in
  import)
    if [[ -z "$identity" ]]; then
      if [[ -n "${MACOS_CERTIFICATE_P12_BASE64:-}" || -n "${MACOS_CERTIFICATE_PASSWORD:-}" ]]; then
        echo "Set MACOS_SIGNING_IDENTITY together with both certificate secrets." >&2
        exit 1
      fi
      echo "Developer ID is not configured; this build will use ad-hoc signing."
      exit 0
    fi
    [[ "$identity" == "Developer ID Application: "* ]] || {
      echo "MACOS_SIGNING_IDENTITY must name a Developer ID Application certificate." >&2
      exit 1
    }
    : "${MACOS_CERTIFICATE_P12_BASE64:?Set the base64-encoded signing certificate secret}"
    : "${MACOS_CERTIFICATE_PASSWORD:?Set the signing certificate password secret}"
    [[ ! -e "$keychain" && ! -e "$marker" ]] || {
      echo "Refusing to replace an existing signing keychain." >&2
      exit 1
    }
    certificate_path="$RUNNER_TEMP/torto-signing.p12"
    trap 'rm -f "$certificate_path"' EXIT
    printf '%s' "$MACOS_CERTIFICATE_P12_BASE64" | base64 --decode > "$certificate_path"
    keychain_password="$(openssl rand -hex 32)"
    security create-keychain -p "$keychain_password" "$keychain"
    touch "$marker"
    security set-keychain-settings -lut 21600 "$keychain"
    security unlock-keychain -p "$keychain_password" "$keychain"
    security import "$certificate_path" -k "$keychain" -P "$MACOS_CERTIFICATE_PASSWORD" \
      -T /usr/bin/codesign -T /usr/bin/security
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
      -k "$keychain_password" "$keychain" > /dev/null
    ;;
  sign)
    app="${2:?Pass the application bundle path}"
    bundle_id="$(plutil -extract CFBundleIdentifier raw -o - "$app/Contents/Info.plist")"
    [[ "$bundle_id" == "io.github.l-chris.torto" ]] || {
      echo "Unexpected application bundle identifier: $bundle_id" >&2
      exit 1
    }
    if [[ -n "$identity" ]]; then
      [[ "$identity" == "Developer ID Application: "* && -f "$marker" && -f "$keychain" ]] || {
        echo "Import the configured Developer ID certificate before signing." >&2
        exit 1
      }
      codesign --force --deep --options runtime --timestamp --keychain "$keychain" \
        --sign "$identity" "$app"
    else
      echo "Ad-hoc signing: rebuilt apps may require Keychain authorization again."
      codesign --force --deep --sign - "$app"
    fi
    codesign --verify --deep --strict --verbose=2 "$app"
    codesign --display -r- "$app"
    ;;
  cleanup)
    if [[ -f "$marker" ]]; then
      security delete-keychain "$keychain"
      rm -f "$marker"
    fi
    ;;
  *)
    echo "Usage: $0 import | sign APP | cleanup" >&2
    exit 2
    ;;
esac
