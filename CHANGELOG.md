# Changelog

## v0.1.0

First release of sellaite for Linux. Each MCP agent gets a headless sway desktop with its own pointer and keyboard focus.

- Launch applications, capture screenshots, wait for the screen to settle, and send mouse or keyboard input through MCP.
- Watch the desktop in an authenticated browser dashboard, send notes, pause or resume the agent, and take over input.
- Review sensitive clicks and typing in the dashboard, then approve, edit, or reject them. Target detection uses the agent's declared intent and available AT-SPI2 data.
- Install with Nix or Cargo. See the [README](https://github.com/conao3/rust-sellaite#install-on-linux) for dependencies and configuration.
