# Vortex stack research (2026-11-09)

Verified on this machine: rustc/cargo 1.95.0 (rustup, shadows pacman rust 1.99.1), gtk4 4.24.1, libadwaita 1.10.0, glib 2.90.1, umu-launcher 1.4.4, flatpak present. **blueprint-compiler NOT installed.** Crate versions from crates.io on 2026-11-09. Items marked (?) are unverified.

## Summary table
| Area | Pick |
|---|---|
| GUI | plain `gtk4` 0.11.5 + `libadwaita` 0.9.2, widgets in code, no Blueprint |
| Async | `glib::spawn_future_local` + `gio::Subprocess` (no tokio) |
| Persistence | `rusqlite` 0.41.1 `bundled` off (system sqlite) + `PRAGMA user_version` migrations |
| XDG | `directories` 6.0.0 for paths; hand-write .desktop files |
| HTTP | `ureq` 3.5.1 (blocking, in a `std::thread`), `flate2` 1.1.11, `tar` 0.4.46, `sha2` 0.11.1 |
| Layout | workspace: `vortex-core` (lib, no GTK) + `vortex-gtk` (bin) |
| Packaging | Arch PKGBUILD native; Flatpak deferred |
| CI | GitHub Actions in `archlinux:latest` container |

## 1. GUI: plain gtk4-rs vs Relm4
- Crates: `gtk4 = { version = "0.11.5", features = ["v4_20"] }`, `libadwaita = { version = "0.9.2", features = ["v1_8"] }`. Both crates require rustc >= 1.92 (gtk4 0.11.5 declares 1.92); we have 1.95.
- Feature flags: gtk4 0.11 has `v4_2 ... v4_24` (v4_24 exists, so system 4.24.1 is fully coverable). libadwaita 0.9.2 has `v1_1 ... v1_10`. System supports `v4_24` + `v1_10`. Recommendation is to **enable lower: `v4_20` + `v1_8`**, because Arch is rolling but the CI container / other distros (Ubuntu 24.04 has gtk 4.14, adw 1.5) lag. Pick the lowest flags that give the widgets used: `AdwNavigationSplitView`/`AdwOverlaySplitView` (adw 1.4, `v1_4`), `AdwDialog`/`AdwAlertDialog`/`AdwSpinner`/`AdwToolbarView` (1.5-1.6, `v1_6`), `AdwBottomSheet` (1.6). Practical: `v4_14` + `v1_6` runs on Ubuntu 24.10+/Fedora; use `v4_20`+`v1_8` if Arch-only for MVP. Raising later is cheap, lowering is not (code uses gated APIs). Do not enable features beyond what is needed.
- `AdwDialog` family (`adw::Dialog`, `adw::AlertDialog`) replaces deprecated `MessageDialog`/`gtk::Dialog`; use `adw::AlertDialog` for confirmations (uninstall), `adw::Dialog`/`AdwNavigationView` for install wizard. File choosing: `gtk::FileDialog` (4.10+, async, `select_folder`, `open`) — portal-aware.
- Relm4 0.11.0 exists. Pros: declarative `view!`, message-passing state. Cons: macro-heavy, extra abstraction layer, pins its own gtk4/adw versions (check they match 0.11/0.9 (?)), harder for LLM agents to get compile-clean (macro errors are opaque), more breaking releases.
- **Recommendation: plain gtk4-rs + libadwaita.** Pattern: `mod ui` with one file per page, subclassing only where state needs it (`glib::Object` subclasses for `GameObject` list-model items via `gio::ListStore` + `gtk::GridView`/`ListView`+`SignalListItemFactory`). Otherwise build widgets with the builder pattern (`gtk::Box::builder()...build()`) and capture state in `Rc<RefCell<..>>` / `glib::clone!`. Single `Rc<AppState>` handle holding core `Library` + channels.
- **Blueprint: skip.** Not installed (`pacman -S blueprint-compiler` needed, adds build step, meson-ish integration, and a second language for agents). Builder code is greppable, compile-checked, and testable. Revisit only if UI count > ~15 complex templates. If ever used, ship compiled `.ui` via `gtk::CompositeTemplate`+`gio::resources` and run `blueprint-compiler` in build.rs (?).
- Wayland: GTK4 defaults to Wayland when `WAYLAND_DISPLAY` set; no code needed. Don't set `GDK_BACKEND`.
- App ID: `io.github.nexusdeveloper902.Vortex` (lowercase user part is the convention for GitHub IDs; must match .desktop, metainfo, `adw::Application::builder().application_id(..)`, icon name).

