# Device mesh

Each node can run capability calls on another node over iroh, Tailscale, or both. Your `rules.toml` decides which capability goes where, `peers.toml` decides who may connect and what they may call, `node.toml` decides which networks carry the connections, and plugins never know the difference. Decision 2 in [design.md](design.md) explains why the mesh uses its own small RPC.

## Pair two devices

Say the Mac should send `speech` to a desktop with a GPU, and the desktop may use the Mac's text to speech.

1. On each device, print its id:

   ```sh
   bricolo id
   ```

   The first line is the endpoint id, the public half of `node.key`. The command creates `node.key` if it does not exist yet. It also prints the `bricolo peer add` command and the `peers.toml` block for the other device.

2. On the Mac, trust the desktop. The desktop may call `tts` here:

   ```sh
   bricolo peer add desktop <desktop id> --allow tts
   ```

3. On the desktop, trust the Mac. The Mac may call `speech` there:

   ```sh
   bricolo peer add mac <mac id> --allow speech
   ```

4. On the Mac, route speech to the desktop in `rules.toml`, with this node as the fallback:

   ```toml
   [routes]
   speech = ["desktop", "local"]
   ```

5. Start or restart `bricolo run` on both. If a node is already running, `bricolo peer add` tells it to reread `peers.toml`. `rules.toml` is read at start.

6. Check the link:

   ```sh
   bricolo peer list
   bricolo status
   ```

   `bricolo peer list` shows each peer's state and the transport that carries it. `bricolo status` prints the full mesh state as JSON under `"mesh"`.

Both sides have to list each other. A node refuses any key that is not in its `peers.toml`, and each node calls its peers over its own connection.

## peers.toml

`peers.toml` lives in the config folder, next to `rules.toml`.

```toml
[peers.desktop]
key = "<endpoint id from bricolo id on the desktop>"
# What the desktop may call on this node.
allow = ["tts", "speech", "disk:read:~/Shared"]
# Optional. Direct addresses to try before discovery and relays.
addrs = ["192.168.1.20:7777"]
# Optional. The desktop's machine name on the tailnet, with a port if it
# listens on another one. Defaults to the table name, "desktop".
tailnet = "gpu-desktop"
```

- The table name (`desktop`) is the name `rules.toml` uses. It may use letters, digits, `-` and `_`. `local` is taken: it means this node.
- `key` is the peer's endpoint id, in hex as `bricolo id` prints it.
- `allow` lists the capabilities the peer may call on this node. Anything missing answers `denied`.
- `disk` always needs a path, written like a plugin grant: `disk:~/Shared` allows reads and writes under `~/Shared`, and `disk:read:~/Shared` allows only reads. `~` is the home folder of the node that serves the call.
- `addrs` is optional. With relays on, the node finds the peer by its id alone.
- `tailnet` is optional and only used by the Tailscale transport: `host` or `host:port`. `host` is the MagicDNS machine name, the host name, or the full MagicDNS name, as `tailscale status` shows them.

`bricolo peer add` appends a block and keeps the rest of the file, comments included. It refuses a name or key that is already listed. To change or remove a peer, edit the file and run `bricolo peer reload`.

## node.toml

```toml
[mesh]
enabled = true           # start the mesh with `bricolo run`
transports = ["iroh"]    # "iroh", "tailscale", or both, in order of preference
relays = true            # iroh: n0's relays and DNS discovery
port = 0                 # iroh: UDP port; 0 picks a free one

[mesh.tailscale]
port = 7787              # UDP port on this device's tailnet address
command = "tailscale"    # the CLI for `tailscale status --json`
```

- `transports` picks the networks. With both, the node dials each peer over the first transport that reaches it, and accepts connections on all of them. Changing it needs a restart of `bricolo run`; nothing else changes.
- With `relays = true` (the default), devices on different networks find each other through n0's DNS discovery and connect through n0's relays when a direct path fails. The relays forward encrypted packets and cannot read them.
- With `relays = false`, iroh publishes nothing and connects only to the `addrs` in `peers.toml`. Use it for a LAN-only setup, and set `port` so the addresses stay valid. A peer with no `addrs` is then out of iroh's reach, and a second transport gets a turn.
- `enabled = false` turns the mesh off. Every peer target then counts as offline.

## Transports

Each transport carries the same connections: the same handshake, the same `peers.toml` check, the same calls. Both prove the node's key from `node.key`, so a peer is listed once whatever carries it. `bricolo peer list` and the hub show which transport a connection uses.

### iroh

The default. iroh finds a peer by its key alone, through n0's DNS discovery, and falls back to n0's relays when no direct path works. iroh listens on every interface, the tailnet's included, so two devices on one tailnet already get a direct path over it.

### Tailscale

For devices that already share a tailnet, or where n0's discovery and relays are not wanted.

