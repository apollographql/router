### make sure the compression state is flushed ([Issue #3035](https://github.com/apollographql/router/issues/3035))

In some cases, the "finish" call to flush the compression state at the end does not flush the entire state, so it has to be called multiple times.

This fixes a regression introduced in 1.16.0 by [#2986](https://github.com/apollographql/router/pull/2986) where large responses would be cut short when compressed.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3037