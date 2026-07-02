### Fix Windows wall-clock race in `uplink::license_stream::test_to_instant` ([Issue/PR #9497](https://github.com/apollographql/router/pull/9497))

`test_to_instant` asserted `past_instant < Instant::now()` after computing `past_instant` via `to_positive_instant`. On Windows the monotonic clock advances at ~16 ms ticks, so the two `Instant::now()` reads inside the same tick return the same value and the strict `<` fails. Loosen to `<=` (the function's actual contract is "≥ now at call time, ≤ now after"). Same T9 wall-clock class as ROUTER-1825's `router_overhead::tracker::test_no_subgraph_requests` fix.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
