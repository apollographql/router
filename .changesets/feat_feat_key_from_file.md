### feat: allow users to load apollo key from file ([PR #5917](https://github.com/apollographql/router/pull/5917))

Users sometimes would rather not pass sensitive keys to the router through environment variables out of an abundance of caution. To help address this, you can now pass an argument `--apollo-key-path` or env var `APOLLO_KEY_PATH`, that takes a file location as an argument which is read and then used as the Apollo key for use with Uplink and usage reporting.

This addresses a portion of #3264, specifically the APOLLO_KEY.

Note: This feature is not available on Windows.

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/5917
