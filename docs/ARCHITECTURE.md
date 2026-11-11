# Vortex architecture

Status: Phase 0 proposal (spec section 19). Inputs: `spec.md`, `docs/research/upstream.md`,
`docs/research/stack.md`. Items marked **[SPIKE]** are unverified and are settled by milestone M1
(`docs/ROADMAP.md`); the code must not depend on them until M1 reports.

## 1. Decisions at a glance

| Topic | Decision |
|---|---|
| Language / UI | Rust (edition 2024, MSRV 1.92), `gtk4` 0.11 + `libadwaita` 0.9, widgets built in code, no Blueprint, no Relm4 |
| Crates | `vortex-core` (lib, no GTK/glib/gio, headless-testable), `vortex-gtk` (bin `vortex`, UI only) |
| Execution | Always `umu-run` with explicit absolute `PROTONPATH` and `WINEPREFIX`. Never plain `wine`, never a shell |
| Processes | `std::process::Command` + reader threads; no tokio, no `gio::Subprocess` |
| Completion | umu-run exit, then Proton's own `wineserver -w`, then `/proc/*/environ` marker scan (section 6) |
| Stop | `wineserver -k`, then SIGTERM umu-run, then SIGKILL remaining marker processes (section 6) |
| Storage | SQLite via `rusqlite` (system lib), `PRAGMA user_version` migrations, WAL |
| UI bridge | core emits plain-data events on `async-channel`; GTK consumes with `glib::spawn_future_local` |
| Downloads | Vortex downloads GE-Proton itself (`ureq`, sha512 verify, atomic rename); umu never downloads Proton |
| Packaging | Arch PKGBUILD first. Flatpak out of scope (host `umu-run` + shared `/tmp` are required) |

Dependencies (whole project): core: `rusqlite`, `serde`, `serde_json`, `thiserror`, `async-channel`,
`ureq`, `flate2`, `tar`, `sha2`, `directories`, `rustix` (signals, euid; keeps `unsafe` out);
dev: `tempfile`. gtk: `gtk4` (feature `v4_14`), `libadwaita` (feature `v1_6`), `vortex-core`.
Adding any other crate needs a one-line justification in the PR. Raise GTK/adw feature levels only
when a widget requires it.

## 2. Workspace layout

```
Cargo.toml                       [workspace] members = ["crates/*"], resolver = "3"
                                 [workspace.lints.rust] unsafe_code = "forbid"
crates/vortex-core/
  src/lib.rs                     module declarations only
  src/error.rs                   Error, Result
  src/paths.rs                   XDG dirs, managed roots, path kinds (HostPath, PrefixDir, WinPath)
  src/db.rs                      Db, schema, migrations, recovery queries
  src/migrations/0001_init.sql
  src/migrations/0002_artwork.sql
  src/runtime.rs                 Proton discovery + validation
  src/runtime_ge.rs              GE-Proton release lookup, download, verify, extract
  src/runner.rs                  Invocation -> umu-run Command (pure, no spawning)
  src/process.rs                 spawn, pipe readers, completion barriers, /proc scan, stop
  src/source.rs                  installer source analysis and candidate ranking
  src/install.rs                 install state machine, crash recovery, exe candidates
  src/launch.rs                  launch a game, run arbitrary exe/msi in an environment
  src/winetricks.rs              verb list/parse, installed verbs, per-verb runs
  src/backup.rs                  tar.gz backup/restore of a prefix
  src/delete.rs                  deletion plans restricted to managed paths
  src/logs.rs                    log files, redaction
  src/diagnose.rs                failure classification from exit status + log lines
  src/desktop.rs                 XDG .desktop entries per game
  src/artwork.rs                 cover art: Steam store search + CDN lookup, custom image, cache
  tests/fake-umu-run             POSIX sh fake of umu-run (see section 13)
  tests/*.rs                     integration tests (fake umu by default, real umu with --ignored)
  tests/fixtures/installers/     .bat installers, C sources for mingw-built test installers
crates/vortex-gtk/
  src/main.rs                    root check, adw::Application, --launch <id> command line
  src/app.rs                     AppState (core handles, event pumps)
  src/game_object.rs             glib::Object subclass for gio::ListStore items
  src/ui/library.rs              library grid/list, search, sort
  src/ui/game_page.rs            details + actions (play, stop, configure, run exe, ...)
  src/ui/install_wizard.rs       source -> candidate -> name/location/runtime -> run -> exe
  src/ui/run_view.rs             live log, process list, "still running" state, Stop
  src/ui/runtimes.rs             runtime list, GE download with progress, default runtime
  src/ui/settings.rs             global defaults
  src/ui/dialogs.rs              confirmations (deletion list), errors, diagnostics
data/                            .desktop, metainfo, icons (app itself)
packaging/arch/PKGBUILD
spikes/                          M1 throwaway scripts + results (never linked into the app)
```

Rule: `vortex-core` never depends on `gtk4`, `glib`, `gio` or `libadwaita`. `cargo tree -p vortex-core`
must not list them (CI checks this). All decisions, validation and filesystem effects live in core;
`vortex-gtk` only renders state and forwards user intent.

## 3. Core module boundaries

