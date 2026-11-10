#!/usr/bin/env bash
# M1 lifecycle spike (throwaway). Drives the real umu-run against throwaway prefixes.
# Usage: WORK=/some/tmp/dir spikes/lifecycle/run.sh <scenario>...
#   scenarios: init s1 nested startb delayed wmic svc s4 s5 s6 stop sigterm s7 s8 exe msi
#   exe/msi need fixtures/exe-src/build.sh first (mingw-w64-gcc, msitools); s8 honours PROTONPATH.
# Everything (prefixes, fixture copies) lives under $WORK (and $HOMEWORK for the "under $HOME" cwd
# case); nothing touches real prefixes. Results go to stdout; save with `| tee`.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
: "${WORK:?set WORK to a throwaway dir}"
: "${PROTONPATH:=$HOME/.local/share/Steam/steamapps/common/Proton - Experimental}"
: "${HOMEWORK:=$HOME/.cache/vortex-m1-spike/home}"   # fixture copies under $HOME (S4)
export PROTONPATH GAMEID=0
WS="$PROTONPATH/files/bin/wineserver"
PFX="$WORK/pfx"
mkdir -p "$WORK" "$HOMEWORK"

now() { date +%s.%3N; }
since() { awk -v a="$1" -v b="$(now)" 'BEGIN{printf "%.2f", b-a}'; }
marker() { od -An -tx1 -N16 /dev/urandom | tr -d ' \n'; }
to_win() { local p=$1; printf 'Z:%s' "${p//\//\\}"; }
log() { printf '[%s] %s\n' "$(date +%T.%3N)" "$*"; }
servers() { pgrep -a -x wineserver || echo "(no wineserver process)"; }
sockdir() { local d; d=$(stat -c '%d %i' "$PFX" 2>/dev/null) || { echo "(no prefix)"; return; }
  set -- $d; printf '/tmp/.wine-%s/server-%x-%x' "$(id -u)" "$1" "$2"; }

# launch CWD MARKER ARGV... -> sets UMU (pid), MON (pid); logs to $WORK/<name>.*
launch() {
  local cwd=$1 m=$2; shift 2
  T0=$(now)
  env -C "$cwd" WINEPREFIX="$PFX" VORTEX_RUN_ID="$m" umu-run "$@" >"$WORK/$NAME.out" 2>"$WORK/$NAME.err" &
  UMU=$!
  python3 -I "$HERE/procscan.py" monitor "$m" "$UMU" 240 >"$WORK/$NAME.mon" &
  MON=$!
  log "$NAME: umu-run pid=$UMU marker=$m cwd=$cwd argv=$*"
}

