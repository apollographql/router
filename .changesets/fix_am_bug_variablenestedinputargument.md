### Prevent connectors error when using a variable in a nested input argument ([PR #7472](https://github.com/apollographql/router/pull/7472))

The connectors plugin will no longer error when using a variable in a nested input argument. The following example would error prior to this change:

```graphql
query Query ($query: String){
    complexInputType(filters: { inSpace: true, search: $query })
}
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/7472
