### Fix telemetry events when trace isn't sampled and preserve attribute types ([PR #5464](https://github.com/apollographql/router/pull/5464))

Improves accuracy and performance of event telemetry by:

- Displaying custom event attributes even if the trace is not sampled 
- Preserving original attribute type instead of converting it to string
- Ensuring `http.response.body.size` and `http.request.body.size` attributes are treated as numbers, not strings

> :warning: Exercise caution if you have monitoring enabled on your logs, as attribute types may have changed. For example, attributes like `http.response.status_code` are now numbers (`200`) instead of strings (`"200"`). 


By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5464