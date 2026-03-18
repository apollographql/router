### Set minimum `h2` version floor to 0.4.13 ([PR #9033](https://github.com/apollographql/router/pull/9033))

The `h2` crate is a transitive dependency (pulled in via `hyper`) and was not previously declared as an explicit workspace dependency. As a result, Renovate does not manage it and the version in `Cargo.lock` would remain pinned indefinitely unless someone manually ran `cargo update`.

Adding `h2` as an explicit workspace dependency with a minimum version of `0.4.13` ensures the router picks up the latest patch release, which includes bug fixes released January 5, 2026.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/9033
