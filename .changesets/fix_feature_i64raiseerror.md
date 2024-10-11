### Return a specific error when attempting to format an Int and it turns out to be bigger than I32 ([PR #6143](https://github.com/apollographql/router/pull/6143))

Improves the behaviour of handling non-spec compliant integer values coming from subgraphs. Instead of just nulling out the value with no feedback, it will now provide an error like `Cannot represent value 51049694213 as a i32`

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/6143
