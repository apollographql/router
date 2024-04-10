### Persisted queries return 4xx errors ([PR #4887](https://github.com/apollographql/router/pull/4887)

Previously, sending an invalid persisted query request could return a 200 status code to the client when they should have returned errors. These requests now return errors as 4xx status codes:

- Sending a PQ ID that is unknown returns 404 (Not Found).
- Sending freeform GraphQL when no freeform GraphQL is allowed returns
  400 (Bad Request).
- Sending both a PQ ID and freeform GraphQL in the same request (if the
  APQ feature is not also enabled) returns 400 (Bad Request).
- Sending freeform GraphQL that is not in the safelist when the safelist
  is enabled returns (403 Forbidden).
- A particular internal error that shouldn't happen returns 500 (Internal
  Server Error).

  By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/4887
