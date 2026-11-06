#!/usr/bin/env bash
# Build vintrdp.app as a universal binary (Apple silicon and Intel) and package
# it as a .dmg and a .zip.
#
#   scripts/dist/package-macos.sh [options]
#
# GnuTLS and friends link statically (see build-deps.sh), so the app needs only
# what macOS ships, not Homebrew. MACOSX_DEPLOYMENT_TARGET sets the oldest macOS
# it runs on (default 13.0).
#
# Signing and notarization run when their credentials are present and are
# skipped (ad-hoc signature, no notarization) when they are not:
#
#   MACOS_SIGN_IDENTITY   a "Developer ID Application: ..." identity in your keychain
#   Notarization, either an App Store Connect API key
#     NOTARY_KEY_ID, NOTARY_ISSUER_ID, NOTARY_KEY_PATH (the .p8 file)
#   or an Apple ID
#     NOTARY_APPLE_ID, NOTARY_PASSWORD (app-specific), NOTARY_TEAM_ID
#
# Set REQUIRE_NOTARIZATION=1 to fail instead of shipping unnotarized files.
source "$(dirname "$0")/lib.sh"

parse_common_args "$@"
set -- ${REST[@]+"${REST[@]}"}
case "${1:-}" in
    -h|--help) sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; common_usage; exit 0 ;;
    "") ;;
    *) die "unknown option: $1" ;;
esac
[ "$(uname -s)" = Darwin ] || die "package-macos.sh runs on macOS only"
need lipo codesign hdiutil ditto iconutil sips

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
NOTARY=()
if [ -n "${NOTARY_KEY_ID:-}" ] && [ -n "${NOTARY_ISSUER_ID:-}" ] && [ -n "${NOTARY_KEY_PATH:-}" ]; then
    NOTARY=(--key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID")
elif [ -n "${NOTARY_APPLE_ID:-}" ] && [ -n "${NOTARY_PASSWORD:-}" ] && [ -n "${NOTARY_TEAM_ID:-}" ]; then
    NOTARY=(--apple-id "$NOTARY_APPLE_ID" --password "$NOTARY_PASSWORD" --team-id "$NOTARY_TEAM_ID")
fi
if [ "$IDENTITY" = "-" ]; then
    NOTARY=()   # Apple refuses to notarize ad-hoc signatures
    msg="no Developer ID identity: signing ad hoc, not notarizing"
    [ "${REQUIRE_NOTARIZATION:-0}" = 1 ] && die "$msg"
    warn "$msg"
elif [ "${#NOTARY[@]}" = 0 ]; then
    msg="signing as '$IDENTITY' but no notarization credentials are set: not notarizing"
    [ "${REQUIRE_NOTARIZATION:-0}" = 1 ] && die "$msg"
    warn "$msg"
fi

# notarytool exits 0 even for a rejected submission, so read the status.
notarize() {  # <file>
    [ "${#NOTARY[@]}" -gt 0 ] || return 0
    local out id status
    group_start "notarize $(basename "$1")"
    out=$(xcrun notarytool submit "$1" "${NOTARY[@]}" --wait --output-format json)
    echo "$out"
    id=$(printf '%s' "$out" | plutil -extract id raw -o - - 2>/dev/null || true)
    status=$(printf '%s' "$out" | plutil -extract status raw -o - - 2>/dev/null || true)
    if [ "$status" != "Accepted" ]; then
        [ -n "$id" ] && xcrun notarytool log "$id" "${NOTARY[@]}" || true
        die "notarization of $(basename "$1") finished as '${status:-unknown}'"
    fi
    group_end
}

WORK=$(stage_dir)
trap 'rm -rf "$WORK"' EXIT

# ── Binary ───────────────────────────────────────────────────────────────────
SLICES=()
for t in aarch64-apple-darwin x86_64-apple-darwin; do
    if [ "$SKIP_BUILD" = 0 ]; then
        "$DIST_ROOT/scripts/dist/build-deps.sh" --target "$t"
        VINTRDP_DEPS="$DIST_ROOT/target/deps/$t" build_app "$t"
    fi
    SLICES+=("$(release_dir "$t")/vintrdp")
done

APP="$WORK/vintrdp.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/vintrdp" "${SLICES[@]}"
strip -x "$APP/Contents/MacOS/vintrdp"
# Nothing outside the system: anything else wouldn't exist on a user's Mac.
EXTRA=$(otool -L "$APP/Contents/MacOS/vintrdp" | awk '/^\t/ {print $1}' | grep -v -E '^(/usr/lib/|/System/Library/)' || true)
[ -z "$EXTRA" ] || die "the app links libraries macOS doesn't ship: $EXTRA"

# ── Icon ─────────────────────────────────────────────────────────────────────
ICONSET="$WORK/vintrdp.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" crates/vintrdp/assets/icon-1024.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" crates/vintrdp/assets/icon-1024.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/vintrdp.icns"

# ── Info.plist ───────────────────────────────────────────────────────────────
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>vintrdp</string>
    <key>CFBundleDisplayName</key><string>vintrdp</string>
    <key>CFBundleIdentifier</key><string>org.vintrdp.vintrdp</string>
    <key>CFBundleExecutable</key><string>vintrdp</string>
    <key>CFBundleIconFile</key><string>vintrdp</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key><string>${MACOSX_DEPLOYMENT_TARGET}</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# ── Sign, notarize, package ──────────────────────────────────────────────────
if [ "$IDENTITY" = "-" ]; then
    codesign --force --sign - "$APP"
else
    codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP"
    codesign --verify --strict --verbose=2 "$APP"
fi
if [ "${#NOTARY[@]}" -gt 0 ]; then
    ditto -c -k --keepParent "$APP" "$WORK/notarize.zip"
    notarize "$WORK/notarize.zip"
    xcrun stapler staple "$APP"
    spctl --assess --type execute --verbose "$APP"
    rm -f "$WORK/notarize.zip"
fi

group_start "zip + dmg"
ZIP="$OUT_DIR/vintrdp-macos-universal-${VERSION}.zip"
DMG="$OUT_DIR/vintrdp-macos-universal-${VERSION}.dmg"
rm -f "$ZIP" "$DMG"
ditto -c -k --keepParent "$APP" "$ZIP"
mkdir -p "$WORK/dmg"
cp -R "$APP" "$WORK/dmg/"
ln -s /Applications "$WORK/dmg/Applications"
hdiutil create -volname "vintrdp" -srcfolder "$WORK/dmg" -ov -format UDZO "$DMG"
[ "$IDENTITY" = "-" ] || codesign --force --timestamp --sign "$IDENTITY" "$DMG"
group_end
notarize "$DMG"
[ "${#NOTARY[@]}" -gt 0 ] && xcrun stapler staple "$DMG"

log "done:"; ls -lh "$OUT_DIR" >&2
