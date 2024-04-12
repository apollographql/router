### Experimental integration with the new Rust query planner

This starts the integration into the Router of the new query planner, being ported from TypeScript to Rust.
Experimental configuration opts into running the new planner,
or running both and checking that they return the same query plan.
The latter is an important part of our testing strategy, to ensure correctness of the port.

**Note:** the new planner is not yet ready for testing at this point.

Nothing changes for the default configuration.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/4948
