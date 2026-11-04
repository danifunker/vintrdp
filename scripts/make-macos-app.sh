#!/bin/sh
# Build vintrdp.app (release build + icon) into target/release/.
#   scripts/make-macos-app.sh
set -eu
cd "$(dirname "$0")/.."

cargo build --release -p vintrdp
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app=target/release/vintrdp.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/vintrdp "$app/Contents/MacOS/vintrdp"

# Icon: every size macOS asks for, from the 1024px master.
iconset=$(mktemp -d)/vintrdp.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z $size $size crates/vintrdp/assets/icon-1024.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z $double $double crates/vintrdp/assets/icon-1024.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/vintrdp.icns"
rm -rf "$(dirname "$iconset")"

cat > "$app/Contents/Info.plist" <<PLIST
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
    <key>CFBundleShortVersionString</key><string>${version}</string>
    <key>CFBundleVersion</key><string>${version}</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict>
</plist>
PLIST

echo "$app"