| Module | Owns | Key API (sketch) |
|---|---|---|
| `paths` | XDG roots, managed roots, path-kind conversions | `Paths::from_xdg()`, `Paths::under(root)` (tests), `to_win(&PrefixDir, &HostPath) -> WinPath`, `to_host(&PrefixDir, &WinPath) -> Option<HostPath>` |
| `db` | the only SQL in the codebase | `Db::open(&Path)`, typed fns per table (`insert_game`, `set_install_state`, ...), `recover_on_startup()` |
| `runtime` | find and validate Proton dirs | `discover(&Paths) -> Vec<Runtime>`, `validate(&Path) -> Result<Runtime>`, `Runtime::wineserver()` |
| `runtime_ge` | GE-Proton install | `latest_release()`, `install(release, &Paths, progress: Sender<Progress>, cancel: &AtomicBool)` |
| `runner` | translating an `Invocation` into argv/env/cwd | `Invocation { program: Program::{Exe(HostPath), System(SystemExe), Winetricks}, args, cwd, prefix, runtime, env, verbose_log_dir }`, `preflight()`, `command(&Tools, marker) -> Command` (pure; unit-tested by inspecting the Command) |
| `process` | lifetime of one run | `Runs::new(Paths, Tools, Arc<Mutex<Db>>)`, `Runs::start(RunSpec) -> (RunHandle, Receiver<RunEvent>)`, `RunHandle::{stop, mark_finished}`, `Runs::stop_detached(run_id)`, `scan_marker(&str) -> Vec<ProcInfo>` |
| `source` | file vs dir analysis, ranking | `analyze(&HostPath) -> SourceAnalysis { kind, candidates: Vec<Candidate{path, score, reasons}>, ambiguous }` |
| `install` | install state machine and recovery | `Install::begin(..)`, `advance(..)`, `exe_candidates(game) -> Vec<HostPath>`, `confirm_exe(..)` |
| `launch` | game launch and "run another exe/msi" | `launch(game_id)`, `run_in_env(env_id, ExecTarget, args)` |
| `winetricks` | dependency installs | `list_verbs()`, `installed(&PrefixDir)`, `install_verb(env_id, verb)` (one run per verb) |
| `backup` | prefix archive and restore | `backup(env_id) -> Backup`, `restore(backup_id, target)` |
| `delete` | safe removal | `plan_environment_removal(env_id) -> DeletionPlan`, `execute(plan)` |
| `logs` | per-run log file, redaction | `LogWriter::create(&Paths, run)`, `redact(&str) -> Cow<str>` |
| `diagnose` | classifying failures | `classify(kind, exit, &[String], &Preflight) -> Vec<Finding>` |
| `desktop` | per-game .desktop files | `write_entry(&Paths, &Game, exe: &Path)`, `remove_entry(..)` |
| `artwork` | cover images (optional, section 15) | `fetch(&Mutex<Db>, &Paths, &Sources, game_id, force)`, `spawn_auto(..) -> Receiver<Updated>`, `set_custom(..)`, `remove(..)`, `path(..)` |

No traits except where a test fake is impossible otherwise. umu-run is faked by a configurable binary
path (`Tools { umu_run: PathBuf }`), Proton's wineserver by a fake runtime dir in tests, HTTP by a
`std::net::TcpListener` serving fixtures on localhost (the base URL is a parameter). That leaves zero
traits in the plan.

## 4. The four path kinds (spec 5.2)

| Kind | Type | Example | Used for |
|---|---|---|---|
| Host path | `HostPath(PathBuf)` absolute | `/mnt/games/GameInstaller/setup.exe` | everything on the Linux side: exe passed to umu-run, `current_dir`, DB columns |
| Prefix path | `PrefixDir(PathBuf)` absolute | `~/.local/share/vortex/prefixes/12` | `WINEPREFIX` only; drive C is `<prefix>/drive_c` (top-level layout, `pfx -> .`) |
| Windows exe path | `WinPath(String)` | `Z:\mnt\games\GameInstaller\setup.exe`, `C:\Games\Foo\foo.exe` | arguments interpreted by Windows programs (`msiexec /i`), display, diagnostics |
| Windows working dir | `WinPath(String)` | `Z:\mnt\games\GameInstaller` | display, diagnostics; produced by Wine from the host `current_dir` |

Rules:
- Constructors validate: `HostPath::new` requires absolute + no `..` components after normalisation;
  `PrefixDir` additionally must not be `/`, `$HOME`, or a parent of another known prefix.
- An `Invocation` carries four named fields: `exe_host`, `cwd_host`, `prefix`, and derived
  `exe_win`/`cwd_win`. Functions take the specific type; no `&str`/`PathBuf` paths cross module APIs.
- `to_win`: host path under `<prefix>/drive_c` -> `C:\...`; anything else -> `Z:` + path with `/` -> `\`.
  `to_host` is the inverse for `C:` and `Z:` only; other drive letters return `None`.
- The Windows CWD is controlled solely by `Command::current_dir(cwd_host)` [VL: propagates through
  umu and the container]. Whether a `cwd_host` inside `drive_c` appears as `C:\...` or `Z:\...` is **[SPIKE S4]**.
- DB stores host paths only. Windows forms are recomputed, never persisted.

## 5. umu-run invocation contract

```
Command::new(tools.umu_run)                // default: "umu-run" resolved on PATH at startup
  .arg(exe_host | r"C:\windows\system32\<name>.exe")  // built-ins by FULL path (cmd, msiexec, wineboot)
  .args(args)                              // argv array, never joined into a string
  .current_dir(cwd_host)                   // the only way the Windows cwd is set
  .env("GAMEID", "0")                      // no protonfixes unless the user overrides GAMEID
  .env("PROTON_SET_GAME_DRIVE", "0")       // no S: game drive (see below); user may override
  .envs(user_env)                          // per-game vars, validated (section 11)
  .env("WINEPREFIX", prefix)               // absolute
  .env("PROTONPATH", runtime_dir)          // absolute, validated (proton + toolmanifest.vdf + wineserver)
  .env("VORTEX_RUN_ID", marker)            // 32 hex chars from /dev/urandom, unique per run
  .env("STEAM_COMPAT_INSTALL_PATH", dir)   // ALWAYS: exe's dir for host exes, else cwd_host
  .env("PRESSURE_VESSEL_FILESYSTEMS_RW", ..) // cwd / exe dir outside $HOME,/mnt,/media,/run/media,/srv,/opt,/tmp
  .stdin(null).stdout(piped).stderr(piped)
  .process_group(0)                        // only isolates Vortex's terminal signals; NOT used for stop