## 2. Async + process management
Options: (a) `MainContext::spawn_local` + `gio::Subprocess`; (b) `async-process` 2.5.0 under glib executor; (c) tokio on side thread + channels.
- **Recommend (a).** glib 0.22.10: `glib::spawn_future_local(async { ... })` runs on the GTK thread; `gio::Subprocess` (via `gio::SubprocessLauncher`) integrates with the glib main loop, gives `wait_future()`, `force_exit()`, `send_signal(sig)`, `stdout_pipe()`/`stderr_pipe()` as `gio::InputStream`. Line reading: wrap in `gio::DataInputStream::new(stream)` and loop `read_line_future(Priority::DEFAULT).await` (returns `Result<Option<GString>>`-ish; (?) exact return type `(glib::GString)`/empty on EOF, check docs.rs for 0.22). No extra dependency, no cross-thread marshalling, no `Send` bounds on widgets.
- Launcher setup: `launcher.set_flags(STDOUT_PIPE | STDERR_PIPE)`, `setenv(k, v, true)` per game, `set_cwd(dir)` (needed for installer working-dir spec), `spawnv(&[...])`. Use `launcher.set_flags(...)` only; for stderr merged into stdout use `STDERR_MERGE`.
- Process group kill for "Stop": gio Subprocess does not setsid. Use `std::os::unix::process::CommandExt::process_group(0)` (stdlib) only if using std Command. With gio, `Subprocess::send_signal(SIGTERM)` hits only the umu-run pid; Wine children survive. Mitigation: run umu-run via `setsid`-less wrapper? Simplest: use `launcher.set_child_setup`? (not safe in Rust). **Alternative: drive `std::process::Command` with `.process_group(0)` + a reader thread per pipe sending lines through `async-channel`/`glib::MainContext::channel`-style (`async_channel` crate 2.x) to a `spawn_future_local` consumer, and kill with `libc::killpg`/`nix::sys::signal::killpg`.** This is the more reliable choice for Stop. (Uncertain which is better; test both. Recommendation: **std::process::Command + process_group(0) + 2 reader threads + `async-channel`**, waiting thread sends `Exit(status)`. Still no tokio.)
  - Final pick: std `Command` + threads + `async-channel` 2.x; it gives killpg, is trivially unit-testable from core without glib, and keeps `vortex-core` GTK-free. `gio::Subprocess` would force core to depend on glib.
- tokio: reject — extra ~30 deps, needs runtime bridge; nothing here needs high concurrency (a handful of processes + one download).
- Also: `umu-run` blocks until the Wine session ends; use `PR_SET_PDEATHSIG` not required. Set `PROTONPATH`, `WINEPREFIX`, `GAMEID` env (umu 1.4.4).
- Log view: append lines into `gtk::TextBuffer` batched (e.g. coalesce per 50ms via `glib::timeout_add_local`) and also tee to a log file under `$XDG_DATA_HOME/vortex/logs/<game>/<ts>.log` from the reader thread (so logs survive UI crash).

