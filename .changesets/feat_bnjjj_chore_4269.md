### General availability of subscription callback protocol  ([Issue #3884](https://github.com/apollographql/router/issues/3884))

The subscription callback protocol feature is now generally available (GA).

**The configuration of subscription callback protocol in GA is incompatible with its configuration from its preview.** Follow the next section to migrate from preview to GA.

#### Migrate from preview to GA

You must update your router configuration with the following steps:

1. Change the name of the option from `subscription.mode.preview_callback` to `subscription.mode.callback`. 

    Failure to use the updated option name when running the router will result in an error and the router won't start.


    In the example of the GA configuration below, the option is renamed as `callback`.
    

2. Update the `public_url` field to include the full URL of your callback endpoint.

    Previously in preview, the public URL used by the router was the automatic concatenation of the `public_url` and `path` fields. In GA, the behavior has changed, and the router uses exactly the value set in `public_url`. This enables you to configure your own public URL, for example if you have a proxy in front of the router and want to configure redirection with the public URL.
    
    In the example of the GA configuration below, the path `/custom_callback` is no longer automatically appended to `public_url`, so instead it has to be set explicitly as `public_url: http://127.0.0.1:4000/custom_callback`.

3. Configure the new `heartbeat_interval` field to set the period that a heartbeat must be sent to the callback endpoint for the subscription operation. 

    The default heartbeat interval is 5 seconds. Heartbeats can be disabled by setting `heartbeat_interval: disabled`.
    
```yaml
subscription:
  enabled: true
  mode:
    callback: #highlight-line
      public_url: http://127.0.0.1:4000/custom_callback
      listen: 0.0.0.0:4000
      path: /custom_callback
      heartbeat_interval: 5secs # can be "disabled", by default it's 5secs
```

#### Changes in callback protocol specifications

The subscription specification has been updated with the following observable changes:

* The router will always answer with the header `subscription-protocol: callback/1.0` on the callback endpoint.

* Extensions data now includes the heartbeat interval (in milliseconds) that you can globally configure. We also switch from snake_case to camelCase notation. An example of a payload sent to the subgraph using callback mode:

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

* When the router is sending a subscription to a subgraph in callback mode, it now includes a specific `accept` header set to `application/json;callbackSpec=1.0` that let's you automatically detect if it's using callback mode or not.


By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4272