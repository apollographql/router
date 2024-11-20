### File Uploads: Remove Placeholders from Query Variables ([PR #6293](https://github.com/apollographql/router/pull/6293))

Fixed an issue where file upload query variables in subgraph requests contained internal placeholders.
According to the [GraphQL Multipart Request Spec](https://github.com/jaydenseric/graphql-multipart-request-spec?tab=readme-ov-file#multipart-form-field-structure), these variables should be set to null.
This fix ensures that the Router complies with the specification and improves compatibility with subgraphs handling file uploads.

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6293
