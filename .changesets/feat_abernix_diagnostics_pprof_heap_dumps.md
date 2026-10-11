### Diagnostics heap dumps are now standard pprof, on Linux and macOS

The experimental diagnostics plugin (`experimental_diagnostics`) now writes heap dumps as gzipped pprof (`.pb.gz`), symbolized inside the router. They open in the diagnostics dashboard as before, and in standard tools such as `go tool pprof`, Pyroscope, and speedscope, without the router binary or `jeprof`.

The diagnostics plugin and continuous profiling (`experimental_continuous_profiling`) now share one heap profiler, on Linux and macOS:

- Allocation sampling stays on while either one needs it. Stopping a diagnostics session no longer switches off sampling that continuous profiling is using, or the other way around.
- `GET /diagnostics/memory/status` adds `sampling_active`, which is `true` whenever sampling is on, including for continuous profiling. In that case, dumps include allocations made since continuous profiling started.

Changes to this experimental feature that may affect you:

- Heap dumps written before this change (`.prof`) are no longer listed, downloaded, deleted, or shown in the dashboard. Remove them from the output directory by hand.
- Heap dumps record in-use bytes only, not allocation counts.
- Where heap profiling isn't available (for example, Windows), listing, downloading, and deleting dumps now work (on an empty list) instead of responding with `501 Not Implemented`, and diagnostic archives no longer include `memory/README.txt`.

By [@abernix](https://github.com/abernix)
