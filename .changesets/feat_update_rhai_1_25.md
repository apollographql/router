### Update `rhai` scripting engine to v1.25.x ([PR #9527](https://github.com/apollographql/router/pull/9527))

The bundled [Rhai](https://rhai.rs) scripting engine has been updated from `~1.23.6` to `~1.25.0` (including the patch `1.25.1`).

Notable changes in this range that may affect router scripts:

- **`stdweb` support removed (1.24.0):** The `stdweb` feature flag is gone; WASM targets now use [`web-time`](https://crates.io/crates/web-time) instead of the unmaintained [`instant`](https://crates.io/crates/instant) crate. This is unlikely to affect typical router Rhai scripts.
- **String methods `split` / `split_rev` are now `pure`:** They can be called on `const` strings without error.
- **`\${...}` escape in multi-line literal strings:** You can now write `\${...}` in a multi-line literal string to produce the literal text `${...}` instead of triggering interpolation.
- **`index_of` fallback for string arguments:** When no matching script function is registered, `index_of` now falls back to value comparison for string arguments.

No changes to the router's Rhai API surface are expected. If your scripts relied on the old `stdweb` feature or specific `sort` behavior on non-totally-ordered arrays (which previously could panic), review your scripts before upgrading.

By [@renovate](https://github.com/apps/renovate) in https://github.com/apollographql/router/pull/9527
