### Fix header propagation where renames have already taken place ([PR #6281](https://github.com/apollographql/router/pull/6281))

https://github.com/apollographql/router/pull/4535 introduced a regression where the following header propagation config would not work:

```yaml
headers:
- propagate:
    named: a
    rename: b
- propagate:
    named: a
    rename: c
```

The goal of the original PR was to prevent multiple headers from being mapped to a single target header. However, it did not consider renames, and instead prevented multiple mappings from the same source header. 
The Router will now propagate headers properly ensuring that a target header is only propagated to once.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6281