```

- Inherited environment is kept (DISPLAY, WAYLAND_DISPLAY, XDG_RUNTIME_DIR, ...). Reserved names
  (`WINEPREFIX`, `PROTONPATH`, `VORTEX_RUN_ID`, `STEAM_COMPAT_*`, `PRESSURE_VESSEL_*`) cannot be set
  by the user. `GAMEID`, `PROTON_*`, `DXVK_*`, `VKD3D_*`, `WINEDLLOVERRIDES` can.
- **Installers are muted** (not a setting): runs of kind `install` (retries included) and the
  game's uninstaller (`launch::run_uninstaller`) get `Invocation::mute_audio`, which sets
  `WINEDLLOVERRIDES=<existing>;winepulse.drv,winealsa.drv,wineoss.drv=d`. `<existing>` is the run's
  own value, else the inherited one; ours comes last so it wins. Proton appends its own overrides
  after it (`append_to_env_str(..., ";")`), so it is kept. Every Wine process of the run inherits
  it; Wine then has no audio driver and reports no render device. It is per-run env only, never
  written to the prefix registry, so launches in the same prefix keep their sound. Launch,
  "Run EXE", winetricks and prefix-init runs are not muted.
- **Uninstaller [VL M16]**: `run_uninstaller` runs `C:\windows\system32\start.exe /wait <exe_win>`
  with the prefix's `drive_c` as cwd. Proton's `steam.exe` wrapper holds the directory of the
  program umu-run starts (and the cwd) open until the run ends. Started directly, an uninstaller
  that removes its own folder (two-phase, relaunched from `%TEMP%`) got `ERROR_SHARING_VIOLATION`
  (32) on `RemoveDirectory` and left the folder behind. `start /wait` passes the exit code through.
- **Built-ins by full path [VL M1 F1]**: with a bare name (`cmd`, `msiexec`) umu-run logs
  "Executable not found", passes the name through, and always exits 0, losing the real code. As
  `C:\windows\system32\cmd.exe` the code propagates (umu still logs the same benign warning).
  `winetricks` is the only bare name (umu special-cases it).
- **Exit codes are 8 bits [VL]**: 3010 arrives as 194, 1641 as 105, 1627 as 91.
  `process::Exit::Low8(u8)` carries only that; `Exit::matches(3010)` compares modulo 256. The full
  Windows code is never known, and no exit code alone means success for installers.
- **Output [VL M1 F2]**: the program's own console output does not reach umu-run's pipes. Logs carry
  umu/Proton/Wine lines only (plus `vortex: ` supervisor notes).
- **Game drive [VL M7]**: umu sets `STEAM_COMPAT_LIBRARY_PATHS` to the first non-root mount point above
  `STEAM_COMPAT_INSTALL_PATH` (empty when there is none, or when the install path is empty, as it was for
  bare built-ins). Proton's `try_get_game_library_dir` then logs
  `Proton: Error: unable to use parent for game drive, path <that value>` (empty when none) on **every**
  run unless the path is a real Steam `steamapps` dir. The line is benign and cannot be suppressed
  from outside. With the game drive enabled, Proton maps `S:` to that mount, so a cwd under `/tmp` showed as
  `S:\vortex-...` instead of `Z:\tmp\vortex-...`, and `S:` changes from run to run.
  `PROTON_SET_GAME_DRIVE=0` removes `S:`, and cwd/exe then follow `paths::to_win` (`C:` / `Z:`).
- No `--config` TOML (no env/cwd control). No `PROTON_VERB` (default `waitforexitandrun` serialises
  against a previous wineserver in the same prefix, which is what we want).
- Prefix init: `umu-run C:\windows\system32\wineboot.exe -u`, exit code 0 required [VL M7]. First ever
  run downloads the Steam Linux Runtime (~700 MB); its stderr is streamed so the UI shows progress text.
- Installer EXE: program = `exe_host`, `cwd_host` = installer's parent. MSI:
  `C:\windows\system32\msiexec.exe /i <exe_win> /qn?`, `cwd_host` = msi's parent (so
  `STEAM_COMPAT_INSTALL_PATH` = msi's parent). `Z:\` paths install [VL M1 S7, M7]; a missing MSI gives 2.
- Verbose mode (`RunSpec::verbose`, per run; the UI offers it as "Retry with verbose log"):
  `UMU_LOG=debug`, `PROTON_LOG=1`, `PROTON_LOG_DIR=logs/<owner>/<run_id>-<kind>.proton/` (next to
  the run log, so `runs.log_path` links both; created 0700 before the spawn) and, unless the run's env
  sets it, `WINEDEBUG=+seh,+loaddll,warn+file` (`runner::VERBOSE_WINEDEBUG`). Setting `WINEDEBUG`
  replaces Proton's `PROTON_LOG` default channel set, which also traces every debug string and unwind.
  `warn+file` logs each failed open as
  `warn:file:NtCreateFile L"\\??\\Z:\\dir\\data3.bin" not found (c0000034)` [VL M19]. Proton writes
  `steam-<SteamGameId>.log` (`steam-0.log`) there itself and sends Wine's stderr to it, not to umu-run's
  pipes. umu prints the whole environment on stderr, which is why redaction runs on every line
  (section 10). Proton Experimental `experimental-11.0-20261027b`: 35 to 75 KB for a small program in an
  existing prefix.
- Before spawning (`Invocation::preflight`): refuse if `geteuid() == 0`; validate user env; verify
  `exe_host` is a regular file and `cwd_host` a dir; verify the runtime still validates and has a
  wineserver. Failures (`Validation`, `NotFound`, `RuntimeInvalid`) become `diagnose` findings
  (`MissingExecutable`, `RuntimeMissing`, `MissingInstallerData`), not spawns.
- One active run per environment, enforced in core by `process::Runs` (in-memory map plus a check for a
  `running|waiting` row of a detached run) -> `Error::Busy`. The `wineserver -w` barrier is per
  prefix, so concurrent runs in one prefix would wait on each other.

## 6. Process lifecycle: completion and stop

### 6.1 Conflict resolved: no process-group stop

`stack.md` proposes `process_group(0)` + `killpg`. `upstream.md` [SRC] shows umu-run starts its child
with `start_new_session=True` (setsid) and sets `PR_SET_CHILD_SUBREAPER`, so every Wine process lives
in a different session and process group than umu-run. `killpg(umu_pid)` would kill umu-run alone and
orphan the game. **Rejected.** Vortex keeps `process_group(0)` only to keep terminal Ctrl-C from
reaching umu-run, and uses the Wine-native and marker-based mechanisms below.

### 6.2 Completion (spec 5.4: never report done while children run)

Run phases, each emitted as `RunEvent::Phase`:

1. **Running**: wait for umu-run to exit [VL: it already waits for `start /b` grandchildren].
   Every 2 s, `scan_marker(marker)` lists processes whose `/proc/<pid>/environ` contains
   `VORTEX_RUN_ID=<marker>`; the UI shows their names from `/proc/<pid>/cmdline` (Wine processes show
   the Windows exe path), filtering infrastructure (`wineserver`, `services.exe`, `winedevice.exe`,
   `plugplay.exe`, `explorer.exe`, `rpcss.exe`, `svchost.exe`, `steam.exe`, `pv-adverb`, `srt-bwrap`, umu python).
2. **WaitingForChildren**: run `WINEPREFIX=<prefix> <runtime>/files/bin/wineserver -w`
   (fall back to `dist/bin`). Returns when the prefix's wineserver exits, i.e. no Wine process of
   that prefix remains [VL M1: from the host it returns rc 0 at once and starts nothing when no server runs, and blocks while one runs]. No timeout:
   a prerequisite installer may sit on a dialog for an hour. The UI shows "Waiting for background
   processes: <names>" with **Stop** and **Mark as finished**.
3. **Draining**: poll the marker scan once per second for up to 10 s for non-Wine leftovers
   (pressure-vessel teardown). Anything left after that becomes `RunEvent::Uncertain(Vec<ProcInfo>)`:
   the UI states that processes are still running and offers Stop or Mark as finished.
4. **Finished { exit_code, user_forced: bool }**. The umu-run exit code is reported but is not by
   itself success or failure for installers (installers return 3010, 1641, etc.); `diagnose` decides.

[VL M1 S2: the marker reached every process, including a child created with a custom Win32 env block,
because Wine builds the Unix environ itself. The fallback below is kept only as documentation, not built.]
Fallback if the marker is lost (installer passes a custom environment block to
`CreateProcess`, or a Wine service started outside the run): `wineserver -w` stays the authoritative
barrier; the process list for the UI falls back to "descendants of umu-run by PPid plus processes whose
`/proc/<pid>/cmdline` exe lies under the prefix or source dir".

### 6.3 Stop (game Stop, cancel install, Stop leftovers)

1. `WINEPREFIX=<prefix> <runtime wineserver> -k` [VL rc 0 from host; **rc 1 when no server runs = nothing
   to stop, not an error**]. Wait up to 5 s for umu-run. [VL M1: this step alone cleared 21 processes in 0.08 s]
2. SIGTERM umu-run if alive (its handler forwards to its whole tree). Wait up to 5 s.
3. SIGKILL every process still matching the marker. Before each kill re-read its environ to guard
   against pid reuse. `// ponytail: re-check then kill has a tiny pid-reuse race; use pidfd if it bites.`
