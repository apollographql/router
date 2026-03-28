### Add `max_strings_interned` configuration option for the Rhai plugin

The Rhai plugin now exposes a `max_strings_interned` option that controls Rhai's internal string interning. Because the router compiles Rhai with the `sync` feature (required for multi-threaded operation), the string interner is protected by a `RwLock`. Under high concurrency, threads encountering new strings must acquire a write lock, which can serialize Rhai execution across concurrent requests.

Setting `max_strings_interned: 0` disables interning entirely, eliminating the lock:

```yaml
rhai:
  scripts: ./rhai
  main: main.rhai
  max_strings_interned: 0
```

**Pros of disabling string interning:**
- Eliminates `RwLock` contention on the string interner. Benchmarks of scripts representative of header manipulation and context key access show approximately 60% latency reduction and 2.5× throughput improvement on Rhai engine execution under 8 concurrent threads.
- Most impactful for deployments with many concurrent requests and scripts that access a large number of distinct string values.

**Cons of disabling string interning:**
- Repeated identical strings are allocated separately rather than sharing a single allocation, increasing memory allocation pressure.
- String equality comparisons lose the interning fast-path (pointer equality) and must compare bytes, though this effect is small in practice.
- For low-concurrency deployments or scripts with limited string variety, the default of 256 interned strings may perform better overall by amortizing allocation costs.

The default (`null`) preserves Rhai's existing behaviour of 256 interned strings for full backward compatibility.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/ROUTER_PR_NUMBER
