### Allow nested object literals in requestless connectors ([PR #10394](https://github.com/apollographql/router/pull/10394))

A `@connect` directive with no `http:` (a virtual connector) failed composition with `REQUESTLESS_SELECTION_USES_REQUEST_DATA` when its selection contained a nested object literal, such as `price: { amount: 1395, currencyCode: "USD" }`, even though it reads nothing from a response. These selections now compose as expected. Selections that do read the response body, like `price { amount }` or a bare `$`, are still rejected.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10394
