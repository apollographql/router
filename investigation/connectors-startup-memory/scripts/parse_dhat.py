#!/usr/bin/env python3
"""Summarize a dhat-heap.json (dhat-rs format v2).

dhat-rs writes per program-point (pp) records under "pps", each with:
  tb  = total bytes ever allocated at this pp
  tbk = total blocks (allocations) at this pp
  gb  = bytes alive at the moment of global heap maximum (t-gmax)  [peak attribution]
  gbk = blocks alive at t-gmax
  fs  = list of frame indices into the top-level "ftbl" frame table

Peak heap (the OOM-relevant number) = sum(pp.gb) across pps.
Total allocated bytes = sum(pp.tb). Total allocations = sum(pp.tbk).

Usage:
  parse_dhat.py <dhat-heap.json> [--top N] [--grep SUBSTR]
  parse_dhat.py <dhat-heap.json> --tsv            # one summary line (for sweeps)
"""
import argparse
import json
import sys


def load(path):
    with open(path) as f:
        return json.load(f)


def summarize(d):
    pps = d.get("pps", [])
    total_bytes = sum(pp.get("tb", 0) for pp in pps)
    total_blocks = sum(pp.get("tbk", 0) for pp in pps)
    peak_bytes = sum(pp.get("gb", 0) for pp in pps)
    peak_blocks = sum(pp.get("gbk", 0) for pp in pps)
    return total_bytes, total_blocks, peak_bytes, peak_blocks


def frame_str(ftbl, idx):
    try:
        return ftbl[idx]
    except (IndexError, TypeError):
        return f"<frame {idx}>"


def top_by_peak(d, n, grep=None):
    ftbl = d.get("ftbl", [])
    pps = d.get("pps", [])
    rows = []
    for pp in pps:
        gb = pp.get("gb", 0)
        if gb <= 0:
            continue
        fs = pp.get("fs", [])
        # Use the shallowest few user frames as the label.
        frames = [frame_str(ftbl, i) for i in fs]
        if grep and not any(grep in f for f in frames):
            continue
        rows.append((gb, pp.get("gbk", 0), pp.get("tb", 0), frames))
    rows.sort(key=lambda r: r[0], reverse=True)
    return rows[:n]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("path")
    ap.add_argument("--top", type=int, default=0)
    ap.add_argument("--grep", default=None, help="only stacks containing this substring")
    ap.add_argument("--frames", type=int, default=8, help="frames to print per stack")
    ap.add_argument("--tsv", action="store_true")
    args = ap.parse_args()

    d = load(args.path)
    tb, tbk, gb, gbk = summarize(d)
    if args.tsv:
        print(f"{tb}\t{tbk}\t{gb}\t{gbk}")
        return
    mib = 1024 * 1024
    print(f"file: {args.path}")
    print(f"total allocated:  {tb:>14,} bytes ({tb/mib:8.2f} MiB)  in {tbk:>12,} allocations")
    print(f"peak heap (gmax): {gb:>14,} bytes ({gb/mib:8.2f} MiB)  in {gbk:>12,} live blocks")
    if args.top:
        print(f"\ntop {args.top} program points by bytes-at-peak"
              + (f" (grep={args.grep!r})" if args.grep else "") + ":")
        for rank, (g, gk, t, frames) in enumerate(top_by_peak(d, args.top, args.grep), 1):
            print(f"\n#{rank}  peak={g:,}B ({g/mib:.2f} MiB)  live_blocks={gk:,}  total_alloc={t:,}B")
            for fr in frames[:args.frames]:
                print(f"      {fr}")


if __name__ == "__main__":
    main()
