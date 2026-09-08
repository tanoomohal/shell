#!/usr/bin/env bash
# Installs the binary, desktop entry, and icons for the current user.
set -euo pipefail
cd "$(dirname "$0")/.."

PREFIX="${PREFIX:-$HOME/.local}"

cargo build --release
install -Dm755 target/release/shell "$PREFIX/bin/shell"
install -Dm644 packaging/shell.desktop "$PREFIX/share/applications/shell.desktop"

for px in 32 48 64 128 256; do
	install -Dm644 "assets/icon-$px.png" \
		"$PREFIX/share/icons/hicolor/${px}x${px}/apps/shell.png"
done

echo "installed to $PREFIX"
echo "if the icon does not appear, run: gtk-update-icon-cache -f -t $PREFIX/share/icons/hicolor"
