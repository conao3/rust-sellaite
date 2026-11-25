# sellaite

`sellaite` gives an AI computer-use agent its own headless Wayland desktop
(sway) and its own input seat, exposed over MCP, so the agent never moves the
user's cursor or focus. See `README.md` for the goal.

## Commands

- `nix develop` — the development shell: the Rust toolchain, Lean and
  leanfran, the Wayland tools the desktop uses (`sway`, `grim`, `wayvnc`,
  `foot`; Linux only), and `SELLAITE_NOVNC_DIR` pointing at nixpkgs' noVNC.
- `make check` — the single verification entry point: `nix fmt -- --ci`,
  `cargo clippy --all-targets -- -D warnings`, the Lean proofs
  (`lake -d lean build --wfail`), the theorem audit
  (`lake -d audit build --wfail`), the check that `.leanfran` and `lean`
  match the sources, and `cargo test`. CI runs it as
  `nix develop --command make check`, followed by `nix build`.
- `make format` — format every file with treefmt (`nixfmt`, `rustfmt`).
- `nix run` — build and run the `sellaite` binary. The package is Linux
  only; on `aarch64-darwin` the flake has the devShell but no package.

## Lean and Rust

- Deterministic logic is defined in Lean inside `leanfran::lean!` blocks and
  generated to Rust by `leanfran-build` (`build.rs`). Today that is the
  keymap layout of `type` (`src/keymap.rs`) and the chord of `key`
  (`src/chord.rs`); seat and desktop state
  transitions, input validation and geometry go to Lean as they grow beyond
  the single bounds check in `SeatInput::click`. The generated sources are cached in
  `.leanfran/cache/` and the Lean package in `lean/`; commit both. The Nix
  build sets `LEANFRAN_OFFLINE=1` and uses the cache only.
