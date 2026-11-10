#!/bin/sh
# Builds fixtures/exe (setup.exe + Redist/prereq.exe + data1.bin) and fixtures/msi/vtest.msi.
set -e
cd "$(dirname "$0")"
out=../exe; mkdir -p "$out/Redist"
x86_64-w64-mingw32-gcc -O2 -o "$out/setup.exe" setup.c
x86_64-w64-mingw32-gcc -O2 -o "$out/Redist/prereq.exe" prereq.c
printf 'DATA1-CONTENT-OK\r\n' > "$out/data1.bin"
mkdir -p ../msi && cp vtest.wxs payload.txt ../msi/ && (cd ../msi && wixl -a x64 -o vtest.msi vtest.wxs && rm vtest.wxs)