# barrier_run NAME FIXTURE DONEFILE [EXE-ARGS...]: copy fixture, run setup.bat (or, with EXE-ARGS,
# the host path setup.exe directly), measure umu-run exit, wineserver -w, done file.
barrier_run() {
  NAME=$1; local fx=$2 done=$3 dir="${BASE:-$WORK}/$1" m; shift 3; m=$(marker)
  rm -rf "$dir"; cp -r "$HERE/fixtures/$fx" "$dir"
  if [ $# -gt 0 ]; then launch "${RUNCWD:-$dir}" "$m" "$dir/setup.exe" "$@"
  else launch "$dir" "$m" cmd /c "$(to_win "$dir/setup.bat")"; fi
  wait "$UMU"; local rc=$?
  log "$NAME: umu-run exited rc=$rc at +$(since "$T0")s; done-file present: $([ -e "$dir/$done" ] && echo yes || echo no)"
  log "$NAME: marker processes right after umu-run exit:"; python3 -I "$HERE/procscan.py" scan "$m" | sed 's/^/    /'
  local tw; tw=$(now); WINEPREFIX="$PFX" "$WS" -w; local wrc=$?
  log "$NAME: wineserver -w rc=$wrc took $(since "$tw")s, returned at +$(since "$T0")s; done-file present: $([ -e "$dir/$done" ] && echo yes || echo no)"
  [ -e "$dir/$done" ] && log "$NAME: $done mtime at +$(awk -v a="$T0" -v b="$(stat -c %.3Y "$dir/$done")" 'BEGIN{printf "%.2f", b-a}')s"
  wait "$MON"
  log "$NAME: monitor (t = seconds since launch):"; sed 's/^/    /' "$WORK/$NAME.mon"
  log "$NAME: marker processes at end:"; python3 -I "$HERE/procscan.py" scan "$m" | sed 's/^/    /'
}

case_init() {
  NAME=init; log "init: umu-run wineboot -u into $PFX"
  local t; t=$(now)
  env WINEPREFIX="$PFX" VORTEX_RUN_ID=init umu-run wineboot -u >"$WORK/init.out" 2>"$WORK/init.err"
  log "init: rc=$? in $(since "$t")s; prefix version=$(cat "$PFX/version" 2>/dev/null)"
  WINEPREFIX="$PFX" "$WS" -w
}

case_s1() {
  NAME=s1
  log "s1(a): no server running. processes before:"; servers | sed 's/^/    /'
  log "s1(a): socket dir $(sockdir): $(ls -A "$(sockdir)" 2>&1 | tr '\n' ' ')"
  local t; t=$(now); WINEPREFIX="$PFX" "$WS" -w; log "s1(a): wineserver -w rc=$? took $(since "$t")s"
  log "s1(a): processes after:"; servers | sed 's/^/    /'
  log "s1(a): socket dir after: $(ls -A "$(sockdir)" 2>&1 | tr '\n' ' ')"
  log "s1(b): server running (cmd /c ping -n 12)"
  local m; m=$(marker)
  launch "$WORK" "$m" cmd /c ping -n 12 127.0.0.1
  until pgrep -x wineserver >/dev/null; do sleep 0.2; done
  log "s1(b): wineserver up at +$(since "$T0")s: $(servers)"
  t=$(now); WINEPREFIX="$PFX" "$WS" -w; local wrc=$?
  log "s1(b): wineserver -w rc=$wrc took $(since "$t")s, returned at +$(since "$T0")s; umu-run alive: $(kill -0 "$UMU" 2>/dev/null && echo yes || echo no)"
  log "s1(b): wineserver processes now:"; servers | sed 's/^/    /'
  wait "$UMU"; log "s1(b): umu-run rc=$? at +$(since "$T0")s"; wait "$MON"
  log "s1(c): -w with a non-existent prefix path"
  t=$(now); WINEPREFIX="$WORK/does-not-exist" "$WS" -w; log "s1(c): rc=$? took $(since "$t")s; dir created: $([ -e "$WORK/does-not-exist" ] && echo yes || echo no)"
}

case_s4() {
  local base m
  for base in "$HOMEWORK" "$WORK" "$PFX/drive_c"; do
    NAME=s4; local dir="$base/vtest-multi"; rm -rf "$dir"; cp -r "$HERE/fixtures/multi" "$dir"; m=$(marker)
    log "s4: cwd=$dir (exe by absolute Z: path)"
    launch "$dir" "$m" cmd /c "$(to_win "$dir/setup.bat")"; wait "$UMU"; log "s4: rc=$?"; wait "$MON"
    sed 's/^/    /' "$dir/result.txt"
  done
  dir="$HOMEWORK/vtest-multi"; rm -f "$dir/result.txt"; m=$(marker)
  log "s4: NEGATIVE: cwd=$WORK (not the installer dir), exe by absolute path"
  launch "$WORK" "$m" cmd /c "$(to_win "$dir/setup.bat")"; wait "$UMU"; log "s4: rc=$?"; wait "$MON"
  sed 's/^/    /' "$dir/result.txt"
  dir="$PFX/drive_c/vtest-multi"; rm -f "$dir/result.txt"; m=$(marker)
  log "s4: drive_c via C: path: cmd /c C:\\vtest-multi\\setup.bat, cwd=$dir"
  launch "$dir" "$m" cmd /c 'C:\vtest-multi\setup.bat'; wait "$UMU"; log "s4: rc=$?"; wait "$MON"
  sed 's/^/    /' "$dir/result.txt"
}

case_s5() {
  NAME=s5; local dir="$HOMEWORK/vtest-multi5" m; rm -rf "$dir"; cp -r "$HERE/fixtures/multi" "$dir"; m=$(marker)
  log "s5: .bat passed directly to umu-run as a host path (no cmd /c)"
  launch "$dir" "$m" "$dir/setup.bat"; wait "$UMU"; log "s5: rc=$?"; wait "$MON"
  sed 's/^/    /' "$dir/result.txt" 2>&1; tail -5 "$WORK/s5.err" | sed 's/^/    err: /'
}

case_s6() {
  NAME=s6; local m; m=$(marker)
  log "s6: parent shell runs umu-run (ping -n 40); SIGKILL the parent shell"
  T0=$(now)
  setsid bash -c 'echo $$ > "$1/s6.parent"; env WINEPREFIX="$2" VORTEX_RUN_ID="$3" umu-run cmd /c ping -n 40 127.0.0.1 >"$1/s6.out" 2>"$1/s6.err"; echo "unreachable"' _ "$WORK" "$PFX" "$m" &
  sleep 8
  local parent umu; parent=$(cat "$WORK/s6.parent"); umu=$(pgrep -P "$parent" -f umu-run)
  log "s6: parent=$parent umu-run=$umu; marker processes before kill: $(python3 -I "$HERE/procscan.py" scan "$m" | wc -l)"
  kill -KILL "$parent"; sleep 2
  log "s6: parent alive: $(kill -0 "$parent" 2>/dev/null && echo yes || echo no); umu-run alive: $(kill -0 "$umu" 2>/dev/null && echo yes || echo no) ppid=$(awk '{print $4}' "/proc/$umu/stat" 2>/dev/null)"
  log "s6: marker processes after parent SIGKILL:"; python3 -I "$HERE/procscan.py" scan "$m" | sed 's/^/    /'
  STOP_UMU=$umu STOP_M=$m stop_sequence
}

# Section 6.3: wineserver -k, wait 5 s; SIGTERM umu-run, wait 5 s; SIGKILL marker leftovers.
stop_sequence() {
  local umu=$STOP_UMU m=$STOP_M t i
  t=$(now); WINEPREFIX="$PFX" "$WS" -k; log "stop: wineserver -k rc=$? took $(since "$t")s"
  for i in $(seq 50); do kill -0 "$umu" 2>/dev/null || break; sleep 0.1; done
  log "stop: after -k: umu-run alive=$(kill -0 "$umu" 2>/dev/null && echo yes || echo no) at +$(since "$t")s; marker procs=$(python3 -I "$HERE/procscan.py" scan "$m" | wc -l)"
  python3 -I "$HERE/procscan.py" scan "$m" | sed 's/^/    /'
  if kill -0 "$umu" 2>/dev/null; then
    kill -TERM "$umu"; for i in $(seq 50); do kill -0 "$umu" 2>/dev/null || break; sleep 0.1; done
    log "stop: after SIGTERM: umu-run alive=$(kill -0 "$umu" 2>/dev/null && echo yes || echo no)"
  fi
  local left; left=$(python3 -I "$HERE/procscan.py" scan "$m" | awk '{print $1}')
  [ -n "$left" ] && { log "stop: SIGKILL leftovers: $left"; kill -KILL $left; sleep 1; }
  log "stop: final marker procs=$(python3 -I "$HERE/procscan.py" scan "$m" | wc -l); wineserver: $(servers)"
}

case_stop() {
  NAME=stop; local dir="$WORK/stop" m; rm -rf "$dir"; cp -r "$HERE/fixtures/nested" "$dir"; m=$(marker)
  sed -i 's/ping -n 15/ping -n 120/' "$dir/Redist/prereq.bat"
  log "stop: nested prereq (ping -n 120) plus main cmd ping -n 120; stop after 10 s"
  printf '@echo off\r\nstart "" cmd /c Redist\\prereq.bat\r\nping -n 120 127.0.0.1 > nul\r\n' > "$dir/setup.bat"
  launch "$dir" "$m" cmd /c "$(to_win "$dir/setup.bat")"; sleep 10
  log "stop: marker procs before stop: $(python3 -I "$HERE/procscan.py" scan "$m" | wc -l)"
  STOP_UMU=$UMU STOP_M=$m stop_sequence
  wait "$UMU"; log "stop: umu-run rc=$? at +$(since "$T0")s"; wait "$MON"; tail -3 "$WORK/stop.mon" | sed 's/^/    /'
}

# Step 2 of 6.3 in isolation: SIGTERM umu-run without a prior wineserver -k.
case_sigterm() {
  NAME=sigterm; local m; m=$(marker)
  launch "$WORK" "$m" cmd /c ping -n 60 127.0.0.1; sleep 8
  log "sigterm: marker procs before: $(python3 -I "$HERE/procscan.py" scan "$m" | wc -l)"
  local t i; t=$(now); kill -TERM "$UMU"
  for i in $(seq 100); do kill -0 "$UMU" 2>/dev/null || break; sleep 0.1; done
  log "sigterm: umu-run alive=$(kill -0 "$UMU" 2>/dev/null && echo yes || echo no) at +$(since "$t")s; marker procs=$(python3 -I "$HERE/procscan.py" scan "$m" | wc -l)"
  python3 -I "$HERE/procscan.py" scan "$m" | sed 's/^/    /'
  sleep 3; log "sigterm: +3 s later marker procs=$(python3 -I "$HERE/procscan.py" scan "$m" | wc -l); wineserver: $(servers)"
  STOP_UMU=$UMU STOP_M=$m stop_sequence; wait "$MON"
}

case_s7() {
  NAME=s7; local dir="$HOMEWORK/vtest-msi" m; mkdir -p "$dir"; printf 'not an msi\n' > "$dir/fake.msi"
  for arg in "$(to_win "$dir/fake.msi")" "$(to_win "$dir/missing.msi")" "$dir/fake.msi"; do
    m=$(marker); log "s7: msiexec /i $arg /qn (cwd=$dir)"
    launch "$dir" "$m" msiexec /i "$arg" /qn; wait "$UMU"; log "s7: rc=$?"; wait "$MON"
  done
}

# Real EXE (mingw): setup.exe reads data1.bin relative to cwd, spawns Redist\prereq.exe and exits.
case_exe() {
  local mode
  for mode in detached noenv breakaway; do
    BASE=$HOMEWORK barrier_run "exe-$mode" exe Redist/prereq.done "$mode"
    sed 's/^/    result: /' "$HOMEWORK/exe-$mode/result.txt"; sed 's/^/    prereq.done: /' "$HOMEWORK/exe-$mode/Redist/prereq.done" 2>/dev/null
  done
  log "exe: NEGATIVE cwd=$WORK (not the installer dir)"
  BASE=$HOMEWORK RUNCWD=$WORK barrier_run exe-othercwd exe Redist/prereq.done detached | grep -v "^ "
  sed 's/^/    result: /' "$HOMEWORK/exe-othercwd/result.txt"
}

# Real MSI (wixl): msiexec /i Z:\...\vtest.msi /qn, then uninstall, then the same with a unix path.
case_msi() {
  NAME=msi; local dir="$HOMEWORK/vtest-msi-real" m arg rc tgt="$PFX/drive_c/Program Files/VortexSpike/payload.txt"
  rm -rf "$dir"; mkdir -p "$dir"; cp "$HERE/fixtures/msi/vtest.msi" "$dir/"
  for arg in "$(to_win "$dir/vtest.msi")" "$dir/vtest.msi"; do
    m=$(marker); log "msi: msiexec /i $arg /qn (cwd=$dir, STEAM_COMPAT_INSTALL_PATH=$dir)"
    STEAM_COMPAT_INSTALL_PATH=$dir launch "$dir" "$m" msiexec /i "$arg" /qn; wait "$UMU"; rc=$?; wait "$MON"
    log "msi: rc=$rc; installed file: $(cat "$tgt" 2>&1)"
    m=$(marker); log "msi: msiexec /x $arg /qn"
    launch "$dir" "$m" msiexec /x "$arg" /qn; wait "$UMU"; rc=$?; wait "$MON"
    log "msi: uninstall rc=$rc; file still present: $([ -e "$tgt" ] && echo yes || echo no)"
  done
}

case_s8() {
  NAME=s8; local m; m=$(marker)
  log "s8: PROTONPATH=$PROTONPATH protonfixes/winetricks present: $([ -e "$PROTONPATH/protonfixes/winetricks" ] && echo yes || echo no)"
  launch "$WORK" "$m" winetricks win10; wait "$UMU"; local rc=$?; wait "$MON"
  log "s8: umu-run winetricks win10 rc=$rc; winetricks.log: $(cat "$PFX/winetricks.log" 2>&1 | tr '\n' ' ')"
  grep -iE 'winetricks|error|not found|executing' "$WORK/s8.err" "$WORK/s8.out" | tail -8 | sed 's/^/    /'
}

for c in "$@"; do
  case $c in
    init|s1|s4|s5|s6|s7|s8|stop|sigterm|exe|msi) "case_$c" ;;
    nested)  barrier_run nested nested Redist/prereq.done ;;
    startb)  barrier_run startb startb Redist/prereq.done ;;
    delayed) barrier_run delayed delayed stage2.done ;;
    wmic)    barrier_run wmic wmic Redist/prereq.done; sed 's/^/    wmic.out: /' "$WORK/wmic/wmic.out" ;;
    svc)     barrier_run svc svc Redist/prereq.done; sed 's/^/    svc.out: /' "$WORK/svc/svc.out" ;;
    *) echo "unknown scenario $c" >&2; exit 2 ;;
  esac
done
