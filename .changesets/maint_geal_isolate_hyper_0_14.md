### isolate uses of hyper 0.14 types ([PR #5175](https://github.com/apollographql/router/pull/5175))

[Hyper](https://hyper.rs/), the HTTP client and server used in the Router, recently reached version 1.0, with various improvements that we need in the Router, but also some API breaking changes.
To reduce the future impact of these breaking changes we have isolated uses of hyper's types. This will make future upgrades simpler.

This will have no impact on the current public API of the Router, it mainly affects internal code and will not affect the Router's normal execution.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5175