### Stop holding the Rhai `scope` lock for the duration of non-curried callbacks ([ROUTER-1876](https://apollographql.atlassian.net/browse/ROUTER-1876))

Non-curried Rhai callbacks (`Fn("name")`, as opposed to closures) ran against a single `Scope` shared across every stage and every subgraph for the life of the router's Rhai instance, guarded by one mutex. That mutex was held for the callback's *entire* execution, serializing every concurrent non-curried callback against every other one -- including sibling subgraph requests fanning out from the same client request.

The router now holds that lock only long enough to clone the scope, then runs the callback against the private clone. Non-curried callbacks that read module-level constants (`apollo_sdl`, `Router.APOLLO_SDL`, etc.) are unaffected. Scripts relying on a named function's mutation of a global variable persisting into the *next* callback invocation -- an undocumented and untested pattern -- will no longer see that mutation persist; each call now sees a fresh copy of the scope as it stood after the script's own top-level initialization.

Closures remain unaffected and are still the recommended pattern for avoiding this lock (see [the Rhai docs](/graphos/routing/customization/rhai#curried-vs-non-curried-callback-functions)); this change reduces the cost of the alternative rather than replacing the recommendation.

By [@rohan-b99](https://github.com/rohan-b99)
