### Enfore JWT expiration for subscriptions ([Issue #3947](https://github.com/apollographql/router/issues/3947))

If a JWT expires whilst a subscription is executing, the subscription should be terminated. This also applies to deferred responses.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4166