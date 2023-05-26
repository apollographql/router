### Set the global allocator in the library crate, not just the executable ([Issue #3126](https://github.com/apollographql/router/issues/3126))

In 1.19, Apollo Router [switched to use jemalloc as the global Rust allocator on Linux](https://github.com/apollographql/router/blob/dev/CHANGELOG.md#improve-memory-fragmentation-and-resource-consumption-by-switching-to-jemalloc-as-the-memory-allocator-on-linux-pr-2882) to reduce memory fragmentation. However this is only done in the executable binary provided by the `apollo-router` crate, so that [custom binaries](https://www.apollographql.com/docs/router/customizations/custom-binary) using the crate as a library were not affected.

Instead, the `apollo-router` library crate now sets the global allocator so that custom binaries also take advantage of this by default. If some other choice is desired, the `global-allocator` Cargo feature flag can be disabled in `Cargo.toml` with:

```toml
[dependencies]
apollo-router = {version = "[…]", default-features = false}
```

Library crates that depend on `apollo-router` (if any) should also do this in order to leave the choice to the eventual executable. (Cargo default features are only disabled if *all* dependents specify `default-features = false`.)

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/3157
