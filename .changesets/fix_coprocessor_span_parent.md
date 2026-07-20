### Coprocessor spans are now correctly parented under the outbound HTTP client span ([PR #9724](https://github.com/apollographql/router/pull/9724))

When a coprocessor is configured, the router's outbound HTTP span was being
reported as a sibling of the coprocessor's server span rather than its parent,
breaking distributed trace hierarchy for OTel-instrumented coprocessors.

Fixed by ensuring the correct span (`http_request`) is active when trace context
is injected into the outgoing request.

By [@OriginLeon](https://github.com/OriginLeon) in https://github.com/apollographql/router/pull/9724
