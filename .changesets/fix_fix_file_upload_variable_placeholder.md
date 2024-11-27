### Remove placeholders from file upload query variables ([PR #6293](https://github.com/apollographql/router/pull/6293))

Previously, file upload query variables in subgraph requests incorrectly contained internal placeholders.
According to the [GraphQL Multipart Request Spec](https://github.com/jaydenseric/graphql-multipart-request-spec?tab=readme-ov-file#multipart-form-field-structure), these variables should be set to null.
This issue has been fixed by ensuring that the router complies with the specification and improving compatibility with subgraphs handling file uploads.

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6293
