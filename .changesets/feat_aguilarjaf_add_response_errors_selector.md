### add ResponseErrors selector to router response ([PR #7882](https://github.com/apollographql/router/pull/7882))

Introducing the `ResponseErrors` selector in telemetry configurations to capture router response errors, allowing users to capture and log errors encountered at the router service layer. This new selector enhances logging for the router service, as it allows users the option to only log router errors instead of the entire router response body to reduce noise.

``` yaml
telemetry:
  instrumentation:
    events:
      router:
         router.error:
            attributes:
               "my_attribute":
                   response_errors: "$.[0]"
                   # Examples: "$.[0].message", "$.[0].locations", "$.[0].extensions", etc.
```

By [@Aguilarjaf](https://github.com/Aguilarjaf) in https://github.com/apollographql/router/pull/7882