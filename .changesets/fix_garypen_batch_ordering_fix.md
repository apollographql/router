### Maintain query ordering within a batch ([Issue #4143](https://github.com/apollographql/router/issues/4143))

A bug in batch manipulation meant that the last element in a batch was treated as the first element. Ordering should be maintained and there is now an updated unit test to ensure this.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4144