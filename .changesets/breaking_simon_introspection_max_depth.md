### Limit the depth of introspection queries ([PR #6601](https://github.com/apollographql/router/pull/6601))

The [schema-intropsection schema](https://spec.graphql.org/draft/#sec-Schema-Introspection.Schema-Introspection-Schema) is recursive: a client can query the fields of the types of some other fields, and so on arbitrarily deep. This can produce responses that grow much faster than the size of the request.

To protect against abusive requests Router now refuses to execute introspection queries that nest list fields too deep and returns an error instead. The criteria matches `MaxIntrospectionDepthRule` in graphql-js, but may change in future versions.

In case it rejects legitimate queries, this check can be disabled in Router configuration:

```yaml
# Do not enable introspection in production!
supergraph:
  introspection: true  # Without this, schema introspection is entirely disabled by default
limits:
  introspection_max_depth: false  # Defaults to true
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6601
