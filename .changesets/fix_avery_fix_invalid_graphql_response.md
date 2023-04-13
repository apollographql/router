### fix: return valid GraphQL responses for invalid requests ([Issue #2934](https://github.com/apollographql/router/issues/2934)), ([Issue #2946](https://github.com/apollographql/router/issues/2946))

This PR fixes the way the router handles invalid requests. The router now returns GraphQL errors nested under a top level `errors` array rather than returning a single GraphQL error JSON object.

A new error code has been introduced: "INVALID_CONTENT_TYPE_HEADER". Older versions of the router returned "INVALID_ACCEPT_HEADER" when an invalid content type header was sent.

By [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) in https://github.com/apollographql/router/pull/2947