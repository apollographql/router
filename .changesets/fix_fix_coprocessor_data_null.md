### Fix Parsing of Coprocessor GraphQL Responses ([PR #7141](https://github.com/apollographql/router/pull/7141))

Standardized GraphQL response parsing and validation between coprocessors and subgraphs to ensure consistent behavior throughout the Router. 

Previously, there were discrepancies in how responses were handled depending on their source. For example, when a coprocessor returned a GraphQL response with `data: null` alongside error information, the Router would improperly omit the `data` field during deserialization.

This fix ensures that all GraphQL responses are processed using the same validation logic regardless of their origin, improving reliability and making error handling more predictable.

Contributed by [@IvanGoncharov](https://github.com/IvanGoncharov) in [#7141](https://github.com/apollographql/router/pull/7141)