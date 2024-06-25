### Add mapping for the router span on Datadog ([Issue #5282](https://github.com/apollographql/router/issues/5282))

Add a new span mapping for datadog for the router span as in `spec_compliant` mode we don't provide the `request` span anymore.
After this change the `router` span name will be mapped to `http.router` attribute name.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5386