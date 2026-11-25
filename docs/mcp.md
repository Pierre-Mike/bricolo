# MCP gateway

The MCP gateway lets an AI agent use the capabilities of every node in the mesh over the Model Context Protocol. `bricolo run` hosts it on every node, so an agent connected to any node sees the whole mesh, and the mesh keeps working when one machine sleeps.

The gateway builds its tool schemas from the standard library WIT in `wit/`. A capability that a node can run from a JSON call becomes a tool with no MCP code of its own.

## Connect an agent

The node must be running (`bricolo run`, or the service from `bricolo service install`).

| Agent | How |
|---|---|
| Claude Code | `bricolo mcp connect claude-code`, or the hub's **Connect to Claude Code** button in Mesh and routes. It runs `claude mcp add --scope user bricolo -- <bricolo> mcp`. |
| Claude Desktop | `bricolo mcp connect claude-desktop`, or the hub's button. It adds `mcpServers.bricolo` to `claude_desktop_config.json` and keeps a `.bak` copy of the old file. |
| Any stdio client | Command `bricolo`, arguments `["mcp"]`. |
| Claude on the web or phone, or any remote client | The HTTP gateway through Tailscale Funnel, added as a custom connector. See below. |

`bricolo mcp` with no subcommand pipes standard input and output to the running node's gateway over the control socket. It exits with code 2 when no node is running.

## Tools

The gateway follows the search pattern, so the tool list stays small however many nodes and capabilities there are:

| Tool | What it does |
|---|---|
| `list_nodes` | Every node with `online`, `local`, its capabilities, and its mesh details |
| `search_capabilities` | Methods on the online nodes that match every word of `query`, each with its node, the JSON Schema of its arguments, and its `read_only` and `destructive` flags. `node` and `capability` narrow it. |
| `call_capability` | Runs `capability.method` with `args` on `node`. `node` defaults to `local`, the node the gateway runs on. |

The gateway also lists one direct tool per method on each online node, such as `laptop_clipboard_get` or `studio_tts_speak`. MCP clients reject dots in tool names, so the name joins node, capability and method with underscores, cut to 64 characters. Each direct tool carries `readOnlyHint` and `destructiveHint` annotations, and an output schema when the method returns a value.

`args` is an object keyed by the WIT parameter names in snake_case. Two conversions make calls easier for an agent:

- A `list<u8>` argument also takes a string, which the gateway sends as its UTF-8 bytes. `disk.write` takes `{"path": "~/Notes/a.md", "data": "text"}`.
- A result that is UTF-8 bytes comes back as text. The structured content keeps the bytes.

## Nodes joining and leaving

Each node names itself with `name` in its `node.toml` (default `local`), and its peers with their names in `peers.toml`. The agent may always write `local` for the node it is connected to.

1. A peer connects. The gateway asks it for `mesh.methods`: the methods that peer lets this node call, after its `peers.toml` allow list.
2. The gateway adds the peer's direct tools and checks its tool list every 2 seconds.
3. When the list changes, it sends `notifications/tools/list_changed`. Connected clients that listen fetch the list again.
4. A peer that disconnects loses its tools the same way. Its method list is fetched fresh when it returns, and cached for 60 seconds while it stays online.

Clients that read the tool list only once still reach every node through `search_capabilities` and `call_capability`, which always see the current mesh.

A gateway sees its own node and its direct peers. A peer's peers stay out of view unless they are paired with this node too.

## What the agent can call

Only the methods in `remote_capabilities!` in `crates/mesh/src/capabilities.rs` run from a JSON call. Today these are `tts`, `speech.transcribe`, `disk`, `clipboard`, `notify`, `claude-code`, and the computer-use capabilities `accessibility`, `process`, `device`, `scripting` and `virtual-screen` ([computer-use.md](computer-use.md)). Adding a capability to that table (see "Adding a capability to the mesh" in docs/mesh.md) makes it a tool too.

Three checks decide whether a call runs:

1. The gateway's policy, `[mcp] allow` and `deny` in node.toml.
2. On this node, the gateway's disk grants, `[mcp] grants`. Without one, every disk call on this node is denied.
3. On a peer, that peer's `peers.toml` entry for this node: its allow list and its disk grants.

