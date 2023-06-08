### chore: split out router events into its own module ([PR #3235](https://github.com/apollographql/router/pull/3235))

Breaks down `./apollo-router/src/router.rs` into its own module `./apollo-router/src/mod.rs` with a sub-module `./apollo-router/src/events/mod.rs` that contains all of the streams that we combine to start a router (entitlement, schema, reload, configuration, shutdown, more streams to be added). This change makes adding new events/modifying existing events a bit easier since it's not in one huge giant file to rule them all.

By [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) in https://github.com/apollographql/router/pull/3235