- Watching: the node rereads `tailscale status` every 10 seconds. When a machine joins, comes online or changes address, offline peers are tried again at once instead of after their backoff, which grows to 30 seconds.
- Losing the address: `tailscale down`, a reconnect or sleep takes the tailnet address away, and the socket on it dies. The node binds again once the address is back, on the new address if it changed, and reconnects without a restart. Until then `bricolo status` shows no bound address for `tailscale`.
- Finding peers: the node runs `tailscale status --json` and dials the peer's tailnet IPs on port 7787 (`[mesh.tailscale] port`, or the port in the peer's `tailnet`). Each node listens on its own tailnet IPv4 address, so only the tailnet reaches the port.
- Trust: Tailscale proves which machine sent a packet, not which bricolo node. So the node still checks the peer's key in the handshake. A tailnet machine whose key is not in `peers.toml` gets `refused`, like any stranger.
- `tailscale` must be running (`tailscale up`). The node looks for the CLI on `PATH`, then inside `/Applications/Tailscale.app` on macOS; set `command` for anything else.
- Each device needs the same port free and, if it filters, an ACL that lets the tailnet reach UDP 7787.

To move a pair of devices from iroh to Tailscale, set `transports = ["tailscale"]` on both, add `tailnet` to each `peers.toml` entry whose name differs from the machine name, and restart `bricolo run`. To keep iroh as the fallback, use `transports = ["tailscale", "iroh"]`.

## rules.toml

A target that answers `unavailable` or `timeout` passes the call to the next one. A peer that is offline counts as `unavailable` at once, without waiting for `timeout_secs`.

```toml
# Seconds a target may take, including a peer. Also the deadline the
# peer itself enforces.
timeout_secs = 30

[routes]
# Transcribe on the desktop, then the laptop, then here.
speech = ["desktop", "laptop", "local"]
# Speak on the Mac only, even when a script runs on the desktop.
tts = ["mac"]
# Read shared notes from the NAS, falling back to the local copy.
disk = ["nas", "local"]
```

Only routable capabilities may appear in `[routes]`: `tts`, `speech`, `disk`, `clipboard`, `notify` and `claude-code` today.

## Debugging with bricolo call

`bricolo call` runs one capability call through the running node's router, as a plugin named `bricolo-call`:

```sh
bricolo call disk read '["~/Shared/todo.md"]' --grant 'disk:read:~/Shared'
bricolo call tts speak '["hello", null]'
```

The arguments are a JSON array with one element per argument. `--grant` gives the call a disk grant, the way `permissions.toml` gives one to a plugin. The answer is JSON: `"ok"` holds the result, and `"text"` repeats a byte result as text when it is UTF-8. On failure, `"call_error"` holds `unavailable`, `timeout`, `denied` or `failed`.

The node that serves a peer's call logs it, for example `served disk.read in 0 ms peer=mac plugin=bricolo-call`.

## Security model

- Identity. `node.key` in the data folder holds the node's secret key, with mode 0600. Its public half is the node's id on the mesh. The data folder stays out of dotfiles, so the key never lands in a repository.
- Who may connect. The node checks the peer's key right after the TLS handshake and closes the connection if the key is not in `peers.toml`. The peer sees `refused` in `bricolo peer list`.
- What a peer may call. Each request names a capability. The serving node answers `denied` if that capability is not in the peer's `allow` list, and checks again on every call, so editing `peers.toml` and running `bricolo peer reload` takes effect at the next call.
- Disk paths, checked twice. Before a disk call leaves, the calling node checks the path against the calling plugin's own grants. The serving node then checks it against the `disk:` grants in the caller's `allow` entry. A plugin cannot reach a path through a peer that it could not name at home, and a peer cannot read past what you granted it.
- Speech files stay on the calling node. `speech.transcribe-file` takes a path. When `speech` routes to a peer, the calling node checks the path against the calling plugin's own grants, reads the file and sends its bytes as `transcribe`. The peer never opens a path for a speech call, so allowing `speech` grants no disk access.
- Only remote capabilities. Peers can reach `tts`, `speech`, `disk`, `clipboard`, `notify`, `claude-code` and registered media streams, which rules.toml can also route. `accessibility`, `process`, `device`, `scripting` and `virtual-screen` answer a peer that names them and has them in its `allow` list, but are never routed: they act on that device's screen, programs and settings, so a caller always picks the node. `log`, `timer`, `kv`, `hotkey`, `window`, `app`, `keyboard`, `mic`, `shell` and `http` never leave the node. `shell` and `http` depend on the calling plugin's allow lists in its own `plugin.toml`, which a peer could not check; for the same reason `virtual-screen.launch` from a peer is refused unless the serving node's own grants allow the program.
- No remote keyboard. A peer that could call `keyboard` could type commands into whatever app has focus on your other device, while nobody watches that screen. Allowing it in `peers.toml` would hand a peer the same reach as a person at the keyboard, so the capability is not in the remote table at all.
- No remote microphone yet. `mic.record-stop` returns a path on the recording node, which means nothing on the caller's. A remote microphone will send the audio as a media stream instead, and recording on another device should show there, as `active_sensors` does in `bricolo status`.
- No loops. A peer's call runs on the serving node's own providers, never through its router, so a call cannot bounce on to a third node.
- Encryption. Every connection, on either transport, is QUIC with TLS 1.3, keyed by the two nodes' keys. Relays only forward encrypted packets. Over Tailscale, WireGuard encrypts the packets once more.
- The ALPN `bricolo/1` names the wire format. A node with a different format cannot connect.

