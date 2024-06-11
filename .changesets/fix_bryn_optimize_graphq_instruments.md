### Optimize graphql instruments ([PR #5375](https://github.com/apollographql/router/pull/5375))

When processing selectors for graphql instruments we need to make sure we perform no heap allocation otherwise performance is affected.

This PR removes Vec allocations that were being performed on every field yielding a significant improvement in performance. 

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5375
