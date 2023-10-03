### only exclude defer/subscriptions if actually part of a batch ([Issue #3956](https://github.com/apollographql/router/issues/3956))

Fix the checking logic so that deferred queries or subscriptions will only be rejected when experimental batching is enabled and the operations are part of a batch.

Without this fix, all subscriptions or deferred queries would be rejected when experimental batching support was enabled.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3959