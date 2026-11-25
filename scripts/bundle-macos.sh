#!/usr/bin/env bash
# Builds target/bundle/bricolo.app: `just bundle-macos`.
#
# 1. Builds `bricolo` and, through `cargo tauri build`, `bricolo-ui` and the .app
#    (identifier com.pierre-mike.bricolo, Info.plist merged from
#    crates/ui/Info.plist: LSUIElement and the usage strings).
# 2. Wraps `bricolo` in a helper app, Contents/Helpers/bricolo.app (bundle id
#    com.pierre-mike.bricolo.node, Info.plist from
#    crates/node/Info.plist), where the hub's Start node and `bricolo service
#    install --app` look for it. In Contents/MacOS next to `bricolo-ui`, the
#    running node counted as the app for LaunchServices, and opening the
#    app brought the node forward instead of starting the hub.
# 3. Signs the helper, then the app, with one identity and no hardened
#    runtime: $BRICOLO_SIGN_IDENTITY if set ("-" means ad-hoc), else the local
#    "bricolo dev" identity (scripts/macos-dev-identity.sh), else
#    ad-hoc. The permissions macOS grants the node stay with the helper's
#    signature, which a rebuild with the same identity keeps.
#
# Then prints `codesign -dv` and `spctl -a -vv`. Gatekeeper rejects a build
# signed with a local identity or ad-hoc; it runs, since it was built here
# and carries no quarantine flag.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

[ "$(uname)" = Darwin ] || {
    echo "macOS only; on Linux use scripts/bundle-linux.sh" >&2
    exit 1
}

app=target/bundle/bricolo.app
helper="$app/Contents/Helpers/bricolo.app"

echo "== build: bricolo (release)"
cargo build --release -p bricolo-node
echo "== build: bricolo-ui and the app (cargo tauri build)"
# Signed below, once bricolo is inside.
(cd crates/ui && cargo tauri build --bundles app --no-sign --ci)

rm -rf "$app"
mkdir -p target/bundle
ditto target/release/bundle/macos/bricolo.app "$app"
mkdir -p "$helper/Contents/MacOS" "$helper/Contents/Resources"
cp target/release/bricolo "$helper/Contents/MacOS/bricolo"
cp crates/node/Info.plist "$helper/Contents/Info.plist"
cp crates/ui/icons/icon.icns "$helper/Contents/Resources/icon.icns"
version="$(target/release/bricolo --version | awk '{print $2}')"
for key in CFBundleShortVersionString CFBundleVersion; do
    /usr/libexec/PlistBuddy -c "Add :$key string $version" "$helper/Contents/Info.plist"
done

identity="${BRICOLO_SIGN_IDENTITY:-}"
if [ -z "$identity" ]; then
    identity="$(scripts/macos-dev-identity.sh --hash)"
    if [ -z "$identity" ]; then
        identity=-
        echo "warning: no \"bricolo dev\" identity; signing ad-hoc." >&2
        echo "  macOS will ask for its permissions again after each rebuild." >&2
        echo "  scripts/macos-dev-identity.sh creates the identity." >&2
    fi
fi
echo "== sign with ${identity/#-/ad-hoc}"
# Inside out: the helper first, then the app.
codesign --force --sign "$identity" --timestamp=none "$helper"
codesign --force --sign "$identity" --timestamp=none "$app"
codesign --verify --strict --verbose=2 "$app"

echo "== codesign -dv $app"
codesign -dv --verbose=2 "$app" 2>&1
echo "== codesign -dv $helper"
codesign -dv --verbose=2 "$helper" 2>&1
echo "== designated requirements"
codesign -d -r- "$app" 2>&1 | grep designated
codesign -d -r- "$helper" 2>&1 | grep designated
echo "== spctl -a -vv $app"
/usr/sbin/spctl -a -vv "$app" 2>&1 || echo "(Gatekeeper rejects local and ad-hoc signatures; expected for a dev build)"
echo "built $app"
