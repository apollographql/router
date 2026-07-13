### Enforce `http_max_request_bytes` on GraphQL queries sent via HTTP GET

The router's `limits.http_max_request_bytes` setting previously only limited the size of the HTTP request body, so it had no effect on GraphQL-over-HTTP GET requests, which carry the query in the URL's query string instead of the body. In environments with a strict `http_max_request_bytes` configured, this allowed the limit to be bypassed by sending a GET request instead of a POST.

GET requests are now checked against the same `http_max_request_bytes` limit, measured against the byte length of the URI's query string, and rejected with a 414 (URI Too Long) response if it's exceeded.

Note that this measures the percent-encoded query string, which is larger than the same query's byte count in a compact POST JSON body (URL encoding can expand some characters to 3 bytes each). A query that fits under the limit as a POST body may need a slightly higher limit to also fit as a GET query string.

By [@carodewig](https://github.com/carodewig)
