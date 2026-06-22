### Ensure `client.name` and `client.version` attributes on router metrics can use selectors ([PR #9502](https://github.com/apollographql/router/pull/9502))

A recent change added `client.name` and `client.version` as standard attributes on `RouterAttributes` to support aliasing. This inadvertently caused the JSON schema to reject selector-based overrides e.g.

```yaml
client.name: 
  request_header: x-my-header
```

for those fields. We now support both the boolean/alias form, as well as the custom selector syntax.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9502
