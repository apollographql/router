### fix(subscription): do not log an error when the websocket stream has been interrupted, keep it in trace level to avoid useless noises ([PR #8344](https://github.com/apollographql/router/pull/8344))

Convert log from `error` level to `trace` when the websocket stream has been interrupted to avoid useless noises.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8344