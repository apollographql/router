### Adds some new (unstable) metrics ([PR #3609](https://github.com/apollographql/router/pull/3609))

Many of our existing metrics are poorly and inconsistently named. In addition they follow prometheus style rather than otel style.

This PR adds some new metrics that will hopefully give us a good foundation to build upon.
New metrics are namespaced `apollo.router.operations.*`.

Until officially documented the metrics should be treated as unstable, as we may need change the names to ensure consistency.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3609
