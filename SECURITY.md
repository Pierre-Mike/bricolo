# Security policy

bricolo runs automations with access to your keyboard, clipboard, microphone, files and shell, and it accepts calls from other devices. We take reports seriously.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private reporting instead: on the repository page, open **Security**, then **Report a vulnerability**.

Include what you found, how to reproduce it, and what an attacker could do with it. We aim to reply within a week.

## What is in scope

- Escaping the plugin sandbox, or a plugin calling a capability it was not granted.
- A peer calling a capability outside its `allow` list in `peers.toml`, or connecting without being listed.
- The control socket (or named pipe on Windows) accepting commands from another user.
- Installs from GitHub releases or peers that skip the sha256 check in `plugins.lock`.
- The hub rendering plugin-supplied strings as HTML.

## What is out of scope

- Scripts. Lua scripts are trusted by design and run with every capability.
- Sidecars. They are local programs you choose to run, outside any sandbox.
- Anything that needs an attacker to already control your user account.

## The security model in short

[docs/mesh.md](docs/mesh.md#security-model) describes how peers authenticate and what they can reach. [docs/design.md](docs/design.md) (Decision 5) describes install-time permissions for plugins.