## 3. Persistence
- **Recommend `rusqlite` 0.41.1** (MSRV 1.89) without `bundled` on Arch (system `sqlite` is always present; avoids C compile). Enable `bundled` only for Flatpak/CI portability (?). Single file `$XDG_DATA_HOME/vortex/vortex.db`.
- Why not TOML/JSON: relational data (games, environments, runtimes, dependencies, backups) with references; crash safety needs atomic multi-record writes. SQLite gives transactions + `journal_mode=WAL`, `synchronous=NORMAL`/`FULL`.
- Crash-safe install state: `installs`/`games.status` enum `installing | installed | failed | interrupted`. Write row with `installing` + install_id BEFORE spawning; transition to `installed` only in a single transaction after verified exit code 0 AND selected-exe-exists check. On startup: `UPDATE ... SET status='interrupted' WHERE status='installing'` (no live pid recorded or pid dead; store pid + start time for liveness). UI shows interrupted with Resume/Remove; never lists as launchable. Staging dirs are named by install_id for cleanup.
- Migrations: `PRAGMA user_version` + ordered `const MIGRATIONS: &[&str]` applied in a transaction each (`BEGIN; ...; PRAGMA user_version=N; COMMIT`). Simple, no crate. Alternative `rusqlite_migration` 2.x (?) does the same with the same mechanism; skip as unneeded. Take a file copy `vortex.db.bak-vN` before a migration. Refuse to open DB with `user_version` > known (downgrade guard).
- `PRAGMA foreign_keys=ON` per connection. Keep DB access on one thread (core `Store` owned behind `Mutex` or accessed on GTK thread; queries are tiny). Use `rusqlite::params!`; store env vars as JSON text column (`serde_json` 1.x) for flexibility.

## 4. XDG
- **`directories` 6.0.0**: `ProjectDirs::from("io.github", "nexusdeveloper902", "Vortex")`? That yields dir `Vortex` / app-id-ish naming varying by platform; on Linux `ProjectDirs` uses only the application arg lowercased (`vortex`). Use `ProjectDirs::from("", "", "vortex")` -> `~/.local/share/vortex`, `~/.config/vortex`, `~/.cache/vortex`. `BaseDirs::data_dir()` for `~/.local/share/applications`. It honors `XDG_*` env vars (needed for test isolation by setting env or, better, by passing explicit root paths into core: `Paths { data, config, cache }` struct with `Paths::from_xdg()` and `Paths::under(tmp)`). Prefer `xdg` crate only if you need `XDG_RUNTIME_DIR`/config search paths; not needed.
- Store in `$XDG_DATA_HOME/vortex/{vortex.db,runtimes/,prefixes/,logs/}`. Runtimes dir: umu also searches `~/.local/share/Steam/compatibilitytools.d` and `$XDG_DATA_HOME/umu/` (?) — spec 3 covers discovery; put GE-Proton in `$XDG_DATA_HOME/vortex/runtimes/GE-ProtonX-Y` and pass `PROTONPATH`.
- .desktop for each game, file `~/.local/share/applications/io.github.nexusdeveloper902.Vortex.game-<id>.desktop`:
  ```
  [Desktop Entry]
  Type=Application
  Name=<Game>
  Exec=vortex --launch <id>      # absolute path to binary; escape per Desktop Entry spec (quote %, \ ", spaces)
  Icon=<abs path to png or icon name>
  Categories=Game;
  StartupWMClass=io.github.nexusdeveloper902.Vortex   # (?) Wayland matches app_id; the launching process sets it
  X-Vortex-Game-Id=<id>
  ```
  After write: `update-desktop-database ~/.local/share/applications` (optional; only needed for MimeType entries). Write atomically (temp + rename), set 0644; on uninstall delete file. Validate with `desktop-file-validate` in tests if installed. Use `gio::DesktopAppInfo` only to read back. Single-instance: `--launch` goes through `gio::Application` `HANDLES_COMMAND_LINE`/action `app.launch(id)` so the existing instance handles it.

## 5. HTTP, archives, hashes
- **`ureq` 3.5.1** (blocking, rustls default; ~20 deps). Run in `std::thread::spawn`, progress via `async-channel` to UI. reqwest pulls tokio/hyper - rejected. Use `ureq::Agent` with timeouts (`timeout_global`). GitHub API: `GET https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases/latest` (needs `User-Agent`); assets `GE-ProtonX-Y.tar.gz` and `GE-ProtonX-Y.sha512sum`. JSON parse with `serde`/`serde_json`.
- Verify: stream download to `*.part`, hash while writing with `sha2::Sha512` (0.11.1; note 0.11 uses `digest` 0.11 API: `Sha512::new()`, `Digest::update`, `finalize()` -> hex via `hex`/manual `{:02x}`; check API change from 0.10 (?)). Compare to the sha512sum file ("<hash>  <name>"). Fail closed on mismatch; only then extract.
- Extract: `flate2::read::GzDecoder` + `tar::Archive::unpack` into `runtimes/.tmp-<name>/` then atomic `rename` to final name (so partial extracts never look like valid runtimes). tar 0.4.46 sanitizes `..` paths in `unpack`; keep `set_preserve_permissions(true)` (Proton needs exec bits). Check for top-level dir name equals expected.
- Add `serde` 1.x + `serde_json` 1.x. `anyhow` 1.x for the binary, `thiserror` 2.x for core errors. `tempfile` 3.27.0 (dev-dep).

