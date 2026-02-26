### Marcelo/router 1609 ([PR #8915](https://github.com/apollographql/router/pull/8915))

This pull request introduces a new startup validation in the Apollo Router to block startup if certain OpenTelemetry (OTEL) environment variables are set, as part of a new policy (ROUTER-1609). It also removes a previous warning about OTEL variable precedence and adds comprehensive tests for the new validation logic.

**OTEL Environment Variable Validation**

* Added a constant array `FORBIDDEN_OTEL_VARS` listing OTEL environment variables that must not be set: `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, and `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` (`apollo-router/src/executable.rs`).
* Implemented `Opt::validate_otel_envs_not_present()` to check for the presence of forbidden OTEL variables, returning an error if any are set and blocking router startup (`apollo-router/src/executable.rs`) [[1]](diffhunk://#diff-f157a81480ffeda854898c919ba73523c13c683c57532d5fa6545ef703846650R291-R307) [[2]](diffhunk://#diff-f157a81480ffeda854898c919ba73523c13c683c57532d5fa6545ef703846650R497-R500).

**Removal of Deprecated Warning**

* Removed the warning that previously notified users when `OTEL_EXPORTER_OTLP_ENDPOINT` was set, since the new validation now blocks startup instead of just warning (`apollo-router/src/executable.rs`).

**Testing**

* Added tests to ensure the new validation logic works as intended, including cases where none, one, or all forbidden variables are set (`apollo-router/src/executable.rs`).<!-- start metadata -->


By [@OriginLeon](https://github.com/OriginLeon) in https://github.com/apollographql/router/pull/8915