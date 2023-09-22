### Introduce OneShotAsyncCheckpoint ([PR #3819](https://github.com/apollographql/router/pull/3819))

The existing AsynCheckpoint requires `Clone` and thus introduces a need for Service Buffering which reduces the performance and resiliance of the router.

This new set of Services, Layers and utility functions removes the requirement for `Clone` and thus the requirement for service buffering.

Existing uses of AsyncCheckpoint within the router are replaced with OneShotAsyncCheckpoint along with the requirement to `buffer()` such services.

If you have a custom plugin that makes use of `AsyncCheckpoint`, we encourage you to migrate to `OneShotAsyncCheckpoint` and thus reduce the requirement for service buffering from your router.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3819