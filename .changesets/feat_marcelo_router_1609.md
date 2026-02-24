### Marcelo/router 1609 ([PR #8915](https://github.com/apollographql/router/pull/8915))

This pull request introduces a new startup validation in the Apollo Router to block startup if certain OpenTelemetry (OTEL) environment variables are set, as part of a new policy (ROUTER-1609). It also removes a previous warning about OTEL variable precedence and adds comprehensive tests for the new validation logic.

**OTEL Environment Variable Validation**

* Added a constant array `FORBIDDEN_OTEL_VARS` listing OTEL environment variables that must not be set: `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, and `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` (`apollo-router/src/executable.rs`).
* Implemented `Opt::validate_otel_envs_not_present()` to check for the presence of forbidden OTEL variables, returning an error if any are set and blocking router startup (`apollo-router/src/executable.rs`) [[1]](diffhunk://#diff-f157a81480ffeda854898c919ba73523c13c683c57532d5fa6545ef703846650R291-R307) [[2]](diffhunk://#diff-f157a81480ffeda854898c919ba73523c13c683c57532d5fa6545ef703846650R497-R500).

**Removal of Deprecated Warning**

* Removed the warning that previously notified users when `OTEL_EXPORTER_OTLP_ENDPOINT` was set, since the new validation now blocks startup instead of just warning (`apollo-router/src/executable.rs`).

**Testing**

* Added tests to ensure the new validation logic works as intended, including cases where none, one, or all forbidden variables are set (`apollo-router/src/executable.rs`).<!-- start metadata -->

<!-- [ROUTER-####] -->
---

**Checklist**

Complete the checklist (and note appropriate exceptions) before the PR is marked ready-for-review.

- [ ] PR description explains the motivation for the change and relevant context for reviewing
- [ ] PR description links appropriate GitHub/Jira tickets (creating when necessary)
- [ ] Changeset is included for user-facing changes
- [ ] Changes are compatible[^1]
- [ ] Documentation[^2] completed
- [ ] Performance impact assessed and acceptable
- [ ] Metrics and logs are added[^3] and documented
- Tests added and passing[^4]
    - [ ] Unit tests
    - [ ] Integration tests
    - [ ] Manual tests, as necessary

**Exceptions**

*Note any exceptions here*

**Notes**

[^1]: It may be appropriate to bring upcoming changes to the attention of other (impacted) groups. Please endeavour to do this before seeking PR approval. The mechanism for doing this will vary considerably, so use your judgement as to how and when to do this.
[^2]: Configuration is an important part of many changes. Where applicable please try to document configuration examples.
[^3]: A lot of (if not most) features benefit from built-in observability and `debug`-level logs. Please read [this guidance](https://github.com/apollographql/router/blob/dev/dev-docs/metrics.md#adding-new-metrics) on metrics best-practices.
[^4]: Tick whichever testing boxes are applicable. If you are adding Manual Tests, please document the manual testing (extensively) in the Exceptions.

By [@OriginLeon](https://github.com/OriginLeon) in https://github.com/apollographql/router/pull/8915