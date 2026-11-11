# Vortex

Vortex is a native Linux desktop launcher, written in Rust with GTK 4 and libadwaita. It installs and
runs Windows games through Proton, using [UMU Launcher](https://github.com/Open-Wine-Components/umu-launcher)
(`umu-run`). Each game gets its own isolated Wine prefix.

Vortex is not affiliated with Valve, Lutris or Bottles.

> **Status: beta.** The install-to-launch workflow is implemented: runtime discovery, installers
> (files and folders, nested prerequisites), executable registration, the library, per-game
> configuration, winetricks, backup and restore, and logs. Verification on a real machine is tracked in
> [`docs/E2E-REPORT.md`](docs/E2E-REPORT.md), which marks each check as PASS, FAIL or NOT RUN.

## Documentation

- [`docs/USER.md`](docs/USER.md): user guide (installing, configuring, runtimes, backups, removal,
  logs, troubleshooting, known limitations).
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): modules, path kinds, the umu-run contract, the process
  lifecycle, the data schema and the security rules.
- [`spec.md`](spec.md): the product specification.

## Install

Download from the [latest release](https://github.com/Nexusdeveloper902/Vortex/releases/latest)
(`sha256sums.txt` and GitHub build provenance attestations are attached):

- `vortex-launcher-X.Y.Z-x86_64-linux.tar.gz`: generic binary tarball (see `INSTALL.txt` inside).
  Built on Arch and smoke-tested on Debian 13 (glibc 2.41, GTK 4.14, libadwaita 1.6), its practical minimum.
- `.deb`: Debian 13 (trixie) or newer, Ubuntu 25.04 or newer. Older releases ship GTK or libadwaita
  too old (GTK 4.14 and libadwaita 1.6 are required).
- `.rpm`: Fedora.
- Arch Linux: build the `vortex-launcher-git` package from [`packaging/arch/PKGBUILD`](packaging/arch/PKGBUILD).

`umu-launcher` is not pulled in by the `.deb`/`.rpm`; install it separately. Winetricks comes from the
runtime via `umu-run`; no host `winetricks` is needed.

## Requirements

Arch Linux packages:

- Runtime: `gtk4`, `libadwaita`, `sqlite` and `umu-launcher`.
  `umu-launcher` is in the `multilib` repository, so enable `multilib` in `/etc/pacman.conf`.
- Build: Rust 1.92 or newer (edition 2024) with `cargo`, and the GTK, libadwaita and SQLite development
  files (the packages above).
- Optional, for the real (`--ignored`) integration tests only: `mingw-w64-gcc` (builds the `.exe`
  fixtures) and `msitools` (builds the MSI fixture). Without them, those tests report NOT RUN.
- A Proton runtime. Winetricks needs GE-Proton or UMU-Proton; Proton Experimental does not support it.
  See the [user guide](docs/USER.md#runtimes).

```sh
sudo pacman -S --needed rust gtk4 libadwaita sqlite umu-launcher
# optional, for the real integration tests
sudo pacman -S --needed mingw-w64-gcc msitools
```

Vortex refuses to run as root, and it never calls `sudo` or `pkexec`. Run it as your normal user.

## Build and run

```sh
git clone https://github.com/Nexusdeveloper902/Vortex.git
cd Vortex
cargo build --workspace
cargo run --bin vortex-launcher
```

## Test

```sh
cargo test -p vortex-core                      # headless, fake umu-run, no GTK needed
cargo test --workspace
# real umu-run integration tests; the Proton path must be absolute
VORTEX_IT_PROTON="$HOME/.local/share/Steam/steamapps/common/Proton - Experimental" \
  cargo test -p vortex-core -- --ignored
```

Run the tests as a normal user. The process tests check that Vortex refuses to spawn as root, so they
only exercise the full paths when run unprivileged.

The checks used by CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p vortex-core -e normal | grep -E 'gtk|glib|gio|adwaita'   # must print nothing
```

`vortex-core` never depends on GTK, GLib, GIO or libadwaita. The last command checks that.

## Layout

- `crates/vortex-core`: the library. All logic: paths, database, runtimes, runner, process supervision,
  source analysis, installs, launch, winetricks, backup, deletion, logs, diagnostics and desktop
  entries. It has no GUI dependencies.
- `crates/vortex-gtk`: the `vortex` binary. It only renders state and forwards user actions to
  `vortex-core`.
- `crates/vortex-core/tests/`: integration tests, `fake-umu-run`, and fixtures.
- `spikes/`: throwaway experiments whose results are in `docs/research/`. They are never linked into the app.
- `packaging/arch/PKGBUILD`: the Arch package `vortex-launcher-git`, built from this git repository.
- `data/`: the desktop entry, AppStream metainfo and icons (app id `io.github.nexusdeveloper902.Vortex`).

## Packaging

On Arch, from a checkout:

```sh
cd packaging/arch
makepkg -s
```

The package installs `/usr/bin/vortex-launcher`, the desktop entry, the metainfo file, the icons and the license.
Check the desktop entry and the metainfo with:

```sh
desktop-file-validate data/io.github.nexusdeveloper902.Vortex.desktop
appstreamcli validate data/io.github.nexusdeveloper902.Vortex.metainfo.xml
```

## License

Vortex is free software under the [GNU General Public License, version 3 or later](LICENSE)
(GPL-3.0-or-later).
