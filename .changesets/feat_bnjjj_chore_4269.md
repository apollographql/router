### Promote subscription callback protocol to GA ([Issue #3884](https://github.com/apollographql/router/issues/3884))

#### Configuration changes

In order to promote subscription callback protocol to GA we finalized the specs and made some breaking changes. That's why when running this new version you'll have an error if you had `subscription.mode.preview_callback` in your configuration it won't start. You'll have to update your configuration and make sure your subgraph implementation is compliant with the stable version of subscription callback protocol. It's not longer `preview_callback` in the configuration but just `callback`. 

Here is an example of configuration:

```yaml
subscription:
  enabled: true
  mode:
    callback:
      public_url: http://127.0.0.1:4000/custom_callback
      listen: 0.0.0.0:4000
      path: /custom_callback
      heartbeat_interval: 5secs # can be "disabled", by default it's 5secs
```

One of the main difference in configuration is the behavior of `public_url`, this field must now include the full url of your callback endpoint, which means if you specified a path like in this example it's set to `/custom_callback` you'll have to also specify that path in the `public_url` field. Previously we automatically added the path to the `public_url` but not anymore. It will let you configure your own public url and use some redirection on your proxy if you have one in front of the router for example.
We also added `heartbeat_interval` configuration field to configure specific heartbeat interval, the heartbeat can also be disabled by setting `disabled`.

#### Changes in callback protocol specifications:

The router will always answer with header `subscription-protocol: callback/1.0` on the callback endpoint.
You can now globally configure a heartbeat or disable it so in extensions data sent to the subgraph it includes the heartbeat interval in milliseconds. We also switch from snake_case to camelCase notation, here is an example of payload sent to the subgraph using callback mode:

```json
{
    "query": "subscription { userWasCreated { name reviews { body } } }",
    "extensions": {
        "subscription": {
            "callbackUrl": "http://localhost:4000/callback/c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
            "subscriptionId": "c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
            "verifier": "XXX",
            "heartbeatIntervalMs": 5000
        }
    }
}
```

When the router is sending a subscription to a subgraph in callback mode it now includes a specific `accept` header set to `application/json+graphql+callback/1.0` which let's you automatically detect if it's using callback mode or not.


By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4272