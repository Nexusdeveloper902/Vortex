# M14 end-to-end report

Date: 2026-11-10. Machine: the developer's Arch Linux desktop, run as an unprivileged user.
Everything ran against real umu-run and Proton. Nothing was stubbed. The fake umu-run was not used.

## Environment

| Component | Version |
|---|---|
| Kernel | 7.2.14-arch1-1 |
| Compositor | Hyprland 0.57.1 (Wayland) |
| rustc | 1.95.0 |
| gtk4 / libadwaita | 4.24.1 / 1.10.0 |
| umu-launcher | 1.4.4 |
| Proton Experimental | experimental-11.0-20261027b (Steam install) |
| GE-Proton | GE-Proton11-12, downloaded by `runtime_ge` (sha512 verified) |
| mingw-w64-gcc | 16.2.0 (fixture installers and games) |
| msitools / wixl | 0.106 (fixture MSI) |

### Isolation

All data lived in one throwaway root, `/var/tmp/vortex-e2e-zzpfwK`. `XDG_DATA_HOME`,
`XDG_CONFIG_HOME`, `XDG_CACHE_HOME` and `XDG_STATE_HOME` pointed at `$ROOT/xdg/{data,config,cache,state}`.
The only shared item was `$ROOT/xdg/data/umu`, a symlink to `~/.local/share/umu`, so the real umu
runtime was used read-only. Desktop entries went to `$ROOT/xdg/data/applications`. The user's real
Vortex data was never opened. When the run ended, no process carried a `VORTEX_RUN_ID` marker,
`pgrep` found no `wineserver`, `umu-run` or `vortex`, and the root was deleted.

## How to reproduce

```sh
export VORTEX_E2E_ROOT=$(mktemp -d /var/tmp/vortex-e2e-XXXXXX)
export XDG_DATA_HOME=$VORTEX_E2E_ROOT/xdg/data XDG_CONFIG_HOME=$VORTEX_E2E_ROOT/xdg/config \
       XDG_CACHE_HOME=$VORTEX_E2E_ROOT/xdg/cache XDG_STATE_HOME=$VORTEX_E2E_ROOT/xdg/state
mkdir -p "$XDG_DATA_HOME" && ln -s ~/.local/share/umu "$XDG_DATA_HOME/umu"
export VORTEX_IT_PROTON="$HOME/.local/share/Steam/steamapps/common/Proton - Experimental"
export VORTEX_E2E_BIN=$PWD/target/release/vortex       # Exec= of the desktop entries
cargo build --release
cargo test -p vortex-core --test e2e -- --ignored --nocapture --test-threads=1
```

`VORTEX_IT_GE=/abs/GE-Proton` skips the GE download. Without `VORTEX_E2E_ROOT` the suite uses a
tempdir with `Paths::under`, and only the kill scenario (6) needs the root. The tests need
`mingw-w64-gcc` and `wixl`. If either is missing they fail, they never skip.

The suite is 3 scenario tests (`it_e2e_install_to_launch`, `it_e2e_missing_installer_data`,
`it_e2e_app_killed_mid_install`) + 1 child helper (`e2e_child_begins_install`, which panics unless
spawned by the kill test).

Code: `crates/vortex-core/tests/e2e.rs`, with fixtures in `crates/vortex-core/tests/fixtures/e2e/`:

- `setup.c`: a multi-file installer. It needs `data1.bin`, `data2.bin` and `setup-1.bin` next to it
  (`data3.bin` too when built with `-DNEED_DATA3`), and returns 3 if one is missing. It records its
  cwd, copies the data and `payload\Game.dat` into `C:\Program Files\E2E Multi Game`, and starts
  `Redist\prereq.exe` with `DETACHED_PROCESS`.
- `prereq.c`: a nested prerequisite. It sleeps 5 s, then writes `C:\vortex-prereq.done`.
- `game.c`: the game, a standalone installer and a tool. It reports module, cwd, argv, one env var
  and the first line of `data\data1.bin`. `VORTEX_E2E_MODE=crash` dereferences NULL, and `exit:N` exits with N.
