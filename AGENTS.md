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
  validation, coordinate and geometry calculations) is defined in Lean inside
  `leanfran::lean!` blocks and generated to Rust by `leanfran-build`.
- Rust owns process management (sway, wayvnc), the Wayland protocol clients
  (virtual pointer and keyboard, screen capture), the MCP server, and the CLI
  (`clap`).
- Every generated function that Rust calls lands in the same change with a
  theorem about a property its callers rely on, and with a `#assay` line in
  `audit/`. `make check` then also runs `lake -d lean build --wfail` and
  `lake -d audit build --wfail`.

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
