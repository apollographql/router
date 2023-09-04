### GraphQL response processing must happen under the execution span ([PR #3732](https://github.com/apollographql/router/pull/3732))

Previously, any event in processing would be reported under the supergraph span, or any plugin span (like rhai) happening in between

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3732