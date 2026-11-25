# Contributing

Thanks for wanting to help. This page covers setup, the checks a change must pass, and the conventions the code follows. [AGENTS.md](AGENTS.md) has the full command table and crate map; coding agents read it too.

## Set up

1. Install [rustup](https://rustup.rs). The pinned toolchain in `rust-toolchain.toml` (Rust 1.99.1, with clippy, rustfmt and `wasm32-wasip2`) installs on first build.
2. Install the tools: `just`, `cargo-nextest`, `cargo-deny` and `lefthook`.

   ```bash
   cargo install just cargo-nextest cargo-deny lefthook
   ```

3. Turn on the pre-commit hook (fmt and clippy):

   ```bash
   lefthook install
   ```

4. Check that everything passes:

   ```bash
   just ci
   ```

On Linux, the hub (`bricolo-ui`) needs the GTK and WebKitGTK packages listed in `.github/workflows/ci.yml`.

## Before you open a pull request

- `just ci` passes: format, clippy with `-D warnings`, the whole test suite, and `cargo deny`.
- If you touched OS-specific code, `just check-linux` and `just check-windows` still compile.
- If you changed a template, `just smoke-templates` passes.
- If you changed `wit/` or `docs/scripts.md`, run `just skill-refs`.
- Docs describe what the code does now. Update the page your change affects, and the "Implementation notes" in [docs/design.md](docs/design.md) for behaviour changes.

## Conventions worth knowing

- OS-specific code lives only under `crates/providers/src/<os>/`, behind `#[cfg(target_os = "...")]`, with an `unavailable` stub elsewhere.
- Library crates return `thiserror` errors; the `bricolo` binary uses `anyhow`.
- Tests never touch the real home folder, the network, the microphone, or macOS permissions. Point `BRICOLO_CONFIG_DIR` and `BRICOLO_DATA_DIR` at a temp folder, and use fakes (a fake GitHub API, fake ffmpeg, fake providers) instead.
- A new capability touches several places in a fixed order. Follow "Conventions" in [AGENTS.md](AGENTS.md).
- The hub's front end is plain HTML, CSS and ES modules with no build step. Set text with `textContent`, never `innerHTML`.

## Ways to help

- **Try it on your machine** and report what breaks, especially on Windows and Wayland desktops. `examples/apps/live-check/live_check.lua` exercises every capability and logs `FAIL` lines.
- **Build an app** and share it as a GitHub release (`bricolo pack`). Good apps may become templates.
- **Port a capability** to an OS that answers `unavailable` today. [docs/linux.md](docs/linux.md) and [docs/windows.md](docs/windows.md) list the gaps.
- **Pick an idea** from [docs/vision.md](docs/vision.md) and open an issue to discuss the design first.

## Commit messages

Write the subject as what the change does, in the present tense, prefixed with the area when it helps: `Mesh: retry a dropped peer after a backoff`. Keep one logical change per commit.

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