4. Emit `Finished { stopped: true }`. If anything survives, emit `Uncertain` again and say so.

### 6.4 Vortex crashes or quits mid-run

umu-run is not killed with Vortex (no PDEATHSIG). On quit with active runs the UI asks: keep running
in background, or Stop. On next start `db::recover_on_startup` checks every run in `running|waiting`:
pid alive with matching `/proc/<pid>/stat` start time, or any marker process present -> keep state,
show "running (detached)" with Stop (section 6.3, works without pipes). Otherwise the run becomes
`interrupted`. [VL M1 S6: after the parent's SIGKILL, umu-run (reparented) and every marker process keep running and
are found by marker scan.] After SIGTERM to umu-run alone, `pv-adverb` + a Wine process outlive it for up to ~3 s,
so the Draining phase is required.

### 6.5 Threads per run

`process::start` spawns one supervisor thread, which spawns umu-run and two reader threads
(stdout, stderr). Readers: read lines (lossy UTF-8), `logs::redact`, append to the run log file
(shared `Arc<Mutex<LogWriter>>`), `try_send` the line to the UI channel (drop on full; the file keeps
everything). The supervisor runs the phases above and writes state changes to the DB.
`RunHandle::stop()` performs 6.3 on a fresh thread and never blocks the caller.

## 7. Install state machine (spec 5.3, 15)

```
            begin()                  prefix ok                 umu-run exited
 Created ─────────► PrefixInit ─────────────────► Running ─────────────────► Waiting
                        │ fail                      │ stop/cancel                │ barriers passed
                        ▼                           ▼                            ▼ (or user "mark finished")
                     Failed                     Cancelled                    SelectExe ──confirm_exe()──► Done
                                                                                 │ user: "installer failed"
                                                                                 ▼
                                                                              Failed
 startup recovery: PrefixInit | Running | Waiting with no live process -> Interrupted
 Interrupted -> Running (re-run installer in same prefix) | SelectExe (user says it finished) | removal
 Failed      -> Running (retry) | removal
```

- States are persisted **before** the action they describe starts (row written as `Running` before
  spawn, with run marker). Every transition is one SQL transaction updating `installs.state` and
  `games.status` together.
- `games.status = 'installed'` is set only by `confirm_exe`, which requires a user-confirmed exe that
  exists as a regular file on the host. Exit codes never mark an install as successful. This is how
  "interrupted installs never appear successful" is guaranteed structurally.
- Source watch: during `Running`/`Waiting` the supervisor checks every 5 s that the source path still
  exists; if not, it emits a `MissingInstallerData` finding and the UI offers Stop (spec 5.2 lifecycle).
