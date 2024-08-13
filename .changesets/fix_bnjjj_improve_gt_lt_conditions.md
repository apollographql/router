### Add the ability for `gt`/`lt` conditions to parse the string selector to number ([PR #5758](https://github.com/apollographql/router/pull/5758))

This will enable the ability to have gt/lt conditions on header selectors for example, if you want to put a specific attribute on a span if the `content-length` header is greater than 100:

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