Methods that can lose work or cut the agent off carry `destructiveHint`, so a client asks the user before each call: `process.stop` and `kill`, `device.lock`, `sleep` and `set-wifi`, every `scripting.run-*`, `virtual-screen.launch` and `destroy`, `claude-code.start`, `send` and `stop`, and the writes (`disk.write`, `clipboard.set`, `set-image`, `set-files`). Reads such as `accessibility.snapshot`, `find`, `wait`, `process.list` and `device.volume` carry `readOnlyHint`. Clicks and typing carry neither: they change an app, so the agent follows the rules in [computer-use.md](computer-use.md) and asks before anything that sends, buys or deletes.

## Configuration

```toml
[mcp]
enabled = true              # default; false turns the gateway off
listen = "127.0.0.1:8790"   # the HTTP gateway; off when missing
allow = ["*"]               # what the agent may see and call
deny = ["*.disk.write"]
grants = ["disk:~/Notes", "disk:read:~/Documents"]
shell = ["/usr/bin/xterm"]  # programs virtual-screen.launch may start
origins = []                # browser origins allowed over HTTP
```

A policy pattern is `node`, `node.capability` or `node.capability.method`, and any part may be `*`: `studio` is every method on the node named studio, `*.clipboard` is the clipboard on every node, and `laptop.disk.write` is one method. A method is allowed when an `allow` pattern matches and no `deny` pattern does. Changes need a restart of `bricolo run`.

## Remote agents: HTTP and Tailscale Funnel

With `[mcp] listen` set, the node also serves MCP's Streamable HTTP transport:

- `POST /mcp` takes one JSON-RPC message. A request gets its answer as JSON; a notification gets `202`.
- `GET /mcp` with `Accept: text/event-stream` streams the server's notifications.
- `GET /health` answers `ok` and needs no token.

Every `/mcp` request needs the token from `bricolo mcp token`, as `Authorization: Bearer <token>`, or in the path as `/mcp/<token>` for clients that cannot set headers, such as Claude's custom connectors. The node keeps the token in `mcp.token` in its data folder, readable only by the user. `bricolo mcp token --rotate` makes a new one; restart the node after.

The gateway refuses any request whose `Origin` header is not listed in `[mcp] origins`, so a web page in a browser on the same machine cannot drive it. It serves plain HTTP: bind it to `127.0.0.1` and let the tunnel add TLS.

To reach it from Claude on the web or the phone:

1. Set `[mcp] listen = "127.0.0.1:8790"` and restart the node.
2. Turn on HTTPS certificates and Funnel for the tailnet in the Tailscale admin console.
3. Run `bricolo mcp funnel`. It runs `tailscale funnel --bg --https=443 8790` and prints `https://<machine>.<tailnet>.ts.net/mcp/<token>`. If this machine already publishes something on 443, check `tailscale funnel status` and pass `--port 8443` or `--port 10000` (the other ports Funnel allows) so the old config stays.
4. In Claude, add that URL as a custom connector.

Anyone with the URL can use the gateway, so treat it as a password. `bricolo mcp funnel --off` (with the same `--port`) stops publishing it, and `bricolo mcp url` prints every address again. Claude asks before each connector call unless you allow the tool.

## Audit log

The gateway logs every call with the target `audit`: the node, the capability, the method and the outcome. A peer logs each call it serves with the caller's node and `bricolo-mcp` as the plugin. `bricolo logs` shows both:

```
INFO audit: capability call client="mcp" node=studio capability=disk method=read outcome="ok"
```

`bricolo call` lands in the same log as `client=bricolo-call`.

## Code

| Path | Job |
|---|---|
| `crates/mcp/src/catalog.rs` | Parses the embedded WIT with `wit-parser` and builds each method's schemas; turns named arguments into the node's positional array |
| `crates/mcp/src/policy.rs` | The `allow` and `deny` patterns |
| `crates/mcp/src/server.rs` | Answers MCP messages against a `Backend` of nodes, and watches the tool list for `list_changed` |
| `crates/mcp/src/stdio.rs` | One JSON-RPC message per line; requests run concurrently |
| `crates/mcp/src/http.rs` | Streamable HTTP with hyper: token, Origin check, event stream |
| `crates/node/src/gateway.rs` | `[mcp]`, and the backend over this node's providers and the mesh |
| `crates/node/src/mcp.rs` | `bricolo mcp`: the stdio bridge, `token`, `url`, `funnel` and `connect` |
| `crates/mesh/src/capabilities.rs` | `METHODS`, `dispatch_remote` for JSON calls to a named peer, and `mesh.methods` |

`crates/mcp` embeds `wit/` with `include_str!`. A test fails when a file there is added, removed or changed without the embedded list following.
