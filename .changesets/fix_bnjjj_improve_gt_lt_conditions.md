### Support `gt`/`lt` conditions for parsing string selectors to numbers ([PR #5758](https://github.com/apollographql/router/pull/5758))

The router now supports greater than (`gt`) and less than (`lt`) conditions for header selectors.
 
The following example applies an attribute on a span if the `content-length` header is greater than 100:

```yaml
telemetry:
  instrumentation:
    spans:
      mode: spec_compliant
      router:
        attributes:
          trace_id: true
          payload_is_to_big: # Set this attribute to true if the value of content-length header is > than 100
            static: true
            condition:
              gt:
              - request_header: "content-length"
              - 100
``` 

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5758