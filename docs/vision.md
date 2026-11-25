# The big idea

bricolo treats all the devices you own as one personal computer. This page explains why that matters, what it already makes possible, and where it can go. For how the parts work today, read [design.md](design.md).

## The problem

Most people who automate their desktop end up with the same mess. A Hammerspoon config on the work laptop. A few shell scripts on the home server. A Python speech model running on the desktop because that's where the GPU is. Each one works, but they can't help each other. If you want your laptop hotkey to use the desktop's GPU, you write a custom HTTP endpoint, hard-code an IP address, and hope it's awake.

The automations themselves are small. The plumbing around them is what costs the time.

## The idea in one sentence

**Write automations against one standard library, and let a router decide which of your devices runs each call.**

### Every device offers the same API

A node on a Mac, a Linux box or a Windows PC exposes the same capabilities: `hotkey`, `clipboard`, `window`, `mic`, `speech`, `tts`, `disk`, `shell`, `http`, `ui` and the rest. They are defined once, in WIT, in [`wit/`](../wit/). Each OS gets its own implementation underneath. An automation written on a Mac runs unchanged on Linux.

### Where a call runs is configuration, not code

```toml
[routes]
speech = ["desktop", "local"]
tts    = ["kitchen-mac", "local"]
disk   = ["nas"]
```

Move speech to a new GPU box and you change one line. The scripts never notice. When the desktop sleeps, the router falls back to the next entry.

### Trust is explicit and narrow

Devices pair by public key. Each peer lists exactly which capabilities the other may call, so the desktop can accept `speech` from the laptop without letting it run `shell`. Plugins go further: each one declares the capabilities it needs, and you approve them at install, like permissions on a phone.

### Building blocks compose

The standard library is deliberately low level. Dictation is `hotkey` plus `mic` plus `speech` plus `keyboard`. A wake-word assistant is `mic` plus a sidecar plus `tts`. Each app is a small file, because the hard parts live in the library.

### The library is the same for people and for AI

Because the capabilities are typed, documented and sandboxed, a coding agent can build an app safely. `bricolo skill install` gives Claude Code, Codex, Gemini CLI, OpenCode or Cursor a skill that plans the app, picks a script, plugin or sidecar, writes it, installs it and checks that it runs. The permissions model limits what a generated plugin can touch.

## What it already does

These run today on the author's machines, from the apps in [`apps/`](../apps/):

- **Dictation.** Press a chord, speak, and the text lands at the cursor, transcribed by a warm Parakeet model.
- **A wake word.** Say "Codec" and a voice agent call starts, with a widget that shows where each turn's time goes.
- **Screen OCR.** Read a screen region or a copied image into Markdown with a local model.
- **Widgets.** Battery, memory, disk and a world clock on the hub's Home view.
- **Model control.** Run the LM Studio server and switch its loaded model from the hub.
- **Habit tools.** A noise machine on a switch, and NoseGuard, which watches the camera and flashes when a finger touches your nose.

And underneath them: hot reload for scripts and plugins, sidecar supervision with health checks, encrypted device-to-device calls over iroh or Tailscale, installs from GitHub releases pinned by sha256, and a signed macOS app that starts at login.

## Where it can go

None of this is promised. These are directions the design already leaves room for, roughly ordered from nearest to furthest.

### Finish the three desktops

- Run the Windows port on real machines and ship the hub there with WebView2.
- Wayland beyond Hyprland: global shortcuts through the portal, and window control on GNOME and KDE.
- Key releases and keyboard layouts on every OS.

### A smarter mesh

- **Remote media.** The mesh already carries media streams. A remote microphone or camera means a phone on the desk becomes the mic for a meeting on the desktop.
- **Automatic routing.** Today you write the route. Next, the router could pick by latency, battery, load or GPU availability, and explain its choice in the hub.
- **Discovery.** Find nodes on the local network without typing addresses, and see whether each link is direct or relayed.
- **Phones and small devices.** An iOS or Android node that offers camera, mic, location and notifications. A Raspberry Pi or a travel router as an always-on node for sensors and home control.

### Apps that build on apps

- **Plugin composition.** The manifest already describes plugins that import other plugins' interfaces. Turning that on lets a "meeting notes" plugin build on "dictation" and "summarize" without copying them.
- **Sharing between friends.** A peer can already serve the plugins it has installed. A small shared catalog, with signatures and reviews, turns that into an app store for people you trust.
- **Sidecars as packages.** Bundle a sidecar with its plugin so one install brings the model too, with its own review step since sidecars run outside the sandbox.

### A personal computer for AI agents

- **Expose the library to agents.** An MCP gateway in front of the node lets any agent call `screen`, `clipboard`, `tts` or `speech` on any of your devices, inside the same permission model. This one is in progress: `bricolo mcp` turns each capability in the WIT into an MCP tool and logs every call.
- **Agents that live on the mesh.** A long-running assistant that hears you on the laptop, thinks on the GPU box, reads files on the NAS and speaks in the kitchen, with every call visible in the hub's logs.
- **Self-healing apps.** The hub already shows what needs attention. An agent could read those failures and propose the fix as a diff.

### Beyond one person

- A household mesh, where each person runs their own nodes and shares a few capabilities, such as the living room speakers.
- A small team mesh for shared tools: a build machine's GPU, a lab's instruments, a meeting room's camera.
- Self-hosted control with Headscale, so nothing depends on a third-party service.

## Principles that stay fixed

Whatever gets built next keeps these:

1. **Local first.** Your devices talk to each other directly. Relays forward packets they cannot read.
2. **Explicit trust.** Nothing runs on a device unless that device's owner allowed that peer, that capability, or that plugin.
3. **One API.** New features arrive as capabilities in the standard library, available to scripts, plugins, peers and agents at once.
4. **Small apps.** If an app gets long, the missing piece probably belongs in the library.

## Get involved

Try it with [Getting started](getting-started.md), build something with [Build your own app](apps.md), and open an issue with what you'd connect. Ideas for capabilities, platforms and apps are the most useful feedback right now.
