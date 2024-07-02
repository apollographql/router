### Add new values for the supergraph `query` selector ([PR #5433](https://github.com/apollographql/router/pull/5433))

Adds support for four new values for the supergraph [`query` selector](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/selectors#supergraph):
 - `aliases`: the number of aliases in the query
 - `depth`: the depth of the query
 - `height`: the height of the query
 - `root_fields`: the number of root fields in the query

You can use this data to understand how your graph is used and to help determine where to set limits.

For example:

```router.yaml
telemetry:
  instrumentation:
    instruments:
      supergraph:
        'query.depth':
          description: 'The depth of the query'
          value:
            query: depth
          unit: unit
          type: histogram
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5433
