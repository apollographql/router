### Connectors apply operation variable defaults to `$args` ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

When a client omitted a variable that the operation declared with a default (for example `query ($limit: Int = 2) { items(limit: $limit) }`), connectors received `null` for the argument in `$args` instead of the default. Because the argument then looked explicitly set, the field's own schema default was skipped as well.

Connectors now coerce arguments the way a GraphQL server would: a provided variable is used as is, an omitted variable takes the operation's default, and an omitted variable with no default leaves the argument unset so the field's schema default applies. An explicit `null` is still passed through as `null`.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/PULL_NUMBER
