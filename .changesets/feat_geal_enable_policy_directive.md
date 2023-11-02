### GraphOS authorization directives: policy directive ([PR #3751](https://github.com/apollographql/router/pull/3751))

> ⚠️ This is an Enterprise feature of the Apollo Router. It requires an organization with a GraphOS Enterprise plan.
> 
> If your organization doesn't currently have an Enterprise plan, you can test out this functionality by signing up for a free Enterprise trial.

We introduce a new GraphOS authorization directive called `@policy`, that is designed to offload authorization policy execution to a coprocessor or Rhai script. it extracts from the query the list of relevant policies, the coprocessor indicates which of those policies failed, then the router filters unauthorized fields, as it does with `@authenticated` and `@requiresScopes`. If you want to know more, check out the [documentation](https://www.apollographql.com/docs/router/configuration/authorization#authenticated).

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3751