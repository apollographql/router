### Add `--listen` to CLI args ([PR #3296](https://github.com/apollographql/router/pull/3296))

Adds `--listen` to CLI args, which allows the user to specify the address to listen on.
It can also be set via environment variable `APOLLO_ROUTER_LISTEN_ADDRESS`.

```bash
router --listen 0.0.0.0:4001
```

By [@ptondereau](https://github.com/ptondereau) and [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3296
