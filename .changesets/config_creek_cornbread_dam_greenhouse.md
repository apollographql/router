### Move `apq` to top level in router.yaml ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/2744))

For improved usability we will be moving items out of `supergraph` in router.yaml. This is because various plugins use stages as part of their yaml config.

```diff
< apq:
<   enabled: true
---
> supergraph:
>   apq:
>     enabled: true
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2745
