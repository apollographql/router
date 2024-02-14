### Configure logging format automatically based on terminal ([Issue #4369](https://github.com/apollographql/router/issues/4369))

You can configure the logging output format when running with an interactive shell.

If both `format` and `tty_format` are configured, then the format used depends on how the router is run:

* If running with an interactive shell, then `tty_format` takes precedence.
* If running with a non-interactive shell, then `format` takes precedence.

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