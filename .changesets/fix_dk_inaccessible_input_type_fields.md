### Treat fields of an `@inaccessible` input type as inaccessible when merging

Composition lets an `@inaccessible` input field be defined in only some subgraphs: the field is kept in the supergraph, and no hint is raised. That exemption now also applies to the fields of an input type that is itself `@inaccessible`, which matches how enum values of an `@inaccessible` enum were already merged. Previously, such a field was dropped from the supergraph with an `INCONSISTENT_INPUT_OBJECT_FIELD` hint, or composition failed when the field was required (`REQUIRED_INPUT_FIELD_MISSING_IN_SOME_SUBGRAPH`) or used in a default value (`INVALID_GRAPHQL`), even though the whole type is hidden from the API schema.

The `REQUIRED_INACCESSIBLE` error for a required `@inaccessible` input field now names the field (for example ``Input field `I.y` ``) instead of only its type.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10524
