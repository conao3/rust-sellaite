# sellaite

`sellaite` gives an AI computer-use agent its own headless Wayland desktop
(sway) and its own input seat, exposed over MCP, so the agent never moves the
user's cursor or focus. See `README.md` for the goal.

## Commands

- `nix develop` — the development shell: the Rust toolchain, Lean and
  leanfran, and the Wayland tools the desktop uses (`sway`, `grim`, `wayvnc`,
  `foot`; Linux only).
- `make check` — the single verification entry point: `nix fmt -- --ci`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test`. CI runs it as
  `nix develop --command make check`, followed by `nix build`.
- `make format` — format every file with treefmt (`nixfmt`, `rustfmt`).
- `nix run` — build and run the `sellaite` binary.

## Lean and Rust

- Deterministic logic (seat and desktop state transitions, input
  validation, coordinate and geometry calculations, keymap layout) is defined
  in Lean inside `leanfran::lean!` blocks and generated to Rust by
  `leanfran-build` (`build.rs`). The generated sources are cached in
  `.leanfran/cache/` and the Lean package in `lean/`; commit both. The Nix
  build sets `LEANFRAN_OFFLINE=1` and uses the cache only.
- Rust owns process management (sway, wayvnc), the Wayland protocol clients
  (virtual pointer and keyboard, screen capture), the MCP server, and the CLI
  (`clap`).
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

## Conventions

- Edition 2024; `rustfmt.toml` pins `style_edition = "2024"`.
- The CLI follows the AWS CLI grammar: `sellaite <command> <subcommand>
  --kebab-case-parameter value`, no positional arguments, JSON output by
  default with PascalCase keys.
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
  opens two `foot` terminals in the desktop, clicks and types through MCP, and
  checks with `swaymsg -t get_seats` that `seat0` keeps its focus. `nix build`
  runs it in the sandbox, so `nativeCheckInputs` carries `sway-unwrapped`,
  `grim` and `foot`, and `FONTCONFIG_FILE` points at a fixed font set.
- The flake uses `sway-unwrapped`: the nixpkgs `sway` wrapper starts
  `dbus-run-session`, which fails where no D-Bus daemon is installed.
