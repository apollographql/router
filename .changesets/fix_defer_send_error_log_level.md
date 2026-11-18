### Log deferred responses dropped after a client disconnect at debug level

When a client disconnected before every part of an `@defer` response was sent, the router logged `error sending deferred response at path …: SendError { .. }` at `ERROR` for each remaining deferred part. The send only fails once the response stream's receiver is gone, which means the client has already left, so there is nothing for an operator to act on. Busy routers serving `@defer` traffic could log hundreds of thousands of these lines a day and inflate error-log counts and alerts. The message is now logged at `DEBUG`.

By [@shachar-ash](https://github.com/shachar-ash) in https://github.com/apollographql/router/pull/10832
