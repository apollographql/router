### Log deferred responses dropped after a client disconnect at debug level

When a client disconnected or a request timed out before every part of an `@defer` response was sent, the router logged `error sending deferred response at path …: SendError { .. }` at `ERROR` for each remaining deferred part. This actually isn't an error, but an expected situation during regular operation.
Busy routers serving `@defer` traffic could log hundreds of thousands of these lines a day and inflate error-log counts and alerts. The message is now logged at `DEBUG`:
```
could not send deferred response at …
```

By [@shachar-ash](https://github.com/shachar-ash) in https://github.com/apollographql/router/pull/10832
