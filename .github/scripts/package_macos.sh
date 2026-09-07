#!/usr/bin/env bash
#
# Assemble, sign, notarize and package the macOS Gaggle.app.
#
# Usage: package_macos.sh <target-triple> <platform-name> <version> <build-number>
#   <target-triple>   e.g. aarch64-apple-darwin
#   <platform-name>   e.g. macos-aarch64   (the latest.json key)
#   <version>         e.g. 2.0.deadbee     (CFBundleShortVersionString)
#   <build-number>    a monotonic integer  (CFBundleVersion)
#
# Reads from the environment (all optional — missing ones downgrade the step
# gracefully so beta/main releases keep flowing until the secrets are added):
#   MACOS_CERT_P12_BASE64     Developer ID Application cert, base64 of the .p12
#   MACOS_CERT_PASSWORD       its export password
#   APPLE_API_KEY_ID          App Store Connect API key id
#   APPLE_API_ISSUER_ID       ... its issuer id
#   APPLE_API_KEY_P8_BASE64   ... base64 of the AuthKey_XXXX.p8
#
# Produces, in dist/:
#   gaggle-<platform>.zip[.sha256]   the auto-update payload — a ditto archive
#                                    of the signed+stapled Gaggle.app
#   Gaggle-<platform>.dmg[.sha256]   the human download — drag-to-Applications
#
# Signing is skipped (ad-hoc `codesign -s -`) when the cert secret is absent;
# notarization is skipped when the API-key secrets are absent. An unsigned build
# still installs — the user does right-click > Open once.

set -euo pipefail

TRIPLE="${1:?target triple}"
PLATFORM="${2:?platform name}"
VERSION="${3:?version}"
BUILD_NUMBER="${4:?build number}"

BIN="target/${TRIPLE}/release"
STAGE="$(mktemp -d)"
APP="${STAGE}/Gaggle.app"
DIST="dist"
mkdir -p "$DIST"

echo "==> Assembling ${APP}"
mkdir -p "${APP}/Contents/MacOS" "${APP}/Contents/Resources"

cp "${BIN}/gaggle-launcher" "${APP}/Contents/MacOS/gaggle-launcher"
cp "${BIN}/gaggle-gui"      "${APP}/Contents/MacOS/gaggle-gui"
chmod +x "${APP}/Contents/MacOS/"*

# Icon: build a proper .icns from the 1024px master with Apple's iconutil.
ICONSET="${STAGE}/Gaggle.iconset"
mkdir -p "$ICONSET"
SRC_ICON="crates/launcher/assets/icon-1024.png"
for sz in 16 32 128 256 512; do
  sips -z "$sz" "$sz"       "$SRC_ICON" --out "${ICONSET}/icon_${sz}x${sz}.png"   >/dev/null
  dbl=$(( sz * 2 ))
  sips -z "$dbl" "$dbl"     "$SRC_ICON" --out "${ICONSET}/icon_${sz}x${sz}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "${APP}/Contents/Resources/AppIcon.icns"

cat > "${APP}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Gaggle</string>
    <key>CFBundleDisplayName</key>
    <string>Gaggle</string>
    <key>CFBundleIdentifier</key>
    <string>com.gaggle.app</string>
    <key>CFBundleExecutable</key>
    <string>gaggle-launcher</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${BUILD_NUMBER}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

# --- code signing --------------------------------------------------------------
SIGN_ID="-"        # ad-hoc by default
NOTARIZE=0

if [ -n "${MACOS_CERT_P12_BASE64:-}" ]; then
  echo "==> Importing Developer ID certificate"
  KEYCHAIN="${STAGE}/build.keychain"
  KEYCHAIN_PW="$(openssl rand -hex 24)"
  CERT_P12="${STAGE}/cert.p12"
  echo "$MACOS_CERT_P12_BASE64" | base64 --decode > "$CERT_P12"

  security create-keychain -p "$KEYCHAIN_PW" "$KEYCHAIN"
  security set-keychain-settings -lut 21600 "$KEYCHAIN"
  security unlock-keychain -p "$KEYCHAIN_PW" "$KEYCHAIN"
  security import "$CERT_P12" -k "$KEYCHAIN" -P "${MACOS_CERT_PASSWORD:-}" \
    -T /usr/bin/codesign -T /usr/bin/security
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KEYCHAIN_PW" "$KEYCHAIN" >/dev/null
  # Make our build keychain the one codesign searches (the runner is ephemeral,
  # so replacing the search list outright is fine).
  security list-keychains -d user -s "$KEYCHAIN"
  security default-keychain -d user -s "$KEYCHAIN"

  SIGN_ID="$(security find-identity -v -p codesigning "$KEYCHAIN" | awk -F'"' '/Developer ID Application/ {print $2; exit}')"
  if [ -z "$SIGN_ID" ]; then
    echo "!! cert imported but no 'Developer ID Application' identity found — falling back to ad-hoc" >&2
    SIGN_ID="-"
  else
    echo "==> Signing as: ${SIGN_ID}"
    if [ -n "${APPLE_API_KEY_ID:-}" ] && [ -n "${APPLE_API_KEY_P8_BASE64:-}" ] && [ -n "${APPLE_API_ISSUER_ID:-}" ]; then
      NOTARIZE=1
    fi
  fi
