### Fix deduplicated subscriptions hanging when one subscription closes ([PR #7879](https://github.com/apollographql/router/pull/7879))

Fixes a regression introduced in v1.50.0. When multiple client subscriptions are deduped onto a single subgraph subscription, and the first client subscription closes, the Router would close the subgraph subscription. The other deduplicated subscriptions would then silently stop receiving events.

Now outgoing subscriptions to subgraphs are kept open as long as _any_ client subscription uses them.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7879