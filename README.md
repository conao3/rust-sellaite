# sellaite

Give an AI computer-use agent its own Linux desktop and its own cursor, so it never takes over yours.

Each agent runs in its own headless Wayland session (sway). Inside that session the agent has its own input seat: its own pointer and keyboard focus. A second seat is reserved for you, so you can watch the agent's desktop in the browser and step in when needed without fighting it for the mouse. Several agents can run side by side, each on its own desktop.

The agent talks to sellaite over MCP (`start_desktop`, `screenshot`, `click`, `type`, ...), so it works with Claude Code, Codex and other MCP clients.

The name comes from the mineral sellaite, read as the Latin *sella* (seat).

## Status

Early development. The MCP server runs one desktop with an agent seat and a user seat; the browser view and the ways to step in are not built yet.

## Usage

sellaite runs on Linux and needs `sway` and `grim` on `PATH` (the Nix package brings both). Start the MCP server over stdio:

```sh
nix run github:conao3/rust-sellaite -- mcp serve
```

See [Use with Claude Code / Codex](#use-with-claude-code--codex) to register it with an MCP client.

Tools:

| Tool | Arguments | What it does |
|---|---|---|
| `start_desktop` | `width`, `height` (default 1280x720) | Starts a headless sway with two seats: `agent` for the agent and `seat0` for you. Returns `XdgRuntimeDir`, `WaylandDisplay` and `Swaysock`. |
| `stop_desktop` | | Stops sway and every application in the desktop. |
| `screenshot` | | Returns the whole desktop as a PNG image. |
| `click` | `x`, `y`, `button` (`left`, `right`, `middle`) | Moves the agent's pointer and clicks. |
| `type` | `text` | Types the text into the window that has the agent's keyboard focus. |
| `key` | `keys` (`Return`, `ctrl+l`, `alt+Tab`, ...) | Presses a key or a chord on the agent's keyboard, with the modifiers (`shift`, `ctrl`, `alt`, `super`) held as modifier state. |

The agent's clicks and keystrokes come from a virtual pointer and a virtual keyboard created on the `agent` seat, so they move only that seat's cursor and focus. Your seat keeps its own focus.

To open an application in the desktop, run it with the environment that `start_desktop` returned:

```sh
XDG_RUNTIME_DIR=<XdgRuntimeDir> WAYLAND_DISPLAY=<WaylandDisplay> foot &
```

## Use with Claude Code / Codex

The tools act on the desktop only; the client opens applications in it with its shell tool, using the `XdgRuntimeDir` and `WaylandDisplay` that `start_desktop` returned. Run the client where `sway`, `grim` and the applications you want (for example `foot`) are on `PATH`.

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
nix develop      # Rust, Lean, leanfran, sway, grim, wayvnc
make check       # format check, clippy, Lean proofs and audit, tests (including a headless sway test)
nix build        # build the sellaite binary
nix run          # run it
```

## License

Apache-2.0
