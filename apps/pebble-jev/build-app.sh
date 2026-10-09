#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
cargo build --release --locked -p pebble-jev
signing_identity=${CODESIGN_IDENTITY:-}
if [ -z "$signing_identity" ]; then
    signing_identity=$(security find-identity -v -p codesigning | awk '/"Apple Development:/ {print $2; exit}')
fi
app=${APP_OUTPUT:-"target/Pebble Jev.app"}
mkdir -p "$app/Contents/MacOS" "$app/Contents/Helpers" "$app/Contents/Resources"
cp LICENSE NOTICE "$app/Contents/Resources/"
cp target/release/pebble-jev "$app/Contents/MacOS/PebbleJev"
for helper in Bluetooth AudioPlayback; do
    source="apps/pebble-jev/native/$helper.swift"
    [ "$helper" = Bluetooth ] && source="crates/pebble-ring/native/Bluetooth.swift"
    xcrun swiftc -O -parse-as-library -target "$(uname -m)-apple-macos26.0" "$source" -o "$app/Contents/Helpers/$helper"
    codesign --force --sign "${signing_identity:--}" --identifier "local.pebble.jev.$helper" "$app/Contents/Helpers/$helper"
done
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>local.pebble.jev</string>
<key>CFBundleName</key><string>Pebble Jev</string>
<key>CFBundleExecutable</key><string>PebbleJev</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>LSMinimumSystemVersion</key><string>26.0</string>
<key>LSUIElement</key><true/>
<key>NSBluetoothAlwaysUsageDescription</key><string>Index 01から録音とボタン状態を受信します。</string>
</dict></plist>
PLIST
codesign --force --sign "${signing_identity:--}" "$app"
printf '%s\n' "Built: $app"
