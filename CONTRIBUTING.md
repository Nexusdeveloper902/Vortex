# Contributing to Vortex

Thanks for helping. Read [`CLAUDE.md`](CLAUDE.md) (the project rules) and
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) before a non-trivial change.

## Build and test

System deps (Arch): `gtk4 libadwaita sqlite umu-launcher winetricks`.

```sh
cargo build --workspace
cargo run --bin vortex-launcher
cargo test --workspace                       # headless, uses a fake umu-run
# real umu-run + Proton integration tests (ignored by default; the path must be absolute)
VORTEX_IT_PROTON="/abs/path/to/Proton - Experimental" cargo test -p vortex-core -- --ignored
```

Run tests as a normal user, never as root. Tests use temp dirs, never your real XDG dirs.

## Rules that matter most

- `vortex-core` never depends on gtk4, glib, gio or libadwaita. GTK code only renders state.
- Windows programs always run through `umu-run` with absolute `PROTONPATH` and `WINEPREFIX`;
  never through a shell, never plain `wine`.
- No `unwrap`/`expect` outside tests. No new crate without a one-line reason in the PR.
- Never fake integration results: a missing tool means the test fails or is reported NOT RUN.

## Pull requests

- One branch per change, PR into `main`. Never push to `main` or force-push shared branches.
- Commits follow Conventional Commits with a scope: `feat(core/process): ...`,
  `fix(gtk/wizard): ...`, `docs: ...`. Keep them small. Releases are automated, so do not edit
  versions or `CHANGELOG.md`.
- Before opening the PR, all of these must pass:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p vortex-core -e normal | grep -E 'gtk|glib|gio|adwaita'   # must print nothing
```

- In the PR body, list what you tested as PASS / FAIL / NOT RUN (with the reason), and known
  limitations. Say which `--ignored` tests you ran.
- No DRM or anti-cheat circumvention.