- Rust owns process management (sway, wayvnc, and the programs of `launch`,
  which `src/launch.rs` finds by `SELLAITE_DESKTOP=<Id>` in `/proc/<pid>/environ`,
  waits for through sway IPC window events, and gives the Xwayland `DISPLAY`
  whose `/tmp/.X<n>-lock` holds sway's PID), the Wayland protocol clients
  (virtual pointer and keyboard, screen capture, the ext-idle-notify-v1 watcher
  of `seat0`), the MCP server, the dashboard (HTTP, the WebSocket proxy to
  wayvnc), and the CLI (`clap`).
- Pause state lives in the desktop's runtime directory. MCP input tools refuse
  while `paused` exists or the user has taken over. `src/runtime.rs` serializes
  pause and idle updates across the dashboard and MCP processes with a file lock.
- Notes are a file queue in the same way: `runtime::add_note` writes each note
  to `notes/.<name>.tmp` and renames it to `notes/<name>` (the name starts
  with the time in nanoseconds), and `runtime::take_notes` reads the names in
  order, skips dot files, and returns a note only after removing its file. The
  MCP server is the only reader of its desktop's queue; `Server::call_tool`
  takes the notes after every tool call (and before `stop_desktop`, which
  removes the directory), appends them to the result's `content`, and drops
  `structuredContent` from that result, because Claude Code shows only
  `structuredContent` when a result has it.
- Every generated function that Rust calls lands in the same change with a
  theorem about a property its callers rely on, and with a `#assay` line in
  `audit/`. `make check` runs `lake -d lean build --wfail`,
  `lake -d audit build --wfail`, and checks that `.leanfran` and `lean` match
  the sources.

### `src/keymap.rs` (`pub mod model`)

`type` uploads a keymap that binds each character of the text to its own
key. Rust calls `split`, `slots` and `evdevKey`.

- `split cap text` cuts the text into pieces whose keymaps fit in `cap` keys
  (`keymap::CAPACITY` = 247, xkb keycodes 9 to 255). Theorems: the pieces
  concatenate to the text (`flatten_split`), every piece is non-empty and
  needs at most `cap` keys (`split_fits`), and a piece ends only when its
  keymap is full and the next character is new to it (`split_chained`).
- `slots text` lists the distinct characters in order of first appearance.
  Theorems: no character appears twice (`slots_nodup`), and adding a
  character extends the slots by that character only when it is new
  (`slots_snoc`).
- `evdevKey slots c` gives the evdev code of `c`'s key. Theorems: a
  character of the text has a key exactly when it occurs in the text
  (`evdevKey_isSome`), and the key `k` of `c` is positive and slot `k - 1`
  holds `c` (`evdevKey_slot`), so distinct characters get distinct keys.
- `#assay` (`audit/Audit/Keymap.lean`) on `split`, its worker `chunksFrom`,
  `slots` and `evdevKey`: 26 mutants, 24 killed, no holes, 1 stillborn, and
  1 untested (`seen ++ [c]` turned into `[c] ++ seen`, which `chunksFrom`
  only reads by membership and length, so the output is the same).
- Outside the audit: `keysym` and `xkb_source` in Rust, which render the
  keymap as xkb text. The unit test `keymap_names_unicode_and_control_keys`
  and the headless sway test check that the compositor accepts it.

### `src/chord.rs` (`pub mod model`)

`key` parses `keys` such as `ctrl+l` or `alt+Tab`, uploads a keymap with the
key at keycode 9 and the modifier keys bound to `Shift`, `Control`, `Mod1`
and `Mod4`, sends the modifier mask with `zwp_virtual_keyboard_v1.modifiers`,
presses and releases the key, and clears the mask. Rust calls `chord`,
`mask` and `Modifier.xkbName`.

- `chord text` splits the text on `+` (`pieces`) and gives the modifier
  parts and the last part as the key. Theorems: when it succeeds the parts
  joined with `+` give the text back and every part is non-empty and has no
  `+` (`chord_some`), and it fails exactly when a part is empty
  (`chord_none`). The `+` key is written `plus`.
- `mask mods` ORs the real-modifier bits (`Modifier.bit`: Shift 0, Control
  2, Mod1 3, Mod4 6). Theorems: bit `i` of the mask is set exactly when a
  modifier in the list has bit `i` (`testBit_mask`), distinct modifiers
  have distinct bits (`bit_injective`), and bit `m.bit` is the xkb real
  modifier named `m.xkbName` in the order Shift, Lock, Control, Mod1 to
  Mod5 (`bit_xkbName`). `keymap::chord_source` writes `m.xkbName` in the
  `modifier_map` of each modifier key, so the mask and the keymap agree.
- `#assay` (`audit/Audit/Chord.lean`) on `chord`, `piecesFrom`, `pieces`,
  `mask`, `Modifier.bit` and `Modifier.xkbName`: 57 mutants, 53 killed, no
  holes, 3 stillborn, and 1 reported equivalent (`ps.any` turned into
  `ps.all` in `chord`), which differs only on texts with a `+` beside an
  empty part; random inputs rarely hold a `+`, and the unit test
  `rejects_empty_parts_and_unknown_names` rejects `ctrl+` and `+l`.
- Outside the audit: the modifier and key name tables in Rust (`modifier`,
  `named_key`), and the rest of `keymap::chord_source`. The unit tests in `src/chord.rs`
  and `src/keymap.rs` and the headless sway test check them.

## Desktops, the dashboard and pause

- Each desktop is a runtime directory `/tmp/sel.<Id>` (`src/runtime.rs`)
  holding sway's sockets, `sellaite.json` (the MCP server's `Pid`, the size and
  `WaylandDisplay`), wayvnc's `vnc.sock`, the files `paused` and
  `user-idle`, and the `notes` directory. `sellaite dashboard serve` finds desktops by scanning these
  directories, so one dashboard serves every MCP server of the user.
- `src/idle.rs` binds `ext_idle_notifier_v1` (version 2 uses
  `get_input_idle_notification`) on `seat0` with a 1 second timeout. `idled`
  writes `user-idle` and completes a pending resume. `resumed` writes `paused`
  and clears `user-idle`. Resume and Give back wait for idle when the seat is
  active. Stop cancels a pending resume.
- `runtime::remove_stale` (run when `mcp serve` starts) removes a directory
  whose `sellaite.json` names a dead process, or that has no `sellaite.json`,
  is older than 60 s and has no Wayland socket that accepts a connection.
- `src/dashboard.rs` serves the dashboard, noVNC static files, the VNC
  WebSocket, and `GET /health` with axum. Its single API, `POST /api/graphql`,
  serves desktop queries and control, note and approval mutations from
  `src/graphql.rs`. Every request checks Host and any configured Tailscale
  login. GraphQL requests and the WebSocket also require a matching Origin.
  Export the schema with `cargo run --quiet -- schema get-sdl --output text`.
  `make check` verifies the export against `schema.graphql`.

