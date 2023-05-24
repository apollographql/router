### Fix router deferred response and body type issues ([Issue #3015](https://github.com/apollographql/router/issues/3015))

The current implementation of the `RouterResponse` processing for coprocessors forces buffering of response data before passing the data to a coprocessor. This is a bug, because deferred responses should be processed progressively with a stream of calls to the coprocessor as each chunk of data becomes available.

Furthermore, the data type was assumed to be valid JSON for both `RouterRequest` and `RouterResponse` coprocessor processing. This is also a bug, because data at this stage of processing was never necessarily valid JSON. This is a particular issue when dealing with deferred (when using `@defer`) `RouterResponses`.

This change fixes both of these bugs by modifying the router so that coprocessors are invoked with a `body` payload which is a JSON `String`, not a JSON `Object`. Furthermore, the router now processes each chunk of response data separately so that a coprocessor will receive multiple calls (once for each chunk) for a deferred response.

For more details about how this works see the [coprocessor documentation](https://www.apollographql.com/docs/router/customizations/coprocessor/).

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3104