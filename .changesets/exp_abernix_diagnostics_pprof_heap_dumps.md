### Diagnostics heap dumps are now standard pprof, and work on macOS

The experimental diagnostics plugin (`experimental_diagnostics`) now saves heap dumps as standard pprof files (`.pb.gz`). Open them in the diagnostics dashboard as before, or in any pprof tool, such as `go tool pprof`, Pyroscope, or speedscope, without needing the router binary or `jeprof`. Heap dumps now work on macOS as well as Linux.

If you already use heap dumps:

- Dumps from earlier versions (`.prof`) are no longer listed or shown in the dashboard. Delete them from your output directory.
- `GET /diagnostics/memory/status` adds `sampling_active`, which says whether memory sampling is on. `profiling_active` now says whether a diagnostics profiling session is running.
- The dashboard needs a recent browser (Chrome 80+, Safari 16.4+, or Firefox 113+) to read the new format.

By [@abernix](https://github.com/abernix)
