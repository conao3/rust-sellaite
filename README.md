# sellaite

Give an AI computer-use agent its own Linux desktop and its own cursor, so it never takes over yours.

Each agent runs in its own headless Wayland session (sway). Inside that session the agent has its own input seat: its own pointer and keyboard focus. A second seat is reserved for you, so you can watch the agent's desktop in the browser and step in when needed without fighting it for the mouse. Several agents can run side by side, each on its own desktop.

The agent talks to sellaite over MCP (`start_desktop`, `screenshot`, `click`, `type`, ...), so it works with Claude Code, Codex and other MCP clients.

The name comes from the mineral sellaite, read as the Latin *sella* (seat).

## Status

Early development. The MCP server runs one desktop with an agent seat and a user seat, and the dashboard shows each desktop in the browser and pauses its agent. Sending the agent a note, approving its actions and taking over the desktop are not built yet.

## Usage

sellaite runs on Linux and needs `sway`, `grim` and `wayvnc` on `PATH` (the Nix package brings them and noVNC). Start the MCP server over stdio:

```sh
nix run github:conao3/rust-sellaite -- mcp serve
```

See [Use with Claude Code / Codex](#use-with-claude-code--codex) to register it with an MCP client.

Tools:

| Tool | Arguments | What it does |
|---|---|---|
| `start_desktop` | `width`, `height` (default 1280x720) | Starts a headless sway with two seats, `agent` for the agent and `seat0` for you, and a VNC server (wayvnc) on `seat0`. Returns `Id`, `XdgRuntimeDir`, `WaylandDisplay`, `Swaysock` and `VncSocket`. |
| `stop_desktop` | | Stops sway and every application in the desktop. |
| `screenshot` | | Returns the whole desktop as a PNG image. |
| `click` | `x`, `y`, `button` (`left`, `right`, `middle`) | Moves the agent's pointer and clicks. |
| `type` | `text` | Types the text into the window that has the agent's keyboard focus. |
| `key` | `keys` (`Return`, `ctrl+l`, `alt+Tab`, ...) | Presses a key or a chord on the agent's keyboard, with the modifiers (`shift`, `ctrl`, `alt`, `super`) held as modifier state. |

The agent's clicks and keystrokes come from a virtual pointer and a virtual keyboard created on the `agent` seat, so they move only that seat's cursor and focus. Your seat keeps its own focus.

While the agent is paused, `click`, `type` and `key` return an error that says the user paused it; `screenshot` and `stop_desktop` still work.

To open an application in the desktop, run it with the environment that `start_desktop` returned:

```sh
XDG_RUNTIME_DIR=<XdgRuntimeDir> WAYLAND_DISPLAY=<WaylandDisplay> foot &
```

## Dashboard

```sh
nix run github:conao3/rust-sellaite -- dashboard serve
```

The dashboard listens on `http://127.0.0.1:8730/` (`--address` changes it) and prints its URL as `{"Url": ...}`. It lists the desktops that running `sellaite mcp serve` processes of the same user have started and shows the selected one (`?desktop=<Id>`) through noVNC in view-only mode, so watching the page sends no input to the desktop.

- **Stop the agent** pauses the agent: its `click`, `type` and `key` return an error until you press **Resume**.
- Any input on your seat (`seat0`), for example from a VNC client connected to the desktop's `VncSocket`, also pauses the agent. sellaite watches `seat0` with ext-idle-notify-v1 (500 ms timeout); the agent's own input is on the `agent` seat and does not pause it.

The dashboard serves noVNC from `--novnc-dir` (or `SELLAITE_NOVNC_DIR`; the Nix package sets it) and proxies the browser's WebSocket to the desktop's wayvnc unix socket. It answers only requests whose `Host` is `localhost`, `127.0.0.1`, `[::1]` or the address it listens on, and it accepts Stop, Resume and the VNC WebSocket only from its own page (`Origin`).

`sellaite mcp serve` removes, when it starts, the runtime directories (`/tmp/sel.*`) that killed runs left behind.

## Use with Claude Code / Codex

The tools act on the desktop only; the client opens applications in it with its shell tool, using the `XdgRuntimeDir` and `WaylandDisplay` that `start_desktop` returned. Run the client where `sway`, `grim`, `wayvnc` and the applications you want (for example `foot`) are on `PATH`.

Claude Code:

```sh
claude mcp add sellaite -- nix run github:conao3/rust-sellaite -- mcp serve
```

For a headless run, pass the server in a config file and allow its tools and the shell:

```json
{
  "mcpServers": {
    "sellaite": {
      "command": "nix",
      "args": ["run", "github:conao3/rust-sellaite", "--", "mcp", "serve"]
    }
  }
}
```

```sh
claude -p "Start a desktop, open foot, type 'echo hello', press Return, take a screenshot and stop the desktop." \
  --mcp-config sellaite.json --allowedTools 'mcp__sellaite__*' Bash
```

The client opens foot with a command such as:

```sh
XDG_RUNTIME_DIR=<XdgRuntimeDir> WAYLAND_DISPLAY=<WaylandDisplay> setsid foot >/dev/null 2>&1 &
```

A fresh foot window has no keyboard focus on the agent seat until the agent clicks it.

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.sellaite]
command = "nix"
args = ["run", "github:conao3/rust-sellaite", "--", "mcp", "serve"]
```

The server speaks MCP over stdio and accepts the protocol versions 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05.

## Development

```sh
nix develop      # Rust, Lean, leanfran, sway, grim, wayvnc, noVNC
make check       # format check, clippy, Lean proofs and audit, tests (including headless sway tests)
nix build        # build the sellaite binary
nix run          # run it
```

## License

Apache-2.0
