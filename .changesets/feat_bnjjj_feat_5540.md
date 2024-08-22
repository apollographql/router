### Telemetry: add configurability of span attributes in logs ([Issue #5540](https://github.com/apollographql/router/issues/5540))

The `telemetry.exporters.logging.stdout.format.json.span_attributes` option is useful if you don't want to display all spans attributes but only some of them. It takes an array of attribute name to include as a root logging attribute.
This will search for the first attribute in the list of span attributes from the root span to the current one and attach it to the outmost json object for the log event.
If you set the same attribute on different spans at different level, then the identical attribute from child spans will take precedence over the one found previously in parent spans.

For example, if you have spans that contains `graphql.document` attribute and you only want to display this span attribute:

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

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5867