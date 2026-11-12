### Lower the CPU cost of span information in logs ([PR #10754](https://github.com/apollographql/router/pull/10754))

So that a log line can show the spans it was written in, the router keeps a copy of each request's span attributes, even when nothing is logged. Keeping that copy now costs about half as much. In a benchmark of a request with three subgraph fetches, the cost per request fell from about 6 µs to about 3 µs.

When your stdout log format shows no span information, the router no longer keeps the copy at all. That is the case when `display_span_list` and `display_current_span` are both `false` and, for JSON, `span_attributes` is empty.

You don't need to change any configuration. Both formats show the span list by default. If you don't use it, turning it off removes the remaining cost:

```yaml
telemetry:
  exporters:
    logging:
      stdout:
        format:
          json:
            display_span_list: false
```

For the `text` format, also set `display_current_span: false`.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10754
