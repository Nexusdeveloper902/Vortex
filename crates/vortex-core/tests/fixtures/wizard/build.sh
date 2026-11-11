#!/bin/sh
# Builds the Lantern Hollow download folder (M16 fixture) into OUTDIR, which must not exist:
#   setup_lantern_hollow.exe, setup_lantern_hollow-1.bin, setup_lantern_hollow-2.bin,
#   redist/prereq_setup.exe, manual/, readme.txt, license.txt
# Needs x86_64-w64-mingw32-gcc and a host cc. Prints where the data split falls.
set -eu
[ $# -eq 1 ] || { echo "usage: build.sh OUTDIR" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
out=$1
[ ! -e "$out" ] || { echo "build.sh: $out already exists" >&2; exit 2; }
work="$out.build"
mkdir -p "$out/redist" "$work"
trap 'rm -rf "$work"' EXIT

w() { x86_64-w64-mingw32-gcc -O2 -static -mwindows -Wall -Werror -Wno-format-truncation -Wno-unused-function -I"$here" "$@" \
        -lcomctl32 -lwinmm -lole32 -luuid -lshell32 -ladvapi32 -lgdi32 -luser32; }
w -o "$out/setup_lantern_hollow.exe" "$here/setup.c"
w -o "$out/redist/prereq_setup.exe" "$here/prereq.c"
w -o "$work/LanternHollow.exe" "$here/game.c"
w -o "$work/unins000.exe" "$here/unins.c"
cc -O2 -Wall -Werror -o "$work/lhpack" "$here/lhpack.c"
"$work/lhpack" "$out/setup_lantern_hollow" "$work/LanternHollow.exe" "$work/unins000.exe"
cp -R "$here/source/." "$out/"
