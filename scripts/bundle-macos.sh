#!/usr/bin/env bash
# Builds Shell.app. macOS reads an app's icon from the bundle rather than from
# anything the process sets at runtime, so a bundle is the only way to get the
# icon into the Dock and the app switcher.
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="Shell"
BUNDLE="target/$APP_NAME.app"
BUNDLE_ID="com.tanoomohal.shell"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"

cargo build --release

rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"

cp target/release/shell "$BUNDLE/Contents/MacOS/$APP_NAME"
cp assets/icon.icns "$BUNDLE/Contents/Resources/icon.icns"

cat > "$BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>$APP_NAME</string>
	<key>CFBundleDisplayName</key>
	<string>$APP_NAME</string>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleExecutable</key>
	<string>$APP_NAME</string>
	<key>CFBundleIconFile</key>
	<string>icon</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
PLIST

# Ad-hoc signature. Without it macOS treats a freshly built binary with
# suspicion and Gatekeeper can refuse to launch it.
codesign --force --deep --sign - "$BUNDLE" 2>/dev/null || \
	echo "warning: ad-hoc signing failed; the app may need a right-click > Open on first launch"

echo "built $BUNDLE"
