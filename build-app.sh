#!/bin/sh
set -eu
cd "$(dirname "$0")"
git submodule update --init -- vendor/mlx-c
cargo build --release --locked --bins
signing_identity=${CODESIGN_IDENTITY:-}
if [ -z "$signing_identity" ]; then
    signing_identity=$(security find-identity -v -p codesigning | awk '/"Apple Development:/ {print $2; exit}')
fi
app=${APP_OUTPUT:-"target/Index Voice.app"}
mkdir -p "$app/Contents/MacOS" "$app/Contents/Helpers"
mkdir -p "$app/Contents/Resources"
cp LICENSE NOTICE "$app/Contents/Resources/"
cp target/release/index-voice "$app/Contents/MacOS/IndexVoice"
cp target/release/pebble-index "$app/Contents/Helpers/pebble-index"
cp target/release/QwenNative target/release/mlx.metallib "$app/Contents/Helpers/"
mkdir -p "$app/Contents/Resources/qwen"
cp licenses/qwen/LICENSE licenses/qwen/NOTICE licenses/qwen/MLX-LICENSE "$app/Contents/Resources/qwen/"
cp vendor/mlx-c/LICENSE "$app/Contents/Resources/qwen/MLX-C-LICENSE"
codesign --force --sign "${signing_identity:--}" --identifier local.pebble.index-voice.QwenNative "$app/Contents/Helpers/QwenNative"
codesign --force --sign "${signing_identity:--}" --identifier local.pebble.index-voice.mlx-metal "$app/Contents/Helpers/mlx.metallib"
for helper in Bluetooth SpeechStream AudioDecode Paste Microphone; do
    xcrun swiftc -O -parse-as-library -target "$(uname -m)-apple-macos26.0" "native/$helper.swift" -o "$app/Contents/Helpers/$helper"
    codesign --force --sign "${signing_identity:--}" --identifier "local.pebble.index-voice.$helper" "$app/Contents/Helpers/$helper"
done
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>local.pebble.index-voice</string>
<key>CFBundleName</key><string>Index Voice</string>
<key>CFBundleExecutable</key><string>IndexVoice</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>LSMinimumSystemVersion</key><string>26.0</string>
<key>LSUIElement</key><true/>
<key>NSBluetoothAlwaysUsageDescription</key><string>Index 01から録音とボタン状態を受信します。</string>
<key>NSMicrophoneUsageDescription</key><string>右Optionを押している間の音声を文字起こしします。</string>
<key>NSSpeechRecognitionUsageDescription</key><string>リングの録音をMac上で文字起こしします。</string>
</dict></plist>
PLIST
codesign --force --sign "${signing_identity:--}" --identifier local.pebble.index-voice.backend "$app/Contents/Helpers/pebble-index"
codesign --force --sign "${signing_identity:--}" "$app"
printf '%s\n' "Built: $app"
