### Log whether safe-listing enforcement was skipped ([Issue #7509](https://github.com/apollographql/router/issues/7509))

When logging unknown operations encountered during safe-listing, include information about whether enforcement was skipped. This will help distinguish between truly problematic external operations (where `enforcement_skipped` is false) and internal operations that are intentionally allowed to bypass safelisting (where `enforcement_skipped` is true).

By [@DaleSeo](https://github.com/DaleSeo) in https://github.com/apollographql/router/pull/7509
