### Don't include metric events in spans ([PR #5649](https://github.com/apollographql/router/pull/5649))

Previously some internal metric events were included in traces and spans. This PR remove this noise.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5649