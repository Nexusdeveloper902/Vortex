# M1 spike results: process lifecycle, nested installers, cwd, MSI

Run on 2026-11-09 on the dev box: Linux 7.2.14-arch1-1, umu-launcher 1.4.4 (`/usr/bin/umu-run`),
Proton - Experimental `experimental-11.0-20261027b` (prefix version 11.0-100), steamrt4 runtime,
x86_64-w64-mingw32-gcc 16.2.0, msitools/wixl 0.106. All runs used throwaway prefixes under the session
scratch dir (`.../scratchpad/w1/pfx`, created with `umu-run wineboot -u`: rc 0, 9.7 s) and fixture copies
under `$HOME/.cache/vortex-m1-spike/` (deleted afterwards). No real prefix was touched.

Scripts: `spikes/lifecycle/run.sh <scenario>` (driver), `procscan.py` (0.5 s `/proc` monitor: marker,
PPid tree from umu-run, Wine-looking `comm`), `fixtures/` (.bat installers; `exe-src/` C + WiX sources,
built by `fixtures/exe-src/build.sh`). Raw output of every run: `spikes/lifecycle/logs/*.log`.
Every number below is copied from those logs. Tags: [VL] = observed in these runs.

Env for all runs: `WINEPREFIX=<abs> PROTONPATH=<abs Experimental> GAMEID=0 VORTEX_RUN_ID=<32 hex>`,
umu-run spawned with `env -C <cwd>`; barrier = `WINEPREFIX=<pfx> "$PROTONPATH/files/bin/wineserver" -w`
run on the host after umu-run exits.

## Verdicts

| ID | Verdict | One line |
|---|---|---|
| S1 | **CONFIRMED** | Host-side Proton `wineserver -w`: rc 0 in 0.00 s with no server, no server started; blocks 13.18 s while one runs |
| S2 | **CONFIRMED** (stronger than assumed) | Marker in `/proc/*/environ` of every process seen, including a child created with a custom env block that lacks it at Windows level |
| S3 | **CONFIRMED** | umu-run itself did not return until every nested/detached/delayed child ended; `wineserver -w` afterwards returned in 0.00 s |
| S4 | **CONFIRMED** | cwd under `drive_c` shows `C:\vtest-multi`; elsewhere `Z:\...`; relative `data1.bin` opens only when cwd = installer dir |
| S5 | **CONFIRMED** | `umu-run cmd /c Z:\...\setup.bat` works; so does `umu-run /host/path/setup.bat` directly |
| S6 | **CONFIRMED** | After SIGKILL of the parent, umu-run (ppid 1) and all 18 marker processes keep running; stop sequence clears them |
| S7 | **CONFIRMED**, with a correction | `msiexec /i Z:\...\vtest.msi /qn` and `/i /unix/path` both install; but bare `msiexec` always exits 0 (see exit codes) |
| S8 | **REFUTED for Experimental**; GE **NOT RUN** | `umu-run winetricks` with Experimental crashes (no `protonfixes/`) rc 1; running the downloaded GE-Proton was denied by the session's permission policy |
| S9 | **REFUTED** | `GE-Proton11-12-x86_64.tar.gz` top-level dir is `GE-Proton11-12-x86_64`, not the tag |

New findings that change the design (details below): **F1** exit codes are lost when the program is a
bare in-prefix name (`cmd`, `msiexec`); **F2** Windows console stdout/stderr of the program does not
reach umu-run's stdout/stderr; **F3** `wineserver -k` returns rc 1 when no server is running;
**F4** SIGTERM to umu-run alone leaves `pv-adverb` + a Wine process for up to ~3 s.

## S1: `wineserver -w` from the host (logs/01-init-s1.log)

```
s1(a): no server running. processes before: (no wineserver process)
s1(a): socket dir /tmp/.wine-1000/server-33-451e: lock
s1(a): wineserver -w rc=0 took 0.00s
s1(a): processes after: (no wineserver process)
s1(a): socket dir after: lock
s1(b): wineserver up at +2.06s: 87024 .../Proton - Experimental/files/lib/wine/../../bin/wineserver
s1(b): wineserver -w rc=0 took 13.18s, returned at +15.27s; umu-run alive: yes
s1(b): umu-run rc=0 at +15.30s
s1(c): -w with a non-existent prefix path: rc=0 took 0.00s; dir created: no
```

