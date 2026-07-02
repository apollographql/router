### Emit startup warning when deprecated `Static(String)` telemetry selector is used ([Issue #1766](https://github.com/apollographql/router/issues/1766))

The string shorthand form for static telemetry attribute selectors (e.g. `my_attribute: "value"`) is now deprecated and will log a warning at startup. Use the object form instead:

```yaml
my_attribute:
  static: "value"
```

The object form additionally supports typed values (bool, int, float, array), making it strictly more capable than the string shorthand.
