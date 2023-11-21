### Configurable JWKS Poll Interval ([Issue #4185](https://github.com/apollographql/router/issues/4185))

The poll interval was previously hardcoded to 60 seconds. It is still the default now, but can be configured through the new `poll_interval` configuration option under each JWKS entry to avoid becoming rate-limited per endpoint.

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/4212