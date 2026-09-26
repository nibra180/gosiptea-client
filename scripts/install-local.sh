#!/usr/bin/env bash
set -euo pipefail

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
prefix=${PREFIX:-"$HOME/.local"}
cd "$project_dir"
cargo build --locked --release
mkdir -p "$prefix/bin"
temporary_binary=$(mktemp "$prefix/bin/.gosiptea-client.XXXXXX")
trap 'rm -f "$temporary_binary"' EXIT
install -m755 target/release/gosiptea-client "$temporary_binary"
mv -f -- "$temporary_binary" "$prefix/bin/gosiptea-client"
install -Dm644 packaging/gosiptea-client.desktop "$prefix/share/applications/gosiptea-client.desktop"
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
fi
printf 'Installed %s and its desktop entry.\n' "$prefix/bin/gosiptea-client"
