### Improve error message produced when subgraphs responses don't include an expected `content-type` header value ([Issue #5359](https://github.com/apollographql/router/issues/5359))

To enhance debuggability when a subgraph response lacks an expected `content-type` header value, the error message now includes additional details.

Examples:

   * ```
     HTTP fetch failed from 'test': subgraph response contains invalid 'content-type' header value \"application/json,application/json\"; expected content-type: application/json or content-type: application/graphql-response+json
      ```
   * ```
     HTTP fetch failed from 'test': subgraph response does not contain 'content-type' header; expected content-type: application/json or content-type: application/graphql-response+json
      ```
By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/5223