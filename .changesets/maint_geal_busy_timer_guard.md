### use a Drop guard to track active requests ([PR #3343](https://github.com/apollographql/router/pull/3343))

manually tracking active requests is error prone because we might return early without decrementing the active requests. To make sure this is done properly, `enter_active_request` now returns a guard struct, that will decrement the count on drop

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3343