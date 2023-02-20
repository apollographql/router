### CORS: Give a more meaningful message for users who misconfigured allow_any_origin ([PR #2634](https://github.com/apollographql/router/pull/2634))

Allowing any origins in the router configuration is done this way:
```yaml
cors:
  allow_any_origin: true
```

It is however intuitive for users to try to set it up like so:
```yaml
cors:
  origins:
    - "*"
```

Unfortunately, this won't work and the error message was neither comprehensive nor actionnable:

```
ERROR panicked at 'Wildcard origin (`*`) cannot be passed to `AllowOrigin::list`. Use `AllowOrigin::any()` instead'
```

This change adds a meaningful error message which will help you successfully set up the router:

```
Invalid CORS configuration: use `allow_any_origin: true` to set `Access-Control-Allow-Origin: *`
```

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2634