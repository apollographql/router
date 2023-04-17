### Invalid requests now return proper GraphQL-shaped errors ([Issue #2934](https://github.com/apollographql/router/issues/2934)), ([Issue #2946](https://github.com/apollographql/router/issues/2946))

Certain invalid HTTP requests — such as when an unacceptable `content-type` header, or an unsupported `accept` header are received — now return proper GraphQL errors nested as elements in a top-level `errors` array, rather than returning a single GraphQL error JSON object, which was unintentional.

This also introduced a more semantically-correct error code, `INVALID_CONTENT_TYPE_HEADER`, rather than using `INVALID_ACCEPT_HEADER` when an invalid `content-type` header was received.

By [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) in https://github.com/apollographql/router/pull/2947