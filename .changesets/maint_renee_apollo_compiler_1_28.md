### Reject `@skip`/`@include` on subscription root fields in validation ([PR #7338](https://github.com/apollographql/router/pull/7338))

This implements a [GraphQL spec RFC](https://github.com/graphql/graphql-spec/pull/860), rejecting subscriptions in validation that can be invalid during execution.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/7338