### Add `max_lifetime` configuration for subscriptions ([PR #9216](https://github.com/apollographql/router/pull/9216))

Adds a new `max_lifetime` field to the `subscription` configuration block, allowing operators to set a maximum duration for how long a subscription can remain open. After the configured duration the subscription is closed and the client receives a terminal error with extension code `SUBSCRIPTION_MAX_LIFETIME_EXCEEDED`.

```yaml
subscription:
  enabled: true
  max_lifetime: 10m  # close subscriptions after 10 minutes
  mode:
    callback:
      public_url: "https://my-router.example.com/subscription/callback"
```

By default (`max_lifetime` unset) there is no lifetime limit, preserving the existing behaviour.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9216
