### Return 503 for rate limit traffic shaping ([PR #9013](https://github.com/apollographql/router/pull/9013))

Reverts [PR #8765](https://github.com/apollographql/router/pull/8765).

When the router's rate limit or buffer capacity is exceeded, it now returns HTTP 503 (Service Unavailable) instead of HTTP 429 (Too Many Requests).

HTTP 429 implies that a specific client has sent too many requests and should back off. HTTP 503 more accurately reflects the situation: the router is temporarily unable to handle the request due to overall service load, not because of the behavior of any individual client.

This change affects both router-level and subgraph-level rate limiting. Documentation has been updated to reflect the new status code.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9013
