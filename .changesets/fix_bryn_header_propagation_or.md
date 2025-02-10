### Header propagation rules passthrough ([PR #6690](https://github.com/apollographql/router/pull/6690))

Header propagation contains logic to prevent headers from being propagated more than once. This was broken
in https://github.com/apollographql/router/pull/6281 which always considered a header propagated regardless if a rule
actually matched.

This PR alters the logic so that only when a header is populated then the header is marked as fixed.

The following will now work again:

```
headers:
  all:
    request:
      - propagate:
          named: a
          rename: b
      - propagate:
          named: b
```

Note that defaulting a head WILL populate a header, so make sure to include your defaults last in your propagation
rules.

```
headers:
  all:
    request:
      - propagate:
          named: a
          rename: b
          default: defaulted # This will prevent any further rule evaluation for header `b`
      - propagate:
          named: b
```

Instead, make sure that your headers are defaulted last:

```
headers:
  all:
    request:
      - propagate:
          named: a
          rename: b
      - propagate:
          named: b
          default: defaulted # OK
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6690
