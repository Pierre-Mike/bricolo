# Domain

bricolo runs personal desktop automations on a node. People build them as scripts and plugins that call one standard library, plus sidecars for work the node can't run itself.

### Runtime

**Node**:
One running `bricolo run` process on a device, which loads scripts and plugins and supervises sidecars.

**Capability**:
One interface of the `bricolo:stdlib` standard library, such as `hotkey`, `clipboard` or `ui`, that scripts and plugins call.

**App**:
One or more scripts or plugins, plus the sidecars they call, that together do one job for the user.

### Parts of an app

**Script**:
One trusted Lua 5.4 file in `scripts/` that the node runs with every capability and reloads on save.
_Avoid_: lua plugin, lua plugins

**Plugin**:
A sandboxed WebAssembly component built from a Rust crate, which can call only the capabilities its `plugin.toml` declares and the user approved at install.
_Avoid_: wasm script, wasm scripts

**Sidecar**:
A local HTTP server in any language, listed under `[sidecars.<name>]` in `node.toml`, that the node starts, health-checks, restarts and stops. Scripts and plugins call it through `http`; it never calls the standard library.
_Avoid_: model server, model servers

### What the user sees

**Hub**:
The `bricolo-ui` window that shows Home, the apps, sidecars, the library, mesh peers and logs.

**Home**:
The hub's first view: what needs attention, the switches the user pinned, and every widget.

**Controls**:
The status line, switches and buttons a script or plugin shows inside its card through the `ui` capability. The WIT and the code call them a `tile` (`set-tile`).

**Widget**:
A display-only view of a value worth glancing at, such as the time in other cities or the battery level, shown in the hub's Widgets view. It has no switches or buttons, and most apps have none.

**Card**:
The hub's box for one plugin or script in the Apps view: its state, one line of status, its shortcuts and its main switch. A click opens its drawer.
_Avoid_: plugin tile, plugin tiles, script tile, script tiles

**Drawer**:
The part of the hub that slides in over the Apps view for one plugin or script, with the tabs Now, Controls, Settings, Shortcuts and Health.

**Setting**:
One key of a plugin's `[settings]` in `plugin.toml`, passed to `init` and overridden per node in `[plugins.<name>]` of `node.toml`, through the drawer's Settings tab or by hand. A setting changes how an app behaves; controls act now.

**Panel**:
A floating window a script or plugin shows through the `ui` capability: a caption, toast, choice list or review.
_Avoid_: popup, popups
