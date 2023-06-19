### Simplify router service tests ([PR #3259](https://github.com/apollographql/router/pull/3259))

Parts of the router service creation were generic, to allow mocking, but the `TestHarness` API allows us to reuse the same code in all cases. We can remove some generic types and simplify the API

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3259