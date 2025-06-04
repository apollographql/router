### Fix otlp metric export when using http protocol ([PR #7595](https://github.com/apollographql/router/pull/7595))

We updated the router dependency for opentelemetry when we released router 2.0.

The opentelemetry dependency changed how it processed endpoints (destinations for metrics and traces) and this was not detected until now.

The router wasn't setting the path correctly, so exporting metrics over http was not working for the default endpoint. Exporting metrics via gRPC was not impacted. Neither were traces.

We have fixed our interactions with the dependency and improved our testing to make sure this does not occur again.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7595
