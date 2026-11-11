# Vortex roadmap

Milestones are sized for one agent session and one PR each. Every milestone: feature branch
`mN-<slug>` in its own worktree, automated tests green (`cargo fmt --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo test --workspace`), PR into `main`. Real
integration tests run where UMU is available (this dev machine has umu-run 1.4.4, Proton Experimental
and the steamrt4 runtime installed; no mingw-w64, no msitools). A test that could not run is reported
as NOT RUN with the reason, never as passed.

Model tiers: **haiku** = scaffolding, CI, packaging, docs. **sonnet** = ordinary modules.
**opus** = process lifecycle, install state machine, spike, end-to-end verification.

## Dependency graph and parallel groups

```
M0 ──► M2, M3, M4, M5, M6, M13a
M1 ──┐
M2 ──┴► M7
M2 ──► M8
M2 + M3 ──► M11
M3 + M4 + M5 + M7 ──► M9
M3 + M7 ──► M10
M6 + M8 + M9 + M10 + M11 ──► M12
all ──► M14 (+ M13b)
```

Run in parallel worktrees (no shared files except `Cargo.toml`/`Cargo.lock` dependency lines):
- **Group A**: M0, M1
- **Group B** (after M0 merged): M2, M3, M4, M5, M6, M13a
- **Group C** (after M2; M7 also after M1; M11 also after M3): M7, M8, M11
- **Group D** (after M7): M9, M10
- Sequential: M12, then M14 (+M13b)

File ownership (a milestone edits only its files; anything else goes to a separate tiny PR first):

| Milestone | Owns |
|---|---|
| M0 | root `Cargo.toml`, `rustfmt.toml`, `.gitignore`, `.github/workflows/ci.yml`, `crates/*/Cargo.toml`, `crates/vortex-core/src/lib.rs` (declares every module from ARCHITECTURE section 2, each file a one-line `//!` doc), `crates/vortex-gtk/src/main.rs` |
| M1 | `spikes/lifecycle/**`, `docs/research/spike-lifecycle.md`, amendments to `docs/ARCHITECTURE.md` section 6/14 |
| M2 | `error.rs`, `paths.rs`, `db.rs`, `migrations/0001_init.sql`, `logs.rs` |
| M3 | `runtime.rs`, `runtime_ge.rs` |
| M4 | `source.rs` |
| M5 | `diagnose.rs`, `tests/fixtures/logs/**` |
| M6 | `desktop.rs` |
| M7 | `runner.rs`, `process.rs`, `tests/fake-umu-run`, `tests/process.rs`, `tests/fixtures/installers/**` |
| M8 | `backup.rs`, `delete.rs`, `tests/backup_delete.rs` |
| M9 | `install.rs`, `tests/install.rs` |
| M10 | `launch.rs`, `winetricks.rs`, `tests/launch.rs` |
| M11 | `vortex-gtk/src/{app.rs,game_object.rs,ui/library.rs,ui/game_page.rs,ui/runtimes.rs,ui/settings.rs}` |
| M12 | `vortex-gtk/src/ui/{install_wizard.rs,run_view.rs,dialogs.rs}`, edits to `game_page.rs`, `main.rs` |
| M13 | `packaging/**`, `data/**`, `README.md`, `docs/USER.md` |
| M14 | `docs/E2E-REPORT.md`, fixes via small follow-up PRs |

`db.rs`: M2 implements typed queries for the entire v1 schema. Later milestones that need another
query append it at the end of `db.rs` (append-only keeps merges trivial).

---

## M0: Workspace scaffold, CI, empty window (haiku)

- Scope: workspace per ARCHITECTURE section 2; edition 2024, `rust-version = "1.92"`,
  `unsafe_code = "forbid"`, clippy `all = warn`; empty module files declared in `lib.rs`;
  `vortex` binary opens an `adw::ApplicationWindow` (app id `io.github.nexusdeveloper902.Vortex`)
  with an `adw::StatusPage` "No games yet"; exits with a message when run as root (rustix `geteuid`).
  CI: GitHub Actions in `archlinux:latest` container (fmt, clippy -D warnings, test), plus a job
  asserting `cargo tree -p vortex-core -e normal` contains no `gtk4|glib|gio|libadwaita`.
