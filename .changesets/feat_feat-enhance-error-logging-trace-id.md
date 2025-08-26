### fix(telemetry): improve error logging for custom trace_id generation ([PR #7910](https://github.com/apollographql/router/pull/7910))

#7909

This pull request improves logging in the `CustomTraceIdPropagator` implementation by enhancing the error message with additional context about the `trace_id` and the error.

Logging enhancement:

* [`apollo-router/src/plugins/telemetry/mod.rs`](diffhunk://#diff-37adf9e170c9b384f17336e5b5e5bf9cd94fd1d618b8969996a5ad56b635ace6L1927-R1927): Updated the error logging statement to include the `trace_id` and the error details as structured fields, providing more context for debugging.
<!-- start metadata -->

<!-- [ROUTER-####] -->
---

**Checklist**

Complete the checklist (and note appropriate exceptions) before the PR is marked ready-for-review.

- [x] PR description explains the motivation for the change and relevant context for reviewing
- [x] PR description links appropriate GitHub/Jira tickets (creating when necessary)
- [x] Changeset is included for user-facing changes
- [x] Changes are compatible[^1]
- [x] Documentation[^2] completed
- [x] Performance impact assessed and acceptable
- [x] Metrics and logs are added[^3] and documented
- Tests added and passing[^4]
    - [x] Unit tests
    - [ ] Integration tests
    - [ ] Manual tests, as necessary

**Exceptions**

*Note any exceptions here*

**Notes**

Performance impact is minimal since this change only affects error logging for invalid trace_ids, which occurs only when malformed trace_ids are provided in requests. The enhanced logging adds structured fields to existing error messages without introducing any runtime overhead for valid trace_ids.

Documentation was completed by adding context about trace_id format requirements and error handling in `docs/source/routing/observability/telemetry/index.mdx`, explaining the W3C Trace Context specification compliance and how the router handles incompatible or malformed trace_ids.

[^1]: It may be appropriate to bring upcoming changes to the attention of other (impacted) groups. Please endeavour to do this before seeking PR approval. The mechanism for doing this will vary considerably, so use your judgement as to how and when to do this.
[^2]: Configuration is an important part of many changes. Where applicable please try to document configuration examples.
[^3]: A lot of (if not most) features benefit from built-in observability and `debug`-level logs. Please read [this guidance](https://github.com/apollographql/router/blob/dev/dev-docs/metrics.md#adding-new-metrics) on metrics best-practices.
[^4]: Tick whichever testing boxes are applicable. If you are adding Manual Tests, please document the manual testing (extensively) in the Exceptions.

By [@juancarlosjr97](https://github.com/juancarlosjr97) in https://github.com/apollographql/router/pull/7910
