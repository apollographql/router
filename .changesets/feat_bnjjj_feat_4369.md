### Ability to automatically switch the logging format depending on the terminal ([Issue #4369](https://github.com/apollographql/router/issues/4369))

You can configure the logging output format when you're running on an interactive shell. If bother `format` and `tty_format` are configured then the format depends on how the router is run:

* In an interactive shell, `tty_format` will take precedence.
* In a non-interactive shell, `format` will take precedence.

You can explicitly set the format in `router.yaml` with `telemetry.exporters.logging.stdout.tty_format`:

```yaml title="router.yaml"
telemetry:
  exporters:
     logging:
       stdout:
         enabled: true
         format: json
         tty_format: text
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4567