### Copy less of the request pipeline on every request ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

Several built-in features copied the stages of the router's request pipeline that run after them on every request, even when they could decide at once whether to let the request through. With the default configuration, a POST request now makes 3 of these copies of the supergraph stage instead of 10, and 4 of the execution stage instead of 15, which lowers CPU and memory use per request.

The affected features are the sandbox and homepage, CSRF protection, content negotiation, enhanced client awareness, the POST-only mutation check, operation limits, persisted query safelisting, automatic persisted queries, request logging, batching, subscriptions, authorization, and extended references in Apollo usage reports. Responses are unchanged.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
