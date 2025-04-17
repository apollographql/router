### Fix metric `apollo.router.operations.batching.size` ([PR #7306](https://github.com/apollographql/router/pull/7306))

Correct the calculation of the `apollo.router.operations.batching.size` metric to reflect accurate batch sizes rather than occasionally returning fractional numbers.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7306