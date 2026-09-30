#!/usr/bin/env bash
set -euo pipefail

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
prefix=${PREFIX:-"$HOME/.local"}
cd "$project_dir"
cargo build --locked --release
mkdir -p "$prefix/bin"
temporary_binary=$(mktemp "$prefix/bin/.sippy.XXXXXX")
trap 'rm -f "$temporary_binary"' EXIT
install -m755 target/release/sippy "$temporary_binary"
mv -f -- "$temporary_binary" "$prefix/bin/sippy"
install -Dm644 packaging/sippy.desktop "$prefix/share/applications/sippy.desktop"
install -Dm644 assets/logo/sippy-icon.png "$prefix/share/icons/hicolor/512x512/apps/sippy.png"
# Remove the previous client's launcher only after Sippy is installed successfully.
rm -f -- "$prefix/bin/gosiptea-client" \
  "$prefix/share/applications/gosiptea-client.desktop" \
  "$prefix/share/icons/hicolor/512x512/apps/gosiptea-client.png"
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$prefix/share/icons/hicolor" >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
fi
printf 'Installed %s and its desktop entry and app icon.\n' "$prefix/bin/sippy"
