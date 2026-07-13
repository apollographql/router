### Fix input-object and enum variable coercion to respect the API schema

When a client sent an operation with an unknown field on an input-object variable, the router's `VALIDATION_INVALID_TYPE_VARIABLE` error message embedded the full composed input type definition, including federation directives (`@join__type`, `@tag`, etc.) and internal subgraph names, regardless of the `introspection` or `redact_query_validation_errors` settings. This error now reports only the type name, matching the existing behavior for enum and scalar coercion errors.

Separately, variable coercion validated input-object fields and enum values against the internal supergraph schema rather than the client-facing API schema, so a variable could reference an `@inaccessible` field or enum value even though the same reference would be rejected in the operation document itself. Variable coercion now validates against the API schema, consistent with how operation documents are already validated.

By [@carodewig](https://github.com/carodewig)
