#!/bin/sh
# Package smoke test, run INSIDE a distro container as root with the package dir mounted.
# Usage: packaging/smoke.sh deb|rpm|tar|tar-debian DIR   (DIR holds the built asset)
# Installs the package, checks linkage, files, metadata, and that the binary fails cleanly
# without a display (as a non-root user, since Vortex refuses root).
set -eu
kind=$1 dir=$2
id=io.github.nexusdeveloper902.Vortex
case $kind in
deb)
	apt-get update -qq
	apt-get install -y -qq --no-install-recommends desktop-file-utils appstream >/dev/null
	apt-get install -y -qq --no-install-recommends "$dir"/*.deb >/dev/null
	root=/usr ;;
rpm)
	dnf install -y -q util-linux desktop-file-utils appstream "$dir"/*.rpm >/dev/null
	root=/usr ;;
tar|tar-debian)
	if [ "$kind" = tar ]; then
		pacman -Syu --noconfirm --needed gtk4 libadwaita sqlite desktop-file-utils appstream >/dev/null
	else
		apt-get update -qq
		apt-get install -y -qq --no-install-recommends libgtk-4-1 libadwaita-1-0 libsqlite3-0 desktop-file-utils appstream tar gzip >/dev/null
	fi
	mkdir -p /opt/t && tar xzf "$dir"/*.tar.gz -C /opt/t --strip-components=1
	root=/opt/t ;;
esac
bin=$root/bin/vortex-launcher
missing=$(ldd "$bin" | grep 'not found' || true)
[ -z "$missing" ] || { echo "FAIL missing libs: $missing"; exit 1; }
for f in share/applications/$id.desktop share/metainfo/$id.metainfo.xml; do
	[ -f "$root/$f" ] || { echo "FAIL missing $f"; exit 1; }
done
ls "$root"/share/icons/hicolor/*/apps/"$id"* >/dev/null || { echo "FAIL no icons"; exit 1; }
desktop-file-validate "$root/share/applications/$id.desktop" || echo "WARN desktop-file-validate"
appstreamcli validate --no-net "$root/share/metainfo/$id.metainfo.xml" || echo "WARN appstreamcli validate"
useradd -m tester 2>/dev/null || adduser -D tester
out=$(setpriv --reuid=tester --regid=tester --init-groups env -u DISPLAY -u WAYLAND_DISPLAY HOME=/home/tester "$bin" 2>&1 || true)
echo "$out"
if echo "$out" | grep -Eqi 'symbol lookup|error while loading|GLIBC_|cannot open shared|not found'; then
	echo "FAIL binary has a loader/symbol error"; exit 1
fi
echo "$out" | grep -Eqi 'display|cannot open|Gtk' || { echo "FAIL no display-related error"; exit 1; }
echo "PASS $kind"
