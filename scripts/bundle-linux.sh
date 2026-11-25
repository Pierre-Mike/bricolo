#!/usr/bin/env bash
# Builds dist/bricolo-<version>-linux-<arch>.tar.gz: `just
# bundle-linux`.
#
# On macOS it runs itself in the `just linux-test` image (Debian bookworm,
# so the binaries need glibc 2.36 or later), with the same volumes: the
# Linux build stays in the bricolo-linux-target volume ($BRICOLO_LINUX_TARGET, as
# for linux-test.sh) and only the tarball
# lands in dist/. On Linux it builds directly.
#
# The tarball holds `bricolo`, `install.sh` and a README. It holds `bricolo-ui` too
# when target/release/bricolo-ui exists: the test image has no GTK or
# WebKitGTK, so only a machine with Tauri's dependencies builds it (the
# release workflow's Ubuntu job, or `just ui` on a Linux desktop).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if [ "$(uname)" != Linux ]; then
    image="${BRICOLO_LINUX_IMAGE:-bricolo-linux-test}"
    context="$(mktemp -d)"
    trap 'rm -rf "$context"' EXIT
    cp scripts/linux-test/Dockerfile rust-toolchain.toml "$context/"
    docker build -q -t "$image" "$context" >/dev/null
    exec docker run --rm \
        -v "$root:/src" \
        -v "${BRICOLO_LINUX_TARGET:-bricolo-linux-target}:/src/target" \
        -v bricolo-linux-registry:/usr/local/cargo/registry \
        -e CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-auto}" \
        "$image" bash /src/scripts/bundle-linux.sh "$@"
fi

echo "== build: bricolo (release)"
cargo build --release -p bricolo-node
version="$(target/release/bricolo --version | awk '{print $2}')"
name="bricolo-$version-linux-$(uname -m)"
stage="target/bundle/$name"
rm -rf "$stage"
mkdir -p "$stage" dist
cp target/release/bricolo "$stage/"
if [ -x target/release/bricolo-ui ]; then
    cp target/release/bricolo-ui "$stage/"
    echo "including bricolo-ui"
else
    echo "no target/release/bricolo-ui: the tarball has bricolo only"
fi

cat >"$stage/install.sh" <<'EOF'
#!/bin/sh
# Copies bricolo (and bricolo-ui, when this tarball has it) to ~/.local/bin, or to
# $PREFIX/bin, then runs `bricolo service install`, which starts the node now
# and at each login as a systemd user service.
#
#   ./install.sh                 install and start the service
#   ./install.sh --no-service    only copy the binaries
#   ./install.sh <args>          pass <args> to `bricolo service install`,
#                                like --session headless
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
bin="${PREFIX:-$HOME/.local}/bin"
mkdir -p "$bin"
install -m 0755 "$here/bricolo" "$bin/bricolo"
echo "installed $bin/bricolo"
ui=""
if [ -f "$here/bricolo-ui" ]; then
    install -m 0755 "$here/bricolo-ui" "$bin/bricolo-ui"
    echo "installed $bin/bricolo-ui"
    ui="--ui"
fi
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "note: $bin is not on PATH; add it in your shell's profile" ;;
esac
if [ "${1:-}" = --no-service ]; then
    exit 0
fi
"$bin/bricolo" service install $ui "$@"
EOF
chmod 0755 "$stage/install.sh"

cat >"$stage/README.txt" <<EOF
bricolo $version for Linux ($(uname -m))

  ./install.sh       copies bricolo to ~/.local/bin and starts the node as a
                     systemd user service (bricolo service install)
  bricolo status          the running node's state
  bricolo service status  the service's state
  bricolo service uninstall && rm ~/.local/bin/bricolo ~/.local/bin/bricolo-ui
                     removes it again; your config in ~/.config/bricolo
                     and data in ~/.local/share/bricolo stay

Built on Debian bookworm: needs glibc 2.36 or later. docs/install.md in the
repository covers X11, Wayland and headless machines.
EOF

tar -czf "dist/$name.tar.gz" -C target/bundle "$name"
echo "== dist/$name.tar.gz"
tar -tzvf "dist/$name.tar.gz"
