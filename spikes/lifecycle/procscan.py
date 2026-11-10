#!/usr/bin/env python3
"""Process observer for the M1 spike (throwaway, not app code).

  procscan.py scan MARKER              list pids whose /proc/<pid>/environ has VORTEX_RUN_ID=MARKER
  procscan.py monitor MARKER ROOT [T]  every 0.5 s print appear/disappear events for every candidate
                                       process; exits when ROOT is gone and no candidate is left
                                       (or after T seconds, default 180)

Candidates = marker processes U descendants of ROOT (PPid tree) U "wine-looking" processes of this
uid (comm mentions wine, .exe, pv-adverb, srt-bwrap, umu-shim). Each event says whether the marker
is present, absent, or the environ was unreadable, so "lost marker" cases are visible.
"""
import os
import sys
import time

UID = os.getuid()
WINEY = ("wine", ".exe", "pv-adverb", "srt-bwrap", "umu-shim")  # matched against comm only


def procs():
    out = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            st = os.stat(f"/proc/{d}")
            if st.st_uid != UID:
                continue
            with open(f"/proc/{d}/stat") as f:
                s = f.read()
            comm = s[s.index("(") + 1 : s.rindex(")")]
            ppid = int(s[s.rindex(")") + 2 :].split()[1])
            with open(f"/proc/{d}/cmdline", "rb") as f:
                cmd = f.read().replace(b"\0", b" ").decode(errors="replace").strip()
            try:
                with open(f"/proc/{d}/environ", "rb") as f:
                    env = f.read().split(b"\0")
            except OSError as e:
                env = e.__class__.__name__
            out[int(d)] = (ppid, comm, cmd, env)
        except (OSError, ValueError):
            pass
    return out


def has_marker(env, marker):
    if isinstance(env, str):
        return "UNREADABLE:" + env
    return "yes" if f"VORTEX_RUN_ID={marker}".encode() in env else "NO"


def candidates(ps, marker, root):
    desc, frontier = set(), {root}
    while frontier:
        frontier = {p for p, v in ps.items() if v[0] in frontier and p not in desc}
        desc |= frontier
    res = set(desc)
    for p, (_, comm, cmd, env) in ps.items():
        if has_marker(env, marker) == "yes" or any(w in comm.lower() for w in WINEY):
            res.add(p)
    res.discard(os.getpid())
    return res, desc


def main():
    mode, marker = sys.argv[1], sys.argv[2]
    if mode == "scan":
        for p, (pp, comm, cmd, env) in sorted(procs().items()):
            if has_marker(env, marker) == "yes":
                print(p, pp, comm, cmd[:160])
        return
    root = int(sys.argv[3])
    limit = float(sys.argv[4]) if len(sys.argv) > 4 else 180
    t0, seen = time.monotonic(), {}
    while True:
        t = time.monotonic() - t0
        ps = procs()
        cand, desc = candidates(ps, marker, root)
        for p in sorted(cand - seen.keys()):
            pp, comm, cmd, env = ps[p]
            seen[p] = comm
            print(f"{t:7.2f} + pid={p} ppid={pp} marker={has_marker(env, marker)} "
                  f"desc_of_umu={'yes' if p in desc else 'no'} comm={comm} cmd={cmd[:140]}", flush=True)
        for p in sorted(seen.keys() - cand):
            print(f"{t:7.2f} - pid={p} comm={seen.pop(p)}", flush=True)
        if (root not in ps and not cand) or t > limit:
            print(f"{t:7.2f} monitor end (root_alive={root in ps}, left={sorted(cand)})", flush=True)
            return
        time.sleep(0.5)


if __name__ == "__main__":
    main()
