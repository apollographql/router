### Isolate usage of hyper v0.14 types for future compatibility ([PR #5175](https://github.com/apollographql/router/pull/5175))

Isolates usage of [hyper](https://hyper.rs/) types in response to the recent release of hyper v1.0. The new major version introduced improvements along with breaking changes. The goal is to reduce the impact of these breaking changes, and ensure that future upgrades are straightforward.

This change only affects internal code and doesn't affect the router's public API or execution.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5175