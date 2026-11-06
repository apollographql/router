### Keep query planner condition exclusions duplicate-free ([PR #10367](https://github.com/apollographql/router/pull/10367))

The set of conditions excluded while resolving a `@key`/`@requires` condition is compared as a set, but adding a condition twice appended a duplicate, which made equality asymmetric and could let the condition resolver cache return a resolution computed for a different set of exclusions. Adding an already-excluded condition is now a no-op. This is a defensive fix: current planner paths never add a duplicate, so query plans do not change.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10367
