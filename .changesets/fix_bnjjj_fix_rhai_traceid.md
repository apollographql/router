### Provide a valid trace id in rhai scripts even if the trace isn't sampled ([PR #5606](https://github.com/apollographql/router/pull/5606))

Before, when calling `traceid()` in a rhai script, if the trace wasn't sampled you won't get the traceid. It's now fixed and you'll get trace id even if the trace is not sampled.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5606