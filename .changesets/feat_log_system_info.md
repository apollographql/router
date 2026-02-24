### feat: add router system info module and diagnostics improvements ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

Adds a single source of truth for router metadata and diagnostics:

- **New `apollo-router/src/info.rs`**: Centralizes static router metadata (version, OS, arch), startup options (redacted for display), config/supergraph path and hash, and Router-relevant environment variable handling. Exposes `RouterSystemInfo`, `StartupOptions`, and helpers for set env var names and safe values for diagnostics.
- **Diagnostics plugin**: System info, export, and service modules now use the shared info module; diagnostics UI and data-access logic updated to surface router info, startup options, and env vars (with safe/redacted display).
- **Executable**: Captures and sets router system info at startup for use by diagnostics and support workflows.

By [@shanemyrick](https://github.com/shanemyrick) in https://github.com/apollographql/router/pull/PULL_NUMBER
