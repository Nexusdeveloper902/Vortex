# Vortex

Native Linux desktop launcher (Rust, gtk4-rs + libadwaita) that installs and runs Windows games
through Proton via UMU Launcher (`umu-run`), one isolated prefix per game. Spec: `spec.md`.

## Read first

- `docs/ARCHITECTURE.md`: modules, path kinds, umu contract, process lifecycle, schema, security.
- `docs/ROADMAP.md`: milestones, file ownership per milestone, parallel groups, acceptance tests.
- `docs/research/upstream.md` (umu/Proton/winetricks facts, [VL] = verified locally) and
  `docs/research/stack.md` (crate choices). Research is input; ARCHITECTURE wins where they differ.

## Commands

```sh
# system deps (Arch): gtk4 libadwaita sqlite umu-launcher winetricks
cargo build --workspace
cargo run --bin vortex                         # the app
cargo test -p vortex-core                      # headless, fake umu-run, no GTK needed
cargo test --workspace
VORTEX_IT_PROTON=/abs/path/to/proton cargo test -p vortex-core -- --ignored   # real umu integration
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -p vortex-core -e normal | grep -E 'gtk|glib|gio|adwaita'   # must print nothing
```

Local Proton for integration tests on the dev box:
`$HOME/.local/share/Steam/steamapps/common/Proton - Experimental` (quote it; it has spaces).

## Layout

- `crates/vortex-core`: lib. All logic: paths, db, runtime, runner, process, source, install,
  launch, winetricks, backup, delete, logs, diagnose, desktop. Plain-data events over `async-channel`.
- `crates/vortex-gtk`: bin `vortex`. UI only: renders state, forwards user intent to core.
- `crates/vortex-core/tests/`: integration tests, `fake-umu-run`, fixture installers and logs.
- `spikes/`: throwaway experiments with results in `docs/research/`. Never linked into the app.
- `packaging/`, `data/`: PKGBUILD, .desktop, metainfo, icons.

## Hard rules

1. `vortex-core` never depends on gtk4, glib, gio or libadwaita. No exceptions.
2. Never run as root, never call sudo/pkexec. The app refuses to start, the runner refuses to spawn.
3. Every Windows program runs through `umu-run` with explicit absolute `PROTONPATH` and `WINEPREFIX`.
   Never substitute plain `wine`, Proton's `proton` script, or host winetricks. The only direct Wine
   binary call allowed is Proton's own `wineserver -w` / `-k` (ARCHITECTURE section 6).
4. Never spawn through a shell. `Command` with argv arrays; launch args are a JSON array, not a string.
5. Never execute community or downloaded scripts. Winetricks verbs are validated names, never files.
6. Never delete outside managed paths. All deletion goes through `delete::DeletionPlan`, shown to the
   user and executed exactly. Never delete installer sources or `prefix_managed=0` prefixes.
7. Keep the four path kinds distinct: `HostPath`, `PrefixDir`, `WinPath` (exe), `WinPath` (cwd).
   No raw `String`/`PathBuf` paths across module APIs; convert only via `paths::to_win`/`to_host`.
   The Windows cwd is set only via `Command::current_dir(host dir)`.
8. Redact secrets: every line from a child process goes through `logs::redact` before it reaches a
   file or the UI. Never log the full environment yourself.
9. Never report an install as complete because a process exited. `installed` requires a
   user-confirmed exe that exists. Uncertain process states are shown to the user, not guessed.
10. Never fake integration results. If umu, Proton, mingw-w64-gcc or msitools is missing, the test
    fails or is reported NOT RUN with the reason. No stubs in `--ignored` tests, no silent skips.
11. Backups never follow symlinks (prefixes contain `dosdevices/z: -> /`).
12. No DRM or anti-cheat circumvention.

## Conventions

- Rust 2024, MSRV 1.92, `unsafe_code = "forbid"`. Signals and euid via `rustix`.
- Errors: core uses `vortex_core::Error` (thiserror) and `vortex_core::Result`. No `unwrap`/`expect`
  outside tests. Diagnostic findings are data (`diagnose::Finding`), not errors. GTK shows
  `Error`'s `Display` in a toast or `adw::AlertDialog`.
- Keep it simple: no trait with a single impl, no tokio, no new crate without a one-line reason in
  the PR. Fakes come from configuration: umu-run path in `Tools`, fake runtime dirs, localhost HTTP.
- Threads: std threads in core; GTK consumes events with `glib::spawn_future_local`. Never block
  the GTK thread on I/O or process waits.
- SQL lives only in `db.rs`. Schema changes = new migration file + `user_version` bump; never edit
  an applied migration. New queries are appended at the end of `db.rs`.
- Tests next to code in `#[cfg(test)] mod tests` for pure logic; process/DB/filesystem tests in
  `crates/vortex-core/tests/` using `tempfile` dirs and `Paths::under(tmp)`; never touch real XDG dirs.
- Fake umu: `crates/vortex-core/tests/fake-umu-run` (POSIX sh) driven by `FAKE_UMU_MODE`
  (`exit:N`, `sleep:S`, `lines:N`, `orphan:S`, `trap`) and recording to `FAKE_UMU_OUT`. Close the
  script's write handle before exec (ETXTBSY).
- Real-umu tests are `#[ignore]` and need `VORTEX_IT_PROTON`; say in the PR which ones you ran.
- GTK: widgets built in code (no Blueprint, no Relm4), one file per page in `vortex-gtk/src/ui/`,
  `adw::AlertDialog` for confirmations, `gtk::FileDialog` for file/folder picking.
- App id `io.github.nexusdeveloper902.Vortex` everywhere (Application, .desktop, metainfo, icons).

## Workflow

- One milestone = one branch `mN-<slug>` = one worktree = one PR into `main`:
  `git worktree add ../vortex-mN -b mN-<slug> main`.
- Edit only the files your milestone owns (ROADMAP file ownership table). Need a change elsewhere
  (e.g. `process.rs` after M7)? Make a small separate PR first.
- `Cargo.toml`/`Cargo.lock` conflicts from parallel milestones: keep both dependency lines, then
  `cargo check` to refresh the lock.
- Before PR: fmt, clippy `-D warnings`, `cargo test --workspace`, plus the milestone's acceptance
  tests. PR body lists acceptance tests as PASS / FAIL / NOT RUN (with reason) and known limitations.
- Commits: Conventional Commits with scope, e.g. `feat(core/process): add wineserver barrier`,
  `fix(gtk/wizard): ...`, `test(core/install): ...`, `docs: ...`. Small, focused commits.
- Never push to `main` directly; never force-push shared branches.
