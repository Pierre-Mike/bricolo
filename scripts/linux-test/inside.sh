#!/usr/bin/env bash
# Runs inside the container that scripts/linux-test.sh starts, and in CI.
#
#   inside.sh            clippy and the suite with no display, then the X11
#                        tests and the smoke test on a desktop
#   inside.sh desktop    the X11 tests and the smoke test
#   inside.sh x11        only the X11 tests
#   inside.sh smoke      only the smoke test
#   inside.sh shell      a shell on the desktop, to poke at things
#   inside.sh demo       the computer-use demo's Linux side (see
#                        scripts/demo-computer-use-linux.sh)
#
# The desktop is Xvfb at 1280x800, openbox (an EWMH window manager), dunst
# (a notification server) and gnome-keyring (a Secret Service, with a
# throwaway keyring unlocked by a throwaway password), inside
# `dbus-run-session`. Needs those, xterm, xdotool, xclip, xdpyinfo, xprop
# and secret-tool (see the Dockerfile).
set -euo pipefail
cd "$(dirname "$0")/../.."
# No core files in the checkout if a desktop program crashes on the way
# out. Set before dbus-run-session, so services D-Bus starts inherit it.
ulimit -c 0

# Every plugin and fixture, as in the justfile.
wasm_packages() {
    for d in plugins/*/ fixtures/*/; do
        [ -f "$d/Cargo.toml" ] && printf -- '-p %s ' "$(basename "$d")"
    done
}

# bricolo-ui is left out: Tauri needs GTK and WebKitGTK, which this image does
# not have. The Ubuntu `ci` job builds and tests it (see docs/ui.md).
build() {
    echo "== build: plugins (wasm32-wasip2) and bricolo"
    # shellcheck disable=SC2046
    cargo build -q --target wasm32-wasip2 --release $(wasm_packages)
    cargo build -q -p bricolo-node
}

headless() {
    echo "== no display: clippy and the whole suite"
    env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
        cargo clippy -q --workspace --exclude bricolo-ui --all-targets -- -D warnings
    env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
        cargo nextest run --workspace --exclude bricolo-ui
}

# Starts Xvfb, openbox and dunst, and waits until each is up. Needs a
# D-Bus session already (see `on_desktop`).
desktop() {
    trap stop_desktop EXIT
    export DISPLAY=:99
    Xvfb "$DISPLAY" -screen 0 1280x800x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
    xvfb=$!
    for _ in $(seq 100); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done
    xdpyinfo >/dev/null || { cat /tmp/xvfb.log; exit 1; }
    openbox >/tmp/openbox.log 2>&1 &
    openbox=$!
    for _ in $(seq 100); do
        xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -q 'window id' && break
        sleep 0.1
    done
    dunst >/tmp/dunst.log 2>&1 &
    dunst=$!
    # Wait for dunst to own the name. NameHasOwner, unlike a call to the
    # name, does not make D-Bus start a second dunst.
    for _ in $(seq 100); do
        dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
            org.freedesktop.DBus.NameHasOwner string:org.freedesktop.Notifications 2>/dev/null |
            grep -q true && break
        sleep 0.1
    done
    keyring
    echo "== desktop: Xvfb $DISPLAY, openbox, $(dunst -v 2>&1 | head -1), $(gnome-keyring-daemon --version | head -1)"
}

# Starts gnome-keyring's Secret Service on the session bus. Its keyrings
# live in a temp folder, never the user's: a throwaway password on stdin
# creates the "login" keyring there, unlocked and the `default` alias. (An
# empty stdin creates nothing, and leaves no default keyring.)
keyring() {
    keyring_dir="$(mktemp -d)"
    printf 'bricolo-test' | XDG_DATA_HOME="$keyring_dir" XDG_RUNTIME_DIR="$keyring_dir" \
        gnome-keyring-daemon --foreground --unlock --components=secrets >/tmp/keyring.log 2>&1 &
    keyring_pid=$!
    for _ in $(seq 100); do
        dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
            org.freedesktop.DBus.NameHasOwner string:org.freedesktop.secrets 2>/dev/null |
            grep -q true && break
        sleep 0.1
    done
    dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
        org.freedesktop.DBus.NameHasOwner string:org.freedesktop.secrets 2>/dev/null |
        grep -q true || { cat /tmp/keyring.log; exit 1; }
    dbus-send --session --print-reply --dest=org.freedesktop.secrets /org/freedesktop/secrets \
        org.freedesktop.Secret.Service.ReadAlias string:default 2>/dev/null |
        grep -q 'collection/login' || { echo "keyring: no default collection"; cat /tmp/keyring.log; exit 1; }
}

