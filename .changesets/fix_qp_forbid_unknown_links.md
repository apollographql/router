### Fixed native query planner regression not forbidding unknown spec links

The legacy JavaScript query planner forbids any usage of unknown `@link` specs in supergraph schemas with either `EXECUTION` or `SECURITY` value set for the `for` argument (aka, the spec's "purpose"). This behavior had not been ported to the native query planner previously. This PR implements the expected behavior in the native query planner.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/7587

