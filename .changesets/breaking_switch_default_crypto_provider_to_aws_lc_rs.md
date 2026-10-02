### Switch the default rustls crypto provider from `ring` to `aws-lc-rs` ([PR #9787](https://github.com/apollographql/router/pull/9787))

Upgrading to `opentelemetry-http` 0.32 pulled in a second major version of `reqwest` whose default TLS backend is `aws-lc-rs`, alongside the router's existing `ring`-based `reqwest`/`tonic`/`fred`/`aws-smithy-http-client` builds. Having two crypto backends in the dependency tree made rustls' own default-provider auto-detection ambiguous. Rather than keep both around, the whole workspace has been consolidated onto `aws-lc-rs`, and `ring` is no longer present anywhere in the dependency tree.

`apollo-router::main()` and the test-only crypto provider installer in `apollo-router::lib` now explicitly install `rustls::crypto::aws_lc_rs::default_provider()` instead of `rustls::crypto::ring::default_provider()` as the process-wide default rustls `CryptoProvider`.

This only affects consumers embedding `apollo-router` as a library:

- If your binary calls `rustls::crypto::ring::default_provider().install_default()` (or otherwise assumes `ring` is the installed provider) before or after calling into `apollo-router`, the two installs will race, and whichever runs first wins — `rustls::crypto::CryptoProvider::install_default` fails (returns an error, which the router ignores) if a provider is already installed. Update your own crypto-provider install to use `aws-lc-rs`, or remove it and rely on the router's install.
- If your binary depends directly on `ring`-backed features of `reqwest`, `tonic`, `fred`, or `aws-smithy-http-client`, switch to their `aws-lc-rs`-backed equivalents to avoid pulling in a second copy of `ring` alongside the router's `aws-lc-rs` build.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9787
