# Vortex

Vortex is a native Linux desktop launcher, written in Rust with GTK 4 and libadwaita. The goal is to
install and run Windows games through Proton, using [UMU Launcher](https://github.com/Open-Wine-Components/umu-launcher)
(`umu-run`), with one isolated Wine prefix per game.

> **Status: early development.** Vortex does not run or install games yet. The current build only
> opens an empty window. Nothing described above is implemented yet.

## Build from source

Requirements:

- Rust 1.92 or newer (edition 2024), with `cargo`
- GTK 4, libadwaita, and SQLite development files

On Arch Linux:

```sh
sudo pacman -S --needed rust gtk4 libadwaita sqlite
```

Build and run the window:

```sh
git clone https://github.com/Nexusdeveloper902/Vortex.git
cd Vortex
cargo build --workspace
cargo run --bin vortex
```

Run the checks used by CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Vortex refuses to run as root. Start it as your normal user.

## Arch Linux package (skeleton)

`packaging/arch/PKGBUILD` is a skeleton for an Arch package. It is not usable yet: there is no
`v0.1.0` release tag and the checksum is still `SKIP`. Once a tag exists, build it from a clean
checkout:

```sh
cd packaging/arch
makepkg -s
```

Runtime dependencies are `gtk4`, `libadwaita`, `sqlite` and `umu-launcher`. `winetricks` is optional.

## Desktop integration

`data/` contains the desktop entry, the AppStream metainfo file, and the application icons
(scalable and symbolic). They use the application ID `io.github.nexusdeveloper902.Vortex`.

Check them with:

```sh
desktop-file-validate data/io.github.nexusdeveloper902.Vortex.desktop
appstreamcli validate data/io.github.nexusdeveloper902.Vortex.metainfo.xml
```

## License

Not decided yet. There is no `LICENSE` file in this repository.
