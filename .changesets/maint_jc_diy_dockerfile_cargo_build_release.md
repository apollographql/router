### Use `cargo build --locked --release` in DIY `Dockerfile.repo` for deterministic, lockfile-respecting builds ([PR #8983](https://github.com/apollographql/router/pull/8983))

The DIY `Dockerfile.repo` previously used `cargo install --path apollo-router`, which doesn't enforce the versions in `Cargo.lock` — resulting in possible non-deterministic dependency resolution and builds that could diverge from what CI produces.

Using `cargo build --locked --release -p apollo-router` ensures the versions in the lockfile are respected and the DIY build path more closely aligns with CI.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/8983
