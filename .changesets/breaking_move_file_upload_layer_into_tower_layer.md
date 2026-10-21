### refactor: move FileUploadLayer into tower layer instead of http_request_wrapper approach ([PR #9658](https://github.com/apollographql/router/pull/9658))

When [subgraph request log events](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/events) are enabled, subgraph requests for file uploads will now report `application/json` as the `http.request.headers` content-type rather than `multipart/form-data`. The HTTP client span continues to report the correct `multipart/form-data` content-type; only the opt-in subgraph log event is affected.

[ROUTER-1863]: https://apollographql.atlassian.net/browse/ROUTER-1863?atlOrigin=eyJpIjoiNWRkNTljNzYxNjVmNDY3MDlhMDU5Y2ZhYzA5YTRkZjUiLCJwIjoiZ2l0aHViLWNvbS1KU1cifQ

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9658
