### Connection shutdown sometimes fails over hot reload ([PR #8169](https://github.com/apollographql/router/pull/8169))

A race in the way that connections were shutdown when a hot-reload is triggered meant that occasionally some connections 
were left in active state and never entered terminating state. This could cause OOMs over time as multiple pipelines are 
left active.

This is now fixed and connections that are opening at the same time as shutdown will immediately terminate.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8169
