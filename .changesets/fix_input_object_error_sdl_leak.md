### Fix input-object and enum variable coercion to respect the API schema

Variable coercion validated input-object fields and enum values against the internal supergraph schema rather than the client-facing API schema, so a variable could reference an `@inaccessible` field or enum value even though the same reference would be rejected in the operation document itself. Variable coercion now validates against the API schema, consistent with how operation documents are already validated.

Separately, an input-object variable with a field that isn't defined on the corresponding input type previously passed validation. The router now rejects such variables by default. This can be relaxed with the `supergraph.strict_variable_validation` option, which logs the unknown field(s) instead of rejecting the request when set to `measure`:

```yaml
supergraph:
  strict_variable_validation: measure # default: enforce
```

By [@carodewig](https://github.com/carodewig)
