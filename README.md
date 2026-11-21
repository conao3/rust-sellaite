# sellaite

Give an AI computer-use agent its own Linux desktop and its own cursor, so it never takes over yours.

Each agent runs in its own headless Wayland session (sway). Inside that session the agent has its own input seat: its own pointer and keyboard focus. A second seat is reserved for you, so you can watch the agent's desktop in the browser and step in when needed without fighting it for the mouse. Several agents can run side by side, each on its own desktop.

The agent talks to sellaite over MCP (`start_desktop`, `screenshot`, `click`, `type`, ...), so it works with Claude Code, Codex and other MCP clients.

The name comes from the mineral sellaite, read as the Latin *sella* (seat).

## Status

Early development. Nothing is usable yet.

## Development

```sh
nix develop      # Rust, Lean, leanfran, sway, grim, wayvnc
make check       # format check, clippy, tests
nix build        # build the sellaite binary
nix run          # run it
```

## License

Apache-2.0