A call can run twice. If a connection breaks after the peer started a call and before the answer arrived, the caller gets `unavailable`, and the router tries the next target. Avoid routing calls that must not repeat, like appending to a file, to a target list with a fallback.

## How a call travels

Each call opens one bidirectional stream on the connection to the peer, whichever transport carries it.

```
client                                            server
  open stream
  frame: header {id, kind: call, capability,
                 method, deadline_ms, plugin}  ──►  checks the allow list
  frame: postcard arguments (a tuple)           ──►  runs the provider, with
  finish                                              the deadline
                                                ◄──  frame: Result<postcard result, call-error>
```

- A frame is a 4-byte big-endian length followed by the bytes. Call frames may hold up to 16 MiB, media frames up to 1 MiB.
- `deadline_ms` is relative, because two devices' clocks may disagree. The server answers `timeout` when it passes.
- Dropping the call on the client drops the stream. The server notices the stream stop and drops the provider's future, which cancels the work.
- Calls and media streams share one connection. A busy media stream does not hold up calls, because QUIC limits how much data each stream may have in flight. The `a_long_stream_does_not_block_short_calls` test checks this.
- A connection also carries unreliable datagrams, for real-time input, audio and video that would rather drop a packet than wait. See "Datagrams" below.

## Media streams

A media stream carries framed byte chunks both ways for as long as it lives, like a microphone feed. The client calls `Mesh::open_stream(peer, capability, method, caller)` and gets a `MediaStream` with a sender and a receiver. The serving node handles it with the `StreamHandler` registered for that capability through `Mesh::register_stream`. The peer still needs the capability in its `allow` list.

No capability uses media streams yet. A remote `mic` is the first planned one. The tests register a `stream-test` echo handler to show that a busy stream and short calls run side by side.

## Datagrams

A datagram is one unreliable message for a capability: it may be lost or arrive out of order, and it never waits behind a lost packet. The client calls `Mesh::send_datagram(peer, capability, payload)`. The serving node hands it to the `DatagramHandler` registered for that capability through `Mesh::register_datagrams`, if the sender has the capability in its `allow` list; anything else is dropped without an answer.

On the wire, a datagram is one byte with the capability's length, the capability, and the payload. When the connection has no datagrams, or the payload does not fit in one (about 1,200 bytes), the message goes on a stream of its own instead (`Kind::Datagram`): late, perhaps, but whole. Payloads over 1 MiB are refused. No capability uses datagrams yet.

## Plugin sharing

A node can install a plugin straight from a peer that has it, by the sha256 of its `plugin.wasm`:

```sh
bricolo install peer:mac/9f2c…   # the sha256 from plugins.lock on the Mac
```

- The serving node must allow it: `allow = ["plugins", ...]` in its `peers.toml` entry for the fetching node. Without it the fetch answers `denied`.
- The serving node sends only plugins that `plugins.lock` lists with that hash and whose installed `plugin.wasm` still has it. It sends the plugin as an `bricolo pack` bundle on a media stream for the capability `plugins`, so it works over any transport.
- The fetching node checks the hash of the component it unpacks, saves the bundle under `downloads/` in its data folder, and installs it with the usual approval. `plugins.lock` records `peer:mac/<sha256>` as the source, so `bricolo install --from-lock` fetches it from the peer again.
- `bricolo install peer:…` asks the running node to fetch, since only it holds the mesh.

## Finding devices

`bricolo peer discover` asks the running node what each transport sees. The Tailscale transport lists the tailnet's machines with their state and addresses, which gives the names to use in `tailnet`; iroh looks devices up by key instead of listing a network, so it lists the peers it was given. A device that `peers.toml` already trusts shows its peer name.

## The mesh capability

`mesh` is reserved. Every trusted peer may call it, whatever its allow list says. `mesh.methods` answers the `(capability, method)` pairs in `METHODS` that the caller's allow list lets it run here. The MCP gateway uses it to show each peer's tools ([docs/mcp.md](mcp.md)).

## Adding a capability to the mesh

`crates/mesh/src/capabilities.rs` holds the table of remote methods. For a new routable capability:

1. Add an entry to `remote_capabilities!`, listing its methods with the argument and result types from the provider trait.
2. Add one line to `impl Peers for Mesh` in `crates/mesh/src/mesh.rs`.
3. Add the capability to `ROUTABLE` in `bricolo-providers` and to the `Peers` trait in `bricolo-router`.
4. Leave out any method that takes a path on the calling node. Wrap the client instead, as `GuardedDisk` and `GuardedSpeech` do, so the calling node checks the path against the calling plugin's grants first.

Argument and result types must implement serde's `Serialize` and `Deserialize`.
