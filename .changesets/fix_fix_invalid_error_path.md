### Truncating Invalid Error Paths ([PR #6359](https://github.com/apollographql/router/pull/6359))

This fix addresses an issue where the router was silently dropping subgraph errors that included invalid paths.
 
According to the [GraphQL Specification](https://spec.graphql.org/draft/#sel-GAPHRPHCAACCpC8-T) an error path must point to a **response field**:
> If an error can be associated to a particular field in the GraphQL result, it must contain an entry with the key path that details the path of the response field which experienced the error.

If a subgraph error includes a path that can't be matched to a response field, the router now truncates the path to the nearest valid field path. 

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6359
