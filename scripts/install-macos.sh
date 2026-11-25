#!/usr/bin/env bash
# Installs target/bundle/bricolo.app for this user: `just
# install-macos`. Copies it to ~/Applications (no sudo), then runs the
# bundle's `bricolo service install --app <it>`, which writes the LaunchAgent
# and starts the node. Arguments go to `bricolo service install`, like --ui to
# open the hub's menu bar item at login too.
#
# Remove it with:
#   ~/Applications/bricolo.app/Contents/Helpers/bricolo.app/Contents/MacOS/bricolo service uninstall
#   rm -rf ~/Applications/bricolo.app
set -euo pipefail
cd "$(dirname "$0")/.."

src=target/bundle/bricolo.app
dest="${HOME:?}/Applications/bricolo.app"
node=Contents/Helpers/bricolo.app/Contents/MacOS/bricolo
[ -x "$src/$node" ] || {
    echo "no $src; run just bundle-macos first" >&2
    exit 1
}

mkdir -p "$HOME/Applications"
# A fresh copy rather than writing over the running node's files: macOS
# kills a signed process whose executable changes under it.
rm -rf "$dest"
ditto "$src" "$dest"
echo "copied $src to $dest"
"$dest/$node" service install --app "$dest" "$@"
echo "bricolo is $dest/$node; to call it as bricolo, link it into a folder on PATH:"
echo "  ln -sf \"$dest/$node\" ~/.local/bin/bricolo"
