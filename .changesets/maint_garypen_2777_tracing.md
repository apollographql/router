### Re-structure the rhai logging output ([Issue #2777](https://github.com/apollographql/router/issues/2777))

The Rhai logging output was implemented about a year ago. Since that time, significant efforts have been made with the rest of the router to standardize on a slightly different format. This change brings rhai log output into line with the rest of the router.

It also addresses the requirement to make the "message" output specifiable by the script author.

The impact of this change can be seen in this example. If we were to `log_info()` in our rhai script:

```
  log_info("this is info");
```

BEFORE:

```
{"timestamp":"2023-04-19T07:46:15.483358Z","level":"INFO","message":"rhai_info","out":"this is info"}
```

AFTER:

```
{"timestamp":"2023-04-19T07:46:15.483358Z","level":"INFO","message":"this is info","target":"src/rhai_logging.rhai"}
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2975