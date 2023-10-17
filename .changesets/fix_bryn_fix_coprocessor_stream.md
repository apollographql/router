### Fix panic when streaming responses to co-processor ([Issue #4013](https://github.com/apollographql/router/issues/4013))

Streamed responses will no longer cause a panic in the co-processor plugin. This affected defer and stream queries.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4014
