### Add `intern_strings` configuration option for the Rhai plugin

The Rhai plugin now exposes an `intern_strings` option that controls Rhai's internal string interning. Under high concurrency, threads encountering new strings must acquire a write lock, which can serialize Rhai execution across concurrent requests.

Setting `intern_strings: false` disables interning, eliminating the lock:

```yaml
rhai:
  scripts: ./rhai
  main: main.rhai
  intern_strings: false
```

String interning can alleviate memory allocation and make string equality checks a little faster. For deployments serving many concurrent requests, the cost likely outweighs the benefit, so we recommend experimenting with `intern_strings: false` and observing if it improves performance.

The default (`true`) preserves the existing behaviour.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/9070