# Stops the desktop in order: the keyring, dunst and openbox before the X
# server.
stop_desktop() {
    for pid in "${keyring_pid:-}" "${dunst:-}" "${openbox:-}" "${xvfb:-}"; do
        [ -n "$pid" ] || continue
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    if [ -n "${keyring_dir:-}" ]; then rm -rf "$keyring_dir"; fi
}

x11_tests() {
    echo "== X11: the desktop tests, one at a time"
    cargo nextest run --workspace --exclude bricolo-ui --run-ignored only --test-threads 1 --no-fail-fast \
        -E 'test(/^x11_/) or test(/::x11_/) or test(=clipboard_round_trip)'
}

smoke() {
    echo "== smoke: bricolo run on the desktop"
    bash scripts/linux-test/smoke.sh
}

# The Linux side of the computer-use demo. A node with its HTTP gateway on
# port 8790 and a GTK dialog (zenity) on the desktop; the agent runs on the
# host. Its home and the dialog's answer go to /demo-out, which the host
# mounts. Writes /demo-out/ready when both are up and /demo-out/done when
# the dialog closes.
demo() {
    out=/demo-out
    export BRICOLO_CONFIG_DIR="$out/config" BRICOLO_DATA_DIR="$out/data"
    mkdir -p "$BRICOLO_CONFIG_DIR"
    printf '[mesh]\nenabled = false\n\n[mcp]\nlisten = "0.0.0.0:8790"\n' >"$BRICOLO_CONFIG_DIR/node.toml"
    ./target/debug/bricolo run >"$out/node.log" 2>&1 &
    node=$!
    for _ in $(seq 100); do ./target/debug/bricolo status >/dev/null 2>&1 && break; sleep 0.1; done
    ./target/debug/bricolo mcp token >/dev/null
    zenity --entry --title="bricolo demo" --text="What should the node write?" >"$out/answer.txt" 2>"$out/zenity.err" &
    dialog=$!
    # GTK registers on the accessibility bus once its window maps.
    sleep 3
    echo "== demo: node $node, dialog $dialog, gateway on :8790"
    touch "$out/ready"
    for _ in $(seq 1200); do kill -0 "$dialog" 2>/dev/null || break; sleep 0.5; done
    kill "$dialog" 2>/dev/null || true
    touch "$out/done"
    sleep 2
    kill "$node" 2>/dev/null || true
}

# Runs this script again with `$1` inside a fresh D-Bus session.
on_desktop() {
    dbus-run-session -- bash "${BASH_SOURCE[0]}" "desktop-$1"
}

case "${1:-all}" in
    all)
        build
        headless
        on_desktop all
        ;;
    desktop)
        build
        on_desktop all
        ;;
    x11)
        build
        on_desktop x11
        ;;
    smoke)
        build
        on_desktop smoke
        ;;
    shell)
        build
        on_desktop shell
        ;;
    demo)
        cargo build -q -p bricolo-node
        on_desktop demo
        ;;
    desktop-demo)
        desktop
        demo
        ;;
    desktop-all)
        desktop
        x11_tests
        smoke
        ;;
    desktop-x11)
        desktop
        x11_tests
        ;;
    desktop-smoke)
        desktop
        smoke
        ;;
    desktop-shell)
        desktop
        exec bash
        ;;
    *)
        echo "usage: $0 [all|desktop|x11|smoke|shell]" >&2
        exit 2
        ;;
esac
