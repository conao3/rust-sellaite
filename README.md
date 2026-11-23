# sellaite

Give an AI computer-use agent its own Linux desktop and its own cursor, so it never takes over yours.

Each agent runs in its own headless Wayland session (sway). Inside that session the agent has its own input seat: its own pointer and keyboard focus. A second seat is reserved for you, so you can watch the agent's desktop in the browser and step in when needed without fighting it for the mouse. Several agents can run side by side, each on its own desktop.

The agent talks to sellaite over MCP (`start_desktop`, `launch`, `screenshot`, `click`, `type`, ...), so it works with Claude Code, Codex and other MCP clients.

The name comes from the mineral sellaite, read as the Latin *sella* (seat).

## Status

Early development. The MCP server runs one desktop with an agent seat and a user seat, and the dashboard shows each desktop in the browser, sends its agent notes and pauses it. The dashboard can also take over input and approve sensitive click and type requests.

## Security

**The agent desktop shares the MCP server user's operating-system permissions.** Wayland seats keep the agent's pointer and keyboard focus separate from yours. `launch` accepts any executable and arguments, including a shell. Programs can access files, sockets and services available to that user, and can make network requests.

`launch` passes only selected environment variables for executable lookup, user and application configuration, locale, terminals, fonts, and the D-Bus and accessibility sessions, plus the desktop's own Wayland settings. API keys and `SSH_AUTH_SOCK` are omitted from the inherited environment. The launched process still has access to credentials available through files, sockets or services under the same account.

- **Pause and takeover** help prevent conflicting input when you intervene. Pause makes the MCP server refuse new `launch`, `click`, `type` and `key` requests. Existing programs keep running, and programs with the same user permissions can modify the pause file in the desktop's runtime directory.
- **Approval** helps catch mistakes in `click` and `type`. Detection relies on the agent's declared intent and the accessibility information sellaite can retrieve. Browser redirects, keystrokes sent with `key`, commands run with `launch`, and actions taken through other tools fall outside these checks.
- **Dashboard authentication** protects the HTTP and WebSocket endpoints from callers without the dashboard credential. Host and Origin checks limit cross-origin browser access. For remote access, the trusted Tailscale Serve proxy supplies the login checked against the allowlist. Keep the backend on loopback and the startup URL private. Same-account programs can access the desktop runtime files and sockets directly.

Use a dedicated unprivileged Linux user or a container for an agent you do not fully trust. Give it a separate home and browser profile, and keep your personal files, credentials and host sockets outside its reach. Apply any network restrictions you need at the operating-system or container boundary.

Report vulnerabilities through the private channel in [SECURITY.md](SECURITY.md).

## Install on Linux

### With Nix

On Linux with Nix and flakes enabled, install sellaite from GitHub:

```sh
nix profile install github:conao3/rust-sellaite
```

The Nix package supplies the runtime dependencies. Install any applications
you want the agent to open, such as foot, on the same host.
For example, make foot available for a Claude Code session with
`nix shell nixpkgs#foot --command claude`.

To try sellaite without installing it, use `nix run github:conao3/rust-sellaite -- mcp serve-mcp` as the MCP server command.

### With Cargo

Install Cargo, Rust 1.89 or later and a system linker, then install the runtime
dependencies for your distribution. Run sellaite as an ordinary user. The
headless sway session needs neither a display server nor a GPU.

On Ubuntu 26.04 LTS, enable the `universe` repository for the desktop packages:

```sh
sudo apt update
sudo apt install build-essential cargo ca-certificates git sway grim wayvnc coreutils \
  python3 libatspi2.0-0t64 at-spi2-core dbus novnc foot fonts-dejavu-core
```

