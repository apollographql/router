### Stop holding the Rhai `scope` lock for the duration of non-curried callbacks  ([PR #9825](https://github.com/apollographql/router/pull/9825))

Non-curried Rhai callbacks (`Fn("name")`, as opposed to closures) ran against a single `Scope` shared across every stage and every subgraph for the life of the router's Rhai instance, guarded by one mutex. That mutex was held for the callback's *entire* execution, serializing every concurrent non-curried callback against every other one - including sibling subgraph requests fanning out from the same client request.

The router now holds that lock only long enough to clone the scope, then runs the callback against the private clone. Non-curried callbacks that read module-level constants (`apollo_sdl`, `Router.APOLLO_SDL`, etc.) are unaffected.

> **Warning**
>
> Global variable mutations in non-curried callbacks (`Fn("name")`) no longer persist across invocations. Each call runs against a private clone of the scope, so a callback's writes to a global are discarded when it returns. Only the router's own `apollo_sdl` / `apollo_start` constants and whatever the script's top-level statements populated at startup are guaranteed visible to later calls. Scripts that relied on a named function mutating a global and reading that mutation in a subsequent callback — an undocumented and untested pattern — will need to be updated (for example, by using curried closures, which remain unaffected).

Closures remain unaffected and are still the recommended pattern for avoiding this lock (see [the Rhai docs](https://www.apollographql.com/docs/graphos/routing/customization/rhai#curried-vs-non-curried-callback-function)); this change reduces the cost of the alternative rather than replacing the recommendation.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9825
