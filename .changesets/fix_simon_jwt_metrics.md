### Fix JWT metrics discrepancy ([PR #7258](https://github.com/apollographql/router/pull/7258))

This fixes the `apollo.router.operations.authentication.jwt` counter metric to behave [as documented](https://www.apollographql.com/docs/graphos/routing/security/jwt#observability): emitted for every request that uses JWT, with the `authentication.jwt.failed` attribute set to true or false for failed or successful authentication.

Previously, it was only used for failed authentication.

The attribute-less and accidentally-differently-named `apollo.router.operations.jwt` counter was and is only emitted for successful authentication, but is deprecated now.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/7258