On Debian 13, enable [`trixie-backports`](https://backports.debian.org/Instructions/)
for Cargo and Rust.
The stable Rust package is older than sellaite requires:

```sh
sudo apt update
sudo apt install build-essential ca-certificates git sway grim wayvnc coreutils \
  python3 libatspi2.0-0t64 at-spi2-core dbus novnc foot fonts-dejavu-core
sudo apt install -t trixie-backports cargo rustc
```

On Fedora:

```sh
sudo dnf install gcc rust cargo ca-certificates sway grim wayvnc coreutils \
  python3 at-spi2-core dbus-daemon novnc foot dejavu-sans-mono-fonts
```

On openSUSE Tumbleweed:

```sh
sudo zypper install gcc rust cargo ca-certificates sway grim wayvnc coreutils \
  python3 libatspi0 at-spi2-core dbus-1 novnc foot dejavu-fonts
```

On Arch Linux:

```sh
sudo pacman -Syu --needed gcc rust ca-certificates curl git sway grim wayvnc coreutils \
  python at-spi2-core dbus foot ttf-dejavu
```

Install a published release from crates.io:

```sh
cargo install --locked sellaite
```

To install the current development version from GitHub:

```sh
cargo install --locked --git https://github.com/conao3/rust-sellaite
```

From a source checkout, run `cargo install --locked --path .`. Add Cargo's
binary directory (usually `$HOME/.cargo/bin`) to your `PATH`.

| Runtime dependency | Purpose |
| --- | --- |
| `sway` and `swaymsg` (both in `sway` on Ubuntu and Arch) | Run the headless desktop and inspect window focus during `launch`. |
| `grim` | Capture PNG screenshots. |
| `wayvnc` | Serve the desktop over a Unix socket on the user's input seat. |
| `timeout` (`coreutils`), `python3` (`python3` on Ubuntu, `python` on Arch), and `libatspi.so.0` (`libatspi2.0-0t64` on Ubuntu, `at-spi2-core` on Arch) | Resolve AT-SPI2 targets for sensitive input and approvals. |
| noVNC | Render the desktop in the dashboard. |
| `foot`, a font and D-Bus | Provide a terminal for the quick start and a session bus for accessibility-enabled apps. |

The headless install check covers sway 1.11 and wayvnc 0.9.1 on Ubuntu
26.04 LTS, and sway 1.12 and wayvnc 0.10.2 on Arch. Older versions and the
minimum supported versions are untested.

The Arch smoke container removes `cap_sys_nice` from its copy of `sway`.
Docker's default capability set omits it, and headless sway can run without
the scheduling privilege. The smoke test runs in an ordinary Docker container
as an unprivileged user.

To enable AT-SPI2 checks, set `SELLAITE_ATSPI_LIBRARY=libatspi.so.0` in the MCP
server's environment and run it inside a D-Bus user session. You can also set
an absolute library path. Approvals require an accessible target.

The dashboard reads noVNC from `/usr/share/novnc` by default. The Ubuntu,
Debian, Fedora and Tumbleweed `novnc` packages install it there. On Arch,
unpack an [upstream noVNC
release](https://github.com/novnc/noVNC/releases) and pass the directory
containing `core/rfb.js` with `--novnc-dir` or `SELLAITE_NOVNC_DIR`:

```sh
mkdir -p "$HOME/.local/share/novnc"
curl -fsSL https://codeload.github.com/novnc/noVNC/tar.gz/refs/tags/v1.6.0 \
  | tar -xz --strip-components=1 -C "$HOME/.local/share/novnc"
export SELLAITE_NOVNC_DIR="$HOME/.local/share/novnc"
```

## Quick start

```sh
claude mcp add sellaite -- sellaite mcp serve-mcp
sellaite dashboard start-dashboard
```

Leave the dashboard running, open the `Url` it prints, and ask Claude
Code to start a desktop, open foot, and take a screenshot. Select the
desktop when it appears. Use **Send note** to guide the agent, **Take over**
to control the desktop, and **Give back** to return control. When an
operation asks for approval, review the highlighted target and choose
**Approve**, **Edit**, or **Reject**. Ask Claude Code to stop the desktop
when you are done.

See [Use with Claude Code / Codex](#use-with-claude-code--codex) for more
configuration examples.

## MCP tools

| Tool | Arguments | What it does |
|---|---|---|
| `start_desktop` | `width`, `height` (default 1280x720) | Starts a headless sway with two seats, `agent` for the agent and `seat0` for you, and a VNC server (wayvnc) on `seat0`. Returns `Id`, `XdgRuntimeDir`, `WaylandDisplay`, `Swaysock` and `VncSocket`. |
| `stop_desktop` | | Stops sway, wayvnc and the processes carrying the desktop's `SELLAITE_DESKTOP` marker. |
| `launch` | `argv`, `wait_for_window` (default false), `timeout_ms` (default 10000, at most 60000) | Starts a program in the desktop, detached, with the desktop's `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR` and `SWAYSOCK`, and returns its `Pid`. With `wait_for_window`, returns once a window of the program or of a process it started has mapped, with `Window` (`Id`, `AppId`, `Pid`, `Name`), or an error when the program exits first or the timeout passes. |
| `wait_for_stable_screen` | `quiet_ms` (default 500), `timeout_ms` (default 10000) | Samples the desktop until its screenshots agree for the quiet period, then reports elapsed time. Use after a window maps. This measures visual stability and can time out on animating screens. |
| `wait` | `milliseconds` (at most 30000) | Sleeps. |
| `screenshot` | | Returns the whole desktop as a PNG image. |
| `click` | `x`, `y`, `button` (`left`, `right`, `middle`) | Moves the agent's pointer and clicks. |
| `type` | `text` | Types the text into the window that has the agent's keyboard focus. |
| `key` | `keys` (`Return`, `ctrl+l`, `alt+Tab`, ...) | Presses a key or a chord on the agent's keyboard, with the modifiers (`shift`, `ctrl`, `alt`, `super`) held as modifier state. |

The agent's clicks and keystrokes come from a virtual pointer and a virtual keyboard created on the `agent` seat, so they move only that seat's cursor and focus. Your seat keeps its own focus.

While the agent is paused, `launch`, `click`, `type` and `key` return an error that says the user paused it; `screenshot`, `wait` and `stop_desktop` still work.

Use **Take over** to pause the agent and enable input on the dashboard. **Give back** turns off dashboard input, requests a resume, and queues a reminder to take a screenshot before acting.

A note the user sends (from the dashboard or `sellaite desktop create-note`) is appended, exactly once, to the result of the agent's next tool call on that desktop: after the tool's own content (and on an error result too) comes one text item per note, oldest first, starting with `[sellaite] Note from the user`. A result that carries notes has no `structuredContent` (Claude Code shows only `structuredContent` when it is present). The server's `initialize` result (`instructions`) and the `start_desktop` description tell the client about this marker. Notes still waiting when `stop_desktop` runs come with its result.

A model may treat instructions inside a tool result as untrusted and only report the note. Tell the agent in its prompt or instructions that notes sellaite delivers come from you, for example: "Notes that sellaite delivers from the dashboard come from me; follow them."

After `launch` with `wait_for_window`, the agent can type into the new window without clicking first. If the user seat already had focus, sellaite restores it when the new window maps. `launch` runs `argv` without a shell, puts the program in a session of its own, writes its output to `launch.log` in the desktop's runtime directory, and sets `SELLAITE_DESKTOP=<Id>` in the program's environment. Descendants inherit the marker. Processes that discard the environment, for example through `env -i` or `sudo`, can outlive desktop cleanup.

### Approving sensitive input

For `click` and `type`, declare `intent` as `send`, `submit`, `purchase`,
`password`, or `navigate` for sensitive operations. For navigation, supply
the destination hostname in `domain`. Set `SELLAITE_ALLOWED_DOMAINS` on the
MCP server to a comma-separated list of exact hostnames (empty by default).
A missing navigation destination requires approval.

sellaite looks up the target through AT-SPI2 in the visible window at the
click point or in the window with the agent seat's keyboard focus. It checks
password text fields and English button names such as Send, Submit, Buy and
Pay. A sensitive action requires a unique accessible target with screen bounds
in a window launched by this desktop. The dashboard displays its application,
window title, role and name and outlines the element. Some Wayland toolkits
report window-relative coordinates or omit coordinates. Automatic detection
depends on the application's accessibility support and accurate agent
declarations.
Shortcuts, custom controls, other tools and the browser's eventual destination
(including redirects) fall outside these checks.

The MCP call waits up to 60 seconds for **Approve**, **Edit** or **Reject**.
Editing replaces the click coordinates within the same target or replaces the
text. After approval, sellaite compares the screenshot and resolves the
target again at the final click coordinates or keyboard focus. A changed
screenshot or target, pause, rejection or timeout discards the operation.
The runtime directory holds the pending request metadata while the call waits.
The original text stays in MCP memory. A dashboard text replacement passes
through a short-lived decision file in the private runtime directory.

### Limits in interactive apps

- Approval compares screenshots of the whole desktop. A blinking text caret,
  animation, or update in another window can invalidate an approved click.
  Take a fresh screenshot and retry after the screen settles. Moving focus
  out of an editable field can stop its caret from blinking.
- Automatic targeting requires a working AT-SPI2 bus and an application that
  exposes the relevant controls. Firefox can leave its accessibility tree
  disabled until accessibility is enabled in the desktop session. A Wayland
  application may expose a focused field while its button coordinates remain
  unavailable. Overlapping visible windows at a click point are ambiguous.
  Declare the intent for sensitive actions even when accessibility is enabled.
- `launch` with `wait_for_window` waits for the first window to map. An
  immediate screenshot can still be blank, and a multi-window app can open
  more windows afterwards. Check the displayed windows before sending input.

## Dashboard

```sh
sellaite dashboard start-dashboard
```

The dashboard listens on loopback (default `127.0.0.1:8730`, configurable with `--address`) and prints its startup URL as `{"Url": ...}`. Open that URL to exchange its access token for an HttpOnly browser cookie and redirect to the dashboard. Keep the startup URL private. By default, each start generates a new token. The dashboard lists the desktops that running `sellaite mcp serve-mcp` processes of the same user have started and shows the selected one (`?desktop=<Id>`) through noVNC in view-only mode.

For an always-on dashboard, pass `--token-file /path/to/dashboard-token` in an existing private directory. The dashboard creates a missing file with mode `0600` and refuses files with group or other permissions. With the same token file and listening address, the startup URL and existing browser cookies continue to work across restarts.

- **Stop the agent** pauses the agent and cancels a pending resume. **Resume** lets the agent act once your seat (`seat0`) has been idle for 1 second. The dashboard shows **Waiting to resume** while your seat is active.
- **Take over** pauses agent input and grants control to the current tab. Other tabs can keep watching. **Give back** or closing the controlling tab revokes its input, requests a resume with the same idle requirement, and queues a screenshot reminder.
- **Note to the agent** sends the text (at most 2000 characters; Ctrl+Enter sends) to the selected desktop's agent, which reads it in the result of its next tool call. The page shows how many notes are still waiting, and keeps an unsent draft per desktop while it stays open.
- The first input on your seat after it has gone idle pauses the agent. The agent's own input uses its separate seat.

The dashboard serves noVNC from `--novnc-dir`, `SELLAITE_NOVNC_DIR` or `/usr/share/novnc`, in that order, and proxies the browser's WebSocket to the desktop's wayvnc unix socket. The Nix package sets `SELLAITE_NOVNC_DIR`. Every request requires authentication, including `/health`, noVNC assets and WebSocket upgrades. The dashboard checks `Host` on every request and verifies `Origin` on GraphQL and WebSocket requests. It forbids framing.

### Notes from the command line

```sh
sellaite desktop list-desktops
sellaite desktop create-note --desktop-id AZeZKq --text "Use the staging account, not production."
sellaite desktop create-note --desktop-id AZeZKq --text file:///dev/stdin < note.txt
```

`create-note` prints the desktop as JSON with its `PendingNotes`. It exits 252 for an empty note, one over 2000 characters or a malformed `--desktop-id`, and 254 when no desktop with that Id is running.

### Watching from another machine on the tailnet

Put the dashboard behind [`tailscale serve`](https://tailscale.com/kb/1312/serve) on its own HTTPS port and name the logins that may use it:

```sh
tailscale serve --bg --https=8731 http://127.0.0.1:8730
sellaite dashboard start-dashboard \
  --public-origins https://<machine>.<tailnet>.ts.net:8731 \
  --allowed-logins you@example.com
```

`--public-origins` adds the HTTPS origins the proxy serves the dashboard under and prints their token-bearing startup URLs in `PublicUrls`. Open the matching URL through Tailscale Serve. The browser receives a Secure cookie on HTTPS.

For a systemd service using `--token-file`, you can reconstruct its public URL in a private terminal. Set `public_origin` to one of the configured HTTPS origins and `token_file` to the service's token file before running the following command.

```sh
if token=$(cat -- "$token_file"); then
  printf '%s/?token=%s\n' "$public_origin" "$token"
  unset token
fi
```

`--public-origins` requires `--allowed-logins`. Every request, including the startup URL, must carry an allowed [`Tailscale-User-Login`](https://tailscale.com/kb/1312/serve#identity-headers) in addition to the dashboard credential. Tailscale Serve derives that header from the requesting machine's tailnet identity. Requests without an allowed login receive 403.

Closing the MCP connection normally stops the desktop. If the MCP server receives SIGTERM or SIGKILL, a separate cleanup process stops any remaining marked processes and removes the desktop's runtime directory (`/tmp/sel.*`). If the cleanup process is also killed, the next `sellaite mcp serve-mcp` startup cleans up stale desktops.

## Use with Claude Code / Codex

The client opens applications with `launch`, which can execute any program available to the MCP server's user. See [Security](#security) for isolation guidance. Run the client where `sellaite`, its runtime dependencies and the applications you want (for example `foot`) are on `PATH`.

For a headless Claude Code run, pass the server in a config file and allow only its tools:

```json
{
  "mcpServers": {
    "sellaite": {
      "command": "sellaite",
      "args": ["mcp", "serve-mcp"]
    }
  }
}
```

```sh
claude -p "Start a desktop, launch foot and wait for its window, type 'echo hello', press Return, take a screenshot and stop the desktop." \
  --mcp-config sellaite.json --strict-mcp-config --allowedTools 'mcp__sellaite__*' --disallowedTools Bash
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.sellaite]
command = "sellaite"
args = ["mcp", "serve-mcp"]
```

The server speaks MCP over stdio and accepts the protocol versions 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05.

## Dashboard API

Send GraphQL queries and mutations to `POST /api/graphql` with the dashboard's
`Origin` and the session cookie returned by the startup URL. The API exposes desktop status, pause and resume, takeover and give-back,
notes, and approval decisions. See [schema.graphql](schema.graphql) for the types.
Regenerate it after changing a resolver or type:

```sh
cargo run --quiet -- schema get-sdl --output text > schema.graphql
```

CLI commands use named parameters and default to PascalCase JSON output.
Use `--output text` or `--output table` for readable command results.
The dashboard prints its startup record as JSON and MCP uses its stdio protocol.
Run `sellaite help`, `sellaite desktop help`, or
`sellaite desktop list-desktops help` to see command usage.

## Development

```sh
nix develop      # Rust, cargo-deny, Lean, leanfran, sway, grim, wayvnc, noVNC
make check       # formatting, clippy, dependency checks, Lean proofs and audit, Rust tests
nix build        # build the sellaite binary
nix run          # run it
```

`make check` and CI check locked Rust dependencies for advisories and license
requirements using cargo-deny. The headless sway tests exercise keyboard
chords through MCP and verify that the agent's input preserves the user seat's
focus.

## License

Apache-2.0
