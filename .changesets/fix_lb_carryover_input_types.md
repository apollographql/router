### Fix directive inputs with `@composeDirective` and Connectors ([PR #7383](https://github.com/apollographql/router/pull/7383))

Prior to this fix, any time a directive added with `@composeDirective` has its own input types (custom scalars, enums, input types) and a Connector is used, those types would be lost and the supergraph would fail to compose.

<!-- https://apollographql.atlassian.net/browse/CNN-755 -->

By [@lennyburdette](https://github.com/lennyburdette) in https://github.com/apollographql/router/pull/7383