### Ensure that batch entry contexts are correctly preserved ([PR #5162](https://github.com/apollographql/router/pull/5162))

Batch processing was not using contexts correctly. A representative context was chosen, the first item in a batch of items, and used to provide context functionality for all the generated responses.

The router now correctly preserves request contexts and uses them during response creation.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5162