### Use `cargo build --release` in DIY `Dockerfile.repo` for deterministic, lockfile-respecting builds ([PR #8983](https://github.com/apollographql/router/pull/8983))

The DIY `Dockerfile.repo` previously used `cargo install --path apollo-router`, which does not honor `Cargo.lock` — resulting in non-deterministic dependency resolution and builds that could diverge from what CI produces.

Switching to `cargo build --release -p apollo-router` ensures the lockfile is respected and the DIY build path more closely aligns with CI.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/8983
