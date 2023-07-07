### fix(subscription): take the callback url path from the configuration ([Issue #3361](https://github.com/apollographql/router/issues/3361))

Previously when you specified the `subscription.mode.callback.path` it was not used, we had an hardcoded value set to `/callback`. It's now using the specified path in the configuration

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3366
