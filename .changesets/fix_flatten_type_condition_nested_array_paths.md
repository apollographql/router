### Report correct locations for type-conditioned flatten matches inside nested lists ([PR #10368](https://github.com/apollographql/router/pull/10368))

When a type-conditioned flatten step in a query plan path (for example `items.@|[Book]`) met a list nested inside the list it was flattening, the router found the matching objects but recorded their location using only the inner list index, dropping the outer one. Entity fetch results and errors could then be attributed to the wrong position in the response. The router now records both indices.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10368
