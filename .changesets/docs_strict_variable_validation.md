### Expand `strict_variable_validation` documentation with examples and migration guidance

The `supergraph.strict_variable_validation` configuration option now has clearer documentation. The updated section includes a concrete schema and query example, the actual `VALIDATION_INVALID_TYPE_VARIABLE` error response shape returned when `enforce` mode rejects a request, a version badge indicating the feature shipped in Router v2.12.0, and guidance on when to use `measure` mode as a temporary migration aid for clients sending unknown input object fields.

By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/9362