- No server: returns at once, rc 0, starts nothing, leaves the socket dir as it was. Non-existent
  prefix: rc 0, creates nothing.
- Server running (`cmd /c ping -n 12`): blocks until the server exits; it returned ~30 ms before
  umu-run did, so umu-run can still be alive (container teardown) right after `-w` returns.
- Works from the host because the container shares host `/tmp` and the PID namespace.

Implication M7: keep phase 2 exactly as designed (Proton's `files/bin/wineserver -w`, no timeout).
No socket-polling fallback needed. Keep the Draining phase: umu-run may outlive `-w` by a few ms.

## S3: barriers vs nested / detached / delayed installers (logs/02..05)

| Scenario | How the child is started | umu-run returned | done-file written | `wineserver -w` after umu-run |
|---|---|---|---|---|
| nested (.bat) | `start "" cmd /c Redist\prereq.bat`, parent exits | +17.54 s | +17.44 s | 0.00 s |
| startb (.bat) | `start /b cmd /c Redist\prereq.bat`, parent exits | +17.56 s | +17.47 s | 0.00 s |
| delayed (.bat) | `start` stage1 → parent exits → after 5 s stage1 `start /b` stage2 (10 s) | +17.05 s | +16.95 s | 0.00 s |
| svc (.bat) | `sc create` + `sc start` (process spawned by services.exe) | +17.94 s | not written (*) | 0.00 s |
| exe-detached | `CreateProcess(DETACHED_PROCESS)`, setup.exe returns 0 at once | +18.47 s | +18.38 s | 0.00 s |
| exe-noenv | as above + custom env block without `VORTEX_RUN_ID` | +19.00 s | +18.90 s | 0.00 s |
| exe-breakaway | `DETACHED_PROCESS \| CREATE_BREAKAWAY_FROM_JOB \| CREATE_NEW_PROCESS_GROUP` | +19.02 s | +18.93 s | 0.00 s |
| wmic (.bat) | `wmic process call create` | +3.43 s | n/a | 0.00 s |

(*) svc: services.exe killed the non-service `cmd.exe` at +13.71 s (it never called the service
dispatcher), but its orphaned `ping` kept running to +18.29 s and umu-run waited for it.
wmic: Wine's wmic printed `Error: Command not supported`, so no child was created; INCONCLUSIVE for WMI.

Excerpt (delayed, logs/03-delayed-wmic.log): setup's `cmd.exe` is gone at 4.06 s, stage2 appears at
8.12 s, and everything (umu-run included) disappears together at 17.27 s:
```
  4.06 + pid=91462 ... comm=cmd cmd=cmd /c stage1.bat
  4.06 - pid=91355 comm=cmd.exe
  8.12 + pid=91699 ... comm=cmd cmd=cmd /c stage2.bat
 17.27 - pid=91024 comm=umu-run
```

Why umu-run waits: the Proton `steam.exe` wrapper process (`wine c:\windows\system32\steam.exe ...`)
stays alive until the last non-system Wine process in the prefix exits, and the pressure-vessel
container ends only after the wineserver exits. That is observed behaviour, not a documented contract.
In every normal-exit run the wineserver was already gone when umu-run returned.

**S3 is CONFIRMED: no redesign needed.** The wineserver barrier is still needed. It is the
authoritative check, it costs 0 s when umu-run already waited, and it covers the cases where umu-run
dies early: SIGTERM (F4), parent killed (S6), or a future umu/Proton that stops waiting.

## S2: `VORTEX_RUN_ID` marker in `/proc/*/environ`

- Every process the monitor saw in every scenario had `marker=yes`: umu-run, pressure-vessel,
  srt-bwrap, pv-adverb, proton (python3), steam.exe, wineserver, services/winedevice/plugplay/svchost/
  explorer/rpcss/tabtip/xalia, cmd, conhost, ping, prereq.exe. No process was `UNREADABLE`, and no
  Wine-looking process lacked the marker. Every Wine process had PPid = pv-adverb (the subreaper
  inside the container), so all were also descendants of umu-run.
- Worst case `exe-noenv`: setup.exe passed `lpEnvironment` = `PATH, SYSTEMROOT, VTEST` only. The
  child's **Windows** env lacked the marker (`prereq.done: marker=NONE`), but its **Unix**
  `/proc/<pid>/environ` still had it:
  `4.06 + pid=101192 ... marker=yes ... comm=prereq.exe`. Wine builds the Unix environ of a new
  process from the Unix side, not from the Win32 env block.
- A service-spawned process (`sc start`) had the marker because services.exe was started in the same
  run (`waitforexitandrun` waits out an older wineserver first, so each run starts its own
  services.exe).
- The marker is also visible to Windows code (`%VORTEX_RUN_ID%` printed by the .bat and exe).

Implication M7: `scan_marker` is reliable for the UI process list and for stop. The PPid/cmdline
fallback in 6.2 is not needed (keep the wineserver barrier as authority anyway).

## S4: cwd (logs/06-s4-s5.log, logs/05-exe.log)

| Host `current_dir` | How invoked | `%CD%` / `GetCurrentDirectory` | relative `data1.bin` |
|---|---|---|---|
| `$HOME/.cache/.../vtest-multi` | `cmd /c Z:\...\setup.bat` | `Z:\home\user\.cache\vortex-m1-spike\home\vtest-multi` | found |
| `/tmp/.../w1/vtest-multi` | same | `Z:\tmp\...\w1\vtest-multi` | found |
| `<pfx>/drive_c/vtest-multi` | `cmd /c Z:\...\pfx\drive_c\vtest-multi\setup.bat` | **`C:\vtest-multi`** (while `%~dp0` = `Z:\tmp\...\drive_c\vtest-multi\`) | found |
| `<pfx>/drive_c/vtest-multi` | `cmd /c C:\vtest-multi\setup.bat` | `C:\vtest-multi` | found |
| `/tmp/.../w1` (not installer dir) | `cmd /c Z:\...\setup.bat` | `Z:\tmp\...\w1` | **missing** (`File not found.`) |
| installer dir (exe) | `umu-run /host/setup.exe` | `Z:\home\...\exe-detached` | found (cwd and module dir) |
| `/tmp/.../w1` (exe) | `umu-run /host/setup.exe` | `Z:\tmp\...\w1` | **missing** via cwd; found via module dir |

- Wine maps a host cwd under the prefix's `drive_c` to `C:\...`. Elsewhere it is `Z:\<path>`.
  `Command::current_dir(host dir)` is enough, and no `start /d` wrapper is needed.
- Relative `data1.bin` (and `call Redist\prereq.bat`, `setup-1.bin`) works only when cwd = installer
  dir. That confirms the contract: `cwd_host` = installer's parent.
- A host exe path in `drive_c` invoked via its `Z:` spelling keeps a `Z:` module path (`%~dp0`).
  Paths written into the DB/desktop entries for in-prefix games should use the `C:` form
  (`paths::to_win` must prefer `C:` for paths under `<prefix>/drive_c`).

## S5: .bat fixtures

`umu-run cmd /c Z:\...\setup.bat` ran every .bat fixture (all scenarios above). Also
`umu-run /host/path/setup.bat` (no `cmd /c`) ran it with the same result (`s5:` block). Note F1:
with bare `cmd` the exit code is lost. Use `C:\windows\system32\cmd.exe` (or the host path of
the .bat) when the exit code matters.

## S6: parent SIGKILL (logs/07-stop-s6.log)

```
s6: parent=110010 umu-run=110012; marker processes before kill: 18
s6: parent alive: no; umu-run alive: yes ppid=1
s6: marker processes after parent SIGKILL: (18 listed: umu-run, srt-bwrap, pv-adverb, proton, steam.exe, wineserver, ..., ping)
stop: wineserver -k rc=0 took 0.05s
stop: after -k: umu-run alive=no at +0.16s; marker procs=0
```

Implication M7/M9: `recover_on_startup` can find a detached run with a marker scan. The stop
sequence works without the pipes.

## Stop sequence (ARCHITECTURE 6.3) (logs/07-stop-s6.log, logs/08-sigterm.log)

Run: setup.bat `start`s a 120 s prereq and itself pings 120 s; stop after 10 s with 21 marker processes.
```
stop: marker procs before stop: 21
stop: wineserver -k rc=0 took 0.05s
stop: after -k: umu-run alive=no at +0.08s; marker procs=0
stop: final marker procs=0; wineserver: (no wineserver process)
stop: umu-run rc=0 at +10.21s
```
Step 1 (`wineserver -k`) alone took everything down within 0.08 s. Steps 2-3 were not reached.

Step 2 in isolation (SIGTERM to umu-run, no `-k`), 18 marker processes:
```
sigterm: umu-run alive=no at +0.11s; marker procs=2
    114047 1 pv-adverb ...
    114444 114047 winedevice.exe C:\windows\system32\winedevice.exe
sigterm: +3 s later marker procs=0; wineserver: (no wineserver process)
stop: wineserver -k rc=1 took 0.00s
```
- **F4**: after SIGTERM, umu-run exits in 0.11 s but `pv-adverb` (reparented to pid 1) and a Wine
  process outlive it for up to ~3 s. umu-run's exit is therefore not "all gone". Keep the marker drain.
- **F3**: `wineserver -k` with no server running returns **rc 1**. Treat it as "nothing to kill",
  not as an error.

Implication M7: keep 6.3 order (`-k` first). Treat `-k` rc 1 as success. After every step, re-scan
the marker before declaring Finished.

## S7: MSI (logs/09-msi-s7.log)

Fixture: `fixtures/exe-src/vtest.wxs` → `wixl -a x64` → `vtest.msi` (one file into
`C:\Program Files\VortexSpike`). Run from cwd = msi's dir with `STEAM_COMPAT_INSTALL_PATH` = msi's dir:
```
msi: msiexec /i Z:\home\user\.cache\vortex-m1-spike\home\vtest-msi-real\vtest.msi /qn
msi: rc=0; installed file: VTEST-MSI-PAYLOAD
msi: msiexec /x Z:\...\vtest.msi /qn  -> uninstall rc=0; file still present: no
msi: msiexec /i /home/user/.cache/vortex-m1-spike/home/vtest-msi-real/vtest.msi /qn
msi: rc=0; installed file: VTEST-MSI-PAYLOAD
msi: uninstall rc=0; file still present: no
```
Both `Z:\` and plain Unix paths install. `getcompatpath` is not needed.

But bare `msiexec` returned rc 0 for a corrupt `fake.msi` and a `missing.msi` too (s7 block). The same
calls through the C: path (manual run, output pasted):
```
C:\windows\system32\msiexec.exe /i Z:\...\vtest-msi\fake.msi -> rc=91      (1627 & 0xff)
C:\windows\system32\msiexec.exe /i Z:\...\vtest-msi\missing.msi -> rc=2
C:\windows\system32\msiexec.exe /i Z:\...\vtest-msi-real\vtest.msi -> rc=0
```

## F1: exit codes (manual runs in the w1 prefix)

```
umu-run cmd /c exit 7                                   -> rc=0
umu-run cmd /c Z:\...\exitcode.exe 9                    -> rc=0
umu-run msiexec /i Z:\nope\missing.msi /qn              -> rc=0
umu-run /host/exitcode.exe 7                            -> rc=7
umu-run /host/exitcode.exe 3010                         -> rc=194   (3010 & 0xff)
umu-run <pfx>/drive_c/windows/system32/cmd.exe /c exit 7 -> rc=7
umu-run 'C:\windows\system32\cmd.exe' /c exit 7          -> rc=7
umu-run <pfx>/drive_c/.../msiexec.exe /i Z:\nope\missing.msi /qn -> rc=2
```
- Exit codes propagate only when arg 1 is a path (host path or `C:\...`). With a bare name
  (umu: "Executable not found", passed through) the code is always 0.
- Codes are truncated to 8 bits: 3010 → 194, 1641 → 105, 1627 → 91.

Implication M7/M9/M10 (section 5 contract): run built-ins as `C:\windows\system32\<name>.exe`, never
bare names. `diagnose` must compare installer codes modulo 256 (0, 194 = 3010 reboot required,
105 = 1641 reboot initiated count as success). Treat a nonzero code as a hint only.

## F2: Windows program output is not on umu-run's pipes

`umu-run /host/exitcode.exe 7` (prints `stdout-line` / `stderr-line`) and `umu-run cmd /c echo
hello-from-cmd`: neither string appears in umu-run's stdout or stderr. Same under a pty (`script -qc`).
umu-run's stderr only carries umu/Proton/Mesa lines (`INFO: ...`, `Proton: Error: unable to use parent
for game drive`, `ntsync: up and running.`, `WARNING: radv ...`).
Implication M7/M5: the run log contains umu/Proton/Wine output, not the installer's console output.
Do not build diagnostics that expect installer text on stdout. `PROTON_LOG=1` (opt-in verbose) is the
way to get Wine-side detail.

## S8: winetricks (logs/10-s8-experimental.log)

```
s8: PROTONPATH=.../Proton - Experimental protonfixes/winetricks present: no
s8: umu-run winetricks win10 rc=1; winetricks.log: No such file or directory
FileNotFoundError: [Errno 2] No such file or directory: '.../Proton - Experimental/protonfixes'
```
umu's `run_command` sets `cwd=$PROTONPATH/protonfixes` for winetricks, and Popen fails before anything
runs. Proton Experimental cannot run winetricks through umu (REFUTED for Experimental).
GE-Proton11-12 was downloaded and verified (sha512 OK), and `protonfixes/winetricks` exists in it.
Running it (`umu-run winetricks win10` in a fresh prefix) was **NOT RUN**: the session's permission
policy denied executing downloaded code. Re-run with
`WORK=<tmp> PROTONPATH=<GE dir> spikes/lifecycle/run.sh init s8`.
Implication M10: hide/disable dependency installs unless the runtime has `protonfixes/winetricks`. Say
so in the UI ("requires GE-Proton or UMU-Proton"). Re-verify S8 on GE before M10 is merged.

## S9: GE tarball layout

```
GE-Proton11-12-x86_64.tar.gz: OK            (sha512sum -c)
   9007 GE-Proton11-12-x86_64               (tar -tzf | top-level component | uniq -c)
version: 1793687165 GE-Proton11-12
compatibilitytool.vdf internal_name/display_name: GE-Proton11-12-x86_64
toolmanifest.vdf require_tool_appid: 4183110 (same steamrt4 as Experimental)
```
REFUTED: the top-level dir is `GE-Proton11-12-x86_64` (asset name minus `.tar.gz`), not the tag.
Implication M3: use the single top-level dir actually found in the archive, and reject archives with
more than one. Display name: the `version` file second field (`GE-Proton11-12`).

## Summary of design impact

- M7 (`process.rs`/`runner.rs`): the design holds (umu-run wait → `wineserver -w` → marker drain).
  Changes: `-k` rc 1 = nothing to stop. Marker drain after SIGTERM needs ~3 s, which fits the 10 s
  window. Built-ins must be spawned as `C:\windows\system32\<x>.exe`. The PPid/cmdline fallback for
  the process list can be dropped.
- M9 (`install.rs`): MSI via `C:\windows\system32\msiexec.exe /i <Z:\ path>` with cwd = msi dir.
  Exit codes are 8-bit. Completion must come from the barrier plus user confirmation, never from the
  exit code (already rule 9).
- M10: winetricks only on runtimes with `protonfixes/winetricks`.
- M3: GE dir name from the archive, not the tag.
- M5: no installer console text in logs.
