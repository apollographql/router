### Enforce feature restrictions for warning-state licenses

The router now enforces license restrictions even when a license is in a warning state. Previously, warning-state licenses could bypass enforcement for restricted features.

If your deployment uses restricted features, the router returns an error instead of continuing to run.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8768