## Conventions

- Edition 2024; `rustfmt.toml` pins `style_edition = "2024"`.
- The CLI follows the AWS CLI grammar: `sellaite <command> <subcommand>
  --kebab-case-parameter value`, no positional arguments, JSON output by
  default with PascalCase keys. Exit codes: 0 on success, 252 for a usage
  error, 254 when the target is missing (no running desktop with the given
  Id), 255 for any other failure.
- Commit messages, comments, test names and documentation are in English.
- `.github/workflows/actions.lock` pins every action. Regenerate it with
  `gh actions-lock --no-interactive` in the same commit as any `uses:` change,
  and confirm `gh actions-lock --verify` exits 0.

## Testing headless sway

- The tests start headless sway (`WLR_BACKENDS=headless`), which needs no
  display.
- Put `XDG_RUNTIME_DIR` under a short path (`mktemp -d /tmp/sel.XXXX`): the
  Wayland socket path is limited to 108 bytes and sway fails to start with a
  long runtime directory.
- `tests/two_seats.rs` drives the real path: it starts `sellaite mcp serve`,
  opens two `foot` terminals in the desktop, clicks, types and presses keys
  through MCP, and checks with `swaymsg -t get_seats` that `seat0` keeps its
  focus. `ctrl+l` and `Return` must arrive in the agent's terminal as `^L`
  and a newline, and `alt+Tab`, bound in sway to `focus right`, must move
  only the agent seat's focus. `nix build`
  runs it in the sandbox, so `nativeCheckInputs` carries `sway-unwrapped`,
  `grim` and `foot`, and `FONTCONFIG_FILE` points at a fixed font set.
- `tests/launch.rs` launches foot with `wait_for_window`, checks that the
  window has the agent seat's focus, types a command and presses `Return`
  with no sleep, and checks that `stop_desktop` ends foot, its shell and an
  orphaned `sleep`. Typing without the window wait loses the text, and the
  stop check fails when `stop_desktop` signals only the direct children.
- `tests/launch.rs` also launches `xterm` with `wait_for_window` and checks
  that the X11 window maps with class `XTerm` and the agent seat's focus,
  and that its `DISPLAY` is the Xwayland display whose `/tmp/.X<n>-lock`
  holds the desktop's sway PID, not the `DISPLAY` of the MCP server. It
  fails when `launch` does not set `DISPLAY`, and it is skipped only when
  `Xwayland` is not on `PATH`. The devShell and `nativeCheckInputs` carry
  `xwayland` and `xterm`.
- `tests/dashboard.rs` checks pause end to end: stale runtime directories
  are removed when `mcp serve` starts, the agent's `click`, `type` and `key`
  leave `seat0` idle, a raw RFB client on `vnc.sock` that only connects does
  not pause, its pointer event pauses the agent, the input tools then return
  the pause error, and the dashboard's Resume and Stop work (and refuse a
  foreign `Origin` or `Host`). It fails when the agent's devices are bound to
  `seat0`, when the watcher is bound to `agent`, and when the pause check is
  removed. `nix build` runs it too, so `nativeCheckInputs` carries `wayvnc`
  and the build sets `SELLAITE_NOVNC_DIR`.
- `tests/dashboard.rs` also checks notes: with two desktops running, notes
  sent through `sellaite desktop create-note` (text and `file://`) and the
  dashboard arrive in order with the next tool call of their desktop only, a
  second call gets none, a refused (paused) `click`, a `screenshot` and
  `stop_desktop` carry the notes sent before them, a `launch` result with
  notes has no `structuredContent`, and a foreign `Origin`,
  an empty note and an unknown Id are refused (403, a GraphQL error, and CLI exit codes 252 and 254). It
  fails when a taken note is not removed, when error results get no notes, and
  when `stop_desktop` does not take the notes before the desktop stops.
- `tests/dashboard_access.rs` needs no desktop: it checks that with
  `--public-origins` and `--allowed-logins` a request without an allowed
  `Tailscale-User-Login`, from a foreign `Host` or with a foreign `Origin` gets
  403 (loopback included), that an authorized GraphQL request reaches body extraction, and
  that `--public-origins` without `--allowed-logins` is a usage error. It fails
  when the login check is removed.
- The flake uses `sway-unwrapped`: the nixpkgs `sway` wrapper starts
  `dbus-run-session`, which fails where no D-Bus daemon is installed.
