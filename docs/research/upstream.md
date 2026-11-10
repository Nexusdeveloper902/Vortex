# Upstream interface research (umu / Proton / winetricks)

Tags: [VL] VERIFIED-LOCALLY, [DOC] FROM-UPSTREAM-DOCS (url), [SRC] read from installed umu 1.4.4 / Proton source (counts as local, not run), [?] UNCERTAIN.
Machine: umu-launcher 1.4.4 (/usr/bin/umu-run, py3.14), winetricks 20260125, Proton - Experimental `experimental-11.0-20261027b` (prefix version 11.0-100), GE-Proton latest = GE-Proton11-12 (2026-11).
Test prefix used for all [VL] runs: /tmp/claude-1000/vtest/pfx with PROTONPATH="~/.local/share/Steam/steamapps/common/Proton - Experimental" (works as a PROTONPATH).

## 1. UMU invocation

### 1.1 CLI
- `umu-run [-h] [-v] [--config FILE] [winetricks]`; usage is `umu-run FILE [ARG...]`, `umu-run winetricks VERB...`, `umu-run --config cfg.toml`. [VL] `umu-run --help`; [DOC] man umu(1) (https://github.com/Open-Wine-Components/umu-launcher/blob/main/docs/umu.1.scd)
- `umu-run --version` -> `umu-launcher version 1.4.4 (...)` printed to stderr/stdout via log; parse with regex `version (\d+\.\d+\.\d+)`. [VL]
- Env vars read (all [SRC] + man): `WINEPREFIX` (must be non-empty absolute after expanduser; default `~/Games/umu/$GAMEID`, or `~/Games/$STORE` if STORE set), `GAMEID` (default `umu-default`; `umu-<n>` => protonfix id `<n>`; any other string e.g. `0` applies no fix), `PROTONPATH` (dir path, bare dir name looked up in `~/.local/share/Steam/compatibilitytools.d`, or codename), `STORE` (egs/gog/battlenet/amazon/humble/itchio/ubisoft), `UMU_LOG` (`1` or `debug` = debug logging; also un-quiets winetricks), `PROTON_VERB`, `PROTONFIXES_DISABLE=1`, `UMU_RUNTIME_UPDATE=0` (don't update SLR), `UMU_NO_RUNTIME`, `UMU_ZENITY=1`, `UMU_HTTP_RETRIES`, `UMU_HTTP_TIMEOUT`, `UMU_FOLDERS_PATH` (relocates `umu/` and `Steam/compatibilitytools.d`), `STEAM_COMPAT_INSTALL_PATH`, `STEAM_COMPAT_LIBRARY_PATHS`.
- Set GAMEID to something deterministic like `umu-0` or a number-free `vortex` string. Using `GAMEID=0` is what the man page examples use. [DOC]
- Unknown PROTON_VERB silently falls back to `waitforexitandrun`. Valid: `waitforexitandrun, run, runinprefix, destroyprefix, getcompatpath, getnativepath`. [SRC umu_consts.PROTON_VERBS]

### 1.2 Running an arbitrary EXE
```
WINEPREFIX=/abs/pfx GAMEID=umu-0 PROTONPATH=/abs/proton umu-run /abs/path/setup.exe [args]
```
- Exe path is made absolute; if it exists on host, `STEAM_COMPAT_INSTALL_PATH` defaults to its parent dir. If the exe does NOT exist on host, umu logs `WARNING: Executable not found: cmd` and passes the name through as-is, so Wine resolves it inside the prefix (PATH / system32). [VL] This is how to run `cmd`, `msiexec`, `wineboot`, `regedit`, `control`, etc.
- Exit code of `umu-run` = exit code of Proton child (`proc.wait()`). [SRC run_command]
- Proton stdout/stderr go to the umu-run stdout/stderr (unless PROTON_LOG=1, see 6). Noise lines to filter: `Proton: Error: unable to use parent for game drive, path` (harmless when STEAM_COMPAT_INSTALL_PATH empty), `ntsync: up and running.`, `WARNING: radv is not a conformant...`. [VL]

### 1.3 TOML --config
Table `[umu]`; required keys `prefix`, `proton`, `exe` (basic strings); optional `game_id`, `store`, `launch_args` (string = space-separated, or array of strings). Requires Python 3.11+. [DOC man umu(5)] [VL man page]
```toml
[umu]
prefix = "~/Games/vortex/foo"
proton = "~/.local/share/Steam/compatibilitytools.d/GE-Proton11-12"
game_id = "0"
exe = "~/foo.exe"
launch_args = ["-opengl"]
store = "gog"
```
No keys for PROTON_VERB, env vars, or cwd. Recommendation: do NOT use --config; use env vars + argv (more control; Command::envs is trivial). [?] whether `exe` in config takes args in a way that preserves quoting; env+argv avoids the question.

### 1.4 Verbs
- Default `waitforexitandrun`: Proton runs `wineserver -w` FIRST (waits for any pre-existing wineserver in the prefix to finish), THEN starts the app (`steam.exe` launcher inside prefix), and returns when that app process returns. It does NOT run `wineserver -w` afterwards. [SRC Proton `proton` script lines ~2201-2204; confirmed read]
- `run`: starts immediately, no pre-wait. [SRC]
- `runinprefix`: runs `wine <args>` directly (no steam.exe wrapper; no `init_session` prefix update). [SRC]
- Practical effect [VL]: `umu-run cmd /c "start /b cmd /c ping -n 8 127.0.0.1 > out"` blocked ~10.7 s, i.e. umu-run did NOT return when the parent cmd exited but after the detached grandchild finished (+ wineserver linger ~2 s). Reason: steam.exe/Wine `start /b`-style children and pv-adverb/container keep running; the container exits when its init-ish process tree and wineserver end. See section 4: treat "umu-run returned" as a strong, but not contractual, completion signal, and confirm with `wineserver -w`.

### 1.5 PROTONPATH tokens and auto-download
- Tokens: `GE-Proton` (latest GE; also `GE-Latest`), `UMU-Proton` / `UMU-Latest`, `umu-scout`, `umu-soldier`, `umu-sniper`, `umu-steamrt4`, `umu-host`. Unset PROTONPATH => UMU-Proton (latest). [SRC ProtonVersion enum]
- Download: GET `https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases/latest` (UMU-Proton: `Open-Wine-Components/umu-proton`), picks the `*sum` asset + the first `*.tar.gz|tar.xz` starting with `GE-Proton`/`UMU-Proton` (skips `-aarch64`), checks sha512, extracts via temp dir, moves to `~/.local/share/Steam/compatibilitytools.d/GE-ProtonX-Y` for `GE-Proton` token. [SRC umu_proton.py]
- With the `*-Latest` tokens ("GE-Latest"/"UMU-Latest") the dir used is `~/.local/share/umu/compatibilitytools/GE-Latest` (umu delta-patched symlink-like dir, "Latest" candidates). PROTONPATH is rewritten internally to the real path. [SRC `latest_candidates`; behaviour not run]
- Only `~/.local/share/Steam/compatibilitytools.d` and `~/.local/share/umu/compatibilitytools` are searched by umu, NOT system paths. Offline: falls back to newest installed GE/UMU-Proton. Download cache: `~/.cache/umu`. Requires >1 GiB tmpfs space (TMPFS_MIN) else falls back to cache dir. [SRC]
- Recommendation for Vortex: do your own download (section 3) and pass an explicit absolute PROTONPATH. Avoids umu doing network work while a game launches, and gives you progress reporting. The GE tarball is ~567 MB (x86_64). [VL size via API]
- Gotcha: if `PROTONPATH` is a bare name and `~/.local/share/Steam/compatibilitytools.d/<name>` exists, umu expands it; otherwise treats as path (expanduser) and requires `toolmanifest.vdf` in it, else error `PROTONPATH ... is not valid, toolmanifest.vdf not found`. [SRC]

### 1.6 Steam Linux Runtime
- Stored in `~/.local/share/umu/<variant>` e.g. `~/.local/share/umu/steamrt4` (702 MB total `~/.local/share/umu` here), plus `umu-shim`, `umu.lock`. Variant chosen from Proton's `toolmanifest.vdf` `require_tool_appid` (Proton Experimental here -> steamrt4). Download on first run from Valve's repo (archive `SteamLinuxRuntime_<codename>.tar.xz`); first launch therefore needs network and several minutes. [VL dir listing; SRC umu_runtime.py] The Steam-installed `SteamLinuxRuntime_*` dirs are NOT reused by umu. [VL: umu dir created separately]
- `~/.local/share/umu/steamrt4/VERSIONS.txt` lists depot/pressure-vessel versions. [VL]
- Ready-check hint for UI: `~/.local/share/umu/<variant>/` exists with install marker; simplest is "run umu-run and stream its stderr"; umu logs `INFO: steamrt4 is up to date`. [VL]
- Respects `$XDG_DATA_HOME` / `$XDG_CACHE_HOME`. [SRC]

### 1.7 MSI
No special handling. `umu-run msiexec /i "/abs/path/x.msi" [/qb ...]` (name resolved in prefix). Host path is converted by Wine automatically? [?] NOT verified; Wine accepts `Z:\...` and unix paths for msiexec /i (Wine msiexec does convert unix paths). Safest: pass `/i` followed by a Windows path computed via `PROTON_VERB=getcompatpath umu-run /unix/path` (prints Windows path to stdout, [SRC verb]) or just build `Z:` + path with `/`->`\`. [VL `Z:\mnt\...` and `Z:\home\...` resolve]

### 1.8 winetricks through umu
- `PROTONPATH=... WINEPREFIX=... umu-run winetricks VERB...`. umu adds `-q`, runs `$PROTONPATH/protonfixes/winetricks` if present else `winetricks` on PATH (works with host winetricks only if run inside the container; umu says "requires UMU-Proton or GE-Proton"). cwd is changed to `$PROTONPATH/protonfixes`. Sets `WINETRICKS_SUPER_QUIET=1` unless UMU_LOG. [SRC] Proton Experimental has no `protonfixes` dir -> untested/UNCERTAIN [?]; use GE-Proton or UMU-Proton for dependency installs.
- If any given verb is already in `<pfx>/winetricks.log` (and not a "settings" verb), umu exits with code 1 without running (`winetricks verb 'x' is already installed`). [SRC umu_util.is_installed_verb] Note this is an `exit(1)` for ANY already-installed verb in the list, even if others are new; call per-verb.
- Verb arg regex: `^[a-zA-Z_0-9-]+(=[a-zA-Z0-9]*)?$` or a path to a .verb file. [SRC]

## 2. Prefix creation

- umu itself creates: `<WINEPREFIX>/` dir, `shadercache/`, `gstreamer-1.0/`, symlink `pfx -> .` (relative "."), `tracked_files` (touched), `pfx.lock`, and `drive_c/users/steamuser` + `drive_c/users/<unixuser> -> steamuser` symlink. [SRC setup_pfx]. Proton then populates the rest on first run.
- First run populates from Proton's default_pfx: [VL] after `umu-run wineboot -u` (27 s incl. everything, runtime already present) the prefix contains: `config_info, creation_sync_guard, dosdevices/, drive_c/, gstreamer-1.0/, pfx -> ., pfx.lock, shadercache/, system.reg(.old), user.reg, userdef.reg, tracked_files (88 KB), version, .update-timestamp`. Layout is therefore TOP-LEVEL (`WINEPREFIX` is the real wine prefix; `pfx` is a self-symlink for Steam-compat compat). Both `$WINEPREFIX/drive_c` and `$WINEPREFIX/pfx/drive_c` work. [VL]
- `version` file content: `11.0-100` (= Proton prefix version of the Proton that last touched it). Proton log line: `Proton: Upgrading prefix from None to 11.0-100`. Prefix is auto-upgraded when a newer-prefix-version Proton runs. Downgrade ("invalid version") warns. [VL; SRC]
- Wine sees `WINEPREFIX=<dir>`; Proton sets `STEAM_COMPAT_DATA_PATH=<dir>`. Both = the same dir in umu. [SRC set_env]
- Initialize without a game: `umu-run wineboot -u` ([VL] works, exit 0) or `umu-run ""` / `umu-run createprefix` (man umu(1) example 4; Proton "fails because exe not found" by design, prefix is still created) [DOC/SRC]. Prefer `wineboot -u`; exit code 0 is meaningful. `PROTON_VERB=run` is not needed.
- Prefix lock: `<pfx>/pfx.lock` flock during umu's setup_pfx and Proton's own creation. [SRC]
- `GAMEID` must be stable per prefix only if you want protonfixes (umu-<id>) applied; protonfixes may modify the prefix on every run (winetricks verbs for known games). GAMEID=`0` => none. [DOC]

## 3. Proton discovery

### 3.1 Locations to scan
| Path | Notes |
|---|---|
| `~/.local/share/Steam/compatibilitytools.d/*` | custom (GE-Proton). Exists, empty here. [VL] |
| `~/.steam/root/compatibilitytools.d/*` | `~/.steam/root` symlinks to Steam dir; same dir normally; dedupe via canonicalize. exists, empty [VL] |
| `~/.steam/steam/compatibilitytools.d` | alias [?] |
| `/usr/share/steam/compatibilitytools.d`, `/usr/local/share/steam/compatibilitytools.d` | system (distro packages e.g. proton-ge-custom-bin, AUR). `/usr/share/steam` exists here; contents: bin, bin32, bin64, compatibilitytools.d [VL] |
| `<steamlib>/steamapps/common/Proton*` | Valve builds. Here: `Proton - Experimental` (dir name has spaces; quote paths). Also `Proton BattlEye Runtime` is NOT a Proton (no `proton` script) so must filter. [VL] Steam libraries beyond default listed in `~/.local/share/Steam/steamapps/libraryfolders.vdf` [?] |
| `~/.var/app/com.valvesoftware.Steam/.local/share/Steam/{compatibilitytools.d,steamapps/common}` | Flatpak Steam. Not installed here [VL absent]. umu's compat dir does NOT include it, so always pass an absolute PROTONPATH. |
| `~/.local/share/umu/compatibilitytools/*` | umu-managed (UMU-Proton, `*-Latest`). |
| Vortex-owned dir, e.g. `~/.local/share/vortex/proton/` | recommended for GE installs Vortex makes (keeps user Steam untouched). umu accepts any absolute dir. |

### 3.2 Identify a Proton dir
- Valid iff executable file `proton` exists AND `toolmanifest.vdf` exists (umu hard-requires toolmanifest.vdf). [VL Experimental has both: `proton`, `toolmanifest.vdf`, `version`, `files/`, `dist.lock`, `proton_3.7_tracked_files`]
- `version` file: single line `<unix-ts> <name>`; Experimental: `1793301796 experimental-11.0-20261027b-x86_64`. [VL] GE-Proton's `version` is similar (`<ts> GE-Proton11-12`) [?]
- `compatibilitytool.vdf` is present in custom tools (GE) and gives `internal_name`, `display_name`; NOT present in Valve's Experimental here [VL `ls` fail]. Use dir name as fallback label. [DOC https://github.com/GloriousEggroll/proton-ge-custom README]
- `toolmanifest.vdf` `require_tool_appid` (Experimental: 4183110) determines the SLR variant. [VL]
- Wine binary: `<proton>/files/bin/wine`, `files/bin/wineserver`, `files/bin/msidb` (older GE: `dist/bin/`). [VL for `files/`]; handle both `files/` and `dist/`.

### 3.3 GE-Proton releases
- API: `GET https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases?per_page=N` (list) or `/releases/latest`. Headers: `Accept: application/vnd.github+json`, `X-GitHub-Api-Version: 2022-11-28`, a User-Agent. Unauthenticated limit 60 req/h/IP. [VL curl; SRC]
- Tag `GE-Proton11-12` (current format `GE-ProtonMAJOR-MINOR`). Assets in the latest release [VL]:
  - `GE-Proton11-12-x86_64.tar.gz` (566,859,228 B) <- NEW: arch suffix now present; older releases (<=GE-Proton10?) use `GE-ProtonX-Y.tar.gz` with no suffix [?]. Match: starts with `GE-Proton`, ends `.tar.gz`, does not contain `aarch64`.
  - `GE-Proton11-12-x86_64.sha512sum` (159 B) content is standard `sha512sum` format "<hash>  <filename>". Older: `GE-ProtonX-Y.sha512sum`.
  - aarch64 variants of both.
  - Asset objects also carry `digest: "sha256:..."` (GitHub-provided) usable for verification without downloading the sum file. [VL]
- URL pattern: `https://github.com/GloriousEggroll/proton-ge-custom/releases/download/<TAG>/<ASSET>`. Tarball top-level dir is `<TAG>` (`GE-Proton11-12/`), extract into the compat dir directly. [SRC umu: `proton = tarball minus .tar.gz` -> dir name] (umu's own derivation `".".join(name.split(".")[:-2])` yields `GE-Proton11-12-x86_64` for the new suffix; umu 1.4.4 apparently handles via its own logic; don't copy that, use the tag as dir name [?] verify top-level dir after first download.)
- Install: extract with `tar -xzf` (flate2+tar crates fine) to a temp dir on the same fs, verify, then `rename` into place (atomic).

## 4. Process lifecycle

### 4.1 Architecture [VL]
Process chain for `umu-run X`: `umu-run (python)` -> `umu` (SLR entry, pv-wrap) -> `srt-bwrap` -> `pv-adverb` -> `umu-shim` -> `proton` (python) -> `wineserver` + `wine-preloader steam.exe` -> app.
- `umu-run` sets `PR_SET_CHILD_SUBREAPER`, spawns the command with `start_new_session=True` (own session/pgid), and forwards SIGINT/SIGTERM to its whole descendant tree (`get_pstree_from_pid` scan of /proc). [SRC run_command, signal_handler]
- IMPORTANT [VL]: container shares the HOST PID namespace (wineserver and pv-adverb visible in host `pgrep`) and host `/tmp` (socket dir `/tmp/.wine-1000/server-33-293e` visible on host). Cgroup unchanged (inherits the Vortex process's cgroup/scope).
- wineserver lingers ~3 s after last client exits (Wine default), so "last process exit" != "wineserver gone".

### 4.2 Completion detection options (evaluated)
1. Wait on `umu-run` child exit. [VL] With a nested `start /b` grandchild, umu-run returned only after the grandchild ended (+~2 s). Good primary signal. Weakness: installers that spawn a detached process and exit (e.g. setup.exe relaunch via `start` into a service/bootstrapper that then spawns the real installer after a delay) could race; and some games' launcher spawns the game and exits (umu-run still waits because wineserver/children keep container alive).
2. `wineserver -w` with the SAME wineserver binary from `<proton>/files/bin/wineserver` and `WINEPREFIX=<pfx>` run on host. [VL] `wineserver -k` from host worked (rc 0) because the socket is in the shared /tmp and same uid; hence `-w` works too (same socket lookup). Best secondary barrier: after the main umu-run returns, run `WINEPREFIX=... <proton>/files/bin/wineserver -w` (returns immediately when no server is running). Caveats: (a) must use Proton's wineserver, a host/other-version wineserver refuses a mismatched protocol ("version mismatch"); (b) if wineserver is launched with a different `WINEPREFIX` string (symlinks, trailing `/pfx`) the socket path (derived from the dev/inode of the prefix dir) is the same only for the same dir inode; `pfx -> .` points to the same inode, so both spellings match [SRC-reasoned]; (c) `wineserver -w` run when no server exists returns 0 at once - it does NOT start a server (`-w` with none running: exits) [?] verify at implementation; (d) the host process needs the Proton lib dirs? Proton's wineserver is a static-ish binary and ran OK from host for -k [VL]; set `LD_LIBRARY_PATH` unnecessary.
3. `waitforexitandrun`: only pre-waits. Not a post-barrier. Use it for default (serialises against previous run in same prefix) but do not rely on it for nested installers.
4. Process group / PID namespace: umu-run puts its children in a new SESSION (setsid), so `killpg(umu_run_pid)` is NOT useful from Vortex's side; Vortex's own `Command::process_group(0)` for umu-run gives a pgid but children created via setsid escape it. The SLR does not unshare PID ns here ([VL]), so /proc scanning for descendants works (environment var `UMU_INVOCATION_ID=<hex>` is in the environ of pv-adverb; umu itself uses this). Vortex could set `VORTEX_RUN_ID=<uuid>` in the env and scan `/proc/*/environ` to find every process of a run (env is inherited by all wine children). That is the most robust "is anything still running from this launch" test. [SRC technique used by umu; local verification of environ inheritance = [?]]
5. Cgroups: `systemd-run --user --scope -p ... umu-run ...` puts the entire tree in one scope; completion = scope has no processes (`systemctl --user is-active`/cgroup.events `populated 0`), kill = `systemctl --user kill --kill-whom=all`. Works because children inherit the cgroup (verified they remain in the same host PID ns; cgroup inheritance is kernel behaviour). Not tested here [?]. Adds a dependency on systemd user session; optional upgrade.
6. Recommended combo for installers: (i) spawn umu-run with env marker; (ii) wait for umu-run exit; (iii) run `wineserver -w` (Proton's) with timeout; (iv) optionally scan /proc for marker env lingering processes; (v) declare done. Show "waiting for background processes..." in UI during (iii)/(iv) with a Cancel.

### 4.3 Clean kill
- Soft: `WINEPREFIX=<pfx> <proton>/files/bin/wineserver -k` [VL rc=0] kills all wine processes in the prefix (SIGINT-then-kill equivalent, terminates processes via wineserver). umu-run then exits. Then SIGTERM umu-run if still alive (its handler propagates to the descendant tree).
- Hard: `-k9`? does not exist; use `wineserver -k` then SIGKILL of marker-env processes.
- Caveats outside container: wineserver binary must match Proton; `WINEPREFIX` must be set (else it looks at `~/.wine`); `WINESERVER` env not needed; if the container gets an isolated /tmp in some configs (Flatpak Vortex or `--unshare-all` style SLR config) the socket would be invisible - Vortex as a Flatpak would need `--filesystem=/tmp` or umu `UMU_CONTAINER_NSENTER` approach. [?] For a native Arch build, fine.

## 5. Working directory

- umu: for normal exe, `Popen(command, cwd=Path.cwd())`: umu-run's own cwd is passed on (only winetricks gets a different cwd). [SRC run_command]
- [VL] cwd propagates through the container to Windows: running from `/home/user/Documents`, `cmd /c cd` printed `Z:\home\user\Documents`. So set `Command::current_dir(<dir>)` on umu-run to control the Windows CWD. No need for `start /d`.
- Host `/home`, `/mnt` (incl. `/mnt/storage` mounts), `/run/media` are visible as `Z:\home`, `Z:\mnt`, `Z:\run\media` [VL dir listings OK]. pressure-vessel shares: $HOME, /mnt, /media, /run/media, /srv, /opt (default exports) plus `STEAM_COMPAT_INSTALL_PATH` and `STEAM_COMPAT_LIBRARY_PATHS`; umu auto-adds the nearest non-root mount point above the exe path to LIBRARY_PATHS. [SRC enable_steam_game_drive; VL for /mnt and /home]. Unusual locations (e.g. /data, /var/games, custom mount at `/`-level other than the defaults) are exposed ONLY if they are mount points above STEAM_COMPAT_INSTALL_PATH or listed in `STEAM_COMPAT_LIBRARY_PATHS` / `PRESSURE_VESSEL_FILESYSTEMS_RW` (`:`-separated). Set `PRESSURE_VESSEL_FILESYSTEMS_RW=/path` for staging dirs outside default exports. [DOC https://gitlab.steamos.cloud/steamrt/steam-runtime-tools/-/blob/main/pressure-vessel/wrap.1.md]
- For setup.exe finding data1.bin / .cab neighbours: Windows resolves siblings relative to the exe's own directory (module path) for most InstallShield/Inno setups; CWD matters for a minority (poorly written launchers using relative paths). Do both: run the exe by absolute path AND set `current_dir` to the exe's parent. `STEAM_COMPAT_INSTALL_PATH` defaults to exe parent, so the dir is also mounted. [SRC; behaviour for data1.bin = reasoning, not tested [?]]
- Under `waitforexitandrun`/`run` Proton also creates the `G:` drive pointing at `STEAM_COMPAT_INSTALL_PATH` (`setup_game_dir_drive`) when set; the "unable to use parent for game drive" message appears when empty - harmless. [VL/SRC]
- Beware: `Z:` maps `/`; installers pick paths like `Z:\home\...`; not an issue for Vortex but default install dir should be `C:\...` in prefix or a user-chosen `Z:\...` host dir.

## 6. Graphics / perf env vars (read from Proton Experimental 11.0 `proton` script [SRC], names verified by grep)

Recognised `PROTON_*` (set to `1`): `PROTON_USE_WINED3D` (WineD3D instead of DXVK/vkd3d), `PROTON_USE_WINED3D11` (alias legacy), `PROTON_DXVK_D3D8`, `PROTON_NO_D3D11`, `PROTON_NO_D3D10`, `PROTON_NO_FSYNC`, `PROTON_FORCE_LARGE_ADDRESS_AWARE`, `PROTON_OLD_GL_STRING`, `PROTON_HIDE_NVIDIA_GPU`, `PROTON_HIDE_INTEL_GPU`, `PROTON_HIDE_VANGOGH_GPU`, `PROTON_HIDE_APU`, `PROTON_SET_GAME_DRIVE`, `PROTON_NO_XIM`, `PROTON_HEAP_DELAY_FREE`, `PROTON_HEAP_ZERO_MEMORY`, `PROTON_DISABLE_NVAPI`, `PROTON_FORCE_NVAPI`, `PROTON_USE_WOW64`, `PROTON_LOG`, `PROTON_LOG_DIR`, `PROTON_CPU_TOPOLOGY`, `PROTON_LIMIT_RESOLUTIONS`, `PROTON_USE_XALIA`, `PROTON_DISABLE_HIDRAW`, `PROTON_MEDIACONV_NO_VIDEO`, `PROTON_CRASH_REPORT_DIR`, `PROTON_LIMIT_ADDRESS_SPACE`, `PROTON_REMOTE_DEBUG_CMD`.
- `PROTON_NO_ESYNC` : NOT in Experimental 11.0 `proton` script (grep: only an internal `noesync` token, line 1394); esync was dropped in favour of ntsync/fsync. Setting it is harmless; Vortex "disable esync" option is only meaningful for GE/older Protons. [SRC; ? for GE-Proton11]
- `PROTON_ENABLE_NVAPI`: NOT present in Experimental 11.0; NVAPI is ENABLED BY DEFAULT (`DXVK_ENABLE_NVAPI=1` unless `PROTON_DISABLE_NVAPI`), `PROTON_FORCE_NVAPI` also sets `DXVK_NVAPI_ALLOW_OTHER_DRIVERS=1`. Old Protons/GE (<=9) use `PROTON_ENABLE_NVAPI=1`. Vortex: offer toggle as `PROTON_ENABLE_NVAPI=1` (legacy) + `PROTON_DISABLE_NVAPI=1` for off. [SRC] Spec should not assume ENABLE_NVAPI is sufficient.
- `PROTON_ENABLE_WAYLAND=1`, `PROTON_PREFER_SDL`: not found in this Experimental build [SRC grep empty]; GE-Proton has Wayland driver toggles [?].
- ntsync is auto-used when kernel supports it: log line `ntsync: up and running.` [VL]. Env to disable: `PROTON_NO_NTSYNC`? [?] not found in script; `PROTON_NO_FSYNC` is the known one.
- Non-Proton passthroughs honoured because Proton forwards the environment: `DXVK_HUD` (e.g. `fps,frametimes,gpuload`), `DXVK_FRAME_RATE`, `DXVK_ASYNC` (GE only), `DXVK_LOG_LEVEL`, `DXVK_STATE_CACHE_PATH`, `VKD3D_CONFIG` (e.g. `dxr,dxr11`), `VKD3D_FEATURE_LEVEL`, `VKD3D_DEBUG`, `VKD3D_SHADER_DEBUG`, `MANGOHUD=1`, `WINEDEBUG`, `WINEDLLOVERRIDES`, `DXVK_ENABLE_NVAPI`, `SDL_VIDEODRIVER`, `LD_PRELOAD` (careful: SLR strips/handles). [DOC https://github.com/doitsujin/dxvk README, https://github.com/HansKristian-Work/vkd3d-proton README]
- Logging: `PROTON_LOG=1` -> file `$PROTON_LOG_DIR/steam-<SteamGameId>.log`; umu sets `SteamGameId` to the GAMEID number (here `SteamGameId=0` for `GAMEID=umu-0`/`0`) so file = `steam-0.log` in `$PROTON_LOG_DIR` else `$HOME`. [SRC setup_logging: also `steam-proton.log` append mode] `PROTON_LOG=1` sets `WINEDEBUG=+timestamp,+pid,+tid,+seh,+debugstr,+loaddll,+mscoree`; `PROTON_LOG=+d3d,-heap` custom channels. Set `PROTON_LOG_DIR=<vortex logs dir>` and a unique GAMEID/SteamGameId? SteamGameId is derived: umu sets `SteamGameId` from GAMEID numeric part; two games with same numeric id overwrite logs (the file is removed at start). Prefer capturing umu-run stdout/stderr yourself (always available) and use PROTON_LOG as an opt-in. [SRC; ? exact SteamGameId derivation for non-numeric GAMEID]
- umu's own logs: `UMU_LOG=debug` -> stderr of umu-run (prints complete env and the full command). [VL]

## 7. Winetricks

- `winetricks list-all` output: sections `===== apps =====`, `===== benchmarks =====`, `===== dlls =====`, `===== fonts =====`, `===== games =====`, `===== settings =====`; each line `verb<padding>Description (Publisher, year) [downloadable]` (optional `[cached]`, `[downloadable]`, `[manual download]`). [VL head]. `winetricks dlls list`, `fonts list`, `settings list`, `apps list` give single sections in same line format (no header). [VL dlls list]
- Winetricks needs no prefix to list. Version `20260125`. Self-update is disabled on Arch package. [VL `--version`]
- Installed verbs: `<WINEPREFIX>/winetricks.log` has one verb per line (appended after success); umu parses exactly that file (strip lines; match, excluding settings verbs). [SRC] File may contain `verb=arg` forms. File absent = none installed. Settings verbs (e.g. `vd=off`, `win10`) are re-appliable, not tracked as "installed" by umu.
- Cache: `~/.cache/winetricks/<verb>/` downloads (also `$WINETRICKS_CACHE`). [?] standard
- Running host winetricks directly (`WINEPREFIX=.. WINE=<proton>/files/bin/wine winetricks ...`) is possible but bypasses the container and Proton's DLL setup; use `umu-run winetricks` (needs GE/UMU-Proton). Note Experimental lacks `protonfixes/` so `umu-run winetricks` would use PATH winetricks inside container [?, unverified].
- `-q` (unattended) is added by umu. Some verbs need `--force` or display a GUI dialog (Windows installer GUIs) even with -q; require a display. Verbs `dotnet*`, `vcrun*`, `d3dx9`, `xact`, `corefonts` are the usual. Each can reboot wineserver; umu-run returns after completion.

## 8. Other observed facts
- Cold run timing [VL]: prefix creation via `wineboot -u` 27 s with SLR already installed. Subsequent `cmd /c` runs ~3-4 s overhead.
- Gotcha [VL]: `umu-run ... | grep` loses nothing but exit codes; use Command with piped stdout/stderr and `wait`.
- DISPLAY/WAYLAND: Proton uses X11 (Xwayland) by default; works under current session. [VL implicit]
- Disk: `~/.local/share/umu` 702 MB; each GE-Proton ~1.3 GB extracted [?]; prefix ~ 250 MB fresh [?].
- Proton Experimental is usable as a fallback runner: the discovery code should accept it, but warn it is a moving target. [VL]

## Highest risk assumptions
1. "umu-run returned == everything done." [VL] held for a `start /b` grandchild, but detached relaunches / delayed bootstrappers can still outlive it; the `wineserver -w` barrier with Proton's own binary is [VL]-supported only for `-k`, not `-w` (not tested). Verify `-w` returns immediately when no server and blocks when one is up, with an installer that spawns a second-stage installer.
2. Visibility of the wineserver socket from the host depends on umu not isolating /tmp. True here (native, bwrap shares /tmp); false for a Flatpak-packaged Vortex or future SLR defaults.
3. `umu-run winetricks` with Proton Experimental (no `protonfixes/winetricks`) is untested; the spec's dependency feature needs GE-Proton/UMU-Proton installed (requires the ~567 MB download), and has first-run-network dependency on both GE and the SLR (~700 MB).
4. GE asset naming changed (`-x86_64` suffix, aarch64 siblings, `.sha512sum`). Hard-coding old names will break; match by prefix/suffix and prefer the API `digest` field. Top-level tarball dir name after extraction must be checked.
5. Env var toggles (`PROTON_NO_ESYNC`, `PROTON_ENABLE_NVAPI`, Wayland) are version-dependent; Experimental 11.0 ignores/changed them. UI should describe them as best-effort and per-Proton-build.
6. MSI path handling (`msiexec /i <unix path>`) not verified; use `Z:\` conversion.
7. Installers relying on CWD (data1.bin): CWD propagation itself is [VL]; whether specific installers need it is untested. Setting both absolute exe path and `current_dir` is the safe superset.
8. `--config` TOML offers no env/cwd control; assumption that Vortex can use it for "game profiles" is weak, prefer env+argv.
9. Prefix layout: top-level with `pfx -> .`. Code that assumes `<pfx>/pfx/drive_c` works only because of the symlink; use `<WINEPREFIX>/drive_c`. Prefix `version` can be upgraded by a newer Proton and not downgraded: switching a prefix to an older Proton is risky.
10. SteamGameId/log filename collisions for PROTON_LOG: derivation for non-numeric GAMEID unverified.
