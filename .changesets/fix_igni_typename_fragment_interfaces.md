### Deal with interfaces on fragment spreads when no __typename is queried ([Issue #2587](https://github.com/apollographql/router/issues/2587))

Operations would over rely on the presence of __typename to resolve selection sets on interface implementers. This changeset checks for the parent type in an InlineFragment, so we don't drop relevant selection set when applicable.

By [@o0Ignition0o](https://github.com/o0Ignition0o) and [@geal](https://github.com/geal) in https://github.com/apollographql/router/pull/3718
