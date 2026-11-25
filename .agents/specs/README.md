# Capability specs for the macOS-first batch

Each `NN-<track>.md` file is one job for one agent, in its own git worktree. The spec decides what "done" means. The tests in it are the contract.

## Rules for every track

1. **The tests are fixed.** Copy each test from the spec into the file it names, as written. You may fix what stops a test from compiling: imports, a missing `use`, a field you add to a test harness such as `Node` in `crates/lua/tests/scripts.rs`, a fake's plumbing. You may not change a test's name, its Lua source, or any expected value in an `assert`. If you believe a test is wrong, stop and say why in your final report instead of changing it.
2. **Follow the new-capability recipe in AGENTS.md, "Conventions".** A new capability touches `wit/<name>.wit` and `wit/world.wit`, a trait in `crates/providers` with an `unavailable` stub, `CAPABILITIES` in `crates/providers/src/registry.rs`, the `Providers` struct and `Providers::local`, the host bindings in `crates/runtime/src/host.rs`, the WIT list in `crates/node/src/scaffold.rs`, and `CAPABILITIES` in `crates/lua/src/stdlib.rs`. Anything kept per plugin is dropped in `Providers::release`. None of these capabilities are routable: do not add them to `ROUTABLE` or the mesh.
3. **Copy the WIT exactly** as the spec writes it, doc comments included.
4. **macOS first.** The macOS code goes under `crates/providers/src/macos/`, behind `#[cfg(target_os = "macos")]`. Linux gets a real implementation only where the spec asks for one; otherwise it uses the stub and answers `unavailable`. The workspace must still pass `just check-linux`.
5. **Desktop capabilities need the main-thread loop.** A macOS provider that talks to the window server, Accessibility, the Keychain or the camera is created only when `Providers::local` gets a `Desktop` (as `window` and `hotkey` are). Without one it is the stub. Tests never have a `Desktop`, so tests never move the real mouse, take a real screenshot, touch the real Keychain or open the camera.
6. **Lua conventions.** A call that fails returns `nil, "<kind>: <message>"` (for example `nil, "failed: no such window"`), as the existing `bricolo.*` tables do. A wrong argument type raises a Lua error. Snake case names: `set_minimized`, `move_to`, `idle_seconds`.
7. **Docs.** Add each new capability to the table in `docs/apps.md` and to `docs/scripts.md` (the `bricolo.<name>` table), in the style of the rows already there. Run `just skill-refs` after the WIT and docs/scripts.md are final.
8. **Done means all of these pass, from the worktree root:**
   - `just lint`
   - `just test` (runs `just plugins` first, so the `caps` fixture is rebuilt)
   - `just check-linux`
   - `just deny` when you added a dependency
9. **Commit your work** on your branch when everything passes, with one message that names the capability. Do not push. Do not touch other worktrees or the main checkout.
10. **Report:** what you built, the files you changed, the output summary of each command above, and anything you left out or doubt.

## Shared facts

- `cargo` must come from rustup; always run through `just`, which sets `PATH`.
- Script callers are named after their file: a script `watcher.lua` calls providers as `Caller { plugin: "watcher.lua", .. }`.
- The `caps` fixture (`fixtures/caps/src/lib.rs`) runs a command when it gets `hotkey(<command>)` and logs `<name>: <result>`. Runtime tests fire commands with `rt.fire("caps", ProviderEvent::Hotkey(cmd.into()))` and read the line with `line(&rt, "caps", "<prefix>")`. Add new commands there, and add the capability to `imports` in both `fixtures/caps/plugin.toml` and `caps_manifest` in `crates/runtime/tests/plugins.rs`.
- In runtime tests on macOS, a desktop call may answer `Unavailable`, `Denied` or `Failed`, depending on the terminal's permissions. The runtime tests in these specs only check that the call reached the host and came back as an error. They never check which error.
