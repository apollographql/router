### GraphQL `ID` fields are now coerced to strings per the spec ([PR #10119](https://github.com/apollographql/router/pull/10119))

The router now coerces integer values on `ID` fields to strings in responses, matching the GraphQL specification requirement that `ID` is serialized as a string. For example, an entity representation that previously contained `{"id": 1}` will now contain `{"id": "1"}`. Subgraphs that parse entity keys strictly by type may need adjustment. Response cache entries written by an older router version may not match after upgrade if they contain integer `ID` values.

By [@tninesling](https://github.com/tninesling) in <https://github.com/apollographql/router/pull/10119>
