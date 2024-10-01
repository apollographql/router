### Support loading Apollo key from file ([PR #5917](https://github.com/apollographql/router/pull/5917))

You can now specific the location to a file containing the Apollo key that's used by Apollo Uplink and usage reporting. The router now supports both the `--apollo-key-path` CLI argument and the `APOLLO_KEY_PATH` environment variable for passing the file containing your Apollo key.

Previously, the router supported only the `APOLLO_KEY` environment variable to provide the key. The new CLI argument and environment variable help users who prefer not to pass sensitive keys through environment variables.

Note: This feature is unavailable for Windows.

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/5917
