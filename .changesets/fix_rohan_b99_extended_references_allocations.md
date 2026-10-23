### Extended reference reporting uses less CPU and memory per request ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

With `telemetry.apollo.metrics_reference_mode: extended`, the router records which input object fields and enum values each request uses. That work now runs about three times faster and allocates far less memory. In benchmarks, an operation that mixes literal arguments, variables, nested input objects, lists and enums went from 11.1 µs and 505 allocations to 4.0 µs and 65 allocations per request. A variable holding a list of 200 input objects went from 288 µs and 13,685 allocations to 88 µs and 60 allocations.

You don't need to change anything.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