- Exe candidates after install: `*.exe` files under the user's install dir (if set) and
  `drive_c/{Program Files,Program Files (x86),GOG Games,Games}`, modified after the install started,
  excluding `unins*`, `*setup*`, `vcredist*`, `dxsetup*`, `dotnet*`, `*crash*`, `*redist*`; ranked
  by name similarity to the game name, then size. The user always confirms or browses.
- Shared prefixes stay possible: games reference an environment; nothing assumes 1:1 except the
  default creation path and deletion (which refuses to delete an environment still referenced).

## 8. Installer source analysis (spec 5.2)

`source::analyze` never executes anything.
- File: `.exe` or `.msi` (case-insensitive) accepted; anything else rejected with a message.
- Dir: walk depth <= 2, collect `.exe`/`.msi`. Score: `+` names `setup.exe`, `install.exe`,
  `autorun.inf open=` target, `setup_*.exe` with sibling `setup_*-N.bin` (GOG/Inno), sibling
  `data1.cab`/`data1.hdr` (InstallShield), top-level `.msi`; `-` depth > 0, path under
  `redist|directx|_commonredist|support|vcredist|dotnet|physx`, names `unins*`, `dxsetup`, `vc_redist*`.
- `ambiguous = candidates.is_empty() || top1.score - top2.score < THRESHOLD` -> UI requires the user to
  pick. Reasons are returned as strings for display.
- The source is used in place (no staging). Staging is not implemented in the MVP; if a location is not
  reachable from the container, the dir is added to `PRESSURE_VESSEL_FILESYSTEMS_RW`. `// ponytail:`
  staging copy added only if a real installer needs it.

## 9. Data model (schema v1)

`$XDG_DATA_HOME/vortex/vortex.db`. Per connection: `PRAGMA foreign_keys=ON; journal_mode=WAL;
busy_timeout=5000`. Times are unix seconds. Migrations: `const MIGRATIONS: &[&str]` (one
`include_str!` per file), applied in order inside a transaction that also sets `user_version`; copy the
DB to `vortex.db.bak-v<N>` before migrating; refuse to open a DB whose `user_version` is newer.

```sql
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);   -- default_runtime, ...

CREATE TABLE environments (
  id             INTEGER PRIMARY KEY,
  prefix_path    TEXT NOT NULL UNIQUE,          -- PrefixDir (WINEPREFIX)
  prefix_managed INTEGER NOT NULL CHECK (prefix_managed IN (0,1)),  -- 1 = Vortex created it in data/prefixes
  runtime_path   TEXT NOT NULL,                 -- absolute Proton dir (PROTONPATH)
  created_at     INTEGER NOT NULL
);

CREATE TABLE games (
  id                  INTEGER PRIMARY KEY,
  name                TEXT NOT NULL,
  environment_id      INTEGER NOT NULL REFERENCES environments(id) ON DELETE RESTRICT,
  status              TEXT NOT NULL CHECK (status IN ('installing','installed','failed','interrupted')),
  install_dir         TEXT,                      -- host path, optional
  install_dir_managed INTEGER NOT NULL DEFAULT 0 CHECK (install_dir_managed IN (0,1)),
  exe_path            TEXT,                      -- host path; NOT NULL when status='installed'
  work_dir            TEXT,                      -- host path; NULL = exe parent
  launch_args         TEXT NOT NULL DEFAULT '[]',  -- JSON array of strings
  env_vars            TEXT NOT NULL DEFAULT '{}',  -- JSON object string->string
  created_at          INTEGER NOT NULL,
  updated_at          INTEGER NOT NULL,
  CHECK (status <> 'installed' OR exe_path IS NOT NULL)
);

CREATE TABLE installs (
  id             INTEGER PRIMARY KEY,
  game_id        INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
  source_path    TEXT NOT NULL,                  -- host path, file or dir, never modified
  source_kind    TEXT NOT NULL CHECK (source_kind IN ('file','dir')),
  installer_path TEXT NOT NULL,                  -- host path of primary installer
  state          TEXT NOT NULL CHECK (state IN ('created','prefix_init','running','waiting',
                   'select_exe','done','failed','cancelled','interrupted')),
  failure        TEXT,                           -- diagnose class, when failed
  started_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);

CREATE TABLE runs (
  id             INTEGER PRIMARY KEY,
  marker         TEXT NOT NULL UNIQUE,           -- VORTEX_RUN_ID
  kind           TEXT NOT NULL CHECK (kind IN ('prefix_init','install','launch','exec','winetricks')),
  environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
  game_id        INTEGER REFERENCES games(id) ON DELETE SET NULL,
  install_id     INTEGER REFERENCES installs(id) ON DELETE SET NULL,
  argv           TEXT NOT NULL,                  -- JSON, redacted
  pid            INTEGER,
  pid_start      INTEGER,                        -- /proc/<pid>/stat field 22, liveness check
  state          TEXT NOT NULL CHECK (state IN ('running','waiting','finished','stopped','interrupted')),
  exit_code      INTEGER,
  user_forced    INTEGER NOT NULL DEFAULT 0,     -- "mark as finished" used
  findings       TEXT,                           -- JSON from diagnose
  log_path       TEXT NOT NULL,
  started_at     INTEGER NOT NULL,
  ended_at       INTEGER
);
CREATE INDEX runs_game ON runs(game_id, started_at);

CREATE TABLE dependencies (                      -- what Vortex installed; winetricks.log stays truth
  environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
  verb           TEXT NOT NULL,
  run_id         INTEGER REFERENCES runs(id) ON DELETE SET NULL,
  state          TEXT NOT NULL CHECK (state IN ('installed','failed')),
  at             INTEGER NOT NULL,
  PRIMARY KEY (environment_id, verb)
);

CREATE TABLE backups (                           -- survive environment deletion (spec 11)
  id             INTEGER PRIMARY KEY,
  environment_id INTEGER REFERENCES environments(id) ON DELETE SET NULL,
  game_name      TEXT NOT NULL,
  archive_path   TEXT NOT NULL UNIQUE,           -- under <data>/backups
  runtime_path   TEXT NOT NULL,
  prefix_version TEXT,                           -- content of <prefix>/version
  size_bytes     INTEGER NOT NULL,
  created_at     INTEGER NOT NULL
);
```

