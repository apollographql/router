### isolate uses of hyper 0.14 types ([PR #5175](https://github.com/apollographql/router/pull/5175))

[Hyper](https://hyper.rs/), the HTTP client and server used in the Router, recently reached version 1.0, with various improvements that we need in the Router, but also some API breaking changes.
To reduce the impact of these breaking changes when we will update the Router, we isolated uses of hyper's types in the Router, to make sure that we do not get stuck updating it all over the code.

This should have no impact on the current public API of the Router, it mainly affects internal code and should not affect the Router's normal execution.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5175