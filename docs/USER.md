# Vortex user guide

Vortex installs and runs Windows games on Linux through Proton, using UMU Launcher (`umu-run`).
Each game gets its own Wine prefix. This guide describes the features that exist in the current MVP.
For what has been verified on real hardware, see `docs/E2E-REPORT.md`. For internals, see
`docs/ARCHITECTURE.md`.

Vortex is not affiliated with Valve, Lutris or Bottles.

## Before you start

- Start Vortex as your normal user. It refuses to run as root, and it never uses `sudo` or `pkexec`.
- Install the packages: `gtk4`, `libadwaita`, `sqlite`, `umu-launcher` (in the `multilib` repository on
  Arch, so enable `multilib`) and `winetricks`.
- For dependency installs (winetricks), use a GE-Proton or UMU-Proton runtime. Proton Experimental
  does not support them (see [Dependencies](#dependencies)).

## Library

The sidebar has All games, Recently played, Needs attention (games with a failed install), and
Runtimes. The `+` menu starts **Install a game** or **Add an existing game**. Each game has a cover
tile, a status (Installing, Installed, Interrupted, Needs attention), and a page with Play, Configure,
Run EXE/MSI, Dependencies, Logs, Back up, Restore and an overflow menu.

Covers are downloaded automatically from Steam's public store: Vortex sends the game's name to
Steam's store search and, when a title matches, saves its cover image. Games without a match keep the
coloured placeholder. Turn this off in **Preferences** (Network, **Download artwork automatically**);
offline mode turns it off too. The game page's overflow menu has **Refresh artwork**, **Choose
image...** (your own JPEG or PNG) and **Remove artwork**.

## Install a game

1. Choose **Install a game**, then pick the installer. You can pick a file (`.exe` or `.msi`) or a
   folder that contains the installer and its data files.
2. If the folder holds several installers, the wizard asks you to choose the one that starts the
   installation. Vortex never runs all of them.
3. On **Name and location**, enter the game name. The install folder is optional and must already
   exist. If you leave it empty, the installer uses its default location inside the game's prefix.
4. Vortex creates the Wine prefix and runs the installer. The installer's folder is its working
   directory, and the installer reads its files in place. Vortex never modifies your source files.
   Installers always run with sound off (there is no option to turn it on). Only the installer and
   the programs it starts are muted; the game keeps its sound.
5. Vortex then shows **Waiting for background processes** until the installer and any prerequisite it
   started have exited. Nested prerequisites (for example a `setup.exe` that starts a redistributable
   and exits) are waited for this way.
6. On **Choose the game's executable**, pick the program that starts the game. Vortex lists `.exe`
   files created during the install, best name match first. Pick one, or use Browse.
7. The game is marked **Installed** only after you confirm an executable. An installer exit code of 0
   on its own never marks a game as installed.

If the installer reports a failure, the wizard shows a diagnosis and offers **Retry**. If processes
are still running when you want to move on, use **Mark as finished** (see
[Running processes](#running-processes-quitting-and-interrupted-installs)). It stops waiting and kills
nothing, then takes you to the executable step.

Installer arguments are not supported yet. An MSI runs through `msiexec` and can show its own
interface.

## Add an existing game

Use **Add an existing game** for a game that is already installed. Enter a name and choose its
executable. The Wine prefix field is optional:

- Empty: Vortex creates a new prefix for the game.
- A folder: Vortex uses that prefix as it is. It never deletes it, and it is not removed when you
  remove the game.

Vortex does not run anything when you add a game.

## Play and Stop

**Play** starts the game's executable with its saved arguments and environment variables, in its
configured runtime. The game's working directory is the executable's folder unless you set another one
on the Configure page. The launch is recorded in the game's logs.

**Stop** stops the game. Vortex first asks the Wine server to shut down, then sends SIGTERM, waits, and
then sends SIGKILL to any process that still belongs to that run. Only one program runs in a given
prefix at a time. While a program runs, Vortex refuses to start another one in the same prefix
(the environment is busy).

## Configure

The Configure page has these groups:

- **General**: name, executable, and working directory (empty means the executable's folder).
- **Runtime**: the Proton runtime. Changing it affects every game that shares the prefix. Four
  switches set Proton variables:
  - **DXVK**: off sets `PROTON_USE_WINED3D=1` (Wine's own OpenGL translation).
  - **VKD3D-Proton**: off sets `PROTON_NO_D3D12=1`.
  - **Esync**: off sets `PROTON_NO_ESYNC=1`.
  - **Fsync**: off sets `PROTON_NO_FSYNC=1`.
- **Launch**: arguments, one per row. Spaces inside a row stay part of that argument.
- **Environment variables**: one `KEY=value` per row. These are added to the environment of the game.
- **Environment**: the prefix folder and the list of installed dependencies.
- **Add to applications menu**: creates a desktop entry that starts this game with `vortex --launch <id>`, using the
  path of the running Vortex binary.
  The entry is written to your applications folder (see [Where Vortex keeps its data](#where-vortex-keeps-its-data)).
- **Danger zone**: Remove completely (see [Uninstall and remove](#uninstall-and-remove)).

Press **Save** to keep your changes.

## Run an EXE or MSI in a game's environment

The game page has **Run EXE/MSI**. Choose an `.exe` or `.msi`. It runs in the same prefix and with the
same runtime as the game, which is useful for setup tools, mod loaders and configuration utilities. It
takes no arguments. An MSI runs through `msiexec /i` and can show its own interface. It appears in the
game's logs as **Run in environment**.

## Dependencies

**Dependencies** on the game page lists winetricks verbs for the game's prefix. You can search the list
and install a verb. Each install is a separate run, and it appears in the logs as **Dependency**. Verbs
that are already installed are recorded and skipped.

- Vortex runs the winetricks that ships with the runtime, through `umu-run`. It never runs the host's
  winetricks, and it never runs downloaded scripts. Only verb names from the runtime's list are accepted.
- Winetricks needs a runtime that includes `protonfixes/winetricks`: GE-Proton or UMU-Proton.
- With Proton Experimental, the dependency list shows **Not available**, and says that GE-Proton or
  UMU-Proton is required.
- Only the verb `win10` has been checked against real winetricks. Larger verbs are untested.

## Runtimes

The **Runtimes** page lists the Proton runtimes Vortex found. Choose **Use as default** to set the
runtime for new games.

Vortex looks for runtimes in its own runtimes folder, in Steam's `compatibilitytools.d` folders, in the
`steamapps/common` folders of your Steam libraries, and in `~/.local/share/umu/compatibilitytools`.

**Install latest GE-Proton** downloads the latest GE-Proton release from GitHub, checks its SHA-512
checksum, and unpacks it into Vortex's runtimes folder. Nothing is installed if the checksum does not
match. It is disabled in offline mode. Turn offline mode off in **Preferences** (Network) to download.

## Backup and restore

- **Back up** archives the game's Wine prefix into the backups folder. Games that share a prefix share
  its backups. Backing up is refused while a program runs in that prefix. Backup archives never follow
  symbolic links.
- **Restore** lists the prefix's backups. Restoring replaces the current prefix after you confirm. The
  current prefix is moved aside and removed only after the restore succeeds. Saves and settings inside
  the prefix are replaced too.

A backup contains the prefix only. Install folders that Vortex created under its `games` folder, and
installer sources, are not included.

## Uninstall and remove

Vortex has two different removal actions. The overflow menu on the game page holds both.

- **Run the game's uninstaller** runs an `unins*.exe` that Vortex found in the install folder (or in
  the prefix's `Program Files`, `Program Files (x86)`, `GOG Games` and `Games` folders). It is available
  for installed games. The game's own uninstaller decides what it removes. Vortex does not track it.
  Like installers, the uninstaller runs with sound off.
- **Remove completely** removes the game from the library and deletes Vortex's files for it. Vortex
  first shows the exact plan:
  - Install folders that Vortex created, under its own `games` folder, with their sizes.
  - The Wine prefix, but only if no other game uses it and Vortex created it.
  - Folders you chose yourself (outside Vortex's `games` folder) are never deleted. They are listed
    under "Vortex will NOT delete these folders", so you can remove them yourself.
  - Installer files and folders you picked are never touched. Adopted prefixes are never touched.

  Nothing is deleted until you confirm the plan. Removal is refused while a program runs in that
  prefix.

## Logs

The game page's **Logs** lists the runs for the game: installer, game launches, run-in-environment,
dependency installs and prefix setup. Each run shows its state (Running, Waiting for processes,
Finished, Stopped, Interrupted) and its exit code. Open a run to read its log. **Export log** in the log
viewer copies the file wherever you choose.

Every line from a program goes through redaction before it reaches a log file or the screen. Lines
that Vortex itself writes start with `vortex: `. Vortex does not log your full environment.

## Running processes, quitting and interrupted installs

- **Waiting for background processes**: an installer or program has exited, but processes it started
  are still running. Vortex keeps watching them. Use **Stop** to stop them, or **Mark as finished** to
  stop waiting. Mark as finished never kills anything.
- **Quit dialog**: if programs are still running when you close Vortex, you get three choices. Cancel
  keeps Vortex open. Keep running in background leaves them running. Stop all and quit stops them first.
- **Detached-run banner**: if processes from an earlier session are still alive when Vortex starts,
  a banner offers **Stop or wait**. Stop them, or keep waiting. Waiting checks until they end and kills
  nothing.
- **Interrupted installs**: if Vortex or the whole system stops during an install, the install shows as
  **Interrupted**, never **Installed**. Use **Continue installation** on the game page to reopen it.
  **Retry** runs the install again.

## Where Vortex keeps its data

All paths follow the XDG base directories. With default settings:

| What | Where |
|---|---|
| Database, prefixes, games, runtimes, backups, logs | `~/.local/share/vortex/` |
| Database | `~/.local/share/vortex/vortex.db` |
| Wine prefixes | `~/.local/share/vortex/prefixes/<environment id>/` |
| Install folders created by Vortex | `~/.local/share/vortex/games/` |
| GE-Proton installed by Vortex | `~/.local/share/vortex/runtimes/` |
| Backups | `~/.local/share/vortex/backups/` |
| Logs | `~/.local/share/vortex/logs/<game or environment id>/<run id>-<kind>.log` |
| Cover images | `~/.local/share/vortex/artwork/<game id>.img` |
| Applications menu entries | `~/.local/share/applications/io.github.nexusdeveloper902.Vortex.game-<id>.desktop` |

Vortex has no configuration file. Its settings are stored in the database. Your installer files and
folders are never copied into Vortex's data folder.

## Troubleshooting

When a run fails, Vortex shows a finding under the failure. A finding has a kind and a confidence.
**Confirmed** means the evidence is unambiguous. **Likely** means a heuristic matched the log or the
exit status.

| Finding (as shown) | Meaning | Confidence |
|---|---|---|
| The installer reported a failure. | The installer exited with an error that is not a reboot code (3010 or 1641), and no known pattern matched. | Likely |
| Proton could not initialise the environment. | The runtime is not a valid Proton, the log reports a missing `toolmanifest.vdf`, or the container tool (pressure-vessel) or a download failed. | Confirmed for a missing `toolmanifest.vdf` or an invalid runtime; Likely otherwise |
| A required component is missing. | The log reports a DLL that Wine could not load. Use **Dependencies** on the game page. | Confirmed |
| The executable was not found. | The executable no longer exists, or a failed run could not find it outside Windows' own folders. | Confirmed (missing file); Likely (log) |
| The program crashed. | The log reports an unhandled exception or a page fault, or the process was killed by a signal. | Confirmed |
| The installer's source files are missing. | The installer's source was moved or deleted. Keep the source folder in place. | Confirmed (preflight); Likely (log) |
| The program was started from the wrong folder. | The log reports a relative file that was not found, and the working directory is not the executable's folder. | Likely |
| A graphics (Vulkan) prerequisite seems to be missing. | The log reports a Vulkan driver or instance error. | Likely |
| The run failed for an unknown reason. See the log. | The run failed and no known pattern matched. | Likely |

Things Vortex cannot diagnose:

- Problems inside the game itself: crashes without a logged exception, wrong behaviour, graphics
  glitches, performance, and save-file problems.
- Anything the logs do not show. Findings come only from the exit status, the preflight checks and the
  logged text, so a clean exit does not prove the game works.
- Anti-cheat and DRM failures. Vortex does not circumvent them.

### Missing tools and first run

- **"umu-run is required..." banner, or "umu-run was not found".** Vortex runs every Windows program
  through UMU Launcher. On Arch install `umu-launcher`; on Debian, Ubuntu and Fedora download a
  package from https://github.com/Open-Wine-Components/umu-launcher/releases. Then press **Check again**.
- **Dependencies refuses to run.** Vortex uses the winetricks bundled with the runtime, not the host's.
  Use a runtime that includes it, such as GE-Proton (Proton Experimental does not).
- **"No Proton runtime found".** Open **Runtimes** and download GE-Proton, or install Proton through
  Steam.
- **The first install or launch in a new prefix is slow.** The first time, umu downloads the Steam Linux
  Runtime (about 700 MB). This happens once, needs a network connection, and is shared by later prefixes.
  Wait for it to finish; a run that looks idle at this stage is not stuck.

When no finding fits, open the run in **Logs**, read the last lines, and use **Export log** if you want
to report the problem.

## Known limitations

- Installer arguments are not supported. An MSI runs interactively.
- Installer candidates are looked for only in the install folder and the standard Program Files, GOG
  and Games folders inside the prefix. Use Browse for anything else.
- The **Default install folder** and **Default prefix folder** preferences are saved but not used yet.
  Offline mode only blocks runtime and artwork downloads.
- Winetricks has been checked for `win10` only.
- Vortex is an early MVP. Metadata other than cover art, cloud saves, community recipes and gamepad configuration are
  not implemented.
- Verification status for real games and real installers is in `docs/E2E-REPORT.md`.