Schema v2 (`0002_artwork.sql`, M17):

```sql
CREATE TABLE artwork (                           -- no row = never looked up
  game_id    INTEGER PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
  source     TEXT NOT NULL CHECK (source IN ('auto','custom','none')),  -- none = no match / removed
  kind       TEXT CHECK (kind IN ('portrait','landscape')),
  steam_app  INTEGER,                            -- matched Steam app id (auto only)
  updated_at INTEGER NOT NULL
);
```

"Last launch information" = latest `runs` row with `kind='launch'` for the game; no extra column.
Runtimes are not stored: they are discovered on each start; a game's `runtime_path` that no longer
validates surfaces as `RuntimeMissing` with a picker.

Filesystem (all under `Paths`): `data/vortex.db`, `data/prefixes/<env_id>/`, `data/runtimes/<GE tag>/`,
`data/runtimes/.tmp-*`, `data/backups/<env_id>-<ts>.tar.gz`, `data/logs/game-<game_id>|env-<env_id>/<run_id>-<kind>.log`
(created with `create_new`, never reused; versions before M28 wrote `logs/<id>/`),
`data/artwork/<game_id>.img` (JPEG or PNG, content-sniffed),
`~/.local/share/applications/io.github.nexusdeveloper902.Vortex.game-<id>.desktop`.

## 10. Logs, redaction, diagnostics

- Every run writes one log file: header (time, kind, redacted argv, cwd, runtime, prefix), then
  interleaved stdout/stderr lines prefixed `O|`/`E|`. Logs are exportable by copying files.
- `logs::redact` (applied before file and UI): for `NAME=value` pairs and `'NAME': 'value'` (umu's
  debug env dump) where NAME matches `(?i)(token|secret|pass|key|auth|cookie|session|credential)`,
  replace value with `<redacted>`; also mask `Bearer \S+`, `gh[pousr]_[A-Za-z0-9]{20,}`, and URL
  userinfo (`https://user:pw@`). Implemented with plain string scanning, no regex crate.
  `// ponytail: name heuristics; add a user-defined redact list if someone needs it.`
- `diagnose::classify` returns `Vec<Finding { class, confidence: Confirmed|Likely, evidence: Vec<String> }>`
  with classes from spec 10: `InstallerFailure`, `RuntimeInitFailure` (umu/pressure-vessel errors,
  `toolmanifest.vdf not found`, runtime download failure), `MissingDependency` (`err:module:import_dll`
  naming the DLL), `MissingExecutable` (preflight), `AppCrash` (`Unhandled exception`, page fault,
  signal exit), `MissingInstallerData` (source vanished, file-not-found lines naming a source file),
  `WrongWorkingDirectory` (Likely only: relative-path not-found with cwd != exe dir), `Unknown`.
  Patterns live in one table in `diagnose.rs` and are tested against real log excerpts from M1.