- Deps: none.
- Acceptance: CI green on the PR; `cargo run --bin vortex` shows the window on Wayland (manual
  screenshot or `WAYLAND_DISPLAY` run noted in PR); `cargo test -p vortex-core` runs without GTK libs
  (CI job on plain `rust` image for core only).

## M1: Risk spike: process lifecycle, nested installers, cwd (opus)

- Scope: shell scripts in `spikes/lifecycle/` driving real `umu-run` with
  `PROTONPATH="$HOME/.local/share/Steam/steamapps/common/Proton - Experimental"` and throwaway prefixes
  under the session scratch dir. Answers S1-S7 from ARCHITECTURE section 14 and records exact
  commands, outputs, timings in `docs/research/spike-lifecycle.md`, tagged [VL] or NOT RUN.
  Test installers (`.bat`, run via `umu-run cmd /c <Z:\path>`):
  1. `multi/setup.bat` + `data1.bin`, `setup-1.bin`, `Redist/prereq.bat`: reads `data1.bin` by
     relative path, writes `cd` output and file contents to a result file.
  2. Nested: `setup.bat` runs `start "" cmd /c Redist\prereq.bat` and exits at once; prereq waits
     ~15 s (`ping -n 15 127.0.0.1`) then writes `prereq.done`.
  3. Detached-with-delay: `start /b` plus a second stage launched after the parent exits.
  4. If `x86_64-w64-mingw32-gcc` is available (not on this machine; `pacman -S mingw-w64-gcc` needs
     the user's approval): `setup.exe` that opens `data1.bin` relative to CWD and to its module dir,
     spawns `prereq.exe` with `DETACHED_PROCESS`, and a variant passing a custom environment block
     without `VORTEX_RUN_ID` (tests S2's worst case). Otherwise mark these NOT RUN.
  5. MSI (S7) only if `wixl` (msitools) is available; else NOT RUN.
- Measure: when umu-run returns vs `prereq.done` time; `wineserver -w` (Proton's binary, host side)
  with and without a running server, and that it does not spawn a server; `/proc/*/environ` marker
  presence for every Wine pid during each scenario; cwd as seen by Windows for host dirs under
  `$HOME` and under `drive_c`; stop sequence (`wineserver -k`, SIGTERM umu-run) leaves zero marker
  processes; SIGKILL of the parent shell leaves umu-run running and findable by marker.
- Deps: none (no Rust). Can run in parallel with M0.
- Acceptance: the report answers every S-item with evidence; ARCHITECTURE section 6/14 amended where
  a fallback is needed. If S3 fails (no barrier observes the prereq), stop and re-plan before M7.

## M2: Core foundation: errors, paths, DB, logs (sonnet)

- Scope: `Error`/`Result`; `Paths::from_xdg()` / `Paths::under(root)`; `HostPath`, `PrefixDir`,
  `WinPath` with `to_win`/`to_host`; `Db::open` with migrations, WAL, foreign keys, downgrade guard,
  backup copy before migrate; typed queries for all v1 tables;
  `recover_on_startup(marker_alive: &dyn Fn(&str) -> bool)` (pid liveness via `/proc/<pid>/stat`
  start time; the marker scan itself is `process::scan_marker` from M7, tests pass a closure);
  `LogWriter` and `redact`.
- Deps: M0.
- Acceptance (unit): path conversion table tests (`drive_c` -> `C:`, others -> `Z:`, round trip,
  spaces, non-ASCII, `..` rejected, `/` and `$HOME` rejected as prefix); fresh DB has
  `user_version=1`; DB with `user_version=99` refuses to open; CHECK constraint rejects
  `status='installed'` without exe; `installing` game + `running` run with dead pid -> both
  `interrupted` after `recover_on_startup`; redaction cases (env dump `'GITHUB_TOKEN': 'x'`,
  `API_KEY=x`, `Bearer`, `ghp_...`, URL userinfo) and non-cases (`KEYBOARD_LAYOUT` is a known false
  positive: assert the chosen behaviour explicitly).

## M3: Runtimes: discovery and GE-Proton download (sonnet)

- Scope: scan locations from upstream.md 3.1 (+ `data/runtimes`), dedupe by canonical path,
  validate (`proton` executable + `toolmanifest.vdf`), label from `compatibilitytool.vdf`
  `display_name` or dir name, version from `version`; locate `files/bin/wineserver` or `dist/bin`.
  GE: `releases/latest`, asset match (prefix `GE-Proton`, `.tar.gz`, not `aarch64`), stream to
  `.part` with sha512, verify against `.sha512sum`, extract to `.tmp-*`, single top-level dir,
  atomic rename; progress + cancel. Functions take explicit dirs (no dependency on M2's `Paths`).
- Deps: M0.
- Acceptance (unit): fake runtime trees (valid, missing manifest, BattlEye Runtime look-alike,
  `dist/` layout, symlinked duplicate); download against a localhost `TcpListener` serving a tiny
  tar.gz + sum: good hash installs, bad hash leaves no dir, cancel leaves no dir, archive with `..`
  entry or two top-level dirs is rejected. Integration (`--ignored`, network): resolve latest GE
  release metadata only (no 567 MB download in CI); full download is exercised in M14.

## M4: Installer source analysis (sonnet)

- Scope: `source::analyze` per ARCHITECTURE section 8. Pure filesystem reads, no execution.
- Deps: M0.
- Acceptance (unit, tempdir trees): spec 5.2 example tree picks `setup.exe`, not `Redist/*`;
  GOG layout `setup_game_1.0.exe` + `setup_game_1.0-1.bin` wins; InstallShield `data1.cab`
  boost; `autorun.inf` target wins; two equal `setup*.exe` -> `ambiguous`; empty dir -> ambiguous
  with no candidates; `.zip` file -> rejected; case-insensitive extensions; symlink loops do not hang.

## M5: Diagnostics classification (sonnet)

- Scope: `diagnose::classify` with pattern table; log fixtures in `tests/fixtures/logs/` (real
  excerpts from M1 when available, clearly labelled synthetic otherwise).
- Deps: M0 (M1 logs improve fixtures; not blocking).
- Acceptance (unit): each class has at least one positive and one negative fixture; missing
  `toolmanifest.vdf` -> `RuntimeInitFailure` Confirmed; `import_dll Library MSVCP140.dll` ->
  `MissingDependency` naming the DLL; vanished source -> `MissingInstallerData` Confirmed;
  working-dir heuristic only ever Likely; clean exit 0 with no patterns -> no findings.

## M6: Desktop entries (haiku)

- Scope: `desktop::write_entry/remove_entry`, Desktop Entry spec escaping for `Exec`, atomic write
  (temp + rename), mode 0644, `X-Vortex-Game-Id`.
- Deps: M0.
- Acceptance (unit): names with spaces, quotes, `%`, non-ASCII escape correctly; `desktop-file-validate`
  passes on the output when installed (else the test says NOT RUN); remove is idempotent.

## M7: Runner and process supervisor (opus)

- Scope: `runner.rs` (Invocation -> Command per ARCHITECTURE section 5, all validation and reserved
  env rules); `process.rs` (supervisor + readers, phases Running/WaitingForChildren/Draining/
  Finished, `Uncertain`, marker scan with infrastructure filter, stop sequence, one-run-per-env
  registry, DB run rows, root refusal); `tests/fake-umu-run`; fake runtime with fake `wineserver`.
  Incorporates M1 findings.
- Deps: M1, M2.
- Acceptance (automated, fake umu): argv/env/cwd recorded by the fake match the Invocation (incl.
  spaces in paths, no shell); reserved env rejected; lines arrive in order and land in the log file
  redacted; exit code propagates; `orphan` mode: `Finished` is not emitted until the orphan (fake
  wineserver `-w`) ends; `Uncertain` emitted when a marker process outlives the drain window;
  stop with `trap` mode ends with zero marker processes; second run on the same env -> `Busy`;
  run row transitions persisted. Integration (`--ignored`, real umu + Proton): M1 scenarios 1-3
  re-run through `process::start`: nested prereq finishes before `Finished`; cwd result file
  shows the installer dir; stop leaves no marker processes.

## M8: Backup/restore and safe deletion (sonnet)

- Scope: `backup.rs`, `delete.rs` per ARCHITECTURE section 11 rules 5 and 6.
- Deps: M2.
- Acceptance (unit/integration, tempdirs): backup of a tree containing `dosdevices/z: -> /` stores a
  symlink, not `/`; restore round-trips file contents, modes and symlinks; archive with `..`, absolute
  path, or file-through-symlink entry is rejected and leaves the target untouched; plan for env A
  never contains env B's prefix or install dir, `$HOME`, `/`, a managed root, or the installer
  source; deleting a prefix that contains a symlink to an outside dir keeps the outside dir; a
  `prefix_managed=0` environment produces a plan without the prefix; deletion refuses an environment
  still referenced by another game. Integration (`--ignored`): back up and restore a real
  umu-initialised prefix, then `umu-run cmd /c echo ok` in the restored prefix succeeds.

## M9: Install state machine and crash recovery (opus)

- Scope: `install.rs` per ARCHITECTURE section 7: create environment + managed prefix, prefix init
  run, installer run (exe and msi), source watch, SelectExe with candidates, `confirm_exe`, retry,
  cancel, recovery transitions; MSI path via `msiexec /i`.
- Deps: M3, M4, M5, M7.
- Acceptance (fake umu): every legal transition, illegal transitions return `Error::Validation`;
  process killed mid-install (simulate by dropping supervisor + reopening DB with dead pid) ->
  `interrupted`, never `installed`; exit code 0 alone never sets `installed`; `confirm_exe` with a
  missing file fails; source dir removed during run -> `MissingInstallerData` finding; candidate
  ranking excludes `unins000.exe` and redists. Integration (`--ignored`, real umu): multi-file `.bat`
  installer from M1 installs into a new managed prefix, prereq completes before SelectExe, the copied
  "game" exe is offered as a candidate.

## M10: Launch, run exe/msi in environment, winetricks (sonnet)

- Scope: `launch.rs` (game launch with args/env/work_dir, last launch from `runs`; run arbitrary
  exe/msi in an existing environment); `winetricks.rs` (parse `list-all`, read `winetricks.log`,
  validate verbs, one run per verb, record `dependencies`, refuse with a clear message when the
  runtime lacks `protonfixes/winetricks`).
- Deps: M3, M7.
- Acceptance (fake umu): launch passes args as separate argv items and env vars; a game with a
  missing exe produces `MissingExecutable` without spawning; second launch while running -> `Busy`;
  `list-all` fixture parse; `winetricks.log` with `vcrun2019` marks it installed; invalid verb
  `foo;rm` rejected; `.verb` path rejected. Integration (`--ignored`): launch a `.bat` "game" that
  echoes `%*` and an env var, assert both in the log; with GE-Proton present, `corefonts` installs
  in prefix A and prefix B's `winetricks.log` stays unchanged (else NOT RUN).

## M11: GTK shell: library, game page, runtimes, settings (sonnet)

- Scope: `AppState`, `GameObject` list model, library grid with search and sort, game page
  (details, Play/Stop/Open dirs/View logs wired to core), runtimes page (discovered list, GE
  download with progress and cancel, default runtime), settings, manual executable registration
  (pick exe -> new environment -> installed).
- Deps: M2, M3.
- Acceptance: core-facing glue has no logic beyond mapping (reviewed); manual check on Wayland with
  screenshots in the PR: empty library, registered game appears and persists after restart, GE
  download progress visible (download may be cancelled). `cargo clippy` clean.

## M12: GTK flows: install wizard, run view, management actions (sonnet)

- Scope: install wizard (file or folder via `gtk::FileDialog`, candidate list with reasons and
  forced choice when ambiguous, name, install location, runtime, run view, exe selection);
  run view (live log, process list, phases, "still running" with Stop / Mark as finished);
  run another exe/msi; winetricks dialog; backup/restore; uninstall (run game uninstaller) vs
  remove environment (deletion plan list in `adw::AlertDialog`); interrupted-install banner with
  Resume / Select exe / Remove; `--launch <id>` and per-game desktop entry toggle; quit with active
  runs asks keep/stop.
- Deps: M6, M8, M9, M10, M11.
- Acceptance: manual walkthrough on Wayland, first with the fake umu (put `tests/fake-umu-run` on
  `PATH` as `umu-run` in a shim dir; no extra override setting), then with real umu: install `.bat` multi-file fixture end to end; screenshots
  of each wizard step, uncertain-state UI, deletion confirmation list. Automated: none beyond clippy.

## M13: Packaging and documentation (haiku)

- M13a (Group B): `packaging/arch/PKGBUILD`, `data/` desktop file, metainfo, icons, build/run
  instructions in `README.md`. Acceptance: `desktop-file-validate` and `appstreamcli validate`
  pass (or NOT RUN if tools missing); `makepkg -si` builds in a clean chroot or is NOT RUN.
- M13b (with M14): `docs/USER.md` (install, runtimes, nested installers, troubleshooting, known
  limitations, implemented vs outstanding requirements from spec 17/18).

## M14: End-to-end demonstration (opus)

Real machine, real umu-run, real GE-Proton downloaded by Vortex, real GUI (human or computer-use).
Results with screenshots and log excerpts go to `docs/E2E-REPORT.md`; every spec 18 criterion is
marked PASS / FAIL / NOT RUN with evidence.

1. Fresh XDG dirs. Download GE-Proton via the runtimes page (verify hash, extraction).
2. Multi-file installer: a directory like spec 5.2 (mingw-built `setup.exe` + `data1.bin`..`data3.bin`
   + `setup-1.bin` + `Redist/prereq.exe`, or the `.bat` set if mingw is unavailable, stated in the
   report). Vortex proposes `setup.exe`, runs it with cwd = source dir, the installer reads its data
   files by relative path, launches the prereq which finishes after the main installer exits; Vortex
   stays in "Waiting for background processes" until it ends; source dir unchanged (checksum before/after).
3. Select the installed exe, launch from the library with launch args and an env var; both visible in
   the launch log. Stop the running game; no marker processes remain.
4. Standalone EXE installer, and MSI installer (msitools-built, or NOT RUN), into separate prefixes.
5. Second game on a different Proton (Experimental vs GE); each launch uses its own runtime (log header).
6. Winetricks verb into game A; game B's prefix untouched.
7. Run another exe in game A's prefix. Backup A, damage A, restore, launch again.
8. Kill Vortex during an install; restart; install shows Interrupted, not installed.
9. Restart Vortex; library persists. Remove game B's environment: confirmation list shown, game A
   launches afterwards, installer sources still present.
10. A failed launch (deleted exe) and a runtime failure (broken PROTONPATH) show distinguishable
    diagnostics.
- A commercial DRM-free installer (e.g. a GOG offline installer the user owns) is a stretch goal,
  reported only if actually run.
- Deps: all.

## M19: Verbose log retry (opus)

Spec 10 "Proton output" and E2E review finding F4 (a failed installer could not name the missing
file). `RunSpec::verbose` fills `Invocation::verbose_log_dir` with a per-run dir next to the run log;
`Installs::retry`, `launch::launch_game` and `winetricks::install_verb` take `verbose`. Proton's log
is redacted in place when the run ends; `diagnose::missing_files` names the files a failed verbose run
could not open (`WINEDEBUG=warn+file`). GTK: "Retry with verbose log" on a failed install, launch and
dependency run; a **Proton log** button in Logs.

- Owns: `runner.rs`, `process.rs` (verbose dir and redaction only), `logs.rs` (Proton log helpers),
  `diagnose.rs` (`missing_files`, `Class::MissingFile`), the `verbose` parameter in `install.rs`,
  `launch.rs`, `winetricks.rs`, the retry buttons in `install_wizard.rs`, `run_view.rs`,
  `game_page.rs`, `manage.rs`, the `protonlog` step of `tests/fake-umu-run`.
- Acceptance: fake-umu tests show the env (`PROTON_LOG=1`, `PROTON_LOG_DIR`, `UMU_LOG=debug`,
  `WINEDEBUG`) only on verbose runs, with the Proton log in the managed logs dir, redacted, and the
  missing file named (`tests/launch.rs`, `tests/install.rs`). Real Proton: `it_verbose_launch_names_the_missing_data_file`
  and `it_wizard_missing_part` (verbose retry names `setup_lantern_hollow-2.bin`).

## Top risks (details: ARCHITECTURE section 14)

1. Completion detection for detached/delayed second-stage installers (S1-S3). Mitigated by the
   two-barrier design and the explicit uncertain state; validated first, in M1.
2. Marker env not inherited in some Wine spawn paths (S2): only affects the process list, not the
   barrier.
3. Test fixtures need mingw-w64-gcc and msitools, not installed here; without them exe/msi
   scenarios are NOT RUN and the demo uses `.bat` installers.
4. First-run network cost (SLR ~700 MB + GE ~567 MB) and offline behaviour.
5. `umu-run winetricks` needs GE/UMU-Proton (S8).
6. Native-only: host `/tmp` sharing is required for host-side `wineserver`; no Flatpak.
