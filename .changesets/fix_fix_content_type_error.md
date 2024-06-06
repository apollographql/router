### Improve error message produced when subgraphs responses don't include an expected `content-type` header value ([Issue #5359](https://github.com/apollographql/router/issues/5359))

To improve a common debuggability challenge when a subgraph response doesn't contain an expected `content-type` header value, the error message produced will include additional details about the error.

Examples:

   * ```
     HTTP fetch failed from 'test': subgraph response contains invalid 'content-type' header value \"application/json,application/json\"; expected content-type: application/json or content-type: application/graphql-response+json
      ```
   * ```
     HTTP fetch failed from 'test': subgraph response does not contain 'content-type' header; expected content-type: application/json or content-type: application/graphql-response+json
      ```
By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/5223