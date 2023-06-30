### Close the subscription when a new schema has been detected during hot reload ([Issue #3320](https://github.com/apollographql/router/issues/3320))

+ The Router will close subscriptions when the hot-reload happens only if schema is different
+ The Router will send a fatal error in subscription with an error code set to `SUBSCRIPTION_SCHEMA_RELOAD`.

For example:

```json
{
  "errors": [
    {
      "message": "subscription has been closed due to a schema reload",
      "extensions": {
        "code": "SUBSCRIPTION_SCHEMA_RELOAD"
      }
    }
  ]
}
```

If a client receive that kind of error they should automatically reconnect the subscription.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3341