# apps

The source of every app this node runs. Each app has one folder, which holds every part of it. `~/.config/bricolo` keeps only what `bricolo install` and `bricolo sidecar add` write there. [Domain.md](../Domain.md) defines the terms.

| Part | Source here | What the node loads | How it gets there |
|---|---|---|---|
| Plugin | `<app>/plugin/`, a standalone crate with its own `[workspace]` | `~/.config/bricolo/plugins/<app>/` | `bricolo install apps/<app>/plugin`, or `bricolo dev` while editing. `plugins.lock` records this folder as the source |
| Script | `<app>/script/<app_name>.lua`, the app's name with `_` for `-` | `~/.config/bricolo/scripts/<app_name>.lua` | `bricolo install apps/<app>/script/<app_name>.lua` |
| Sidecar | `<app>/sidecar/` | The same folder: `node.toml` points at it | `bricolo sidecar add`, with the line in the sidecar's README |
| Widget | The `set_tile` calls in the plugin's `src/lib.rs` or the script | The node's memory, drawn by the hub | The plugin or script sets it when it runs |

An app is a plugin or a script, not both. A sidecar that several apps use, or that `[speech]` or `[tts]` names in `node.toml`, goes in `shared/sidecars/<name>/`.

## What is here

Every app is a plugin.

| App | Sidecar |
|---|---|
| brown-noise | none; plays `~/.config/bricolo/sounds/noise` through `audio` |
| dictation | `shared/sidecars/parakeet`, through `[speech]` |
| dsh | `dsh` (3080), the `/opt/homebrew/bin/dsh` program; no source here |
| karaoke | none |
| keystroke-typer | none |
| lmstudio | none |
| mac-status | none |
| noseguard | `noseguard` (8802) |
| ovis-ocr | `ovis-ocr` (8797) |
| password-typer | `touch-id` (8804) |
| tailscale-funnel | `tailscale-funnel` (8795) |
| tts | `shared/sidecars/pocket-tts`, through `[tts]` |
| voice-agent | `shared/sidecars/pocket-tts` as `pocket-en` (8791), and 8792, which no `node.toml` entry runs |
| wake-word | `wake-word` (8803) |
| world-clock | none |

Shared sidecars: `parakeet` (8765, `[speech]`) and `pocket-tts`, which runs twice, as `pocket-en` (8791, `[tts]`) and `pocket-fr` (8793).

Paths under `/Users/me` and `/path/to/bricolo` are placeholders. Before installing an app, set its paths for your machine in the `[shell]` or `[disk]` allow list of its `plugin.toml`, and in `[settings]` (or `[plugins.<name>]` in `node.toml`).

`just plugins`, `just test` and CI don't build these crates. Build one with `cargo build --target wasm32-wasip2 --release` in its `plugin/` folder.
