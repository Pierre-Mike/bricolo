# The hub in a browser

The hub's front end (`crates/ui/ui/`) needs `window.__TAURI__`, which only bricolo-ui provides. These pages provide a fake one, so the real HTML, CSS and JavaScript run in a plain browser for design work and screenshots. Nothing here talks to a node.

```sh
just hub-dev        # python3 crates/ui/dev/serve.py 8770
```

Then open http://localhost:8770/dev/hub.html, or a panel at http://localhost:8770/dev/panel.html?kind=review. `serve.py` turns caching off (`Cache-Control: no-store`), so an edit under `crates/ui/ui/` shows on the next load. It takes the port as its argument and defaults to 8765, but the recipe passes 8770 because the parakeet sidecar holds 8765 on a machine that runs it.

| File | What it does |
|---|---|
| `fixtures.js` | The data: 8 plugins and 2 scripts from `apps/` and `examples/apps`, their tiles, widgets, settings and hotkeys, 3 sidecars, 2 mesh peers, the macOS grants and the capability catalog, and what each source writes to the log |
| `fake-tauri.js` | `window.__TAURI__`: the commands bricolo-ui registers (`snapshot`, `request`, `start_node`, `hide_hub`, `panel_current`, `dismiss_panel`), the node's answers to every request the hub sends, and the `node`, `status`, `ui`, `logs`, `log` and `panel` events |
| `serve.py` | A standard-library server for `crates/ui` that sends `Cache-Control: no-store` |
| `hub.html`, `panel.html` | Copies of `ui/index.html` and `ui/panel.html` that load the two scripts first and the real CSS and modules through `../ui/`. Keep their bodies the same as the originals |

## What you see

Home lists four problems: noseguard is off because its sidecar does not answer, live_check.lua runs an old copy after a refused reload, the noseguard sidecar is unhealthy, and Input Monitoring is missing. tailscale-funnel is turned off, and dictation's chord was moved in the hub.

The fake acts like a node. A new log line arrives every second or two, the world clock and battery move, and switches and buttons change the tiles. Turn off, Turn on, Reload, Restart, changing a shortcut and Save and reload change what the hub shows. A few examples:

- Restart the noseguard sidecar, then reload the noseguard plugin: it starts.
- Run dictation's first shortcut in its Shortcuts tab: the Recording badge shows. Run it again to stop.
- Ask on Camera grants it at once. Ask on Input Monitoring opens "System Settings", and 3 seconds later it counts as granted.
- A shortcut another app already uses is refused, and so are `cmd+space` and `cmd+tab`.

`window.__rhFake` in the console exposes the state and `log(source, level, message)`.

## Query parameters

| Parameter | Effect |
|---|---|
| `?state=down` | No node answers. Start node brings it up after a second |
| `?state=empty` | A node with no plugins, scripts, sidecars or mesh |
| `?view=<name>` | Opens on `home`, `apps`, `sidecars`, `library`, `mesh` or `logs`, like `bricolo-ui --view` |
| `?recording=1` | dictation holds the microphone, so the Recording badge shows |
| `?quiet=1` | No new log lines and no moving widgets, for screenshots that stay the same |
| `?kind=<kind>` | panel.html only: `caption`, `toast`, `choice` (or `choice-list`) or `review`, at the size bricolo-ui gives that window |

The page follows the system's light or dark setting. Pins and hidden widgets stay in the browser's local storage, as they do in bricolo-ui.

When the node's answers or bricolo-ui's commands change shape, update `fake-tauri.js` and `fixtures.js` to match. The comments at the top of `fixtures.js` list the Rust types they copy.

## Scenes and screenshots

`scenes.js` puts the hub in a state from the URL, so a screenshot needs no clicks: `open=plugin:voice-agent` (or `open=script:live_check`, without `.lua`) opens that app's drawer, `tab=settings` picks the tab, `edit=1` makes one unsaved settings change, and `palette=call` opens the palette with that text typed. `just hub-shots` (`shots.sh`) renders `docs/screenshots/hub-{home,apps,settings,logs}.png` with headless Chrome at 1040×700 in dark mode. Set `CHROME` to use another Chrome binary.
