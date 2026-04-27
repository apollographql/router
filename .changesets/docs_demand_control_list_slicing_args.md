### Document list-type argument support for `slicingArguments` in demand control ([PR #9196](https://github.com/apollographql/router/pull/9196))

The router supports using the length of list-type arguments as the cost multiplier in `@listSize(slicingArguments: [...])`, but this was not documented. Adds a new "List-type arguments in `slicingArguments`" subsection to the demand control docs with schema examples, query examples (both inline arrays and variables), and cost calculation breakdowns.

By [@shanemyrick](https://github.com/shanemyrick) in https://github.com/apollographql/router/pull/9196
