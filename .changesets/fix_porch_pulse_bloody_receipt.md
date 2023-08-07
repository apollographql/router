### Close the subscription when a new schema has been detected during hot reload ([Issue #3320](https://github.com/apollographql/router/issues/3320))

Router hot reloads on schema updates didn't close running subscriptions, which could imply out of date query plans.
This changeset allows the router to signal clients that a `SUBSCRIPTION_SCHEMA_RELOAD` happened, and close the running subscription, so the clients can subscribe again:


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


By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3341