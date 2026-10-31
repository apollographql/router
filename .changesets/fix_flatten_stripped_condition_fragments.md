### Drop no-op fragments left behind when the planner removes handled `@skip`/`@include` conditions

When a fetch is already gated by an `Include`/`Skip` condition node, the query planner removes the matching `@include`/`@skip` from the fetch's selections. If that left an inline fragment with no directives whose type condition was its parent's own type, the fragment was kept, producing redundant nesting such as:

```graphql
... on GridCard {
  ... on GridCard {
    display
  }
}
```

The planner now inlines such fragments into their parent, merging their selections with any siblings. Since fields under `@skip`/`@include` that use `@requires` started conditioning the `@requires` inputs on those directives, this pattern became common in entity fetches for those fields, adding plan size and planner cost (up to ~8% higher cost on large production plans). Top-level fragments of a fetch's selection set are left untouched, as entity fetch inputs and operations must start with one fragment per entity type.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10665
