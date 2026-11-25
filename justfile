# Task runner. `just ci` runs what GitHub Actions runs.

set shell := ["bash", "-euo", "pipefail", "-c"]

# rustup's cargo first: it honours rust-toolchain.toml, Homebrew's does not.
export PATH := env("HOME") + "/.cargo/bin:" + env("PATH")

# Every plugin and test fixture: the folder names under plugins/ and fixtures/.
wasm_packages := `for d in plugins/*/ fixtures/*/; do [ -f "$d/Cargo.toml" ] && printf -- '-p %s ' "$(basename "$d")"; done`

# List the recipes.
default:
    @just --list

# Build the node and the bricolo binary (debug).
build:
    cargo build

# Build every plugin and fixture for wasm32-wasip2 (release).
plugins:
    cargo build --target wasm32-wasip2 --release {{wasm_packages}}

# Run every test with nextest. The runtime tests load the built plugins.
test: plugins
    cargo nextest run --workspace

# Formatting and clippy, as the pre-commit hook runs them.
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Licenses (no GPL) and advisories.
deny:
    cargo deny check

# Everything CI checks.
ci: lint test deny

# Check that the workspace compiles for Linux. Needs the rustup target
# x86_64-unknown-linux-gnu, and on macOS zig as the C compiler. Skips bricolo-ui:
# Tauri needs GTK and WebKitGTK through pkg-config, which a cross-check from
# macOS does not have. CI builds bricolo-ui natively on Ubuntu instead.
check-linux:
    if [ "$(uname)" = Darwin ]; then export CC_x86_64_unknown_linux_gnu="$PWD/scripts/linux-cc" AR_x86_64_unknown_linux_gnu="$PWD/scripts/linux-ar"; fi; \
        cargo check --workspace --exclude bricolo-ui --target x86_64-unknown-linux-gnu

# Check that the workspace compiles, and that clippy passes, for Windows.
# Needs the rustup target x86_64-pc-windows-gnu and, off Windows, zig as
# the C compiler. Skips bricolo-ui, whose WebView2 build needs Windows. CI
# builds and tests everything natively on windows-latest.
check-windows:
    case "$(uname)" in MINGW*|MSYS*) ;; *) export CC_x86_64_pc_windows_gnu="$PWD/scripts/windows-cc" AR_x86_64_pc_windows_gnu="$PWD/scripts/windows-ar" ;; esac; \
        cargo clippy --workspace --exclude bricolo-ui --all-targets --target x86_64-pc-windows-gnu -- -D warnings

# Build the hub, bricolo-ui, and bricolo next to it (release). Run `bricolo run`, then
# `target/release/bricolo-ui`; or start bricolo-ui alone and press Start node.
ui:
    cargo build --release -p bricolo-ui -p bricolo-node

# Fixtures and a fake Tauri stand in for bricolo-ui and the node (crates/ui/dev).
# Serve the hub in a browser: http://localhost:8770/dev/hub.html.
hub-dev:
    python3 crates/ui/dev/serve.py 8770

# Preview the project site (site/) as GitHub Pages serves it, with the
# screenshots from docs/screenshots/: http://localhost:8080.
site:
    rm -rf target/site && mkdir -p target/site/screenshots
    cp site/index.html site/styles.css site/app.js target/site/
    cp docs/screenshots/*.png target/site/screenshots/
    python3 -m http.server 8080 --directory target/site

# Render the hub's screenshots in docs/screenshots/ from the dev harness,
# with headless Chrome (no node, nothing on screen).
hub-shots:
    crates/ui/dev/shots.sh

# The bundle is signed with the local "bricolo dev" identity, else
# ad-hoc. Run scripts/macos-dev-identity.sh once to create the identity.
# Build target/bundle/bricolo.app: bricolo-ui with bricolo inside, signed.
bundle-macos:
    scripts/bundle-macos.sh

# The node starts now and at each login. Extra args go to `bricolo service
# install`, like --ui.
# Copy the bundle to ~/Applications and run `bricolo service install --app` on it.
install-macos *args: bundle-macos
    scripts/install-macos.sh {{args}}

# The tarball has bricolo and install.sh, and bricolo-ui only where Tauri's GTK
# dependencies are. On macOS it builds in the linux-test container.
# Build dist/bricolo-<version>-linux-<arch>.tar.gz.
bundle-linux:
    scripts/bundle-linux.sh

# Clippy and the tests with no display, then the X11 tests under Xvfb with
# openbox and dunst, then a smoke test of `bricolo run`. `just linux-test x11`
# or `smoke` runs one part; `shell` opens a shell on that desktop.
# Run the whole suite on Linux in Docker (scripts/linux-test.sh).
linux-test *args:
    scripts/linux-test.sh {{args}}

# The computer-use demo: Claude Code, with only the MCP gateway's tools,
# edits a document in TextEdit through the accessibility tree; the script
# checks the file on disk. macOS, unlocked; costs one agent run.
demo-computer-use:
    scripts/demo-computer-use.sh

# The same demo on Linux in Docker: the node and a GTK dialog on Xvfb,
# the agent on the host through the HTTP gateway. Costs one agent run.
demo-computer-use-linux:
    scripts/demo-computer-use-linux.sh

# Rebuild and reinstall plugins/<plugin> on every save; a running node reloads it.
dev plugin:
    cargo run -q -p bricolo-node -- dev plugins/{{plugin}}

# Build plugins/<plugin> and install it, asking to approve its capabilities.
install plugin:
    cargo build -p {{plugin}} --target wasm32-wasip2 --release
    cargo run -q -p bricolo-node -- install plugins/{{plugin}}

# Generate every `bricolo new` and `bricolo new-script` template outside the repo,
# test, build, check and install it into a temp home, and run them all.
smoke-templates:
    scripts/smoke-templates.sh

# Rewrite skills/create-bricolo-app/references/capabilities.md from wit/ and
# docs/scripts.md. A test fails while it is out of date.
skill-refs:
    BRICOLO_WRITE_SKILL_REFS=1 cargo nextest run -p bricolo-node --test skill capability_cheat_sheet_is_current

# For an installed bricolo without the repository, `bricolo skill install` copies
# the skill compiled into it instead.
# Link skills/create-bricolo-app into ~/.agents/skills and ~/.claude/skills, where coding agents find it.
install-skill:
    cargo run -q -p bricolo-node -- skill install --agent all --link --source skills/create-bricolo-app

# `just eval-create-app --agent gemini "<request>"` picks the agent:
# claude (default), gemini or codex. Costs one agent run.
# Run a headless coding agent with the skill on a one-line request, in a temp Bricolo home, and check that what it built runs.
eval-create-app *request:
    scripts/eval-create-app.sh {{request}}
