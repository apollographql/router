### Replace Rhai specific hot-reload functionality with general hot-reload ([PR #6950](https://github.com/apollographql/router/pull/6950))

In Router 2.0 the rhai hot-reload capability was not working. The cause was the architectural improvements to the router which meant that the entire service stack is no longer re-created for each request.

The fix adds the rhai source files into the primary list of elements, configuration, schema, etc..., watched by the router and removes the old Rhai-specific file watching logic.

If --hot-reload is enabled, the router will reload on changes to Rhai source code just like it would for changes to configuration, for example.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/6950