fi

if [ "$SIGN_ID" = "-" ]; then
  CODESIGN_FLAGS=(--force)                              # ad-hoc
else
  CODESIGN_FLAGS=(--force --timestamp --options runtime) # Developer ID + hardened runtime
fi

# Sign inside-out: nested binaries first, then the bundle.
codesign "${CODESIGN_FLAGS[@]}" --sign "$SIGN_ID" "${APP}/Contents/MacOS/gaggle-gui"
codesign "${CODESIGN_FLAGS[@]}" --sign "$SIGN_ID" "${APP}/Contents/MacOS/gaggle-launcher"
codesign "${CODESIGN_FLAGS[@]}" --sign "$SIGN_ID" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP" || true

# --- notarize (non-fatal: an un-notarized signed build still installs) --------
STAPLED=0
NOTARY_ARGS=()
if [ "$NOTARIZE" = "1" ]; then
  echo "==> Notarizing"
  API_KEY_P8="${STAGE}/AuthKey.p8"
  echo "$APPLE_API_KEY_P8_BASE64" | base64 --decode > "$API_KEY_P8"
  NOTARY_ARGS=(--key "$API_KEY_P8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID" --wait)
  NOTARIZE_ZIP="${STAGE}/notarize.zip"
  ditto -c -k --keepParent "$APP" "$NOTARIZE_ZIP"
  if xcrun notarytool submit "$NOTARIZE_ZIP" "${NOTARY_ARGS[@]}" \
     && xcrun stapler staple "$APP"; then
    STAPLED=1
    echo "==> Stapled"
  else
    echo "!! notarization/stapling failed — shipping the signed-but-not-notarized build" >&2
  fi
else
  echo "==> Skipping notarization (no App Store Connect API key secrets)"
fi

# --- auto-update payload: a ditto archive of the finished .app ----------------
UPDATE_ZIP="${DIST}/gaggle-${PLATFORM}.zip"
rm -f "$UPDATE_ZIP"
ditto -c -k --keepParent "$APP" "$UPDATE_ZIP"
shasum -a 256 "$UPDATE_ZIP" | awk '{print $1}' > "${UPDATE_ZIP}.sha256"

# --- human download: a drag-to-Applications .dmg ----------------------------
DMG="${DIST}/Gaggle-${PLATFORM}.dmg"
DMG_ROOT="$(mktemp -d)"
cp -R "$APP" "${DMG_ROOT}/Gaggle.app"
ln -s /Applications "${DMG_ROOT}/Applications"
rm -f "$DMG"
hdiutil create -volname "Gaggle" -srcfolder "$DMG_ROOT" -ov -format UDZO "$DMG"
if [ "$STAPLED" = "1" ]; then
  # The .dmg needs its own notarization ticket before it can be stapled —
  # notarizing the .app (via notarize.zip) doesn't register one for the disk
  # image. Submit the .dmg itself, then staple. Non-fatal: the .app inside is
  # already notarized+stapled, so a drag-installed copy passes Gatekeeper
  # regardless.
  if xcrun notarytool submit "$DMG" "${NOTARY_ARGS[@]}" \
     && xcrun stapler staple "$DMG"; then
    echo "==> Stapled the .dmg"
  else
    echo "!! notarizing/stapling the .dmg failed (non-fatal)" >&2
  fi
fi
shasum -a 256 "$DMG" | awk '{print $1}' > "${DMG}.sha256"

echo "==> Done:"
ls -la "$UPDATE_ZIP" "$DMG"
