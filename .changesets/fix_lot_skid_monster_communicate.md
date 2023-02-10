### Add missing `status` attribute on some metrics ([PR #2593](https://github.com/apollographql/router/pull/2593))

When it created a status code 500 we didn't provide the `status` attribute in metrics. Instead of having an empty `status` attribute on your metrics you'll have `status="500"`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2593