## 6. Testing
- Workspace:
  ```
  Cargo.toml            [workspace] members=["crates/*"], resolver="3"
  crates/vortex-core/   lib: paths, store(db), runtime discovery/download, umu command builder, process runner, desktop entries, install state machine
  crates/vortex-gtk/    bin: all gtk4/libadwaita code; depends on vortex-core
  ```
  Rule: core never imports gtk/glib/gio. Core exposes plain-data events (`enum InstallEvent`) over `async_channel`/`std::sync::mpsc`; GTK crate maps them to widgets. `cargo test -p vortex-core` needs no display and no GTK dev libs (also lets CI test core on a bare Rust image).
- GTK crate tests: keep logic out of it; for smoke, `gtk::init()` requires display (use `xvfb-run`/`weston --headless` only if ever needed). `cargo clippy -p vortex-gtk` still needs gtk dev libs.
- Fake `umu-run`: core's runner takes the program path in a config (`Umu { bin: PathBuf }`). Tests write a shell script into a `tempfile::tempdir()` (chmod 755) that prints env (`PROTONPATH`, `WINEPREFIX`, `GAMEID`), args, `$PWD`, emits stdout/stderr lines, sleeps, or `exit N`; test assertions: working dir, env propagation, line streaming order, exit code mapping, kill (script traps TERM, spawns `sleep 300 &` child to verify killpg), and "interrupted install never marked installed". Marker `#!/bin/sh`. Avoid `ETXTBSY`: write file, `sync`/drop handle before exec (known flake on Linux when writing and exec'ing from threads).
- DB tests: `rusqlite::Connection::open_in_memory()` plus a file-based test for migration (open v1 fixture, migrate, assert). Crash sim: insert `installing` row, reopen store, assert becomes `interrupted`.
- Download tests: serve from `std::net::TcpListener` local fake (no extra dep) or inject a `trait Fetch` (one impl + test fake is justified here). Hash/extract tests build tiny tar.gz in-memory with `tar::Builder`.
- Use `insta`? No. Plain `assert_eq!`.

## 7. Packaging
- Arch PKGBUILD (git or tagged release):
  ```
  pkgname=vortex  pkgver=0.1.0  pkgrel=1
  arch=(x86_64)  license=(GPL-3.0-or-later)   # (?) license undecided
  depends=(gtk4 libadwaita sqlite umu-launcher)   # umu-launcher in AUR/extra (?) check; on this box /usr/bin/umu-run exists
  makedepends=(cargo)  optional: optdepends=('winetricks: dependency installs')
  prepare(){ cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"; }
  build(){ export RUSTUP_TOOLCHAIN=stable CARGO_TARGET_DIR=target; cargo build --frozen --release -p vortex-gtk; }
  check(){ cargo test --frozen -p vortex-core; }
  package(){ install -Dm755 target/release/vortex -t "$pkgdir/usr/bin"
    install -Dm644 data/io.github.nexusdeveloper902.Vortex.desktop -t "$pkgdir/usr/share/applications"
    install -Dm644 data/io.github.nexusdeveloper902.Vortex.metainfo.xml -t "$pkgdir/usr/share/metainfo"
    install -Dm644 data/icons/hicolor/scalable/apps/io.github.nexusdeveloper902.Vortex.svg -t "$pkgdir/usr/share/icons/hicolor/scalable/apps"
    install -Dm644 data/icons/hicolor/symbolic/apps/io.github.nexusdeveloper902.Vortex-symbolic.svg -t "$pkgdir/usr/share/icons/hicolor/symbolic/apps" }
  ```
  Binary name `vortex` (cargo `[[bin]] name`); the package name is free of "-git" for tagged releases. Commit `Cargo.lock`. Validate: `appstreamcli validate --pedantic`, `desktop-file-validate`, `namcap`.
