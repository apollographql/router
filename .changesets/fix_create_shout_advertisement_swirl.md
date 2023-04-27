### Improve performance of span metrics ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/3008))

The Router surfaces metrics about span durations. This used to be implemented as an OTel Exporter. However, this is costly as the spans are sent over a channel to be processed.
Instead, the processing of spans to metrics is noe implemented as a `SpanProcessor` directly.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/3009
