### refactor: move license enforcement into a tower layer ([PR #9774](https://github.com/apollographql/router/pull/9774))

License enforcement (halting requests with a 500 when a license is expired, and rate-limited logging of expiry warnings) previously lived in an `axum::middleware::from_fn_with_state` handler. It's now a plain `tower::Layer`/`Service` pair (`LicenseLayer`/`LicenseService`), following the same pattern already used for `FileUploadLayer`. This is an internal implementation change with no effect on router behavior, configuration, or responses.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9774