- Verbose runs: Proton's log is written by Proton, not by `LogWriter`. The supervisor rewrites it
  through `logs::redact` (`logs::redact_file`) before `Finished`. The log viewer redacts every line
  again when it shows a file. `diagnose::missing_files` reads only the failed-open lines of that log
  (never `classify`'s patterns: its header has lines like `pressure-vessel: <version>`). It names files
  under the run's `cwd_win`, leaves out `.dll/.drv/.manifest/.local/.config/.mui` probes, and only for a
  failed run: `MissingInstallerData` (install) or `MissingFile` (other kinds), Likely.
  Nothing is reported as Confirmed unless the evidence is unambiguous (spec 10, last line).
- Retention (M25): `logs::prune(db, paths, KEEP_RUNS_PER_OWNER = 20)` runs at startup on a worker thread
  (after recovery). Per log directory it keeps the newest N runs and every running/waiting run, and
  deletes the older runs' log file and `.proton` dir. It only touches paths directly inside
  `logs/<owner>/` (canonicalized), never follows links, and keeps the `runs` rows. No `DeletionPlan`:
  these are fixed managed files the user never chose, like the artwork cache.
- Removal (M28): `plan_game_removal` lists the game's `logs/game-<id>/` and, when the environment
  goes too, `logs/env-<id>/` as plan items (strictly inside `logs/`, same checks as prefixes). SQLite
  reuses the highest deleted `games`/`runs` id, so a new game can get an old id; its log dir is then
  fresh, and `LogWriter::create` refuses an existing file instead of truncating it.
- Cleanup tools (M25): `delete::CleanupPlan` (`plan_backup_removal`, `plan_runtime_removal`,
  `execute_cleanup`) follows the `DeletionPlan` contract: shown first, rebuilt and compared before
  deleting. A backup archive must be a direct child of `backups/`; a runtime must be a direct child of
  `runtimes/` (never Steam's) and no environment may use it (refused, naming the games). Directories go
  through `remove_dir_all`, which unlinks symlinks without following them.

## 11. Security rules (spec 12)

1. Refuse to start if `geteuid() == 0` (gtk `main`) and refuse to spawn if root (core runner, defence
   in depth). No `sudo`/`pkexec` anywhere.
2. Never spawn through a shell. argv arrays only. Launch args are stored as a JSON array.
3. Never execute community scripts or downloaded scripts. Winetricks verbs are validated against
   `^[a-zA-Z0-9_-]+(=[a-zA-Z0-9]*)?$` and must appear in `winetricks list-all`; `.verb` file paths are
   rejected.
4. Env var names validated `^[A-Za-z_][A-Za-z0-9_]*$`; reserved names (section 5) rejected; values
   must not contain NUL.
5. Deletion (`delete.rs`): a `DeletionPlan` lists `(HostPath, bytes)` items; the UI shows it and
   executes exactly that plan. An item is allowed only if, after `canonicalize`, it is (a) strictly
   inside a managed root (`data/prefixes`, `data/runtimes`, `data/backups`, `data/logs`) or (b) a
   recorded `install_dir` with `install_dir_managed=1`; and it is not `/`, `$HOME`, a managed root
   itself, or an ancestor of another environment's prefix or install dir. The original installer
   source is never in a plan. A `prefix_managed=0` prefix is never an item; when its environment is
   removed it is listed in the plan's `manual` (shown as "Vortex will NOT delete").
   Custom locations (M21, spec 4): the wizard may create a prefix outside `data/prefixes`
   (`paths::check_prefix_location`: empty or missing dir, parent exists and is writable, not `/` or
   `$HOME`, not inside `data/`, not inside or around a known prefix). It is recorded
   `prefix_managed=0`: Vortex created it, but a user-chosen location is outside every managed root,
   so rule (a) cannot vouch for it and deletion stays limited to `data/prefixes`. Likewise a missing
   install dir outside `data/games` is created (`paths::new_dir_location`) with
   `install_dir_managed=0`. Restore refuses targets outside `data/prefixes`, so such a prefix can be
   backed up but not restored. `std::fs::remove_dir_all` does not follow symlinks (prefixes contain
   `dosdevices/z: -> /`); tests assert that a symlink to an outside dir survives deletion.
6. Backups: `tar::Builder` with `follow_symlinks(false)` (mandatory: `z:` -> `/`). Restore unpacks into
   `data/prefixes/.restore-<id>/`, rejects absolute paths, `..`, and entries written through a symlink,
   then renames into place; the old prefix is moved aside and removed only after a successful rename.
   Restoring onto a prefix whose `version` is newer than the backup's runtime warns (no downgrade).
7. Downloads: HTTPS to `api.github.com` / `github.com` / `objects.githubusercontent.com` only;
   sha512 must match the release `.sha512sum` before extraction; extract to `.tmp-*`, require a single
   top-level dir, then atomic rename. Partial downloads never look like valid runtimes.
   Artwork (section 15) is the only other network use: Steam's public store search JSON and CDN
   images, no API key, no redirects followed, never when turned off or in offline mode.
8. No secrets in logs (section 10). Vortex never writes to `~/.wine`, the user's Steam dirs, or any
   prefix it did not create, except when the user explicitly points an environment at it
   (`prefix_managed=0`; never deleted by Vortex), or chooses a custom location for a new prefix
   (rule 5; also `prefix_managed=0`).
9. No DRM/anti-cheat circumvention features.

## 12. Threading model

- GTK thread: owns widgets and the `Db` handle `Arc<Mutex<rusqlite::Connection>>` (queries are tiny;
  workers lock it briefly). No blocking I/O or process waits on this thread.
- Workers: plain `std::thread`s spawned by core (run supervisors, pipe readers, downloads, backups,
  winetricks list, deletions). Long operations take `&AtomicBool` for cancel where meaningful.
- Events: core returns `async_channel::Receiver<Event>` (bounded, e.g. 1024); workers use
  `send_blocking`/`try_send`. GTK runs `glib::spawn_future_local(async move { while let Ok(ev) =
  rx.recv().await { ... } })`. Core never sees glib.
- Event types are plain enums in core: `RunEvent { Phase, Line, Processes(Vec<ProcInfo>), Uncertain,
  Finished }`, `Progress { bytes, total, stage }`, `InstallEvent`. UI coalesces `Line`s into the
  `TextBuffer` on a 50 ms `timeout_add_local`.
- Single instance: `adw::Application` with `HANDLES_COMMAND_LINE`; `vortex --launch <id>` from a
  .desktop entry goes to the running instance.

## 13. Errors and testing

- `vortex_core::Error` (thiserror) with variants such as `Io { path, source }`, `Db(rusqlite::Error)`,
  `InvalidPath(String)`, `Validation(String)`, `RuntimeInvalid { path, reason }`, `Busy(EnvId)`
  (run already active), `Download(String)`, `Cancelled`, `NotFound(String)`. `pub type Result<T>`.
  No `unwrap`/`expect` outside tests and provably-infallible spots. Diagnosis findings are data, not
  errors. GTK shows `Error`'s `Display` in a toast or `adw::AlertDialog`; no `anyhow` in core.
- Tests sit next to code (`#[cfg(test)] mod tests`) for pure logic; `crates/vortex-core/tests/` for
  process and DB integration.
- `tests/fake-umu-run` (POSIX sh, committed): logs `$PWD`, args and selected env to a file named by
  `FAKE_UMU_OUT`; behaviour by `FAKE_UMU_MODE` = `exit:<n>`, `sleep:<s>`, `lines:<n>`,
  `orphan:<s>` (spawns `setsid sleep <s>` carrying the marker env, then exits, to exercise the
  barriers), `trap` (ignores SIGTERM). A fake runtime dir in a tempdir contains `proton`,
  `toolmanifest.vdf` and `files/bin/wineserver` (sh script: `-w` blocks while a pidfile's pid lives,
  `-k` kills it). Write scripts, close the handle, then exec (avoid ETXTBSY).
- Real-UMU tests: `#[ignore]` tests run with `cargo test -p vortex-core -- --ignored`, require
  `VORTEX_IT_PROTON=/abs/proton/dir` and fail loudly (panic) if it is missing. They are never skipped
  silently and never stubbed. Fixture installers: `.bat` files run via `cmd /c`, and, when
  `x86_64-w64-mingw32-gcc` exists, small C programs in `tests/fixtures/installers/src/` built by the
  test. If the compiler is absent the test fails with "mingw-w64-gcc not installed"; report it as NOT RUN.

## 14. Risks and spike items (settled in M1)

| ID | Assumption | Fallback if false |
|---|---|---|
| S1 | Proton's `wineserver -w` run on the host with `WINEPREFIX` blocks while the prefix server lives and returns at once when none exists, without starting one | Poll for the server socket `/tmp/.wine-<uid>/server-<dev>-<ino>/socket` + marker scan |
| S2 | `VORTEX_RUN_ID` is inherited by every Wine process, including `start`, detached `CreateProcess`, and service-spawned processes | PPid tree + cmdline-under-prefix heuristics for the UI list; wineserver barrier unchanged |
| S3 | Nested installer: main exits, prereq continues; umu-run or the wineserver barrier holds until the prereq ends | Required: barrier semantics; if both fail the design is wrong; stop and re-plan |
| S4 | `current_dir` under `drive_c` shows as `C:\...`; under source dir as `Z:\...`; relative `data1.bin` opens | Add `start /d <WinPath>` wrapper via `cmd` |
| S5 | `.bat` fixtures run via `umu-run cmd /c <WinPath>` | Use mingw-built exe fixtures only |
| S6 | Killing Vortex (SIGKILL) leaves umu-run and the game running and re-discoverable via marker | Document; recovery marks interrupted |
| S7 | `msiexec /i Z:\...` installs from a host path | Use `PROTON_VERB=getcompatpath` to convert |
| S8 | `umu-run winetricks` works with GE-Proton (Experimental lacks `protonfixes/`) | Require GE/UMU-Proton for dependency installs; say so in UI |
| S9 | GE tarball top-level dir equals the tag (`GE-Proton11-12`) despite `-x86_64` asset suffix | Use the actual single top-level dir name |

**M1 results** (evidence: `docs/research/spike-results.md`): S1 confirmed, S2 confirmed, S3 confirmed
(umu-run itself waited for every nested/detached/delayed child, and `wineserver -w` is kept as the authority),
S4 confirmed (`drive_c` cwd shows `C:\...`), S5 confirmed, S6 confirmed, S7 confirmed (`Z:\` and Unix paths both
install), S8 refuted for Experimental (no `protonfixes/`, umu crashes rc 1); GE NOT RUN, S9 refuted (top-level
dir is `GE-Proton11-12-x86_64`, so use the archive's actual single top dir). New: exit codes propagate only when
arg 1 is a path; bare `cmd`/`msiexec` always give 0, so section 5 must spawn built-ins as
`C:\windows\system32\<name>.exe` (follow-up PR for section 5 / M7). Codes are 8-bit (3010 -> 194, 1641 -> 105).
The program's own console output does not reach umu-run's pipes.

Other known limits: host `/tmp` sharing is required for the host-side wineserver calls (native build
only, no Flatpak); first use needs network for the SLR (~700 MB) and GE-Proton (~567 MB); switching a
prefix to an older Proton is refused without explicit confirmation (prefix `version` cannot downgrade).

## 15. Artwork (M17, spec 7: optional, offline keeps working)

- Source: Steam's public store, no API key, no HTML scraping. `GET
  store.steampowered.com/api/storesearch/?term=<name>&l=english&cc=US` (JSON `items[].id/name`), then
  `shared.steamstatic.com/store_item_assets/steam/apps/<appid>/` `library_600x900_2x.jpg`, then
  `library_600x900.jpg` (portrait), then `header.jpg` (landscape). The game's name is the only data sent.
- Query (M22, `artwork::clean_query`): the game name minus build/edition/platform/installer noise
  (whole words, case-insensitive; brackets holding noise dropped; `_`, `.` and lone `-` are
  separators). Only version-looking numbers go (a dot, or `v`/`b`/`r`/`build` prefix); a plain
  number never does, and the first word is kept (except installer words). That term is what is sent.
- Matching (`artwork::classify`): names normalized (lowercase, no ™®© or apostrophes, `&` = and,
  punctuation to spaces, leading "the" dropped, roman I-XX = arabic on non-first tokens). Extras
  (names with soundtrack, ost, dlc, demo, season pass, artbook; the store's `type` is `app` for all
  of them) are ignored while a base game is present. `Confident` = any exact match, else the first
  result whose title before a `:`/` - ` subtitle matches: only this is attached. Otherwise
  `Candidates(up to 10)`: nothing attached, `artwork` row `none` with `candidates = N` (migration
  0003) so the UI says "N possible matches"; no results is plain `none`.
- Manual pick (`artwork::search_manual`, `pick`): the GTK "Search Steam..." dialog searches (term
  prefilled, sent only on user action), or takes a store URL / app id (`parse_app_id`), downloads that
  app's art and records `auto` with `steam_app` = the choice. Allowed when `artwork_auto = "0"`,
  refused in offline mode. No thumbnails: the search JSON's image URLs are not fetched.
- Opt-out (NDA): an `artwork` row `none` written before any lookup (`opt_out`, `opt_out_install`;
  menu toggle `allow_auto` deletes the row). The wizard switch is on the first step and persisted
  right after `begin` creates the game. Fail closed: only `installed` games are looked up
  (`games_without_artwork`, non-forced `fetch`), so failed, cancelled, interrupted and installing
  games never send their name, whichever wizard button ends the flow.
- Checks: 20 s timeout, redirects refused, Content-Type `image/jpeg|png` and JPEG/PNG magic bytes,
  5 MB cap (1 MB for the search JSON, 20 MB for a user-picked file). Written as `.tmp-<id>` then renamed.
- State: no `artwork` row = never looked up; the GTK library starts one background pass per game per
  session (`spawn_auto`, std thread, `Updated { game_id, path }` events). No match records `none` so it
  is not queried again; network errors record nothing (retried next session) and only go to stderr.
  `force` (Refresh artwork) replaces the image on a match and keeps it otherwise. A background pass
  never overwrites a row written meanwhile (custom image, removal).
- Off switch: setting `artwork_auto = "0"` or `offline_mode = "1"` means zero requests.
- Deletion: "Remove artwork" deletes only `data/artwork/<id>.img` and records `none`. Removing a game
  deletes its cover after the `DeletionPlan` (like the desktop entry): a single file at a fixed path
  derived from the game id, so no plan item is needed. User-picked source files are never touched.