- Metainfo: `<component type="desktop-application">`, `<id>io.github.nexusdeveloper902.Vortex</id>`, `metadata_license` CC0-1.0, `project_license`, summary, description, `<launchable type="desktop-id">...desktop</launchable>`, `<provides><binary>vortex</binary></provides>`, `<content_rating type="oars-1.1"/>`, releases list, `<url type="homepage">`. Install dir `/usr/share/metainfo/`.
- Flatpak feasibility: **defer; not MVP.** The sandbox cannot run host `umu-run`; to run it you need `--talk-name=org.freedesktop.Flatpak` and `flatpak-spawn --host umu-run ...` (this escapes the sandbox entirely, and Flathub rejects/frowns on it). Alternatively bundle umu inside the Flatpak: then Proton runs nested in bwrap/pressure-vessel -> needs `--device=all`, `--share=network`, nested sandbox (`--allow=devel`/`--talk-name`), and Steam Runtime container conflicts (pressure-vessel in Flatpak is only workable with Steam's own flatpak patches) (?). Heroic and Bottles ship Flatpaks with broad perms (`--filesystem=...`, `--device=all`). Implications: `flatpak-spawn --host` means env/cwd/paths differ (host paths, stdout streaming works but process-group kill must be via `flatpak-spawn --host` signalling; `--forward-fd`), and process tracking is hard. Keep the runner behind `Umu { bin, wrapper: Option<Vec<String>> }` so a `flatpak-spawn --host` prefix can be added later. Also use XDG portals for file chooser (`gtk::FileDialog` does this automatically).

## 8. Tooling and CI
- `rustfmt.toml`: `edition = "2024"`, `max_width = 100`, `use_small_heuristics = "Max"` optional. Workspace Cargo.toml: `[workspace.package] edition="2024" rust-version="1.92"`; `[workspace.lints.clippy] all = {level="warn",priority=-1}`, `pedantic` NOT enabled wholesale (noisy with gtk-rs closures); allow `needless_pass_by_value` only if pedantic used. `[workspace.lints.rust] unsafe_code = "forbid"` for core (gtk crate uses none either; `libc`/`nix` killpg is safe via `nix` crate 0.30 (?) or `rustix` — avoid `unsafe`). CI gate: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p vortex-core`.
- GitHub Actions: Ubuntu runners lag (ubuntu-24.04: gtk 4.14, adw 1.5) -> would force `v4_14`/`v1_5` flags and fail with `v4_20`/`v1_8`. Use an Arch container:
  ```yaml
  name: ci
  on: [push, pull_request]
  jobs:
    check:
      runs-on: ubuntu-latest
      container: archlinux:latest
      steps:
        - run: pacman -Syu --noconfirm base-devel rust gtk4 libadwaita sqlite git
        - uses: actions/checkout@v4
        - run: cargo fmt --all --check
        - run: cargo clippy --workspace --all-targets --locked -- -D warnings
        - run: cargo test --workspace --locked
  ```
  Pacman `rust` (1.99.1) is fine (>=1.92); add `clippy`/`rustfmt` come with `rust` package on Arch. `actions/checkout` inside minimal container needs `git` installed first (done) (?). Cache `~/.cargo` and `target` with `Swatinem/rust-cache@v2` (?) after toolchain install. Optional second job on `ubuntu-latest` testing only `vortex-core` (no GTK deps) to prove portability.
- Dev: `cargo install cargo-deny` optional (skip for MVP).

## Open items / uncertain
1. Exact return type of `DataInputStream::read_line_future` in gtk-rs 0.22 (only relevant if gio route chosen; recommended route avoids it).
2. `sha2` 0.11 API delta from 0.10 (check docs.rs before coding).
3. License choice, umu-launcher package availability for PKGBUILD `depends`.
4. Whether Relm4 0.11.0 pins gtk4 0.11 / libadwaita 0.9 (irrelevant if plain gtk-rs chosen).
5. GitHub Actions Arch container + checkout@v4 (node runtime in container) not tested here.
