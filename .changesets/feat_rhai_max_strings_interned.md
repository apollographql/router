### Add `intern_strings` configuration option for the Rhai plugin

The Rhai plugin now exposes an `intern_strings` option that controls Rhai's internal string interning. Because the router compiles Rhai with the `sync` feature (required for multi-threaded operation), the string interner is protected by a `RwLock`. Under high concurrency, threads encountering new strings must acquire a write lock, which can serialize Rhai execution across concurrent requests.

Setting `intern_strings: false` disables interning entirely, eliminating the lock:

```yaml
rhai:
  scripts: ./rhai
  main: main.rhai
  intern_strings: false
```

**Pros of disabling string interning:**
- Eliminates `RwLock` contention on the string interner. Most impactful for deployments with many concurrent requests and scripts that access a large number of distinct string values.

**Cons of disabling string interning:**
- Repeated identical strings are allocated separately rather than sharing a single allocation, increasing memory allocation pressure.
- String equality comparisons lose the interning fast-path (pointer equality) and must compare bytes, though this effect is small in practice.
- For low-concurrency deployments or scripts with limited string variety, the default of 256 interned strings may perform better overall by amortizing allocation costs.

The default (`true`) preserves Rhai's existing behaviour of 256 interned strings for full backward compatibility.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/ROUTER_PR_NUMBER