- `game.wxs`: an MSI that installs `MsiGame.exe`.

## Results (final run: 4 passed, 0 failed, about 316 s)

| # | Scenario | Test | Result |
|---|---|---|---|
| 1 | Multi-file installer: directory with setup.exe, BIN files and a nested prereq | `it_e2e_install_to_launch` | PASS |
| 2 | Standalone EXE (GE-Proton) and MSI (Proton Experimental) | `it_e2e_install_to_launch` | PASS |
| 3 | Two runtimes; winetricks isolation | `it_e2e_install_to_launch` | PASS |
| 4 | Run another EXE/MSI in an existing prefix | `it_e2e_install_to_launch` | PASS |
| 5 | Backup and restore | `it_e2e_install_to_launch` | PASS |
| 6 | App killed mid-install | `it_e2e_app_killed_mid_install` (+ helper `e2e_child_begins_install`) | PASS |
| 7 | Remove one game | `it_e2e_install_to_launch` | PASS |
| 8a | Failed launch: non-zero exit, crash, broken runtime | `it_e2e_install_to_launch` | PASS (needs #21) |
| 8b | Missing installer resource | `it_e2e_missing_installer_data` | PASS |
| - | Persistence across a DB reopen | `it_e2e_install_to_launch` | PASS |

On `main` without the fix PRs, scenario 7 fails because the desktop entry stays behind (#20), and
scenario 8a fails with `findings None` for a launch that exits 42 (#21).

### Evidence

**1. Multi-file installer.** `analyze` ranked `setup.exe` at 60 and `Redist/prereq.exe` at -80, and the
result was not ambiguous. The states were `PrefixInit -> Running -> Waiting -> SelectExe`, and the
install run went through `Running, WaitingForChildren, Draining`. Records written by the installer:

```
setup.exe saw cwd=Z:\var\tmp\vortex-e2e-zzpfwK\sources\E2E Multi Installer
prereq.done written 5.01 s after setup.exe started, 0.61 s before SelectExe
exe candidates ["C:\\Program Files\\E2E Multi Game\\Game.exe"]      (unins000.exe excluded)
installed as game 2 (desktop entry .../xdg/data/applications/io.github.nexusdeveloper902.Vortex.game-2.desktop)
```

The launch used args `["--mode","two words","$HOME"]` and env `VORTEX_E2E_VAR=hello e2e`:

```
module=C:\Program Files\E2E Multi Game\Game.exe
cwd=C:\Program Files\E2E Multi Game
arg=--mode
arg=two words
arg=$HOME            (kept literally: no shell)
env=hello e2e
data1=DATA1-CONTENT-OK
```

The source tree sha256 (no symlink following, 8 entries) was `ae5524b1…ee99190` before and after
every step. The installer was never stopped early, and the 5 s prereq finished before `SelectExe`.

**2. EXE and MSI.** `StandaloneSetup.exe` scored 100 and installed into prefix 3 on GE-Proton11-12.
`E2E MSI Game.msi` scored 100 and installed into prefix 4 on Proton Experimental. Both launched with exit 0.

**3. Runtimes and winetricks.** `winetricks list-all` gave 572 verbs. `win10` was installed in env 3
(GE), and its dependency row became `Installed`. The tree hashes of prefixes 2 and 4 did not change.
Winetricks on Proton Experimental was refused with `RuntimeInvalid` ("GE-Proton or UMU-Proton is required").

**4. Run another program.** `E2E Tool.exe`, run from a directory with a space in its name, inside prefix 2:

```
module=Z:\var\tmp\vortex-e2e-zzpfwK\sources\tools dir\E2E Tool.exe
cwd=Z:\var\tmp\vortex-e2e-zzpfwK\sources\tools dir
arg=x y
env=tool
```

The MSI was then run again in its existing prefix and finished with exit 0.

**5. Backup and restore.** The backup was `backups/2-1794341327.tar.gz` (124,240,798 bytes). After
`Game.exe` was deleted, the launch failed with `not found: executable …/Game.exe`. After the restore,
the launch exited 0.

**6. Killed mid-install.** A child test process began an install and was SIGKILLed while its install
run was live. umu-run and the installer kept running. A new `Installs` saw one detached run.
`stop_detached` ended it in about 1 s and `scan_marker` came back empty. The install became
`Interrupted`, the game `Interrupted`, and `exe_path` stayed `None`, so it was not reported as installed.

**7. Remove a game.** The deletion plan was exactly `[prefixes/3 (291,702,745 bytes)]`. After it ran,
the prefix 2 hash and all four source hashes were unchanged, game 3's desktop entry was gone (#20),
and game 2 still launched.

**8. Diagnostics.**

```
exit 42  -> [{"class":"Unknown","confidence":"Likely","evidence":["exited with Code(42); no known pattern matched"]}]
crash    -> [{"class":"AppCrash","confidence":"Likely","evidence":["exit code 5 is the low byte of STATUS_ACCESS_VIOLATION (0xC0000005)"]}]
bad PROTONPATH -> runtime at …/sources/no-such-proton is not usable: No such file or directory (os error 2)
missing data3.bin -> install Failed, InstallerFailure, exit 3, log logs/5/20-install.log (21 lines)
```

The crash log has no crash line, because Wine's crash dialog output does not reach umu's pipes. The
classification comes from the 8-bit exit code (see limitations).

**Persistence.** After the DB was reopened, the library listed `E2E Killed Install (Interrupted)`,
`E2E MSI Game (Installed)` and `E2E Multi Game (Installed)`.

## GUI demonstration (release build, isolated env)

These flows were observed on screen and also checked in the DB, on disk or in the process list:

| Flow | Observed | Screenshot |
|---|---|---|
| Empty library on first start | yes | [00](screenshots/00-empty-library.png) |
| Library with the e2e games | yes | [01](screenshots/01-library-e2e-games.png) |
| Wizard: folder source, installer choice, name and runtime (GE) | yes | [02](screenshots/02-wizard-source.png) [03](screenshots/03-wizard-installer.png) [04](screenshots/04-wizard-name-runtime.png) |
| Wizard: installing (prefix, then setup) | yes | [05](screenshots/05-wizard-installing-prefix.png) [06](screenshots/06-wizard-installing-setup.png) |
| Wizard: executable choice, done (game 6, prefix 6) | yes | [07](screenshots/07-wizard-executable.png) [08](screenshots/08-wizard-done.png) |
| Game page | yes | [09](screenshots/09-game-page.png) |
| Configure args and env, saved to the DB (`["gui two words"]`, `{"VORTEX_E2E_VAR":"from the gui"}`) | yes | [10](screenshots/10-configure.png) [11](screenshots/11-configure-args-env.png) |
| Play from the library, with args and env reaching the game | yes | [12](screenshots/12-game-running.png) |
| Library persists after an app restart | yes | [13](screenshots/13-library-after-restart.png) |
| Game page refreshed after Save (#23) | yes | [14](screenshots/14-game-page-after-save.png) |
| Runtimes page | yes | (screenshot removed) |
| Dependencies dialog, then win10 installed (winetricks.log and DB row) | yes | [16](screenshots/16-dependencies.png) [17](screenshots/17-dependency-run.png) |
| Failed launch (crash) dialog with findings (#21) | yes | [18](screenshots/18-launch-failed-diagnosis.png) |
| Logs page and log viewer | yes | [19](screenshots/19-logs.png) [20](screenshots/20-log-viewer.png) |
| Run EXE/MSI in the game's prefix | yes | [21](screenshots/21-run-exe-in-prefix.png) |
| Quit guard, then "Stop all and quit" (no processes left) | yes | [22](screenshots/22-quit-guard.png) |
| `vortex --launch 6` with the app closed | yes (the game ran) | - |
| `vortex --launch 4` with the app open (one process, run shown) | yes | [23](screenshots/23-launch-via-cli-running-instance.png) |
| Deletion-plan dialog, then removal of "E2E Broken Installer" (source hash unchanged) | yes | [24](screenshots/24-deletion-plan.png) [25](screenshots/25-library-after-removal.png) |
| Back up, then Restore (a file created after the backup disappeared) | yes | [26](screenshots/26-restore-dialog.png) [27](screenshots/27-restore-confirm.png) [28](screenshots/28-restore-done.png) |
| Empty content pane on main before #22 | bug | [bug](screenshots/bug-empty-content-before-fix.png) |

**Not verified in the GUI:**

- The GE-Proton download from the Runtimes page. It was only done in core, by `runtime_ge` in the e2e suite.
- The wizard's "Waiting for background processes" state. umu-run itself waits for the detached prereq,
  so that phase passed too quickly to see.
- The detached-run banner and resuming an interrupted install after the app is killed. This is covered
  in core (scenario 6) but was not driven in the GUI.
- Running an installed game's uninstaller, and "Mark as finished" for an installer that hangs.
- No GUI test runs in CI, because CI has no display. The GUI checks above were manual, with ydotool and wtype on Hyprland.

## Spec 17 (MVP scope)

| # | Feature | Status | Code |
|---|---|---|---|
| 1 | Proton runtime discovery | implemented | `runtime.rs` `discover`, `validate` |
| 2 | UMU integration | implemented | `runner.rs` `Invocation::command`, `process.rs` |
| 3 | GE-Proton support | implemented | `runtime_ge.rs` |
| 4 | Per-game prefix | implemented | `install.rs` `begin` (PrefixInit) |
| 5 | EXE installer | implemented | `install.rs`, `source.rs` `analyze` |
| 6 | MSI installer | implemented | `runner.rs` (msiexec /i), `source.rs` |
| 7 | Multi-file directory | implemented | `source.rs` `analyze` (scoring, siblings) |
| 8 | Installer working directory | implemented | `runner.rs` `cwd_win`, `Command::current_dir` |
| 9 | Nested prerequisites | implemented | `process.rs` barrier: umu exit, `wineserver -w`, marker drain |
| 10 | Arbitrary EXE/MSI in a prefix | implemented | `launch.rs` `run_in_env`, `ui/manage.rs` |
| 11 | Manual exe registration | implemented | `install.rs` `confirm_exe`, `register_existing` |
| 12 | Persistent library | implemented | `db.rs`, `ui/library.rs` |
| 13 | Launch with saved configuration | implemented | `launch.rs` `game_invocation`, `ui/configure.rs` |
| 14 | Basic winetricks | implemented (GE/UMU-Proton only) | `winetricks.rs` |
| 15 | Install and launch logs | implemented | `logs.rs`, `ui/run_view.rs` |
| 16 | Environment removal | implemented | `delete.rs` `DeletionPlan` |
| 17 | Desktop integration | implemented | `desktop.rs`, `data/`, `--launch` in `vortex-gtk` |

## Spec 18 (acceptance criteria)

| Criterion | Result | Evidence |
|---|---|---|
| Import an EXE installer | PASS | scenario 2, GUI wizard |
| Import an MSI installer | PASS | scenario 2 |
| Select a directory with an EXE and several BIN files | PASS | scenario 1, GUI [02](screenshots/02-wizard-source.png) |
| Identify or select the primary installer | PASS | analyze 60 vs -80; GUI [03](screenshots/03-wizard-installer.png) |
| Directory structure preserved | PASS | source hash unchanged (8 entries) |
| Relative paths and data files accessible | PASS | setup needs the BINs; `data1=DATA1-CONTENT-OK` |
| Dedicated prefix | PASS | prefixes 2, 3, 4, 6, one per game |
| Installer runs through UMU and Proton | PASS | real umu-run 1.4.4, explicit PROTONPATH and WINEPREFIX |
| Nested prerequisite completes | PASS | prereq.done 0.61 s before SelectExe |
| No premature termination of children | PASS | 5 s detached prereq finished |
| Installation completes | PASS | user-confirmed exe exists; status Installed |
| Prefix persists | PASS | relaunches, DB reopen, app restart |
| Additional executables in the prefix | PASS | scenario 4, GUI [21](screenshots/21-run-exe-in-prefix.png) |
| Dependencies without affecting other games | PASS | scenario 3 hashes |
| Different Proton versions per game | PASS | GE (3, 6) and Experimental (2, 4) |
| Prefix backup and restore | PASS | scenario 5, GUI [28](screenshots/28-restore-done.png) |
| Register an installed exe | PASS | SelectExe, `confirm_exe`; GUI [07](screenshots/07-wizard-executable.png) |
| Launch from the graphical library | PASS | GUI [12](screenshots/12-game-running.png) |
| Same prefix and runtime reused | PASS | launch report module path in the install prefix |
| Args and env respected | PASS | scenario 1 report; GUI configure and play |
| Games stay registered after a restart | PASS | DB reopen; GUI [13](screenshots/13-library-after-restart.png) |
| Interrupted installs not shown as successful | PASS | scenario 6: Interrupted, no exe |
| Uninstalling one game leaves the others intact | PASS | scenario 7 hashes |
| Sources not destroyed | PASS | source hashes after every step, including deletions |
| Destructive operations confirmed | PASS | GUI [24](screenshots/24-deletion-plan.png), [27](screenshots/27-restore-confirm.png), [22](screenshots/22-quit-guard.png) |
| Failed launches give accessible diagnostics | PASS (with #21) | scenario 8a; GUI [18](screenshots/18-launch-failed-diagnosis.png) |
| Runtime and install errors distinguishable | PASS | RuntimeInvalid vs InstallerFailure vs AppCrash |
| Missing resources and wrong cwd diagnosable | PARTIAL | missing `data3.bin` gives InstallerFailure with exit 3 and a readable log, but no finding names the missing file, because the installer printed nothing. The cwd is logged in the run header and is always correct by construction. |

## Bugs found

| PR | Bug | Regression test |
|---|---|---|
| [#20](https://github.com/Nexusdeveloper902/Vortex/pull/20) | Removing a game left its `.desktop` entry behind | `removing_a_game_removes_only_its_desktop_entry` |
| [#21](https://github.com/Nexusdeveloper902/Vortex/pull/21) | Finished launches were never diagnosed (`findings` NULL), the GUI showed nothing, and crash exit codes were `Unknown` | `finished_launch_is_diagnosed_before_finished`, diagnose unit tests |
| [#22](https://github.com/Nexusdeveloper902/Vortex/pull/22) | Empty main window: the navigation view was parented twice (adw CRITICAL) | none (needs a display); screenshot before and after |
| [#23](https://github.com/Nexusdeveloper902/Vortex/pull/23) | The game page showed stale args and env after Configure > Save | none (needs a display); [14](screenshots/14-game-page-after-save.png) |

## Known limitations and observations

- Exit codes are 8 bits under umu, so a crash shows up as exit 5 (0xC0000005 & 0xff). A real program
  that exits 5 on purpose is also reported as a "Likely" AppCrash. Wine's crash text does not reach the log.
- Winetricks only works on GE-Proton or UMU-Proton. Proton Experimental is refused by design.
- The Logs page lists runs with a `game_id`. "Run EXE" and winetricks runs (no game) are in the log
  directory but not on the page.
- The quit guard calls an exec run "a program", not by its exe name.
- The dependency run view keeps its process list after the run ends.
- The e2e suite takes about 5 minutes and downloads GE-Proton unless `VORTEX_IT_GE` is set.
- Not tested: real commercial installers (InstallShield, Inno, NSIS), games with DRM or anti-cheat, controllers, multi-GPU.
