### Add configurability of span attributes in logs ([Issue #5540](https://github.com/apollographql/router/issues/5540))

The router supports a new  `telemetry.exporters.logging.stdout.format.json.span_attributes` option that enables you to choose a subset of all span attributes to display in your logs.

When `span_attributes` is specified, the router searches for the first attribute in its input list of span attributes from the root span to the current span and attaches it to the outermost JSON object for the log event. If you set the same attribute name for different spans at different levels, the router chooses the attributes of child spans before the attributes of parent spans.


For example, if you have spans that contains `span_attr_1` attribute and you only want to display this span attribute:

```yaml title="router.yaml"
telemetry:
  exporters:
     logging:
       stdout:
         enabled: true
         format: 
           json:
             display_span_list: false
             span_attributes:
             - span_attr_1
```

Example output with a list of spans:

```json
{
  "timestamp": "2023-10-30T14:09:34.771388Z",
  "level": "INFO",
  "fields": {
    "event_attr_1": "event_attr_1",
    "event_attr_2": "event_attr_2"
  },
  "target": "event_target",
  "span_attr_1": "span_attr_1"
}
```

To learn more, go to [`span_attributes`](https://www.apollographql.com/docs/router/configuration/telemetry/exporters/logging/stdout#span_attributes) docs.
By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5867