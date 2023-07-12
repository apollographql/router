### Synthesize defer labels without RNG or collisions ([PR #3381](https://github.com/apollographql/router/pull/3381) and [PR #3423](https://github.com/apollographql/router/pull/3423))

The `@defer` directive accepts a `label` argument, but it is optional. To more accurately handle deferred responses, the Router internally rewrites queries to add labels on the `@defer` directive where they are missing. Responses eventually receive the reverse treatment to look as expected by client.

This was done be generating random strings, handling collision with existing labels, and maintaining a `HashSet` of which labels had been synthesized. Instead, we now add a prefix to pre-existing labels and generate new labels without it. When processing a response, the absence of that prefix indicates a synthetic label.

By [@SimonSapin](https://github.com/SimonSapin) and [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/3381 and https://github.com/apollographql/router/pull/3423