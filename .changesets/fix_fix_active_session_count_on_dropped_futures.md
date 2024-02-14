### Fix active_session_count when future is dropped ([Issue #4601](https://github.com/apollographql/router/issues/4601))

Fixes [an issue](https://github.com/apollographql/router/issues/4601) where `apollo_router_session_count_active` would increase indefinitely due
to the request future getting dropped before a counter could be decremented.

By [@xuorig](https://github.com/xuorig) in https://github.com/apollographql/router/pull/4619