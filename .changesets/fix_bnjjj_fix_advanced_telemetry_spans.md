### Fix custom attributes for spans and histogram when used with response_event ([PR #5221](https://github.com/apollographql/router/pull/5221))

It will fix several issues found:

+ Custom attributes based on response_event in spans were not added properly
+ Histograms using response_event selectors were not updated properly
+ Static selector to set a static value is now able to take a Value instead of just a string
+ Static selector to set a static value is now set at every stages
+ New `on_graphql_error` selector also available on supergraph
+ You can now override the status of a span using `otel.status_code` attribute to change the status of a span

For example, by default spans are marked in error if you have a critical error or http status code != 200, now if you want to mark your span in error if you have a graphql error in response body for example then you can have this kind of configuration:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        attributes:
          otel.status_code:
            static: error
            condition:
              eq:
              - true
              - on_graphql_error: true
      supergraph:
        attributes:
          otel.status_code:
            static: error
            condition:
              eq:
              - true
              - on_graphql_error: true
``` 

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5221