### Stop cloning the service tree in router layers that decide synchronously

`BoxCloneService::clone` is deep: it clones every boxed layer beneath it, down to the next buffer. Several built-in layers in the router, supergraph, and execution stages moved a clone of their inner service into their response future on every request, even though they decide synchronously whether to pass the request on. Each of those clones re-cloned everything beneath it, so the cost grew with the depth of the service stack.

These layers now run their check when they are called and pass the request to the inner service directly:

- the sandbox and homepage layer, CSRF protection, router content negotiation, enhanced client awareness, the HTTP POST-only mutation check, and operation limits
- extended reference reporting, and extraction of authorization requirements
- persisted query safelisting, and automatic persisted queries for requests without a `persistedQuery` extension
- request logging when no `router.request` event is configured
- batching query plan analysis for requests that are not part of a batch
- subscription execution for operations that are not subscriptions

With the default configuration, a POST request now clones the supergraph service tree 3 times instead of 10, and the execution service tree 4 times instead of 15. The remaining clones are in layers that must wait on the request body, query parsing, or query planning before calling the next service. The execution service also stops copying the Apollo telemetry configuration on every request.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
