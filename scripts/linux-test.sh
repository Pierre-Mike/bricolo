#!/usr/bin/env bash
# Runs the whole test suite on Linux, in a Docker container: `just linux-test`.
#
# 1. Builds the image in scripts/linux-test/ (toolchain, nextest, Xvfb,
#    openbox, dunst, xdotool, xterm).
# 2. Mounts this checkout at /src. The Linux build goes to a named volume
#    mounted over /src/target, so it never mixes with the host's target/.
#    $BRICOLO_LINUX_TARGET names it (default bricolo-linux-target). Give each git
#    worktree its own: cargo compares file times, so a build from another
#    worktree's newer sources counts as fresh here.
#    The cargo registry is another named volume, so rebuilds are quick.
# 3. Runs scripts/linux-test/inside.sh: the suite with no display, the X11
#    tests under Xvfb, then a smoke test of `bricolo run`.
#
# Arguments go to inside.sh: `scripts/linux-test.sh x11` runs only the X11
# tests, `smoke` only the smoke test, `shell` opens a shell instead.
# CARGO_BUILD_JOBS passes through: `CARGO_BUILD_JOBS=2 just linux-test`
# when the linker runs out of memory in a small Docker VM. So does
# CARGO_INCREMENTAL: `CARGO_INCREMENTAL=0` keeps the incremental cache,
# 10 GB and more, out of the build volume, as CI does.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
image="${BRICOLO_LINUX_IMAGE:-bricolo-linux-test}"

# The image only needs the Dockerfile and the toolchain file; a small build
# context keeps target/ out of it.
context="$(mktemp -d)"
trap 'rm -rf "$context"' EXIT
cp "$root/scripts/linux-test/Dockerfile" "$root/rust-toolchain.toml" "$context/"
docker build -q -t "$image" "$context" >/dev/null

tty=()
if [ -t 0 ] && [ -t 1 ]; then tty=(-it); fi
exec docker run --rm ${tty[@]+"${tty[@]}"} \
    -v "$root:/src" \
    -v "${BRICOLO_LINUX_TARGET:-bricolo-linux-target}:/src/target" \
    -v bricolo-linux-registry:/usr/local/cargo/registry \
    -e CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-auto}" \
    ${CARGO_BUILD_JOBS:+-e CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS"} \
    ${CARGO_INCREMENTAL:+-e CARGO_INCREMENTAL="$CARGO_INCREMENTAL"} \
    "$image" bash /src/scripts/linux-test/inside.sh "$@"
