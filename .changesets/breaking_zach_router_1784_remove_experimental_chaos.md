### Remove `experimental_chaos`

The `experimental_chaos` config section and its chaos-testing plugin have been removed. It was an internal QE tool for forcing schema/config hot reloads to reproduce memory-leak and reload bugs, with no customer usage. Router 3.0 removes it entirely.

If your config sets `experimental_chaos`, remove it — there is no replacement.

```yaml
# Before (no longer supported)
experimental_chaos:
  force_schema_reload: 30s
  force_config_reload: 2m

# After
# (remove the experimental_chaos section)
```

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/0000
