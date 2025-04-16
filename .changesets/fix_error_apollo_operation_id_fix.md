### Fix Apollo request metadata generation for errors ([PR #7021](https://github.com/apollographql/router/pull/7021))

* Fixes the Apollo operation ID and name generated for requests that fail due to parse, validation, or invalid operation name errors.
* Updates the error code generated for operations with an invalid operation name from GRAPHQL_VALIDATION_FAILED to GRAPHQL_UNKNOWN_OPERATION_NAME

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/7021