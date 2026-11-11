#!/bin/sh
# Usage: packaging/make-tarball.sh VERSION  (repo root, after `cargo build --release -p vortex-gtk`)
set -eu
v=$1
d=vortex-launcher-$v-x86_64-linux
rm -rf "$d"
install -Dm755 target/release/vortex-launcher "$d/bin/vortex-launcher"
install -Dm644 data/io.github.nexusdeveloper902.Vortex.desktop -t "$d/share/applications"
install -Dm644 data/io.github.nexusdeveloper902.Vortex.metainfo.xml -t "$d/share/metainfo"
cp -r data/icons "$d/share/icons"
install -m644 LICENSE README.md "$d/"
cat > "$d/INSTALL.txt" <<'TXT'
Vortex (vortex-launcher) binary tarball (x86_64, built on Arch Linux: needs a recent glibc, GTK 4.14+, libadwaita 1.6+, SQLite).
Install for the current user:   cp -r bin share ~/.local/
Install system-wide (as root):  cp -r bin share /usr/local/
umu-launcher and winetricks must be installed separately. Run Vortex as a normal user, never as root.
TXT
tar czf "$d.tar.gz" "$d"
rm -rf "$d"
