
### Optimize GraphQL instruments ([PR #5375](https://github.com/apollographql/router/pull/5375))

When processing selectors for GraphQL instruments, heap allocations should be avoided for optimal performance. This change removes Vec allocations that were previously performed per field, yielding significant performance improvements.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5375
