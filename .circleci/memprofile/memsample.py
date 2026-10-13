#!/usr/bin/env python3
# Usage: memsample.py <out.tsv> [interval_s]
# Every interval, writes the container's cgroup memory plus RSS of every compiler/linker process.
# Rows: ts  kind  pid  ppid  rss_anon_kb  rss_file_kb  label
#   kind=CGROUP: rss_anon_kb = cgroup anon, rss_file_kb = cgroup file (page cache), label = memory.current
import os
import sys
import time

WATCH = {"rustc", "ld", "ld.bfd", "ld.lld", "ld.gold", "mold", "collect2", "cc", "gcc", "cargo"}
out = open(sys.argv[1], "w", buffering=1)
interval = float(sys.argv[2]) if len(sys.argv) > 2 else 0.5


def cgroup():
    # cgroup v2 first; v1 (memory controller under /sys/fs/cgroup/memory) as a fallback.
    if os.path.exists("/sys/fs/cgroup/memory.stat"):
        base, anon_key, file_key, cur_file = "/sys/fs/cgroup", "anon", "file", "memory.current"
    else:
        base, anon_key, file_key, cur_file = "/sys/fs/cgroup/memory", "total_rss", "total_cache", "memory.usage_in_bytes"
    stat = {}
    for line in open(f"{base}/memory.stat"):
        k, v = line.split()
        stat[k] = int(v)
    cur = int(open(f"{base}/{cur_file}").read())
    return stat.get(anon_key, 0) // 1024, stat.get(file_key, 0) // 1024, cur // 1024


def label(name, argv):
    # rustc: crate name (+ #test for test harness builds).  linkers: output file name.
    if name == "rustc":
        crate = argv[argv.index("--crate-name") + 1] if "--crate-name" in argv else "?"
        return crate + ("#test" if "--test" in argv else "")
    if "-o" in argv:
        return os.path.basename(argv[argv.index("-o") + 1])
    return ""


while True:
    t = time.time()
    try:
        anon, file, cur = cgroup()
        out.write(f"{t:.2f}\tCGROUP\t0\t0\t{anon}\t{file}\t{cur}\n")
    except OSError:
        pass
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            status = {}
            for line in open(f"/proc/{pid}/status"):
                k, _, v = line.partition(":")
                status[k] = v.strip()
            name = status.get("Name", "")
            if name not in WATCH:
                continue
            argv = [a.decode(errors="replace") for a in open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0") if a]
            anon_kb = int(status.get("RssAnon", "0 kB").split()[0])
            file_kb = int(status.get("RssFile", "0 kB").split()[0])
            out.write(f"{t:.2f}\t{name}\t{pid}\t{status.get('PPid', '0')}\t{anon_kb}\t{file_kb}\t{label(name, argv)}\n")
        except (OSError, ValueError, IndexError):
            continue
    time.sleep(max(0.0, interval - (time.time() - t)))
